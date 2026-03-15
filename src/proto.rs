//! Re-exports prost-generated protobuf types and provides helpers for
//! building/decoding the Smart container messages used by the R10.

// Include the prost-generated code for all packages.
// Generated code triggers clippy warnings we can't fix.
#[allow(clippy::doc_markdown, clippy::must_use_candidate)]
pub mod smart {
    include!(concat!(env!("OUT_DIR"), "/gdi.proto.smart.rs"));
}

#[allow(clippy::doc_markdown, clippy::must_use_candidate)]
pub mod event_sharing {
    include!(concat!(env!("OUT_DIR"), "/gdi.proto.event_sharing.rs"));
}

#[allow(clippy::doc_markdown, clippy::must_use_candidate)]
pub mod launch_monitor {
    include!(concat!(env!("OUT_DIR"), "/gdi.proto.launch_monitor.rs"));
}

use prost::Message;

/// Decoded shot data from the R10.
///
/// All speeds are in m/s, all angles in degrees, spin in RPM.
/// Timing values are absolute device timestamps in milliseconds.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ShotData {
    pub shot_id: u32,
    pub shot_type: ShotType,
    pub ball: Option<BallData>,
    pub club: Option<ClubData>,
    pub swing: Option<SwingData>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ShotType {
    Practice,
    Normal,
}

#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BallData {
    /// Vertical launch angle (degrees).
    pub launch_angle: f32,
    /// Horizontal launch direction (degrees).
    pub launch_direction: f32,
    /// Initial ball velocity (m/s).
    pub ball_speed: f32,
    /// Spin axis tilt (degrees).
    pub spin_axis: f32,
    /// Total spin rate (RPM).
    pub total_spin: f32,
    /// Backspin component (RPM). Computed: `total_spin * cos(spin_axis)`.
    pub backspin: f32,
    /// Sidespin component (RPM). Computed: `total_spin * sin(spin_axis)`.
    pub sidespin: f32,
    /// How spin was determined.
    pub spin_calc_type: SpinCalcType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SpinCalcType {
    Ratio,
    BallFlight,
    Other,
    Measured,
}

#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ClubData {
    /// Club head velocity at impact (m/s).
    pub club_head_speed: f32,
    /// Face angle at impact (degrees).
    pub face_angle: f32,
    /// Club path angle (degrees).
    pub path_angle: f32,
    /// Angle of attack (degrees).
    pub attack_angle: f32,
}

#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SwingData {
    /// Backswing start (ms, absolute device time).
    pub backswing_start: u32,
    /// Downswing start (ms).
    pub downswing_start: u32,
    /// Impact moment (ms).
    pub impact: u32,
    /// Follow-through end (ms).
    pub follow_through_end: u32,
}

/// R10 device state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum DeviceState {
    Standby,
    InterferenceTest,
    Waiting,
    Recording,
    Processing,
    Error,
}

impl DeviceState {
    #[must_use]
    pub fn from_proto(v: i32) -> Option<Self> {
        match v {
            0 => Some(Self::Standby),
            1 => Some(Self::InterferenceTest),
            2 => Some(Self::Waiting),
            3 => Some(Self::Recording),
            4 => Some(Self::Processing),
            5 => Some(Self::Error),
            _ => None,
        }
    }
}

/// R10 device error.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DeviceError {
    pub code: ErrorCode,
    pub severity: ErrorSeverity,
    pub tilt: Option<(f32, f32)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ErrorCode {
    Unknown,
    Overheating,
    RadarSaturation,
    PlatformTilted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ErrorSeverity {
    Warning,
    Serious,
    Fatal,
}

// ── Smart message builders ──

/// Build a Subscribe(LAUNCH_MONITOR) Smart message.
#[must_use]
pub fn build_subscribe_request() -> Vec<u8> {
    use event_sharing::{
        AlertMessage, AlertType, EventSharingService, SubscribeRequest,
    };
    use smart::Smart;

    let subscribe = SubscribeRequest {
        alerts: vec![AlertMessage {
            r#type: Some(AlertType::LaunchMonitor.into()),
            interval: None,
        }],
        target_distance: None,
    };

    let es = EventSharingService {
        subscribe_request: Some(subscribe),
        subscribe_response: None,
        alert_notification: None,
        support_request: None,
        support_response: None,
    };

    let smart = Smart {
        event_sharing: Some(es),
        launch_monitor_service: None,
    };

    smart.encode_to_vec()
}

/// Build a WakeUpRequest Smart message.
#[must_use]
pub fn build_wakeup_request() -> Vec<u8> {
    use launch_monitor::{Service, WakeUpRequest};
    use smart::Smart;

    let lm = Service {
        status_request: None,
        status_response: None,
        wake_up_request: Some(WakeUpRequest {}),
        wake_up_response: None,
        tilt_request: None,
        tilt_response: None,
        start_tilt_cal_request: None,
        start_tilt_cal_response: None,
        reset_tilt_cal_request: None,
        reset_tilt_cal_response: None,
        shot_config_request: None,
        shot_config_response: None,
    };

    let smart = Smart {
        event_sharing: None,
        launch_monitor_service: Some(lm),
    };

    smart.encode_to_vec()
}

/// Outcome of decoding a Smart protobuf message from the R10.
#[derive(Debug, Clone)]
pub enum SmartEvent {
    /// Subscribe response received (success/fail).
    SubscribeResponse { success: bool },
    /// WakeUp response.
    WakeUpResponse { status: i32 },
    /// Device state change.
    StateChange(DeviceState),
    /// Shot data received.
    Shot(ShotData),
    /// Device error.
    Error(DeviceError),
    /// Calibration status update.
    CalibrationStatus { status: i32, result: i32 },
    /// Launch monitor response (status, tilt, shot config, etc.).
    LaunchMonitorResponse,
    /// Unknown or unhandled Smart message.
    Unknown,
}

/// Decode a Smart protobuf message and return a high-level event.
///
/// # Errors
///
/// Returns `Err` if protobuf deserialization fails.
pub fn decode_smart(pb_data: &[u8]) -> Result<SmartEvent, prost::DecodeError> {
    let smart = smart::Smart::decode(pb_data)?;

    // EventSharing extension (field 30)
    if let Some(es) = &smart.event_sharing {
        if let Some(resp) = &es.subscribe_response {
            let success = resp
                .alert_status
                .first()
                .is_some_and(|s| s.subscribe_status() == event_sharing::subscribe_response::alert_status_message::Status::Success);
            return Ok(SmartEvent::SubscribeResponse { success });
        }

        if let Some(notif) = &es.alert_notification {
            return Ok(decode_alert_notification(notif));
        }

        return Ok(SmartEvent::Unknown);
    }

    // LaunchMonitor extension (field 38)
    if let Some(lm) = &smart.launch_monitor_service {
        if let Some(resp) = &lm.wake_up_response {
            return Ok(SmartEvent::WakeUpResponse {
                status: resp.status.unwrap_or(0),
            });
        }
        return Ok(SmartEvent::LaunchMonitorResponse);
    }

    Ok(SmartEvent::Unknown)
}

fn decode_alert_notification(notif: &event_sharing::AlertNotification) -> SmartEvent {
    // AlertDetails is encoded in prost as an optional field on AlertNotification
    // via the extension mechanism. With prost, extensions become regular fields.
    let Some(details) = &notif.details else {
        return SmartEvent::Unknown;
    };

    // Shot data
    if let Some(metrics) = &details.metrics {
        return SmartEvent::Shot(decode_metrics(metrics));
    }

    // State change
    if let Some(ds) = details
        .state
        .as_ref()
        .and_then(|s| s.state)
        .and_then(DeviceState::from_proto)
    {
        return SmartEvent::StateChange(ds);
    }

    // Error
    if let Some(err) = &details.error {
        return SmartEvent::Error(decode_error(err));
    }

    // Calibration
    if let Some(cal) = &details.tilt_calibration {
        return SmartEvent::CalibrationStatus {
            status: cal.status.unwrap_or(0),
            result: cal.result.unwrap_or(0),
        };
    }

    SmartEvent::Unknown
}

fn decode_metrics(m: &launch_monitor::Metrics) -> ShotData {
    let ball = m.ball_metrics.as_ref().map(|b| {
        let axis_rad = b.spin_axis.unwrap_or(0.0).to_radians();
        let total = b.total_spin.unwrap_or(0.0);
        BallData {
            launch_angle: b.launch_angle.unwrap_or(0.0),
            launch_direction: b.launch_direction.unwrap_or(0.0),
            ball_speed: b.ball_speed.unwrap_or(0.0),
            spin_axis: b.spin_axis.unwrap_or(0.0),
            total_spin: total,
            backspin: total * axis_rad.cos(),
            sidespin: total * axis_rad.sin(),
            spin_calc_type: match b.spin_calculation_type.unwrap_or(0) {
                0 => SpinCalcType::Ratio,
                1 => SpinCalcType::BallFlight,
                3 => SpinCalcType::Measured,
                _ => SpinCalcType::Other,
            },
        }
    });

    let club = m.club_metrics.as_ref().map(|c| ClubData {
        club_head_speed: c.club_head_speed.unwrap_or(0.0),
        face_angle: c.club_angle_face.unwrap_or(0.0),
        path_angle: c.club_angle_path.unwrap_or(0.0),
        attack_angle: c.attack_angle.unwrap_or(0.0),
    });

    let swing = m.swing_metrics.as_ref().map(|s| SwingData {
        backswing_start: s.back_swing_start_time.unwrap_or(0),
        downswing_start: s.down_swing_start_time.unwrap_or(0),
        impact: s.impact_time.unwrap_or(0),
        follow_through_end: s.follow_through_end_time.unwrap_or(0),
    });

    ShotData {
        shot_id: m.shot_id.unwrap_or(0),
        shot_type: if m.shot_type.unwrap_or(0) == 1 {
            ShotType::Normal
        } else {
            ShotType::Practice
        },
        ball,
        club,
        swing,
    }
}

fn decode_error(e: &launch_monitor::Error) -> DeviceError {
    DeviceError {
        code: match e.code.unwrap_or(0) {
            1 => ErrorCode::Overheating,
            2 => ErrorCode::RadarSaturation,
            3 => ErrorCode::PlatformTilted,
            _ => ErrorCode::Unknown,
        },
        severity: match e.severity.unwrap_or(0) {
            1 => ErrorSeverity::Serious,
            2 => ErrorSeverity::Fatal,
            _ => ErrorSeverity::Warning,
        },
        tilt: e
            .device_tilt
            .as_ref()
            .map(|t| (t.roll.unwrap_or(0.0), t.pitch.unwrap_or(0.0))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscribe_request_encodes() {
        let data = build_subscribe_request();
        assert!(!data.is_empty());
        // Should round-trip through Smart decode
        let smart = smart::Smart::decode(data.as_slice()).unwrap();
        let es = smart.event_sharing.unwrap();
        let req = es.subscribe_request.unwrap();
        assert_eq!(req.alerts.len(), 1);
    }

    #[test]
    fn wakeup_request_encodes() {
        let data = build_wakeup_request();
        assert!(!data.is_empty());
        let smart = smart::Smart::decode(data.as_slice()).unwrap();
        assert!(smart.launch_monitor_service.unwrap().wake_up_request.is_some());
    }

    #[test]
    fn decode_shot_metrics() {
        use launch_monitor::*;

        let metrics = Metrics {
            shot_id: Some(42),
            shot_type: Some(1), // NORMAL
            ball_metrics: Some(BallMetrics {
                launch_angle: Some(12.5),
                launch_direction: Some(-1.2),
                ball_speed: Some(67.0),    // m/s
                spin_axis: Some(5.0),      // degrees
                total_spin: Some(2800.0),  // RPM
                spin_calculation_type: Some(0), // RATIO
                golf_ball_type: Some(1),   // CONVENTIONAL
            }),
            club_metrics: Some(ClubMetrics {
                club_head_speed: Some(44.0),
                club_angle_face: Some(1.5),
                club_angle_path: Some(-2.0),
                attack_angle: Some(-3.5),
            }),
            swing_metrics: Some(SwingMetrics {
                back_swing_start_time: Some(1000),
                down_swing_start_time: Some(1500),
                impact_time: Some(1800),
                follow_through_end_time: Some(2200),
                end_recording_time: None,
            }),
        };

        let shot = decode_metrics(&metrics);
        assert_eq!(shot.shot_id, 42);
        assert_eq!(shot.shot_type, ShotType::Normal);

        let ball = shot.ball.unwrap();
        assert!((ball.ball_speed - 67.0).abs() < 0.01);
        assert!((ball.total_spin - 2800.0).abs() < 0.01);
        // back ≈ 2800 * cos(5°) ≈ 2789.4
        assert!((ball.backspin - 2789.4).abs() < 1.0);
        // side ≈ 2800 * sin(5°) ≈ 244.0
        assert!((ball.sidespin - 244.0).abs() < 1.0);

        let club = shot.club.unwrap();
        assert!((club.club_head_speed - 44.0).abs() < 0.01);
        assert!((club.face_angle - 1.5).abs() < 0.01);

        let swing = shot.swing.unwrap();
        assert_eq!(swing.impact, 1800);
    }
}
