//! Shot data types, plus helpers for building and decoding the protobuf
//! envelope messages used by the R10.

// prost-generated code for tenover's R10 schema (`proto/*.proto`). Internal:
// the public API is the wrapper types and functions below. Generated code
// triggers lints we can't fix, and declares messages the crate never builds.
#[allow(
    dead_code,
    clippy::doc_markdown,
    clippy::must_use_candidate,
    clippy::trivially_copy_pass_by_ref
)]
pub(crate) mod envelope {
    include!(concat!(env!("OUT_DIR"), "/tenover.r10.envelope.rs"));
}

#[allow(
    dead_code,
    clippy::doc_markdown,
    clippy::must_use_candidate,
    clippy::trivially_copy_pass_by_ref
)]
pub(crate) mod alerts {
    include!(concat!(env!("OUT_DIR"), "/tenover.r10.alerts.rs"));
}

#[allow(
    dead_code,
    clippy::doc_markdown,
    clippy::must_use_candidate,
    clippy::trivially_copy_pass_by_ref
)]
pub(crate) mod monitor {
    include!(concat!(env!("OUT_DIR"), "/tenover.r10.monitor.rs"));
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
    pub kind: ShotKind,
    pub ball: Option<BallData>,
    pub club: Option<ClubData>,
    pub swing: Option<SwingData>,
}

/// Whether the device flagged a shot as a warmup swing or a regular one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ShotKind {
    Warmup,
    Regular,
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
    /// Spin axis tilt (degrees), + = tilted right (the ball curves right).
    /// The device reports the opposite sign; the decoder negates it.
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
    /// Estimated from the other launch values (the R10's default with an
    /// unmarked ball).
    Estimated,
    /// Derived from the tracked ball flight.
    FlightModel,
    /// Any other spin source the device reports.
    Other,
    /// Measured directly from a marked ball.
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
    NoiseCheck,
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
            1 => Some(Self::NoiseCheck),
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
    NotLevel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ErrorSeverity {
    Warning,
    Serious,
    Fatal,
}

// ── Envelope message builders ──

/// Build the envelope that subscribes to the R10's shot-data alerts.
#[must_use]
pub fn build_subscribe_request() -> Vec<u8> {
    use alerts::{AlertChannel, SubscribeCommand, Topic, TopicSelection};

    let subscribe = SubscribeCommand {
        topics: vec![TopicSelection {
            topic: Some(Topic::ShotData.into()),
        }],
    };

    envelope::Envelope {
        alerts: Some(AlertChannel {
            subscribe: Some(subscribe),
            ..AlertChannel::default()
        }),
        monitor: None,
    }
    .encode_to_vec()
}

/// Environment settings sent to the R10 for its internal flight model.
///
/// All fields are optional in the protocol.
#[derive(Debug, Clone, Copy, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Environment {
    /// Ambient temperature (°C).
    pub temperature: Option<f32>,
    /// Relative humidity (0.0–1.0, not percent).
    pub humidity: Option<f32>,
    /// Altitude (meters).
    pub altitude: Option<f32>,
    /// Air density (kg/m³).
    pub air_density: Option<f32>,
    /// Distance from the device to the tee (yards).
    pub tee_distance_yd: Option<f32>,
}

fn monitor_envelope(channel: monitor::MonitorChannel) -> Vec<u8> {
    envelope::Envelope {
        alerts: None,
        monitor: Some(channel),
    }
    .encode_to_vec()
}

/// Build the envelope carrying a [`Environment`] (environment settings).
#[must_use]
pub fn build_environment_request(config: &Environment) -> Vec<u8> {
    use monitor::{EnvironmentSettings, MonitorChannel};

    monitor_envelope(MonitorChannel {
        environment: Some(EnvironmentSettings {
            air_temperature_c: config.temperature,
            relative_humidity: config.humidity,
            elevation_m: config.altitude,
            air_density_kg_m3: config.air_density,
            tee_distance_yd: config.tee_distance_yd,
        }),
        ..MonitorChannel::default()
    })
}

/// Build the envelope that wakes (arms) the R10.
#[must_use]
pub fn build_wakeup_request() -> Vec<u8> {
    use monitor::{MonitorChannel, WakeCommand};

    monitor_envelope(MonitorChannel {
        wake: Some(WakeCommand {}),
        ..MonitorChannel::default()
    })
}

/// Outcome of decoding a protobuf envelope from the R10.
#[derive(Debug, Clone)]
pub enum EnvelopeEvent {
    /// Reply to the shot-data subscription (accepted or rejected).
    SubscribeResult { success: bool },
    /// Reply to the wake (arm) command. `status` 0 = woken, 1 = already
    /// active from a previous session, 2 = failed.
    WakeResult { status: i32 },
    /// Device state change.
    StateChange(DeviceState),
    /// Shot data received.
    Shot(ShotData),
    /// Device error.
    Error(DeviceError),
    /// Leveling check report. `verdict` 1 = level, 2 = releveling advised,
    /// 3 = releveling needed; `attempt` 0 = ok, 1 = failed, 2 = device moved.
    LevelCheck { verdict: i32, attempt: i32 },
    /// Reply to an environment ([`Environment`]) update.
    EnvironmentResult { success: bool },
    /// Any other launch monitor reply (status, leveling, etc.).
    MonitorReply,
    /// Unknown or unhandled envelope.
    Unknown,
}

/// Decode a protobuf envelope from the R10 and return a high-level event.
///
/// # Errors
///
/// Returns `Err` if protobuf deserialization fails.
pub fn decode_envelope(pb_data: &[u8]) -> Result<EnvelopeEvent, prost::DecodeError> {
    let env = envelope::Envelope::decode(pb_data)?;

    // Alert channel (field 30)
    if let Some(channel) = &env.alerts {
        if let Some(result) = &channel.subscribe_result {
            let success = result.outcomes.first().is_some_and(|o| {
                o.outcome() == alerts::subscribe_result::topic_outcome::Outcome::Accepted
            });
            return Ok(EnvelopeEvent::SubscribeResult { success });
        }

        if let Some(notification) = &channel.notification {
            return Ok(decode_notification(notification));
        }

        return Ok(EnvelopeEvent::Unknown);
    }

    // Launch monitor channel (field 38)
    if let Some(channel) = &env.monitor {
        if let Some(result) = &channel.wake_result {
            return Ok(EnvelopeEvent::WakeResult {
                status: result.outcome.unwrap_or(0),
            });
        }
        if let Some(result) = &channel.environment_result {
            return Ok(EnvelopeEvent::EnvironmentResult {
                success: result.accepted.unwrap_or(false),
            });
        }
        return Ok(EnvelopeEvent::MonitorReply);
    }

    Ok(EnvelopeEvent::Unknown)
}

fn decode_notification(notification: &alerts::Notification) -> EnvelopeEvent {
    let Some(report) = &notification.monitor else {
        return EnvelopeEvent::Unknown;
    };

    // Shot data
    if let Some(shot) = &report.shot {
        return EnvelopeEvent::Shot(decode_metrics(shot));
    }

    // State change
    if let Some(ds) = report
        .status
        .as_ref()
        .and_then(|s| s.phase)
        .and_then(DeviceState::from_proto)
    {
        return EnvelopeEvent::StateChange(ds);
    }

    // Error
    if let Some(fault) = &report.fault {
        return EnvelopeEvent::Error(decode_error(fault));
    }

    // Leveling (tilt calibration)
    if let Some(level) = &report.level {
        return EnvelopeEvent::LevelCheck {
            verdict: level.verdict.unwrap_or(0),
            attempt: level.attempt.unwrap_or(0),
        };
    }

    EnvelopeEvent::Unknown
}

fn decode_metrics(m: &monitor::ShotReadout) -> ShotData {
    let ball = m.ball.as_ref().map(|b| {
        // The device's tilt is + for a ball curving left (a closed face to
        // path tilts it +); negate it to + = right like the launch direction.
        let axis = -b.spin_tilt_deg.unwrap_or(0.0);
        let axis_rad = axis.to_radians();
        let total = b.spin_rpm.unwrap_or(0.0);
        BallData {
            launch_angle: b.vertical_angle_deg.unwrap_or(0.0),
            launch_direction: b.horizontal_angle_deg.unwrap_or(0.0),
            ball_speed: b.speed_mps.unwrap_or(0.0),
            spin_axis: axis,
            total_spin: total,
            backspin: total * axis_rad.cos(),
            sidespin: total * axis_rad.sin(),
            spin_calc_type: match b.spin_source.unwrap_or(0) {
                0 => SpinCalcType::Estimated,
                1 => SpinCalcType::FlightModel,
                3 => SpinCalcType::Measured,
                _ => SpinCalcType::Other,
            },
        }
    });

    let club = m.club.as_ref().map(|c| ClubData {
        club_head_speed: c.head_speed_mps.unwrap_or(0.0),
        face_angle: c.face_deg.unwrap_or(0.0),
        path_angle: c.path_deg.unwrap_or(0.0),
        attack_angle: c.attack_deg.unwrap_or(0.0),
    });

    let swing = m.timing.as_ref().map(|t| SwingData {
        backswing_start: t.takeaway.unwrap_or(0),
        downswing_start: t.transition.unwrap_or(0),
        impact: t.impact.unwrap_or(0),
        follow_through_end: t.finish.unwrap_or(0),
    });

    ShotData {
        shot_id: m.shot_number.unwrap_or(0),
        kind: if m.kind.unwrap_or(0) == 1 {
            ShotKind::Regular
        } else {
            ShotKind::Warmup
        },
        ball,
        club,
        swing,
    }
}

fn decode_error(f: &monitor::Fault) -> DeviceError {
    DeviceError {
        code: match f.cause.unwrap_or(0) {
            1 => ErrorCode::Overheating,
            2 => ErrorCode::RadarSaturation,
            3 => ErrorCode::NotLevel,
            _ => ErrorCode::Unknown,
        },
        severity: match f.urgency.unwrap_or(0) {
            1 => ErrorSeverity::Serious,
            2 => ErrorSeverity::Fatal,
            _ => ErrorSeverity::Warning,
        },
        tilt: f
            .orientation
            .as_ref()
            .map(|o| (o.roll_deg.unwrap_or(0.0), o.pitch_deg.unwrap_or(0.0))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscribe_request_encodes() {
        let data = build_subscribe_request();
        assert!(!data.is_empty());
        // Should round-trip through envelope decode
        let env = envelope::Envelope::decode(data.as_slice()).unwrap();
        let req = env.alerts.unwrap().subscribe.unwrap();
        assert_eq!(req.topics.len(), 1);
        assert_eq!(req.topics[0].topic(), alerts::Topic::ShotData);
    }

    #[test]
    fn wakeup_request_encodes() {
        let data = build_wakeup_request();
        assert!(!data.is_empty());
        let env = envelope::Envelope::decode(data.as_slice()).unwrap();
        assert!(env.monitor.unwrap().wake.is_some());
    }

    #[test]
    fn environment_request_encodes_tee_distance() {
        let config = Environment {
            tee_distance_yd: Some(2.3),
            ..Environment::default()
        };
        let data = build_environment_request(&config);
        let env = envelope::Envelope::decode(data.as_slice()).unwrap();
        let req = env.monitor.unwrap().environment.unwrap();
        assert_eq!(req.tee_distance_yd, Some(2.3));
        assert!(req.air_temperature_c.is_none());
        assert!(req.relative_humidity.is_none());
        assert!(req.elevation_m.is_none());
        assert!(req.air_density_kg_m3.is_none());
    }

    #[test]
    fn environment_request_encodes_all_fields() {
        let config = Environment {
            temperature: Some(21.0),
            humidity: Some(0.45),
            altitude: Some(120.0),
            air_density: Some(1.2),
            tee_distance_yd: Some(6.0),
        };
        let data = build_environment_request(&config);
        let env = envelope::Envelope::decode(data.as_slice()).unwrap();
        let req = env.monitor.unwrap().environment.unwrap();
        assert_eq!(req.air_temperature_c, Some(21.0));
        assert_eq!(req.relative_humidity, Some(0.45));
        assert_eq!(req.elevation_m, Some(120.0));
        assert_eq!(req.air_density_kg_m3, Some(1.2));
        assert_eq!(req.tee_distance_yd, Some(6.0));
    }

    #[test]
    fn decode_environment_result() {
        use monitor::{EnvironmentResult, MonitorChannel};

        let data = monitor_envelope(MonitorChannel {
            environment_result: Some(EnvironmentResult {
                accepted: Some(true),
            }),
            ..MonitorChannel::default()
        });

        let event = decode_envelope(&data).unwrap();
        assert!(matches!(
            event,
            EnvelopeEvent::EnvironmentResult { success: true }
        ));
    }

    #[test]
    fn decode_shot_metrics() {
        use monitor::{BallLaunch, ClubDelivery, ShotReadout, SwingTiming};

        let metrics = ShotReadout {
            shot_number: Some(42),
            kind: Some(1), // regular
            ball: Some(BallLaunch {
                vertical_angle_deg: Some(12.5),
                horizontal_angle_deg: Some(-1.2),
                speed_mps: Some(67.0),
                spin_tilt_deg: Some(5.0),
                spin_rpm: Some(2800.0),
                spin_source: Some(0), // estimated
                marking: Some(1),     // plain ball
            }),
            club: Some(ClubDelivery {
                head_speed_mps: Some(44.0),
                face_deg: Some(1.5),
                path_deg: Some(-2.0),
                attack_deg: Some(-3.5),
            }),
            timing: Some(SwingTiming {
                takeaway: Some(1000),
                transition: Some(1500),
                impact: Some(1800),
                finish: Some(2200),
                capture_end: None,
            }),
        };

        let shot = decode_metrics(&metrics);
        assert_eq!(shot.shot_id, 42);
        assert_eq!(shot.kind, ShotKind::Regular);

        let ball = shot.ball.unwrap();
        assert!((ball.ball_speed - 67.0).abs() < 0.01);
        assert!((ball.total_spin - 2800.0).abs() < 0.01);
        // back ≈ 2800 * cos(5°) ≈ 2789.4
        assert!((ball.backspin - 2789.4).abs() < 1.0);
        // Wire tilt +5° is 5° left: side ≈ 2800 * sin(-5°) ≈ -244.0
        assert!((ball.sidespin + 244.0).abs() < 1.0);

        let club = shot.club.unwrap();
        assert!((club.club_head_speed - 44.0).abs() < 0.01);
        assert!((club.face_angle - 1.5).abs() < 0.01);

        let swing = shot.swing.unwrap();
        assert_eq!(swing.impact, 1800);
    }
}
