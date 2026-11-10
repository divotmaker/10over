//! Convert tenover shot data to FRP types.

use flightrelay::types::{ClubData as FrpClubData, Launch};
use flightrelay::units::Velocity;

use crate::proto::{BallData, ClubData};

/// Convert [`BallData`] to an FRP [`Launch`].
///
/// The R10 reports launch speed, both launch angles, total spin and spin
/// axis. Total spin and axis are sent with the backspin and sidespin
/// computed from them, all four as measured values. A total spin of 0 is
/// sent as 0 with no axis. The R10 reports no flight result.
///
/// `aim_offset_deg` ([`Config::aim_offset_deg`](crate::config::Config::aim_offset_deg))
/// is added to the launch direction. [`BallData`] stays in the device's
/// frame; only the FRP output is corrected. Spin is not changed.
#[must_use]
#[allow(clippy::cast_possible_truncation)] // spin rates fit an i32
pub fn launch(b: &BallData, aim_offset_deg: f64) -> Launch {
    let total_spin = b.total_spin.round() as i32;
    Launch {
        launch_speed: Some(Velocity::MetersPerSecond(f64::from(b.ball_speed))),
        launch_azimuth: Some(aimed(f64::from(b.launch_direction), aim_offset_deg)),
        launch_elevation: Some(f64::from(b.launch_angle)),
        backspin_rpm: Some(b.backspin.round() as i32),
        sidespin_rpm: Some(b.sidespin.round() as i32),
        total_spin_rpm: Some(total_spin),
        spin_axis: (total_spin != 0).then_some(f64::from(b.spin_axis)),
    }
}

/// `angle` turned by the aim offset; exactly `angle` when the offset is 0.
fn aimed(angle: f64, aim_offset_deg: f64) -> f64 {
    if aim_offset_deg == 0.0 {
        angle
    } else {
        angle + aim_offset_deg
    }
}

/// Whether the device tracked the club: it reports a face angle, path and
/// attack angle of exactly 0 together when it did not (a slow swing).
#[allow(clippy::float_cmp)] // untracked club angles are exact zeros
fn club_tracked(c: &ClubData) -> bool {
    !(c.face_angle == 0.0 && c.path_angle == 0.0 && c.attack_angle == 0.0)
}

/// Convert [`ClubData`] to FRP [`ClubData`](FrpClubData).
///
/// The R10 provides club head speed, face angle, path, and attack angle.
/// Smash factor, swing plane, offset, height, spin loft, and low point are
/// not available.
///
/// `aim_offset_deg` ([`Config::aim_offset_deg`](crate::config::Config::aim_offset_deg))
/// is added to the face angle and path, the horizontal angles, when the club
/// was tracked. A club whose face angle, path and attack angle are all
/// exactly 0 was not tracked: those zeros are not measurements, so they are
/// sent as they are. The attack angle is vertical and never shifts.
/// [`ClubData`] stays in the device's frame; only the FRP output is
/// corrected.
#[must_use]
pub fn club_data(c: &ClubData, aim_offset_deg: f64) -> FrpClubData {
    let aim = if club_tracked(c) { aim_offset_deg } else { 0.0 };
    FrpClubData {
        club_speed: Some(Velocity::MetersPerSecond(f64::from(c.club_head_speed))),
        path: Some(aimed(f64::from(c.path_angle), aim)),
        attack_angle: Some(f64::from(c.attack_angle)),
        face_angle: Some(aimed(f64::from(c.face_angle), aim)),
        ..FrpClubData::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::SpinCalcType;

    fn launch_plain(b: &BallData) -> Launch {
        launch(b, 0.0)
    }

    /// Ball data as the decoder builds it: components from total and axis.
    fn ball_data(total_spin: f32, spin_axis: f32) -> BallData {
        let axis_rad = spin_axis.to_radians();
        BallData {
            launch_angle: 14.2,
            launch_direction: -1.3,
            ball_speed: 67.2,
            spin_axis,
            total_spin,
            backspin: total_spin * axis_rad.cos(),
            sidespin: total_spin * axis_rad.sin(),
            spin_calc_type: SpinCalcType::Estimated,
        }
    }

    #[test]
    fn launch_conversion() {
        let frp = launch_plain(&ball_data(2800.0, 5.0));
        assert_eq!(
            frp.launch_speed,
            Some(Velocity::MetersPerSecond(f64::from(67.2_f32)))
        );
        assert_eq!(frp.launch_elevation, Some(f64::from(14.2_f32)));
        assert_eq!(frp.launch_azimuth, Some(f64::from(-1.3_f32)));
        assert_eq!(frp.backspin_rpm, Some(2789));
        assert_eq!(frp.sidespin_rpm, Some(244));
        assert_eq!(frp.total_spin_rpm, Some(2800));
        assert_eq!(frp.spin_axis, Some(5.0));
    }

    #[test]
    fn spin_axis_sign_matches_sidespin() {
        // Axis tilted right: ball curves right, sidespin positive.
        let right = launch_plain(&ball_data(3000.0, 12.0));
        assert_eq!(right.spin_axis, Some(12.0));
        assert!(right.sidespin_rpm.unwrap() > 0);
        // Axis tilted left: ball curves left, sidespin negative.
        let left = launch_plain(&ball_data(3000.0, -12.0));
        assert_eq!(left.spin_axis, Some(-12.0));
        assert!(left.sidespin_rpm.unwrap() < 0);
    }

    #[test]
    fn spin_pairs_agree() {
        let frp = launch_plain(&ball_data(4321.0, -17.5));
        let mut components = Launch {
            backspin_rpm: frp.backspin_rpm,
            sidespin_rpm: frp.sidespin_rpm,
            ..Launch::default()
        };
        components.complete_spin();
        assert_eq!(components.total_spin_rpm, frp.total_spin_rpm);
        let axis = components.spin_axis.unwrap();
        assert!((axis - frp.spin_axis.unwrap()).abs() < 0.05);
    }

    #[test]
    fn zero_total_spin_sends_total_zero_and_no_axis() {
        let frp = launch_plain(&ball_data(0.0, 3.0));
        assert_eq!(frp.total_spin_rpm, Some(0));
        assert_eq!(frp.spin_axis, None);
        assert_eq!(frp.backspin_rpm, Some(0));
        assert_eq!(frp.sidespin_rpm, Some(0));
    }

    #[test]
    fn club_data_conversion() {
        let club = ClubData {
            club_head_speed: 44.0,
            face_angle: 1.5,
            path_angle: -2.0,
            attack_angle: -3.5,
        };

        let frp = club_data(&club, 0.0);
        assert_eq!(
            frp.club_speed,
            Some(Velocity::MetersPerSecond(f64::from(44.0_f32)))
        );
        assert_eq!(frp.face_angle, Some(f64::from(1.5_f32)));
        assert_eq!(frp.path, Some(f64::from(-2.0_f32)));
        assert_eq!(frp.attack_angle, Some(f64::from(-3.5_f32)));
        // R10 doesn't provide these
        assert_eq!(frp.club_speed_post, None);
        assert_eq!(frp.smash_factor, None);
        assert_eq!(frp.spin_loft, None);
        assert_eq!(frp.low_point_distance, None);
    }

    fn club(face: f32, path: f32, attack: f32) -> ClubData {
        ClubData {
            club_head_speed: 30.0,
            face_angle: face,
            path_angle: path,
            attack_angle: attack,
        }
    }

    #[test]
    fn the_aim_offset_shifts_the_launch_azimuth_only() {
        let ball = ball_data(2800.0, 5.0);
        let plain = launch(&ball, 0.0);
        let aimed = launch(&ball, 2.0);
        assert_eq!(
            aimed.launch_azimuth,
            Some(f64::from(-1.3_f32) + 2.0),
            "added, + = right"
        );
        let negative = launch(&ball, -1.0);
        assert_eq!(negative.launch_azimuth, Some(f64::from(-1.3_f32) - 1.0));
        // Nothing else moves.
        assert_eq!(
            Launch {
                launch_azimuth: plain.launch_azimuth,
                ..aimed
            },
            plain
        );
    }

    #[test]
    fn no_aim_offset_is_the_device_value_exactly() {
        let mut ball = ball_data(2800.0, 5.0);
        ball.launch_direction = -0.0;
        let frp = launch(&ball, 0.0);
        let azimuth = frp.launch_azimuth.unwrap();
        assert_eq!(azimuth.to_bits(), (-0.0_f64).to_bits());
        let tracked = club_data(&club(1.5, -2.0, -3.5), 0.0);
        assert_eq!(tracked.face_angle, Some(f64::from(1.5_f32)));
        assert_eq!(tracked.path, Some(f64::from(-2.0_f32)));
    }

    #[test]
    fn a_tracked_club_shifts_face_and_path_but_not_attack() {
        let frp = club_data(&club(1.5, -2.0, -3.5), 2.5);
        assert_eq!(frp.face_angle, Some(f64::from(1.5_f32) + 2.5));
        assert_eq!(frp.path, Some(f64::from(-2.0_f32) + 2.5));
        assert_eq!(frp.attack_angle, Some(f64::from(-3.5_f32)));
        assert_eq!(
            frp.club_speed,
            Some(Velocity::MetersPerSecond(f64::from(30.0_f32)))
        );
    }

    #[test]
    fn a_club_with_one_nonzero_angle_is_tracked() {
        // Face and path read 0 on a tracked club that happens to be square.
        let frp = club_data(&club(0.0, 0.0, -4.0), 1.0);
        assert_eq!(frp.face_angle, Some(1.0));
        assert_eq!(frp.path, Some(1.0));
        assert_eq!(frp.attack_angle, Some(-4.0));
    }

    #[test]
    fn an_untracked_club_is_left_alone() {
        let frp = club_data(&club(0.0, 0.0, 0.0), 3.0);
        assert_eq!(frp.face_angle, Some(0.0));
        assert_eq!(frp.path, Some(0.0));
        assert_eq!(frp.attack_angle, Some(0.0));
        assert_eq!(
            frp.club_speed,
            Some(Velocity::MetersPerSecond(f64::from(30.0_f32)))
        );
    }
}
