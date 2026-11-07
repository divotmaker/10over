use std::cell::RefCell;
use std::collections::VecDeque;
use std::io;
use std::rc::Rc;

use flightrelay::driver::{ConnectHost, DeviceCommand, DeviceEvent, HostFlags};
use flightrelay::units::Distance;
use flightrelay::{Launch, RawDir};

use super::*;
use crate::gfdi::{MSG_CONFIGURATION, MSG_DEVICE_INFO};
use crate::testing;

/// MultiLink handle the fake device grants.
const HANDLE: u8 = 0x05;
/// The fake device's BLE address.
const ADDRESS: &str = "F5:D1:00:00:00:01";

/// What the fake device has to say, and what the host wrote to it.
#[derive(Default)]
struct Wire {
    rx: VecDeque<Vec<u8>>,
    tx: Vec<Vec<u8>>,
    /// Fail the next read with this error kind.
    fail: Option<io::ErrorKind>,
    /// Once `rx` is empty, read this chunk forever, 1 ms apart.
    chatter: Option<Vec<u8>>,
    /// Times the session released the device.
    released: u32,
}

/// A transport over a shared [`Wire`].
#[derive(Clone, Default)]
struct Fake(Rc<RefCell<Wire>>);

impl Fake {
    /// Queue one message from the device, split at MTU 20.
    fn send(&self, frame: &[u8]) {
        let mut w = self.0.borrow_mut();
        w.rx.extend(testing::chunks(HANDLE, frame, 20));
    }

    fn send_envelope(&self, envelope: &[u8]) {
        self.send(&testing::protobuf_frame(envelope));
    }

    /// Release the device, as `BleTransport::disconnect` does.
    fn release(&mut self) {
        self.0.borrow_mut().released += 1;
    }

    /// Queue the device side of a handshake.
    fn handshake(&self) {
        self.0
            .borrow_mut()
            .rx
            .push_back(testing::register_response(HANDLE));
        self.send(&testing::device_info_frame());
        self.send(&testing::configuration_frame());
        self.send(&testing::ack_frame(MSG_CONFIGURATION, 0, &[]));
    }
}

impl Transport for Fake {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, io::Error> {
        let mut w = self.0.borrow_mut();
        if let Some(kind) = w.fail.take() {
            return Err(kind.into());
        }
        let Some(c) = w.rx.pop_front().or_else(|| w.chatter.clone()) else {
            return Err(io::ErrorKind::WouldBlock.into());
        };
        if w.rx.is_empty() && w.chatter.is_some() {
            std::thread::sleep(Duration::from_millis(1));
        }
        buf[..c.len()].copy_from_slice(&c);
        Ok(c.len())
    }

    fn write(&mut self, data: &[u8]) -> Result<(), io::Error> {
        self.0.borrow_mut().tx.push(data.to_vec());
        Ok(())
    }

    fn write_register(&mut self, _data: &[u8]) -> Result<(), io::Error> {
        Ok(())
    }
}

/// A host that ticks without inputs and stops after `stop_after` ticks.
struct Host {
    latched: DeviceCommand,
    ticks: u32,
    stop_after: Option<(u32, Stop)>,
}

impl ConnectHost for Host {
    fn tick(&mut self) {
        self.ticks += 1;
        if let Some((n, stop)) = &self.stop_after
            && self.ticks >= *n
        {
            stop.stop();
        }
    }

    fn latched(&self) -> &DeviceCommand {
        &self.latched
    }
}

/// The host side of a test: flags and every event the driver emitted.
struct Rig {
    flags: HostFlags,
    events: Vec<DeviceEvent>,
}

impl Rig {
    fn new(wants_raw: bool) -> Self {
        let flags = HostFlags::new();
        flags.set_wants_raw(wants_raw);
        Self {
            flags,
            events: Vec::new(),
        }
    }

    fn connect(&mut self, driver: &mut R10, fake: &Fake) -> Result<R10Session<Fake>, Error> {
        self.connect_until(driver, fake, None)
    }

    fn connect_until(
        &mut self,
        driver: &mut R10,
        fake: &Fake,
        stop_after: Option<u32>,
    ) -> Result<R10Session<Fake>, Error> {
        self.establish(driver, fake, stop_after, keep)
    }

    /// The handshake as `connect` runs it, with `release` freeing the
    /// device on close.
    fn establish(
        &mut self,
        driver: &mut R10,
        fake: &Fake,
        stop_after: Option<u32>,
        release: fn(&mut Fake),
    ) -> Result<R10Session<Fake>, Error> {
        let stop = Stop::new();
        let mut host = Host {
            latched: DeviceCommand::default(),
            ticks: 0,
            stop_after: stop_after.map(|n| (n, stop.clone())),
        };
        let events = &mut self.events;
        let mut sink = |e: DeviceEvent| events.push(e);
        let mut ctx = ConnectCtx::new(&stop, Events::new(&mut sink, &self.flags), &mut host);
        driver.establish(&mut ctx, fake.clone(), ADDRESS, TEST_MTU, release)
    }

    fn poll(&mut self, session: &mut R10Session<Fake>) -> Result<(), Error> {
        let events = &mut self.events;
        let mut sink = |e: DeviceEvent| events.push(e);
        session.poll(
            Duration::from_millis(30),
            &mut Events::new(&mut sink, &self.flags),
        )
    }

    /// Take the FRP events seen so far.
    fn frp(&mut self) -> Vec<FrpEvent> {
        self.events
            .drain(..)
            .filter_map(|e| match e {
                DeviceEvent::Frp(f) => Some(f),
                _ => None,
            })
            .collect()
    }

    fn logs(&self, level: Level) -> Vec<String> {
        self.events
            .iter()
            .filter_map(|e| match e {
                DeviceEvent::Log(l, line) if *l == level => Some(line.clone()),
                _ => None,
            })
            .collect()
    }
}

fn driver(range: Option<Distance>) -> R10 {
    R10::new(Config { range }).expect("valid config")
}

fn kinds(events: &[FrpEvent]) -> Vec<&'static str> {
    events
        .iter()
        .map(|e| match e {
            FrpEvent::ShotTrigger { .. } => "trigger",
            FrpEvent::Launch { .. } => "launch",
            FrpEvent::ClubPath { .. } => "club",
            FrpEvent::ShotFinished { .. } => "finished",
            FrpEvent::DeviceTelemetry { .. } => "telemetry",
            FrpEvent::Alert { .. } => "alert",
            FrpEvent::RawFrame(_) => "raw",
            _ => "other",
        })
        .collect()
}

/// Every telemetry pair, in order.
fn telemetry(events: &[FrpEvent]) -> Vec<(String, String)> {
    events
        .iter()
        .filter_map(|e| match e {
            FrpEvent::DeviceTelemetry {
                telemetry: Some(t), ..
            } => {
                let mut pairs: Vec<_> = t.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                pairs.sort();
                Some(pairs)
            }
            _ => None,
        })
        .flatten()
        .collect()
}

fn pair(k: &str, v: &str) -> (String, String) {
    (k.to_owned(), v.to_owned())
}

fn launches(events: &[FrpEvent]) -> Vec<Launch> {
    events
        .iter()
        .filter_map(|e| match e {
            FrpEvent::Launch { launch, .. } => Some(launch.clone()),
            _ => None,
        })
        .collect()
}

fn raw_frames(events: &[FrpEvent]) -> Vec<RawFrame> {
    events
        .iter()
        .filter_map(|e| match e {
            FrpEvent::RawFrame(f) => Some(f.clone()),
            _ => None,
        })
        .collect()
}

/// A connected session that has been armed (subscribe, wake, waiting).
fn armed(rig: &mut Rig, driver: &mut R10, fake: &Fake) -> R10Session<Fake> {
    fake.handshake();
    let mut session = rig.connect(driver, fake).expect("handshake");
    fake.send_envelope(&testing::subscribe_result_envelope(true));
    fake.send_envelope(&testing::wake_result_envelope(0));
    fake.send_envelope(&testing::state_envelope(2));
    rig.poll(&mut session).expect("poll");
    session
}

#[test]
fn info_names_a_measured_exclusive_garmin() {
    let info = R10::info(&Config::default());
    assert_eq!(info.kind, "tenover");
    assert_eq!(info.source, Source::Measured);
    assert_eq!(info.manufacturer, Some("Garmin"));
    assert!(info.exclusive);
    assert_eq!(info.raw.as_ref().map(|r| r.transport), Some("r10-ble"));
    assert_eq!(R10::STOP_WAIT, Duration::from_secs(6));
}

#[test]
fn new_refuses_an_invalid_config() {
    let err = R10::new(Config {
        range: Some(Distance::Feet(-1.0)),
    })
    .unwrap_err();
    assert_eq!(err.field.as_deref(), Some("range"));
}

#[test]
fn handshake_identifies_the_r10_and_keeps_its_messages_private() {
    let fake = Fake::default();
    fake.handshake();
    let mut rig = Rig::new(true);
    let mut d = driver(None);
    let session = rig.connect(&mut d, &fake).expect("handshake");

    let id = session.identity();
    assert_eq!(id.device, ADDRESS);
    assert_eq!(id.manufacturer.as_deref(), Some("Garmin"));
    assert_eq!(id.model.as_deref(), Some("R10"));
    assert_eq!(id.label(), format!("Garmin R10 ({ADDRESS})"));
    assert_eq!(id.firmware, None);

    // Everything before the handshake finished went to the audit log only,
    // the device information included.
    let audit = rig.logs(Level::Trace);
    assert!(audit.iter().any(|l| l.starts_with("rx register ")));
    assert!(audit.iter().any(|l| l.starts_with("rx notify 13a0 ")));
    assert!(audit.iter().any(|l| l.starts_with("tx write 1388 ")));
    let frp = rig.frp();
    assert!(raw_frames(&frp).is_empty(), "{frp:?}");
    assert_eq!(
        telemetry(&frp),
        [
            pair("ready", "false"),
            pair("roll", "ok"),
            pair("tilt", "ok")
        ]
    );
}

#[test]
fn shot_sends_each_event_once_and_numbers_keep_counting() {
    let fake = Fake::default();
    let mut rig = Rig::new(false);
    let mut d = driver(None);
    let mut session = armed(&mut rig, &mut d, &fake);
    let frp = rig.frp();
    assert_eq!(
        telemetry(&frp),
        [
            pair("ready", "false"),
            pair("roll", "ok"),
            pair("tilt", "ok"),
            pair("device_state", "waiting"),
            pair("ready", "true"),
            pair("roll", "ok"),
            pair("tilt", "ok"),
        ]
    );

    fake.send_envelope(&testing::shot_envelope(41, 2800.0, 5.0));
    rig.poll(&mut session).expect("poll");
    let frp = rig.frp();
    assert_eq!(
        kinds(&frp),
        ["trigger", "launch", "club", "finished", "telemetry"]
    );
    let FrpEvent::ShotTrigger { key, timestamp } = &frp[0] else {
        panic!("trigger first");
    };
    assert_eq!(key.shot_number, 1);
    assert_eq!(*timestamp, None, "the R10 has no wall clock");
    assert_eq!(telemetry(&frp), [pair("ready", "false")]);
    let launch = &launches(&frp)[0];
    assert_eq!(launch.total_spin_rpm, Some(2800));
    assert_eq!(launch.spin_axis, Some(5.0));
    assert!(launch.backspin_rpm.is_some() && launch.sidespin_rpm.is_some());

    // A retransmit of the same device shot is not a new shot.
    fake.send_envelope(&testing::shot_envelope(41, 2800.0, 5.0));
    rig.poll(&mut session).expect("poll");
    assert!(kinds(&rig.frp()).iter().all(|k| *k != "trigger"));

    // A new session of the same driver keeps counting.
    let fake2 = Fake::default();
    let mut session2 = armed(&mut rig, &mut d, &fake2);
    rig.frp();
    fake2.send_envelope(&testing::shot_envelope(1, 2500.0, -3.0));
    rig.poll(&mut session2).expect("poll");
    let frp = rig.frp();
    let FrpEvent::ShotTrigger { key, .. } = &frp[0] else {
        panic!("trigger first");
    };
    assert_eq!(key.shot_number, 2);
}

#[test]
fn zero_spin_shot_sends_total_zero_with_no_axis() {
    let fake = Fake::default();
    let mut rig = Rig::new(false);
    let mut d = driver(None);
    let mut session = armed(&mut rig, &mut d, &fake);
    rig.frp();
    fake.send_envelope(&testing::shot_envelope(7, 0.0, 4.0));
    rig.poll(&mut session).expect("poll");
    let launch = &launches(&rig.frp())[0];
    assert_eq!(launch.total_spin_rpm, Some(0));
    assert_eq!(launch.spin_axis, None);
}

#[test]
fn states_and_errors_become_telemetry_and_alerts() {
    let fake = Fake::default();
    let mut rig = Rig::new(false);
    let mut d = driver(None);
    let mut session = armed(&mut rig, &mut d, &fake);
    rig.frp();

    fake.send_envelope(&testing::state_envelope(3)); // recording
    fake.send_envelope(&testing::state_envelope(0)); // standby: not shown
    fake.send_envelope(&testing::state_envelope(4)); // processing
    fake.send_envelope(&testing::error_envelope(3, 2, Some((4.5, -6.25))));
    fake.send_envelope(&testing::error_envelope(2, 0, None));
    rig.poll(&mut session).expect("poll");
    let frp = rig.frp();
    assert_eq!(
        telemetry(&frp),
        [
            pair("device_state", "recording"),
            pair("device_state", "processing"),
            pair("roll", "4.5"),
            pair("tilt", "-6.2"),
        ]
    );
    let alerts: Vec<_> = frp
        .iter()
        .filter_map(|e| match e {
            FrpEvent::Alert { severity, message } => Some((*severity, message.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        alerts,
        [
            (
                Severity::Error,
                "R10: NotLevel (roll=4.5, pitch=-6.2)".to_owned()
            ),
            (Severity::Warn, "R10: RadarSaturation".to_owned()),
        ]
    );
}

#[test]
fn raw_frames_are_whole_messages_before_their_shot_and_never_identity() {
    let fake = Fake::default();
    let mut rig = Rig::new(true);
    let mut d = driver(None);
    let mut session = armed(&mut rig, &mut d, &fake);
    rig.frp();

    // A device information message after the handshake stays private.
    fake.send(&testing::device_info_frame());
    let shot = testing::protobuf_frame(&testing::shot_envelope(9, 3000.0, 2.0));
    fake.send(&shot);
    rig.poll(&mut session).expect("poll");
    let audit = rig.logs(Level::Trace);
    let frp = rig.frp();

    let frames = raw_frames(&frp);
    assert!(
        frames
            .iter()
            .all(|f| f.label.as_deref() != Some("notify 13a0") && f.seq == 0)
    );
    assert!(audit.iter().any(|l| l.starts_with("rx notify 13a0 ")));
    let unit_id = testing::DEVICE_INFO_UNIT_ID.to_le_bytes();
    assert!(
        frames
            .iter()
            .all(|f| !f.data.windows(4).any(|w| w == unit_id)),
        "unit ID leaked"
    );

    // The shot message is one frame (it crossed as several chunks), sent
    // before the shot events built from it, then our ACK of it.
    let shot_frame = crate::cobs::decode(&shot[1..shot.len() - 1]).unwrap();
    let k = kinds(&frp);
    let first_raw = k.iter().position(|k| *k == "raw").expect("a raw frame");
    let trigger = k.iter().position(|k| *k == "trigger").expect("a trigger");
    assert!(first_raw < trigger);
    let FrpEvent::RawFrame(f) = &frp[first_raw] else {
        unreachable!()
    };
    assert_eq!(f.dir, RawDir::Rx);
    assert_eq!(f.label.as_deref(), Some("notify 13b4"));
    assert_eq!(f.data, shot_frame);
    assert!(f.timestamp.is_some());
    assert!(
        frames
            .iter()
            .any(|f| f.dir == RawDir::Tx && f.label.as_deref() == Some("write 1388"))
    );
}

#[test]
fn no_raw_frames_unless_wanted() {
    let fake = Fake::default();
    let mut rig = Rig::new(false);
    let mut d = driver(None);
    let mut session = armed(&mut rig, &mut d, &fake);
    fake.send_envelope(&testing::shot_envelope(9, 3000.0, 2.0));
    rig.poll(&mut session).expect("poll");
    assert!(raw_frames(&rig.frp()).is_empty());
}

#[test]
fn the_tee_distance_goes_out_after_the_wake() {
    let fake = Fake::default();
    let mut rig = Rig::new(true);
    let mut d = driver(Some(Distance::Feet(7.5)));
    let mut session = armed(&mut rig, &mut d, &fake);
    // The environment write went out after the wake result.
    rig.poll(&mut session).expect("poll");
    let env = crate::proto::build_environment_request(&crate::proto::Environment {
        tee_distance_yd: Some(2.5),
        ..crate::proto::Environment::default()
    });
    let frames = raw_frames(&rig.frp());
    assert!(
        frames
            .iter()
            .any(|f| f.label.as_deref() == Some("write 13b3")
                && f.data.windows(env.len()).any(|w| w == env.as_slice())),
        "{frames:?}"
    );

    fake.send_envelope(&testing::environment_result_envelope(true));
    rig.poll(&mut session).expect("poll");
    assert!(
        rig.logs(Level::Info)
            .iter()
            .any(|l| l == "device accepted tee distance 2.5 yd")
    );
}

#[test]
fn without_a_range_nothing_is_sent_after_the_wake() {
    let fake = Fake::default();
    let mut rig = Rig::new(true);
    let mut d = driver(None);
    let mut session = armed(&mut rig, &mut d, &fake);
    rig.poll(&mut session).expect("poll");
    // The only request after the handshake is the wake: no environment.
    let wake = crate::proto::build_wakeup_request();
    let requests: Vec<_> = raw_frames(&rig.frp())
        .into_iter()
        .filter(|f| f.label.as_deref() == Some("write 13b3"))
        .collect();
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert!(
        requests[0]
            .data
            .windows(wake.len())
            .any(|w| w == wake.as_slice())
    );
}

#[test]
fn a_protocol_error_is_an_alert_and_the_session_goes_on() {
    let fake = Fake::default();
    let mut rig = Rig::new(false);
    let mut d = driver(None);
    let mut session = armed(&mut rig, &mut d, &fake);
    rig.frp();
    // A protobuf message whose envelope does not decode.
    fake.send(&testing::protobuf_frame(&[0xff, 0xff, 0xff]));
    fake.send_envelope(&testing::shot_envelope(3, 2000.0, 1.0));
    rig.poll(&mut session).expect("poll goes on");
    let frp = rig.frp();
    assert!(frp.iter().any(|e| matches!(
        e,
        FrpEvent::Alert { severity: Severity::Warn, message } if message.starts_with("R10 protocol error:")
    )));
    assert!(kinds(&frp).contains(&"trigger"));
}

#[test]
fn a_transport_error_ends_the_session() {
    let fake = Fake::default();
    let mut rig = Rig::new(false);
    let mut d = driver(None);
    let mut session = armed(&mut rig, &mut d, &fake);
    fake.0.borrow_mut().fail = Some(io::ErrorKind::BrokenPipe);
    let err = rig.poll(&mut session).unwrap_err();
    assert!(
        matches!(err, Error::Protocol(crate::Error::Transport(_))),
        "{err}"
    );

    let mut backoff = Backoff::default();
    let events = &mut rig.events;
    let mut sink = |e: DeviceEvent| events.push(e);
    let mut out = Events::new(&mut sink, &rig.flags);
    let ended = SessionEnd {
        error: Some(&err),
        connected_for: Some(Duration::from_secs(1)),
        attempts: 0,
    };
    assert_eq!(
        d.retry_delay(&ended, &mut backoff, &mut out),
        Some(Duration::from_secs(1))
    );
    let frp = rig.frp();
    assert!(matches!(
        frp.last(),
        Some(FrpEvent::Alert { severity: Severity::Warn, message })
            if message.starts_with("R10 connection error: transport error:")
    ));
}

#[test]
fn a_stop_ends_the_handshake() {
    // The device never answers; the host stops on its second tick.
    let fake = Fake::default();
    let mut rig = Rig::new(false);
    let mut d = driver(None);
    let started = Instant::now();
    let err = rig.connect_until(&mut d, &fake, Some(2)).unwrap_err();
    assert!(matches!(err, Error::Stopped), "{err}");
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn close_reports_not_ready() {
    let fake = Fake::default();
    let mut rig = Rig::new(false);
    let mut d = driver(None);
    let session = armed(&mut rig, &mut d, &fake);
    rig.frp();
    let events = &mut rig.events;
    let mut sink = |e: DeviceEvent| events.push(e);
    session
        .close(&mut Events::new(&mut sink, &rig.flags))
        .expect("close");
    assert_eq!(telemetry(&rig.frp()), [pair("ready", "false")]);
}

#[test]
fn close_releases_the_device() {
    let fake = Fake::default();
    fake.handshake();
    let mut rig = Rig::new(false);
    let mut d = driver(None);
    let session = rig
        .establish(&mut d, &fake, None, Fake::release)
        .expect("handshake");
    assert_eq!(fake.0.borrow().released, 0);
    let events = &mut rig.events;
    let mut sink = |e: DeviceEvent| events.push(e);
    session
        .close(&mut Events::new(&mut sink, &rig.flags))
        .expect("close");
    assert_eq!(fake.0.borrow().released, 1);
}

#[test]
fn a_stop_as_the_connect_succeeds_releases_the_transport() {
    let fake = Fake::default();
    let result = finish_scan(Ok(fake.clone()), true, Fake::release);
    assert!(matches!(result, Err(Error::Stopped)));
    assert_eq!(fake.0.borrow().released, 1);

    // Without a stop the transport is handed over, still connected.
    let kept = finish_scan(Ok(fake.clone()), false, Fake::release).expect("connected");
    assert_eq!(kept.0.borrow().released, 1);
}

#[test]
fn the_handshake_is_bounded_while_the_device_keeps_talking() {
    // Registered, then a NAK on every read: no poll is ever empty.
    let fake = Fake::default();
    {
        let mut w = fake.0.borrow_mut();
        w.rx.push_back(testing::register_response(HANDLE));
        let nak = testing::ack_frame(MSG_CONFIGURATION, 1, &[]);
        let chunks = testing::chunks(HANDLE, &nak, 20);
        assert_eq!(chunks.len(), 1, "one NAK per read");
        w.chatter = chunks.into_iter().next();
    }
    let mut rig = Rig::new(false);
    rig.flags.set_min_level(Level::Info);
    let mut d = driver(None);
    d.handshake_timeout = Duration::from_millis(300);
    let started = Instant::now();
    // The host's stop after 30 ticks (3 s) only guards against a hang.
    let err = rig.connect_until(&mut d, &fake, Some(30)).unwrap_err();
    assert!(
        matches!(err, Error::Protocol(crate::Error::HandshakeTimeout)),
        "{err}"
    );
    let took = started.elapsed();
    assert!(
        took >= Duration::from_millis(300) && took < Duration::from_secs(2),
        "{took:?}"
    );
}

#[test]
fn a_refused_subscription_is_a_warning() {
    let fake = Fake::default();
    let mut rig = Rig::new(false);
    let mut d = driver(None);
    let mut session = armed(&mut rig, &mut d, &fake);
    assert!(rig.logs(Level::Warn).is_empty());
    fake.send_envelope(&testing::subscribe_result_envelope(false));
    rig.poll(&mut session).expect("poll");
    assert_eq!(
        rig.logs(Level::Warn),
        ["device refused the shot subscription"]
    );
}

#[test]
fn debug_output_leaves_out_the_address() {
    let fake = Fake::default();
    fake.handshake();
    let mut rig = Rig::new(false);
    let mut d = driver(None);
    let session = rig.connect(&mut d, &fake).expect("handshake");
    let debug = format!("{session:?}");
    assert!(debug.starts_with("R10Session"), "{debug}");
    assert!(!debug.contains(ADDRESS), "{debug}");
}

#[test]
fn messages_are_captured_only_while_someone_can_receive_them() {
    let fake = Fake::default();
    let mut rig = Rig::new(false);
    rig.flags.set_min_level(Level::Debug);
    let mut d = driver(None);
    let mut session = armed(&mut rig, &mut d, &fake);
    assert!(!session.client.transport().enabled());
    assert!(rig.logs(Level::Trace).is_empty());

    // Raw frames wanted: capture starts with the next poll.
    rig.flags.set_wants_raw(true);
    fake.send_envelope(&testing::shot_envelope(9, 3000.0, 2.0));
    rig.poll(&mut session).expect("poll");
    assert!(session.client.transport().enabled());
    assert!(!raw_frames(&rig.frp()).is_empty());
}

#[test]
fn audit_lines_do_not_depend_on_raw_frames_being_wanted() {
    let run = |wants_raw: bool| {
        let fake = Fake::default();
        let mut rig = Rig::new(wants_raw);
        let mut d = driver(None);
        let mut session = armed(&mut rig, &mut d, &fake);
        fake.send(&testing::device_info_frame());
        fake.send_envelope(&testing::shot_envelope(9, 3000.0, 2.0));
        rig.poll(&mut session).expect("poll");
        assert!(session.client.transport().enabled());
        rig.logs(Level::Trace)
    };
    let without = run(false);
    assert_eq!(without, run(true));
    let info = testing::device_info_frame();
    let decoded = crate::cobs::decode(&info[1..info.len() - 1]).unwrap();
    let hex = decoded.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    });
    assert!(
        without
            .iter()
            .filter(|l| **l == format!("rx notify 13a0 {hex}"))
            .count()
            == 2,
        "the handshake's and the later device information: {without:?}"
    );
}

#[test]
fn identity_messages_are_classified_by_type() {
    // The ACK of the device information is private wherever it appears.
    let ack = testing::ack_frame(MSG_DEVICE_INFO, 0, &[0xAA, 0xBB]);
    let frame = crate::gfdi::parse_cobs_frame(&ack[1..ack.len() - 1]).unwrap();
    assert!(frame.carries_identity());
}

#[cfg(feature = "frp")]
#[test]
fn help_lists_the_optional_range() {
    assert!(Config::help_toml().contains("# range = (unset)"));
    let config = Config::from_toml_str("range = \"7ft\"").expect("valid");
    assert_eq!(config.range, Some(Distance::Feet(7.0)));
    let err = Config::from_toml_str("range = \"0yd\"").unwrap_err();
    assert_eq!(err.field.as_deref(), Some("range"));
}

#[cfg(feature = "frp")]
#[test]
fn run_refuses_unknown_keys_and_invalid_values() {
    use std::process::ExitCode;
    let run = |args: &[&str]| {
        flightrelay::run_args::<R10>(
            args.iter().map(|a| (*a).to_owned()),
            flightrelay::RunOptions::default(),
        )
    };
    assert_eq!(run(&["--set", "rnage=7ft"]), ExitCode::from(2));
    assert_eq!(run(&["--set", "range=0ft"]), ExitCode::from(2));
    assert_eq!(run(&["--set", "range=seven"]), ExitCode::from(2));
    assert_eq!(run(&["--help"]), ExitCode::SUCCESS);
}
