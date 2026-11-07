//! Frame builders for tests (`testing` feature).
//!
//! Builds the bytes a device or host puts on the wire, so a downstream
//! crate's tests can drive a [`Transport`](crate::Transport) or an
//! [`IdentityFilter`](crate::capture::IdentityFilter) without knowing the
//! frame layout. All identity values are synthetic.

use prost::Message;

use crate::gfdi::{self, MSG_DEVICE_INFO};
use crate::multilink;
use crate::proto::{alerts, envelope, monitor};

/// Unit ID carried by [`device_info_frame`]. Synthetic.
pub const DEVICE_INFO_UNIT_ID: u32 = 0x5EED_1D01;

/// Bluetooth friendly name carried by [`device_info_frame`]. Synthetic.
pub const DEVICE_INFO_NAME: &str = "Approach R10 SYNTH";

/// A GFDI frame as it crosses the wire, standard header:
/// `[0x00] COBS([length] [msg_type] [payload] [crc]) [0x00]`.
#[must_use]
pub fn encode_frame(msg_type: u16, payload: &[u8]) -> Vec<u8> {
    gfdi::wrap_cobs(&gfdi::build_frame(msg_type, payload))
}

/// An ACK of `acked_type` with `status` and an optional response payload,
/// as it crosses the wire.
#[must_use]
pub fn ack_frame(acked_type: u16, status: u8, payload: &[u8]) -> Vec<u8> {
    gfdi::wrap_cobs(&gfdi::build_ack(acked_type, status, payload))
}

/// A device information message as the device sends it, carrying
/// [`DEVICE_INFO_UNIT_ID`] and [`DEVICE_INFO_NAME`].
#[must_use]
pub fn device_info_frame() -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&150u16.to_le_bytes()); // protocol version
    p.extend_from_slice(&119u16.to_le_bytes()); // product number
    p.extend_from_slice(&DEVICE_INFO_UNIT_ID.to_le_bytes());
    p.extend_from_slice(&1u16.to_le_bytes()); // software version
    p.extend_from_slice(&512u16.to_le_bytes()); // max packet size
    for name in [DEVICE_INFO_NAME.as_bytes(), b"R10", b"Approach R10"] {
        #[allow(clippy::cast_possible_truncation)]
        p.push(name.len() as u8); // all names < 256 bytes
        p.extend_from_slice(name);
    }
    encode_frame(MSG_DEVICE_INFO, &p)
}

/// `bytes` split into BLE chunks of at most `mtu` bytes, each led by the
/// MultiLink `handle` byte. Empty for an `mtu` below 2.
#[must_use]
pub fn chunks(handle: u8, bytes: &[u8], mtu: usize) -> Vec<Vec<u8>> {
    multilink::chunk_with_handle(bytes, handle, mtu)
}

/// `bytes` as a single BLE chunk led by the MultiLink `handle` byte.
#[must_use]
pub fn with_handle(handle: u8, bytes: &[u8]) -> Vec<u8> {
    [&[handle][..], bytes].concat()
}

/// The MultiLink registration response granting `handle`, as the device
/// sends it on the register channel.
#[must_use]
pub fn register_response(handle: u8) -> Vec<u8> {
    let mut r = vec![0x00, 0x01];
    r.extend_from_slice(&1u64.to_le_bytes()); // transaction id
    r.extend_from_slice(&1u16.to_le_bytes()); // service: GFDI
    r.extend_from_slice(&[0x00, handle, 0x00]); // status success, handle, flags
    r
}

/// The device's capability message, as it crosses the wire.
#[must_use]
pub fn configuration_frame() -> Vec<u8> {
    let mut bitmap = [0u8; 13];
    bitmap[gfdi::CAP_SWING_SENSOR / 8] |= 1 << (gfdi::CAP_SWING_SENSOR % 8);
    let mut p = vec![13u8];
    p.extend_from_slice(&bitmap);
    encode_frame(gfdi::MSG_CONFIGURATION, &p)
}

/// A protobuf response message carrying `envelope` in one fragment, as it
/// crosses the wire.
#[must_use]
pub fn protobuf_frame(envelope: &[u8]) -> Vec<u8> {
    let len = u32::try_from(envelope.len()).unwrap_or(u32::MAX);
    let mut p = Vec::with_capacity(14 + envelope.len());
    p.extend_from_slice(&1u16.to_le_bytes()); // request id
    p.extend_from_slice(&0u32.to_le_bytes()); // offset
    p.extend_from_slice(&len.to_le_bytes()); // total length
    p.extend_from_slice(&len.to_le_bytes()); // fragment length
    p.extend_from_slice(envelope);
    encode_frame(gfdi::MSG_PROTOBUF_RESPONSE, &p)
}

/// An envelope holding a launch monitor report, as a notification.
fn report_envelope(report: monitor::MonitorReport) -> Vec<u8> {
    envelope::Envelope {
        alerts: Some(alerts::AlertChannel {
            notification: Some(alerts::Notification {
                topics: vec![alerts::Topic::ShotData.into()],
                monitor: Some(report),
            }),
            ..alerts::AlertChannel::default()
        }),
        monitor: None,
    }
    .encode_to_vec()
}

/// A regular shot with ball and club data: ball speed 67 m/s, launch 12.5°
/// up and 1.2° left, `total_spin_rpm` about `spin_axis_deg`; club speed 44
/// m/s, face 1.5°, path -2°, attack -3.5°.
#[must_use]
pub fn shot_envelope(shot_id: u32, total_spin_rpm: f32, spin_axis_deg: f32) -> Vec<u8> {
    report_envelope(monitor::MonitorReport {
        shot: Some(monitor::ShotReadout {
            shot_number: Some(shot_id),
            kind: Some(1),
            ball: Some(monitor::BallLaunch {
                vertical_angle_deg: Some(12.5),
                horizontal_angle_deg: Some(-1.2),
                speed_mps: Some(67.0),
                spin_tilt_deg: Some(spin_axis_deg),
                spin_rpm: Some(total_spin_rpm),
                spin_source: Some(0),
                marking: None,
            }),
            club: Some(monitor::ClubDelivery {
                head_speed_mps: Some(44.0),
                face_deg: Some(1.5),
                path_deg: Some(-2.0),
                attack_deg: Some(-3.5),
            }),
            timing: None,
        }),
        ..monitor::MonitorReport::default()
    })
}

/// A device state notification. `phase` is the wire value: 0 standby,
/// 1 noise check, 2 waiting, 3 recording, 4 processing, 5 error.
#[must_use]
pub fn state_envelope(phase: i32) -> Vec<u8> {
    report_envelope(monitor::MonitorReport {
        status: Some(monitor::DeviceStatus { phase: Some(phase) }),
        ..monitor::MonitorReport::default()
    })
}

/// A device error notification. `cause`: 1 overheating, 2 radar saturation,
/// 3 not level; `urgency`: 0 warning, 1 serious, 2 fatal; `tilt` is
/// `(roll, pitch)` in degrees.
#[must_use]
pub fn error_envelope(cause: i32, urgency: i32, tilt: Option<(f32, f32)>) -> Vec<u8> {
    report_envelope(monitor::MonitorReport {
        fault: Some(monitor::Fault {
            cause: Some(cause),
            urgency: Some(urgency),
            orientation: tilt.map(|(roll, pitch)| monitor::Orientation {
                roll_deg: Some(roll),
                pitch_deg: Some(pitch),
            }),
        }),
        ..monitor::MonitorReport::default()
    })
}

/// The reply to the shot-data subscription.
#[must_use]
pub fn subscribe_result_envelope(accepted: bool) -> Vec<u8> {
    use alerts::subscribe_result::{TopicOutcome, topic_outcome::Outcome};
    let outcome = if accepted {
        Outcome::Accepted
    } else {
        Outcome::Rejected
    };
    envelope::Envelope {
        alerts: Some(alerts::AlertChannel {
            subscribe_result: Some(alerts::SubscribeResult {
                outcomes: vec![TopicOutcome {
                    outcome: outcome.into(),
                    topic: None,
                }],
            }),
            ..alerts::AlertChannel::default()
        }),
        monitor: None,
    }
    .encode_to_vec()
}

/// The reply to the wake command: `status` 0 woken, 1 already active,
/// 2 failed.
#[must_use]
pub fn wake_result_envelope(status: i32) -> Vec<u8> {
    monitor_reply(monitor::MonitorChannel {
        wake_result: Some(monitor::WakeResult {
            outcome: Some(status),
        }),
        ..monitor::MonitorChannel::default()
    })
}

/// The reply to an environment update.
#[must_use]
pub fn environment_result_envelope(accepted: bool) -> Vec<u8> {
    monitor_reply(monitor::MonitorChannel {
        environment_result: Some(monitor::EnvironmentResult {
            accepted: Some(accepted),
        }),
        ..monitor::MonitorChannel::default()
    })
}

fn monitor_reply(channel: monitor::MonitorChannel) -> Vec<u8> {
    envelope::Envelope {
        alerts: None,
        monitor: Some(channel),
    }
    .encode_to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_info_frame_parses() {
        let f = device_info_frame();
        let frame = gfdi::parse_cobs_frame(&f[1..f.len() - 1]).unwrap();
        assert_eq!(frame.msg_type(), MSG_DEVICE_INFO);
        let info = gfdi::parse_device_info(frame.payload()).unwrap();
        assert_eq!(info.unit_id, DEVICE_INFO_UNIT_ID);
        assert_eq!(info.friendly_name, DEVICE_INFO_NAME);
    }

    #[test]
    fn ack_frame_names_its_type() {
        let f = ack_frame(gfdi::MSG_PROTOBUF_RESPONSE, 0, &[]);
        let frame = gfdi::parse_cobs_frame(&f[1..f.len() - 1]).unwrap();
        assert_eq!(frame.msg_type(), gfdi::MSG_ACK);
        assert_eq!(frame.acked_type(), Some(gfdi::MSG_PROTOBUF_RESPONSE));
        assert_eq!(frame.payload(), [0xB4, 0x13, 0x00]);
    }

    #[test]
    fn envelopes_decode() {
        use crate::proto::{DeviceState, EnvelopeEvent, ErrorCode, decode_envelope};
        let EnvelopeEvent::Shot(shot) = decode_envelope(&shot_envelope(7, 0.0, 4.0)).unwrap()
        else {
            panic!("expected a shot");
        };
        assert_eq!(shot.shot_id, 7);
        assert!(shot.ball.is_some() && shot.club.is_some());
        assert!(matches!(
            decode_envelope(&state_envelope(2)).unwrap(),
            EnvelopeEvent::StateChange(DeviceState::Waiting)
        ));
        let EnvelopeEvent::Error(e) =
            decode_envelope(&error_envelope(3, 2, Some((1.0, 2.0)))).unwrap()
        else {
            panic!("expected an error");
        };
        assert_eq!((e.code, e.tilt), (ErrorCode::NotLevel, Some((1.0, 2.0))));
        assert!(matches!(
            decode_envelope(&subscribe_result_envelope(true)).unwrap(),
            EnvelopeEvent::SubscribeResult { success: true }
        ));
        assert!(matches!(
            decode_envelope(&wake_result_envelope(1)).unwrap(),
            EnvelopeEvent::WakeResult { status: 1 }
        ));
        assert!(matches!(
            decode_envelope(&environment_result_envelope(false)).unwrap(),
            EnvelopeEvent::EnvironmentResult { success: false }
        ));
    }

    #[test]
    fn register_response_grants_the_handle() {
        let r = register_response(9);
        assert_eq!(
            multilink::parse_register_response(&r),
            Some((multilink::RegisterStatus::Success, 9, 0))
        );
    }

    #[test]
    fn chunks_lead_with_the_handle() {
        let c = chunks(0x07, &[1, 2, 3, 4, 5], 3);
        assert_eq!(c, [vec![7, 1, 2], vec![7, 3, 4], vec![7, 5]]);
        assert_eq!(with_handle(0x07, &[9]), [7, 9]);
    }
}
