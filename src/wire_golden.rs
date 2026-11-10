//! Golden wire bytes for the protobuf layer.
//!
//! Every envelope builder is checked against its exact encoded bytes, and
//! every fixture frame below is decoded from raw bytes into the crate's
//! wrapper types. The tests use only the public wrapper API, so they hold the
//! wire format fixed independently of the schema's message and field names.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io;
use std::rc::Rc;

use crate::gfdi;
use crate::proto::{
    EnvelopeEvent, Environment, ErrorCode, ErrorSeverity, ShotData, ShotKind, SpinCalcType,
    build_environment_request, build_subscribe_request, build_wakeup_request, decode_envelope,
};
use crate::{Client, Event, Transport};

// ── Builders ──

const SUBSCRIBE: &[u8] = &[0xf2, 0x01, 0x06, 0x0a, 0x04, 0x0a, 0x02, 0x08, 0x08];
const WAKEUP: &[u8] = &[0xb2, 0x02, 0x02, 0x1a, 0x00];
const ENVIRONMENT_EMPTY: &[u8] = &[0xb2, 0x02, 0x02, 0x5a, 0x00];
const ENVIRONMENT_TEE: &[u8] = &[0xb2, 0x02, 0x07, 0x5a, 0x05, 0x2d, 0x33, 0x33, 0x13, 0x40];
const ENVIRONMENT_ALL: &[u8] = &[
    0xb2, 0x02, 0x1b, 0x5a, 0x19, 0x0d, 0x00, 0x00, 0xa8, 0x41, 0x15, 0x66, 0x66, 0xe6, 0x3e, 0x1d,
    0x00, 0x00, 0xf0, 0x42, 0x25, 0x9a, 0x99, 0x99, 0x3f, 0x2d, 0x00, 0x00, 0xc0, 0x40,
];

#[test]
fn subscribe_request_bytes() {
    assert_eq!(build_subscribe_request(), SUBSCRIBE);
}

#[test]
fn wakeup_request_bytes() {
    assert_eq!(build_wakeup_request(), WAKEUP);
}

#[test]
fn environment_request_bytes() {
    assert_eq!(
        build_environment_request(&Environment::default()),
        ENVIRONMENT_EMPTY
    );
    assert_eq!(
        build_environment_request(&Environment {
            tee_distance_yd: Some(2.3),
            ..Environment::default()
        }),
        ENVIRONMENT_TEE
    );
    assert_eq!(
        build_environment_request(&Environment {
            temperature: Some(21.0),
            humidity: Some(0.45),
            altitude: Some(120.0),
            air_density: Some(1.2),
            tee_distance_yd: Some(6.0),
        }),
        ENVIRONMENT_ALL
    );
}

// ── Fixture frames (envelope payloads, device → host) ──

/// Regular shot, all ball/club/swing metrics.
const SHOT_FULL: &[u8] = &[
    0xf2, 0x01, 0x53, 0x1a, 0x51, 0x08, 0x08, 0xca, 0x3e, 0x4c, 0x12, 0x4a, 0x08, 0x2a, 0x10, 0x01,
    0x1a, 0x1d, 0x0d, 0x00, 0x00, 0x48, 0x41, 0x15, 0x9a, 0x99, 0x99, 0xbf, 0x1d, 0x00, 0x00, 0x86,
    0x42, 0x25, 0x00, 0x00, 0xa0, 0x40, 0x2d, 0x00, 0x00, 0x2f, 0x45, 0x30, 0x00, 0x38, 0x01, 0x22,
    0x14, 0x0d, 0x00, 0x00, 0x30, 0x42, 0x15, 0x00, 0x00, 0xc0, 0x3f, 0x1d, 0x00, 0x00, 0x00, 0xc0,
    0x25, 0x00, 0x00, 0x60, 0xc0, 0x2a, 0x0f, 0x08, 0xe8, 0x07, 0x10, 0xdc, 0x0b, 0x18, 0x88, 0x0e,
    0x20, 0x98, 0x11, 0x28, 0xc4, 0x13,
];
/// Warmup shot, ball metrics only, measured spin on a marked ball.
const SHOT_BALL_MEASURED: &[u8] = &[
    0xf2, 0x01, 0x2c, 0x1a, 0x2a, 0x08, 0x08, 0xca, 0x3e, 0x25, 0x12, 0x23, 0x08, 0x07, 0x10, 0x00,
    0x1a, 0x1d, 0x0d, 0x00, 0x00, 0xf2, 0x41, 0x15, 0x00, 0x00, 0x20, 0x40, 0x1d, 0x00, 0x00, 0xcc,
    0x41, 0x25, 0x00, 0x00, 0x20, 0xc1, 0x2d, 0x00, 0x80, 0xbb, 0x45, 0x30, 0x03, 0x38, 0x02,
];
/// Ball speed only, flight-model spin, no shot kind.
const SHOT_FLIGHT_MODEL: &[u8] = &[
    0xf2, 0x01, 0x14, 0x1a, 0x12, 0x08, 0x08, 0xca, 0x3e, 0x0d, 0x12, 0x0b, 0x08, 0x08, 0x1a, 0x07,
    0x1d, 0x00, 0x00, 0x48, 0x42, 0x30, 0x01,
];
/// Spin source 2 (other), nothing else.
const SHOT_SPIN_OTHER: &[u8] = &[
    0xf2, 0x01, 0x0f, 0x1a, 0x0d, 0x08, 0x08, 0xca, 0x3e, 0x08, 0x12, 0x06, 0x08, 0x09, 0x1a, 0x02,
    0x30, 0x02,
];

/// Device state notification; the last byte is the state value.
fn state_frame(state: u8) -> Vec<u8> {
    vec![
        0xf2, 0x01, 0x0b, 0x1a, 0x09, 0x08, 0x08, 0xca, 0x3e, 0x04, 0x0a, 0x02, 0x08, state,
    ]
}

/// Error: not level, fatal, with tilt roll 4.5 / pitch -6.25.
const ERROR_NOT_LEVEL: &[u8] = &[
    0xf2, 0x01, 0x19, 0x1a, 0x17, 0x08, 0x08, 0xca, 0x3e, 0x12, 0x1a, 0x10, 0x08, 0x03, 0x10, 0x02,
    0x1a, 0x0a, 0x0d, 0x00, 0x00, 0x90, 0x40, 0x15, 0x00, 0x00, 0xc8, 0xc0,
];
/// Error: overheating, serious, no tilt.
const ERROR_OVERHEAT: &[u8] = &[
    0xf2, 0x01, 0x0d, 0x1a, 0x0b, 0x08, 0x08, 0xca, 0x3e, 0x06, 0x1a, 0x04, 0x08, 0x01, 0x10, 0x01,
];
/// Error: radar saturation, warning, no tilt.
const ERROR_SATURATION: &[u8] = &[
    0xf2, 0x01, 0x0d, 0x1a, 0x0b, 0x08, 0x08, 0xca, 0x3e, 0x06, 0x1a, 0x04, 0x08, 0x02, 0x10, 0x00,
];
/// Leveling check: releveling advised (2), device moved (2).
const LEVEL_CHECK: &[u8] = &[
    0xf2, 0x01, 0x0d, 0x1a, 0x0b, 0x08, 0x08, 0xca, 0x3e, 0x06, 0x22, 0x04, 0x08, 0x02, 0x10, 0x02,
];
/// Notification carrying only its topic, no launch monitor payload.
const NOTIFICATION_EMPTY: &[u8] = &[0xf2, 0x01, 0x04, 0x1a, 0x02, 0x08, 0x08];
const SUBSCRIBE_OK: &[u8] = &[
    0xf2, 0x01, 0x0a, 0x12, 0x08, 0x0a, 0x06, 0x08, 0x00, 0x12, 0x02, 0x08, 0x08,
];
const SUBSCRIBE_FAIL: &[u8] = &[
    0xf2, 0x01, 0x0a, 0x12, 0x08, 0x0a, 0x06, 0x08, 0x01, 0x12, 0x02, 0x08, 0x08,
];
/// Topic list reply (no wrapper event).
const TOPICS_REPLY: &[u8] = &[0xf2, 0x01, 0x06, 0x2a, 0x04, 0x08, 0x08, 0x10, 0x01];

/// Wake result; the last byte is the status.
fn wake_result_frame(status: u8) -> Vec<u8> {
    vec![0xb2, 0x02, 0x04, 0x22, 0x02, 0x08, status]
}

const ENVIRONMENT_ACCEPTED: &[u8] = &[0xb2, 0x02, 0x04, 0x62, 0x02, 0x08, 0x01];
const ENVIRONMENT_REJECTED: &[u8] = &[0xb2, 0x02, 0x04, 0x62, 0x02, 0x08, 0x00];
/// State reply: armed.
const STATE_REPLY: &[u8] = &[0xb2, 0x02, 0x06, 0x12, 0x04, 0x0a, 0x02, 0x08, 0x02];
/// Level reply: roll 1.0, pitch -2.0.
const LEVEL_REPLY: &[u8] = &[
    0xb2, 0x02, 0x0e, 0x32, 0x0c, 0x0a, 0x0a, 0x0d, 0x00, 0x00, 0x80, 0x3f, 0x15, 0x00, 0x00, 0x00,
    0xc0,
];

fn decode(bytes: &[u8]) -> EnvelopeEvent {
    decode_envelope(bytes).expect("fixture decodes")
}

fn shot(bytes: &[u8]) -> ShotData {
    match decode(bytes) {
        EnvelopeEvent::Shot(s) => s,
        other => panic!("expected Shot, got {other:?}"),
    }
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.01
}

#[test]
fn decode_shot_full() {
    let s = shot(SHOT_FULL);
    assert_eq!(s.shot_id, 42);
    assert_eq!(s.kind, ShotKind::Regular);

    let b = s.ball.expect("ball");
    assert!(close(b.launch_angle, 12.5));
    assert!(close(b.launch_direction, -1.2));
    assert!(close(b.ball_speed, 67.0));
    // The wire's tilt of 5 is 5° left.
    assert!(close(b.spin_axis, -5.0));
    assert!(close(b.total_spin, 2800.0));
    assert!((b.backspin - 2789.35).abs() < 0.1);
    assert!((b.sidespin + 244.04).abs() < 0.1);
    assert_eq!(b.spin_calc_type, SpinCalcType::Estimated);

    let c = s.club.expect("club");
    assert!(close(c.club_head_speed, 44.0));
    assert!(close(c.face_angle, 1.5));
    assert!(close(c.path_angle, -2.0));
    assert!(close(c.attack_angle, -3.5));

    let w = s.swing.expect("swing");
    assert_eq!(w.backswing_start, 1000);
    assert_eq!(w.downswing_start, 1500);
    assert_eq!(w.impact, 1800);
    assert_eq!(w.follow_through_end, 2200);
}

#[test]
fn decode_shot_ball_measured() {
    let s = shot(SHOT_BALL_MEASURED);
    assert_eq!(s.shot_id, 7);
    assert_eq!(s.kind, ShotKind::Warmup);
    assert!(s.club.is_none());
    assert!(s.swing.is_none());
    let b = s.ball.expect("ball");
    assert!(close(b.launch_angle, 30.25));
    assert!(close(b.launch_direction, 2.5));
    assert!(close(b.ball_speed, 25.5));
    // The wire's tilt of -10 is 10° right.
    assert!(close(b.spin_axis, 10.0));
    assert!(close(b.total_spin, 6000.0));
    assert!((b.backspin - 5908.85).abs() < 0.1);
    assert!((b.sidespin - 1041.89).abs() < 0.1);
    assert_eq!(b.spin_calc_type, SpinCalcType::Measured);
}

#[test]
fn decode_shot_flight_model_and_other() {
    let s = shot(SHOT_FLIGHT_MODEL);
    assert_eq!(s.shot_id, 8);
    assert_eq!(s.kind, ShotKind::Warmup);
    let b = s.ball.expect("ball");
    assert!(close(b.ball_speed, 50.0));
    assert!(close(b.launch_angle, 0.0));
    assert!(close(b.total_spin, 0.0));
    assert_eq!(b.spin_calc_type, SpinCalcType::FlightModel);

    let s = shot(SHOT_SPIN_OTHER);
    assert_eq!(s.shot_id, 9);
    assert_eq!(s.ball.expect("ball").spin_calc_type, SpinCalcType::Other);
}

#[test]
fn decode_state_changes() {
    use crate::proto::DeviceState::{Error, NoiseCheck, Processing, Recording, Standby, Waiting};
    for (value, want) in [
        (0, Standby),
        (1, NoiseCheck),
        (2, Waiting),
        (3, Recording),
        (4, Processing),
        (5, Error),
    ] {
        match decode(&state_frame(value)) {
            EnvelopeEvent::StateChange(got) => assert_eq!(got, want),
            other => panic!("state {value}: expected StateChange, got {other:?}"),
        }
    }
}

#[test]
fn decode_errors() {
    let EnvelopeEvent::Error(e) = decode(ERROR_NOT_LEVEL) else {
        panic!("expected Error");
    };
    assert_eq!(e.code, ErrorCode::NotLevel);
    assert_eq!(e.severity, ErrorSeverity::Fatal);
    let (roll, pitch) = e.tilt.expect("tilt");
    assert!(close(roll, 4.5));
    assert!(close(pitch, -6.25));

    let EnvelopeEvent::Error(e) = decode(ERROR_OVERHEAT) else {
        panic!("expected Error");
    };
    assert_eq!(e.code, ErrorCode::Overheating);
    assert_eq!(e.severity, ErrorSeverity::Serious);
    assert!(e.tilt.is_none());

    let EnvelopeEvent::Error(e) = decode(ERROR_SATURATION) else {
        panic!("expected Error");
    };
    assert_eq!(e.code, ErrorCode::RadarSaturation);
    assert_eq!(e.severity, ErrorSeverity::Warning);
}

#[test]
fn decode_level_check() {
    assert!(matches!(
        decode(LEVEL_CHECK),
        EnvelopeEvent::LevelCheck {
            verdict: 2,
            attempt: 2
        }
    ));
}

#[test]
fn decode_subscribe_results() {
    assert!(matches!(
        decode(SUBSCRIBE_OK),
        EnvelopeEvent::SubscribeResult { success: true }
    ));
    assert!(matches!(
        decode(SUBSCRIBE_FAIL),
        EnvelopeEvent::SubscribeResult { success: false }
    ));
}

#[test]
fn decode_wake_results() {
    for status in 0..3 {
        match decode(&wake_result_frame(status)) {
            EnvelopeEvent::WakeResult { status: got } => assert_eq!(got, i32::from(status)),
            other => panic!("expected WakeResult, got {other:?}"),
        }
    }
}

#[test]
fn decode_environment_results() {
    assert!(matches!(
        decode(ENVIRONMENT_ACCEPTED),
        EnvelopeEvent::EnvironmentResult { success: true }
    ));
    assert!(matches!(
        decode(ENVIRONMENT_REJECTED),
        EnvelopeEvent::EnvironmentResult { success: false }
    ));
}

#[test]
fn decode_other_frames() {
    assert!(matches!(decode(STATE_REPLY), EnvelopeEvent::MonitorReply));
    assert!(matches!(decode(LEVEL_REPLY), EnvelopeEvent::MonitorReply));
    assert!(matches!(decode(NOTIFICATION_EMPTY), EnvelopeEvent::Unknown));
    assert!(matches!(decode(TOPICS_REPLY), EnvelopeEvent::Unknown));
    assert!(matches!(decode(&[]), EnvelopeEvent::Unknown));
}

#[test]
fn builders_decode_as_requests() {
    // Requests carry no device → host payload the decoder reports.
    assert!(matches!(decode(WAKEUP), EnvelopeEvent::MonitorReply));
    assert!(matches!(decode(SUBSCRIBE), EnvelopeEvent::Unknown));
}

// ── Through the client (GFDI + COBS + MultiLink handle) ──

#[derive(Clone, Default)]
struct Mock {
    rx: Rc<RefCell<VecDeque<Vec<u8>>>>,
    tx: Rc<RefCell<Vec<Vec<u8>>>>,
}

impl Transport for Mock {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, io::Error> {
        match self.rx.borrow_mut().pop_front() {
            Some(data) => {
                let len = data.len().min(buf.len());
                buf[..len].copy_from_slice(&data[..len]);
                Ok(len)
            }
            None => Err(io::Error::from(io::ErrorKind::WouldBlock)),
        }
    }

    fn write(&mut self, data: &[u8]) -> Result<(), io::Error> {
        self.tx.borrow_mut().push(data.to_vec());
        Ok(())
    }

    fn write_register(&mut self, _data: &[u8]) -> Result<(), io::Error> {
        Ok(())
    }
}

/// A 5044 frame carrying `envelope`, COBS-wrapped, behind MultiLink handle 1.
fn response_chunk(envelope: &[u8]) -> Vec<u8> {
    let len = u32::try_from(envelope.len()).unwrap();
    let mut payload = Vec::new();
    payload.extend_from_slice(&1u16.to_le_bytes()); // request id
    payload.extend_from_slice(&0u32.to_le_bytes()); // offset
    payload.extend_from_slice(&len.to_le_bytes()); // total length
    payload.extend_from_slice(&len.to_le_bytes()); // chunk length
    payload.extend_from_slice(envelope);
    let frame = gfdi::build_frame(gfdi::MSG_PROTOBUF_RESPONSE, &payload);
    let mut chunk = vec![1u8];
    chunk.push(0x00);
    chunk.extend_from_slice(&crate::cobs::encode(&frame));
    chunk.push(0x00);
    chunk
}

fn registered_client() -> (Client<Mock>, Mock) {
    let mock = Mock::default();
    let mut client = Client::new(mock.clone(), 20);
    client.start().unwrap();
    mock.rx.borrow_mut().push_back(vec![
        0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x01, 0x00,
        0x00,
    ]);
    assert!(matches!(
        client.poll().unwrap(),
        Some(Event::Registered { handle: 1 })
    ));
    (client, mock)
}

#[test]
fn client_decodes_fixture_frames() {
    let (mut client, mock) = registered_client();
    let mut feed = |envelope: &[u8]| {
        mock.rx.borrow_mut().push_back(response_chunk(envelope));
        client.poll().unwrap()
    };

    let Some(Event::Shot(s)) = feed(SHOT_FULL) else {
        panic!("expected Shot");
    };
    assert_eq!(s.shot_id, 42);
    assert!(close(s.ball.expect("ball").ball_speed, 67.0));
    assert!(close(s.club.expect("club").club_head_speed, 44.0));

    assert!(matches!(
        feed(ENVIRONMENT_REJECTED),
        Some(Event::EnvironmentResult { success: false })
    ));
    assert!(matches!(feed(&state_frame(2)), Some(Event::Ready)));

    let Some(Event::DeviceError(e)) = feed(ERROR_NOT_LEVEL) else {
        panic!("expected DeviceError");
    };
    assert_eq!(e.code, ErrorCode::NotLevel);
}

#[test]
fn client_sends_builder_bytes() {
    let (mut client, mock) = registered_client();
    client
        .send_environment(&Environment {
            tee_distance_yd: Some(2.3),
            ..Environment::default()
        })
        .unwrap();

    let mut wire = Vec::new();
    for chunk in mock.tx.borrow().iter() {
        assert_eq!(chunk[0], 1); // MultiLink handle
        wire.extend_from_slice(&chunk[1..]);
    }
    let frame = crate::cobs::decode(&wire[1..wire.len() - 1]).unwrap();
    let parsed = gfdi::parse_frame(&frame).unwrap();
    assert_eq!(parsed.msg_type, gfdi::MSG_PROTOBUF_REQUEST);
    let (_hdr, pb) = gfdi::parse_frag_header(&parsed.payload).unwrap();
    assert_eq!(pb, ENVIRONMENT_TEE);
}

/// A draw (face closed to the path) arrives with a positive tilt on the wire,
/// so the device's sign is + = left. Decoded, the axis and the sidespin are
/// negative: tilted left, curving left, the same sense as the launch
/// direction. A captured shot had face 3.9°, path 16.8° and tilt 22.2°.
#[test]
fn a_draw_decodes_tilted_left() {
    let s = shot(&crate::testing::shot_envelope(5, 3000.0, 22.2));
    let b = s.ball.expect("ball");
    assert!(close(b.spin_axis, -22.2));
    assert!(b.sidespin < 0.0);
    assert!(b.backspin > 0.0);
}
