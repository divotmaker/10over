//! Convert 10over shot data types to FRP domain types.

use flightrelay::types::{BallFlight, ClubData as FrpClubData};
use flightrelay::units::Velocity;

use crate::proto::{BallData, ClubData};

/// Convert [`BallData`] to an FRP [`BallFlight`].
///
/// The R10 provides launch speed, angles, and spin. It reports total spin and
/// spin axis; both are sent along with the backspin and sidespin components
/// computed from them. A total spin of 0 sends neither total spin nor axis.
/// It does not compute carry, landing, total distance, max height, or flight
/// time — those fields are left as `None`.
#[must_use]
#[allow(clippy::cast_possible_truncation)]
pub fn ball_flight(b: &BallData) -> BallFlight {
    let total_spin = b.total_spin.round() as i32;
    let spin_measured = total_spin > 0;
    BallFlight {
        launch_speed: Some(Velocity::MetersPerSecond(f64::from(b.ball_speed))),
        launch_azimuth: Some(f64::from(b.launch_direction)),
        launch_elevation: Some(f64::from(b.launch_angle)),
        backspin_rpm: Some(b.backspin.round() as i32),
        sidespin_rpm: Some(b.sidespin.round() as i32),
        total_spin_rpm: spin_measured.then_some(total_spin),
        spin_axis: spin_measured.then_some(f64::from(b.spin_axis)),
        ..BallFlight::default()
    }
}

/// Convert [`ClubData`] to FRP [`ClubData`](FrpClubData).
///
/// The R10 provides club head speed, face angle, path, and attack angle.
/// Smash factor, swing plane, offset, height, spin loft, and low point are
/// not available.
#[must_use]
pub fn club_data(c: &ClubData) -> FrpClubData {
    FrpClubData {
        club_speed: Some(Velocity::MetersPerSecond(f64::from(c.club_head_speed))),
        path: Some(f64::from(c.path_angle)),
        attack_angle: Some(f64::from(c.attack_angle)),
        face_angle: Some(f64::from(c.face_angle)),
        ..FrpClubData::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::SpinCalcType;

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
            spin_calc_type: SpinCalcType::Ratio,
        }
    }

    #[test]
    fn ball_flight_conversion() {
        let frp = ball_flight(&ball_data(2800.0, 5.0));
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
        // R10 doesn't provide these
        assert_eq!(frp.carry_distance, None);
        assert_eq!(frp.carry_offline, None);
        assert_eq!(frp.total_distance, None);
        assert_eq!(frp.total_offline, None);
        assert_eq!(frp.flight_time, None);
        assert_eq!(frp.descent_angle, None);
    }

    #[test]
    fn spin_axis_sign_matches_sidespin() {
        // Axis tilted right: ball curves right, sidespin positive.
        let right = ball_flight(&ball_data(3000.0, 12.0));
        assert_eq!(right.spin_axis, Some(12.0));
        assert!(right.sidespin_rpm.unwrap() > 0);
        // Axis tilted left: ball curves left, sidespin negative.
        let left = ball_flight(&ball_data(3000.0, -12.0));
        assert_eq!(left.spin_axis, Some(-12.0));
        assert!(left.sidespin_rpm.unwrap() < 0);
    }

    #[test]
    fn spin_pairs_agree() {
        let frp = ball_flight(&ball_data(4321.0, -17.5));
        let mut components = BallFlight {
            backspin_rpm: frp.backspin_rpm,
            sidespin_rpm: frp.sidespin_rpm,
            ..BallFlight::default()
        };
        components.complete_spin();
        assert_eq!(components.total_spin_rpm, frp.total_spin_rpm);
        let axis = components.spin_axis.unwrap();
        assert!((axis - frp.spin_axis.unwrap()).abs() < 0.05);
    }

    #[test]
    fn zero_total_spin_sends_neither_total_nor_axis() {
        let frp = ball_flight(&ball_data(0.0, 0.0));
        assert_eq!(frp.total_spin_rpm, None);
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

        let frp = club_data(&club);
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
}
