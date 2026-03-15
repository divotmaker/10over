//! Convert 10over shot data types to FRP domain types.

use flightrelay::types::{BallFlight, ClubData as FrpClubData};
use flightrelay::units::Velocity;

use crate::proto::{BallData, ClubData};

/// Convert [`BallData`] to an FRP [`BallFlight`].
///
/// The R10 provides launch speed, angles, and spin. It does not compute
/// carry distance, total distance, max height, or flight time — those
/// fields are left as `None`.
#[must_use]
pub fn ball_flight(b: &BallData) -> BallFlight {
    BallFlight {
        launch_speed: Some(Velocity::MetersPerSecond(f64::from(b.ball_speed))),
        launch_azimuth: Some(f64::from(b.launch_direction)),
        launch_elevation: Some(f64::from(b.launch_angle)),
        carry_distance: None,
        total_distance: None,
        roll_distance: None,
        max_height: None,
        flight_time: None,
        #[allow(clippy::cast_possible_truncation)]
        backspin_rpm: Some(b.backspin.round() as i32),
        #[allow(clippy::cast_possible_truncation)]
        sidespin_rpm: Some(b.sidespin.round() as i32),
    }
}

/// Convert [`ClubData`] to FRP [`ClubData`](FrpClubData).
///
/// The R10 provides club head speed, face angle, path, and attack angle.
/// Smash factor, swing plane, offset, and height are not available.
#[must_use]
pub fn club_data(c: &ClubData) -> FrpClubData {
    FrpClubData {
        club_speed: Some(Velocity::MetersPerSecond(f64::from(c.club_head_speed))),
        club_speed_post: None,
        path: Some(f64::from(c.path_angle)),
        attack_angle: Some(f64::from(c.attack_angle)),
        face_angle: Some(f64::from(c.face_angle)),
        dynamic_loft: None,
        smash_factor: None,
        swing_plane_horizontal: None,
        swing_plane_vertical: None,
        club_offset: None,
        club_height: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::SpinCalcType;

    #[test]
    fn ball_flight_conversion() {
        let ball = BallData {
            launch_angle: 14.2,
            launch_direction: -1.3,
            ball_speed: 67.2,
            spin_axis: 5.0,
            total_spin: 2800.0,
            backspin: 2789.4,
            sidespin: 244.0,
            spin_calc_type: SpinCalcType::Ratio,
        };

        let frp = ball_flight(&ball);
        assert_eq!(
            frp.launch_speed,
            Some(Velocity::MetersPerSecond(f64::from(67.2_f32)))
        );
        assert_eq!(frp.launch_elevation, Some(f64::from(14.2_f32)));
        assert_eq!(frp.launch_azimuth, Some(f64::from(-1.3_f32)));
        assert_eq!(frp.backspin_rpm, Some(2789));
        assert_eq!(frp.sidespin_rpm, Some(244));
        // R10 doesn't provide these
        assert_eq!(frp.carry_distance, None);
        assert_eq!(frp.total_distance, None);
        assert_eq!(frp.flight_time, None);
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
    }
}
