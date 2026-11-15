use std::cell::RefCell;
use std::collections::VecDeque;
use std::io;
use std::rc::Rc;

use flightrelay::Distance;
use flightrelay::driver::{DeviceEvent, DeviceStatus};
use flightrelay::testing::{
    TestHost, alerts, bodies, clubs, kinds, launches, raw_frames, status, statuses,
};
use flightrelay::{FrpBody, Launch, RawDir, ShotTrigger};

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
    /// Once `rx` is empty, read this chunk forever.
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

/// The host side of a test: a [`TestHost`] (flags, stepped clock, sink) and
/// the events taken from it that no assertion has consumed yet.
struct Rig {
    host: TestHost,
    events: Vec<DeviceEvent>,
}

impl Rig {
    fn new(wants_raw: bool) -> Self {
        Self::on(TestHost::new(), wants_raw)
    }

    fn on(host: TestHost, wants_raw: bool) -> Self {
        host.flags.set_wants_raw(wants_raw);
        Self {
            host,
            events: Vec::new(),
        }
    }

    fn connect(&mut self, driver: &mut R10, fake: &Fake) -> Result<R10Session<Fake>, Error> {
        self.establish(driver, fake, keep)
    }

    /// The handshake as `connect` runs it, with `release` freeing the
    /// device on close.
    fn establish(
        &mut self,
        driver: &mut R10,
        fake: &Fake,
        release: fn(&mut Fake),
    ) -> Result<R10Session<Fake>, Error> {
        self.host
            .connect_with(|ctx| driver.establish(ctx, fake.clone(), ADDRESS, TEST_MTU, release))
    }

    /// Step the session through 30 ms of the host's time: what the fake
    /// device has queued is read and handled.
    fn poll(&mut self, session: &mut R10Session<Fake>) -> Result<(), Error> {
        self.host
            .run_for(Duration::from_millis(30), |now, out| session.step(now, out))
    }

    /// Close the session.
    fn close(&mut self, session: R10Session<Fake>) {
        self.host.with(|_, out| session.close(out));
    }

    /// Take the events seen so far.
    fn take(&mut self) -> Vec<DeviceEvent> {
        self.events.extend(self.host.take());
        std::mem::take(&mut self.events)
    }

    /// The log lines at `level` seen so far.
    fn logs(&mut self, level: Level) -> Vec<String> {
        self.events.extend(self.host.take());
        flightrelay::testing::logs(&self.events)
            .into_iter()
            .filter(|(l, _)| *l == level)
            .map(|(_, line)| line.to_owned())
            .collect()
    }
}

fn driver(range: Option<Distance>) -> R10 {
    R10::new(Config {
        range,
        ..Config::default()
    })
    .expect("valid config")
}

fn aimed_driver(aim_offset_deg: f64) -> R10 {
    R10::new(Config {
        aim_offset_deg: Some(aim_offset_deg),
        ..Config::default()
    })
    .expect("valid config")
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
}

#[test]
fn new_refuses_an_invalid_config() {
    let err = R10::new(Config {
        range: Some(Distance::Feet(-1.0)),
        ..Config::default()
    })
    .unwrap_err();
    assert_eq!(err.field.as_deref(), Some("range"));
    let err = R10::new(Config {
        aim_offset_deg: Some(11.0),
        ..Config::default()
    })
    .unwrap_err();
    assert_eq!(err.field.as_deref(), Some("aim_offset_deg"));
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
    let events = rig.take();
    assert!(raw_frames(&events).is_empty(), "{events:?}");
    // The host starts every session `not_armed`; the driver adds nothing.
    assert!(statuses(&events).is_empty(), "{events:?}");
}

#[test]
fn shot_sends_each_event_once_and_numbers_keep_counting() {
    let fake = Fake::default();
    let mut rig = Rig::new(false);
    let mut d = driver(None);
    let mut session = armed(&mut rig, &mut d, &fake);
    assert_eq!(
        status(&rig.take()),
        DeviceStatus::new()
            .with_telemetry("device_state", "waiting")
            .with_telemetry("tilt", "unknown")
            .with_telemetry("roll", "unknown")
    );

    fake.send_envelope(&testing::shot_envelope(41, 2800.0, 5.0));
    rig.poll(&mut session).expect("poll");
    let events = rig.take();
    assert_eq!(
        kinds(&events),
        ["shot.trigger", "shot.launch", "shot.club", "shot.finished"]
    );
    let FrpBody::ShotTrigger(ShotTrigger { key, timestamp }) = bodies(&events)[0] else {
        panic!("trigger first");
    };
    assert_eq!(key.shot_number, 1);
    assert_eq!(*timestamp, None, "the R10 has no wall clock");
    let after = status(&events);
    assert_eq!(after.blockers, ["not_armed"]);
    assert_eq!(after.telemetry["tilt"], "unknown");
    assert_eq!(after.telemetry["roll"], "unknown");
    let launch = launches(&events)[0];
    assert_eq!(launch.total_spin_rpm, Some(2800));
    // The wire's tilt of 5 is 5° left: the axis and sidespin are negative.
    assert_eq!(launch.spin_axis, Some(-5.0));
    assert!(launch.backspin_rpm.is_some());
    assert!(launch.sidespin_rpm.expect("sidespin") < 0);

    // A retransmit of the same device shot is not a new shot.
    fake.send_envelope(&testing::shot_envelope(41, 2800.0, 5.0));
    rig.poll(&mut session).expect("poll");
    assert!(!kinds(&rig.take()).contains(&"shot.trigger"));

    // A new session of the same driver keeps counting.
    let fake2 = Fake::default();
    let mut session2 = armed(&mut rig, &mut d, &fake2);
    rig.take();
    fake2.send_envelope(&testing::shot_envelope(1, 2500.0, -3.0));
    rig.poll(&mut session2).expect("poll");
    let events = rig.take();
    let FrpBody::ShotTrigger(ShotTrigger { key, .. }) = bodies(&events)[0] else {
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
    rig.take();
    fake.send_envelope(&testing::shot_envelope(7, 0.0, 4.0));
    rig.poll(&mut session).expect("poll");
    let events = rig.take();
    let launch = launches(&events)[0];
    assert_eq!(launch.total_spin_rpm, Some(0));
    assert_eq!(launch.spin_axis, None);
}

/// The first launch and club data of one shot (device shot 5).
fn aimed_shot(d: &mut R10, envelope: &[u8]) -> (Launch, flightrelay::ClubData) {
    let fake = Fake::default();
    let mut rig = Rig::new(false);
    let mut session = armed(&mut rig, d, &fake);
    rig.take();
    fake.send_envelope(envelope);
    rig.poll(&mut session).expect("poll");
    let events = rig.take();
    (launches(&events)[0].clone(), clubs(&events)[0].clone())
}

#[test]
fn the_aim_offset_corrects_the_horizontal_angles_of_a_tracked_shot() {
    let shot = testing::shot_envelope(5, 2800.0, 5.0);
    let (plain_launch, plain_club) = aimed_shot(&mut driver(None), &shot);
    assert_eq!(plain_launch.launch_azimuth, Some(f64::from(-1.2_f32)));
    assert_eq!(plain_club.face_angle, Some(1.5));
    assert_eq!(plain_club.path, Some(-2.0));

    let (launch, club) = aimed_shot(&mut aimed_driver(1.5), &shot);
    assert_eq!(launch.launch_azimuth, Some(f64::from(-1.2_f32) + 1.5));
    assert_eq!(club.face_angle, Some(3.0));
    assert_eq!(club.path, Some(-0.5));
    // Elevation, spin and the vertical club angle are the device's.
    assert_eq!(
        Launch {
            launch_azimuth: plain_launch.launch_azimuth,
            ..launch
        },
        plain_launch
    );
    assert_eq!(club.attack_angle, plain_club.attack_angle);
    assert_eq!(club.club_speed, plain_club.club_speed);
}

#[test]
fn the_aim_offset_leaves_an_untracked_club_alone() {
    let shot = testing::shot_envelope_untracked_club(5, 2800.0, 5.0);
    let (launch, club) = aimed_shot(&mut aimed_driver(-2.0), &shot);
    assert_eq!(launch.launch_azimuth, Some(f64::from(-1.2_f32) - 2.0));
    assert_eq!(club.face_angle, Some(0.0));
    assert_eq!(club.path, Some(0.0));
    assert_eq!(club.attack_angle, Some(0.0));
}

#[test]
fn a_new_aim_offset_applies_to_the_running_session() {
    let fake = Fake::default();
    let mut rig = Rig::new(false);
    let mut d = driver(None);
    let mut session = armed(&mut rig, &mut d, &fake);
    rig.take();
    fake.send_envelope(&testing::shot_envelope(1, 2800.0, 5.0));
    rig.poll(&mut session).expect("poll");
    assert_eq!(
        launches(&rig.take())[0].launch_azimuth,
        Some(f64::from(-1.2_f32))
    );

    let aimed = Config {
        aim_offset_deg: Some(1.2),
        ..Config::default()
    };
    assert!(!Config::restart_needed(d.config(), &aimed));
    d.apply_config(&aimed);
    fake.send_envelope(&testing::shot_envelope(2, 2800.0, 5.0));
    rig.poll(&mut session).expect("poll");
    assert_eq!(
        launches(&rig.take())[0].launch_azimuth,
        Some(f64::from(-1.2_f32) + 1.2)
    );

    d.apply_config(&Config::default());
    fake.send_envelope(&testing::shot_envelope(3, 2800.0, 5.0));
    rig.poll(&mut session).expect("poll");
    assert_eq!(
        launches(&rig.take())[0].launch_azimuth,
        Some(f64::from(-1.2_f32))
    );
}

#[test]
fn a_not_level_reading_is_flagged_until_the_device_arms_or_takes_a_shot() {
    let fake = Fake::default();
    let mut rig = Rig::new(false);
    let mut d = driver(None);
    let mut session = armed(&mut rig, &mut d, &fake);
    let mut seen = rig.take();
    // Poll what `envelopes` queued; the tilt, roll and flags after it.
    let mut after = |envelopes: &[Vec<u8>]| {
        for e in envelopes {
            fake.send_envelope(e);
        }
        rig.poll(&mut session).expect("poll");
        let events = rig.take();
        let s = status(&events);
        seen.extend(events);
        (
            s.telemetry["tilt"].clone(),
            s.telemetry["roll"].clone(),
            s.flagged.into_iter().collect::<Vec<_>>(),
        )
    };
    let level = |tilt: &str, roll: &str, flagged: &[&str]| {
        let flagged = flagged.iter().map(|&k| k.to_owned()).collect();
        (tilt.to_owned(), roll.to_owned(), flagged)
    };
    let not_level = testing::error_envelope(3, 2, Some((4.5, -6.25)));

    // The not-level error flags both; arming again takes the flags off.
    let flagged = level("-6.2°", "4.5°", &["roll", "tilt"]);
    assert_eq!(after(std::slice::from_ref(&not_level)), flagged);
    let rearm = [testing::state_envelope(3), testing::state_envelope(2)];
    assert_eq!(after(&rearm), level("unknown", "unknown", &[]));
    // Another error with a reading is not flagged: the device decides.
    let saturated = testing::error_envelope(2, 0, Some((0.0, -0.04)));
    assert_eq!(after(&[saturated]), level("0.0°", "0.0°", &[]));
    // A shot takes the reading and its flags off.
    assert_eq!(after(&[not_level]), flagged);
    let shot = testing::shot_envelope(3, 2800.0, 5.0);
    assert_eq!(after(&[shot]), level("unknown", "unknown", &[]));

    // Every state's actor carries the same, declared keys.
    let info = R10::info(&Config::default());
    for status in statuses(&seen) {
        let actor = info.bus_actor("x", Duration::from_secs(2), None, &status);
        let keys: Vec<String> = actor.telemetry.expect("telemetry").into_keys().collect();
        assert_eq!(keys, ["device_state", "roll", "tilt"]);
    }
}

#[test]
fn states_and_errors_become_status_and_alerts() {
    let fake = Fake::default();
    let mut rig = Rig::new(false);
    let mut d = driver(None);
    let mut session = armed(&mut rig, &mut d, &fake);
    rig.take();

    fake.send_envelope(&testing::state_envelope(3)); // recording
    fake.send_envelope(&testing::state_envelope(0)); // standby: not shown
    fake.send_envelope(&testing::state_envelope(4)); // processing
    fake.send_envelope(&testing::error_envelope(3, 2, Some((4.5, -6.25))));
    fake.send_envelope(&testing::error_envelope(2, 0, None));
    rig.poll(&mut session).expect("poll");
    let events = rig.take();
    // As the actor carries them: a key not reported has its declared value.
    let info = R10::info(&Config::default());
    let seen: Vec<_> = statuses(&events)
        .into_iter()
        .map(|s| {
            let actor = info.bus_actor("x", Duration::from_secs(2), None, &s);
            let t = actor.telemetry.expect("telemetry");
            (
                t["device_state"].clone(),
                t["tilt"].clone(),
                t["roll"].clone(),
            )
        })
        .collect();
    let state =
        |label: &str, tilt: &str, roll: &str| (label.to_owned(), tilt.to_owned(), roll.to_owned());
    assert_eq!(
        seen,
        [
            state("recording", "unknown", "unknown"),
            state("processing", "unknown", "unknown"),
            state("processing", "-6.2°", "4.5°"),
            state("processing", "-6.2°", "4.5°"),
            state("processing", "-6.2°", "4.5°"),
            // An error without a reading: the reading no longer holds.
            state("processing", "unknown", "unknown"),
            state("processing", "unknown", "unknown"),
            state("processing", "unknown", "unknown"),
        ]
    );
    let alerts: Vec<_> = alerts(&events)
        .into_iter()
        .map(|a| (a.severity, a.message.clone()))
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
    rig.take();

    // A device information message after the handshake stays private.
    fake.send(&testing::device_info_frame());
    let shot = testing::protobuf_frame(&testing::shot_envelope(9, 3000.0, 2.0));
    fake.send(&shot);
    rig.poll(&mut session).expect("poll");
    let audit = rig.logs(Level::Trace);
    let events = rig.take();

    let frames = raw_frames(&events);
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
    let k = kinds(&events);
    let first_raw = k
        .iter()
        .position(|k| *k == "shot.raw")
        .expect("a raw frame");
    let trigger = k
        .iter()
        .position(|k| *k == "shot.trigger")
        .expect("a trigger");
    assert!(first_raw < trigger);
    let FrpBody::ShotRaw(f) = bodies(&events)[first_raw] else {
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
    assert!(raw_frames(&rig.take()).is_empty());
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
    let events = rig.take();
    let frames = raw_frames(&events);
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
    let events = rig.take();
    let requests: Vec<_> = raw_frames(&events)
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
    rig.take();
    // A protobuf message whose envelope does not decode.
    fake.send(&testing::protobuf_frame(&[0xff, 0xff, 0xff]));
    fake.send_envelope(&testing::shot_envelope(3, 2000.0, 1.0));
    rig.poll(&mut session).expect("poll goes on");
    let events = rig.take();
    assert!(
        alerts(&events)
            .iter()
            .any(|a| a.severity == Severity::Warn && a.message.starts_with("R10 protocol error:"))
    );
    assert!(kinds(&events).contains(&"shot.trigger"));
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
}

#[test]
fn a_stop_ends_the_handshake() {
    // The device never answers; the host stops on its second tick.
    let fake = Fake::default();
    let mut rig = Rig::on(TestHost::new().stop_after_ticks(2), false);
    let mut d = driver(None);
    let err = rig.connect(&mut d, &fake).unwrap_err();
    assert!(matches!(err, Error::Stopped), "{err}");
}

#[test]
fn close_releases_the_device() {
    let fake = Fake::default();
    fake.handshake();
    let mut rig = Rig::new(false);
    let mut d = driver(None);
    let session = rig
        .establish(&mut d, &fake, Fake::release)
        .expect("handshake");
    assert_eq!(fake.0.borrow().released, 0);
    rig.close(session);
    assert_eq!(fake.0.borrow().released, 1);
}

/// A fake that registers, then answers every read with a NAK: no poll is
/// ever empty.
fn chattering() -> Fake {
    let fake = Fake::default();
    let mut w = fake.0.borrow_mut();
    w.rx.push_back(testing::register_response(HANDLE));
    let nak = testing::ack_frame(MSG_CONFIGURATION, 1, &[]);
    let chunks = testing::chunks(HANDLE, &nak, 20);
    assert_eq!(chunks.len(), 1, "one NAK per read");
    w.chatter = chunks.into_iter().next();
    drop(w);
    fake
}

#[test]
fn the_handshake_is_bounded_while_the_device_keeps_talking() {
    // Each pass ticks, and each tick takes 1 ms of the host's time.
    let mut rig = Rig::on(TestHost::new().tick_takes(Duration::from_millis(1)), false);
    rig.host.flags.set_min_level(Level::Info);
    let started = rig.host.now();
    let mut d = driver(None);
    let err = rig.connect(&mut d, &chattering()).unwrap_err();
    assert!(
        matches!(err, Error::Protocol(crate::Error::HandshakeTimeout)),
        "{err}"
    );
    assert_eq!(rig.host.now() - started, HANDSHAKE_TIMEOUT);
}

#[test]
fn a_stop_ends_the_handshake_while_the_device_keeps_talking() {
    let mut rig = Rig::on(TestHost::new().stop_after_ticks(5), false);
    rig.host.flags.set_min_level(Level::Info);
    let mut d = driver(None);
    let err = rig.connect(&mut d, &chattering()).unwrap_err();
    assert!(matches!(err, Error::Stopped), "{err}");
    assert_eq!(rig.host.ticks(), 5);
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
    rig.host.flags.set_min_level(Level::Debug);
    let mut d = driver(None);
    let mut session = armed(&mut rig, &mut d, &fake);
    assert!(!session.client.transport().enabled());
    assert!(rig.logs(Level::Trace).is_empty());

    // Raw frames wanted: capture starts with the next poll.
    rig.host.flags.set_wants_raw(true);
    fake.send_envelope(&testing::shot_envelope(9, 3000.0, 2.0));
    rig.poll(&mut session).expect("poll");
    assert!(session.client.transport().enabled());
    assert!(!raw_frames(&rig.take()).is_empty());
}

#[test]
fn identity_audit_lines_do_not_depend_on_raw_frames_being_wanted() {
    let run = |wants_raw: bool| {
        let fake = Fake::default();
        let mut rig = Rig::new(wants_raw);
        let mut d = driver(None);
        let mut session = armed(&mut rig, &mut d, &fake);
        fake.send(&testing::device_info_frame());
        fake.send_envelope(&testing::shot_envelope(9, 3000.0, 2.0));
        rig.poll(&mut session).expect("poll");
        assert!(session.client.transport().enabled());
        // The device information messages carry identity; the shot's
        // messages are audit lines only while raw frames are not wanted.
        let mut lines = rig.logs(Level::Trace);
        lines.retain(|l| l.contains(" 13a0 "));
        lines
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
    assert!(Config::help_toml().contains("# aim_offset_deg = (unset)"));
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
    assert_eq!(run(&["--set", "aim_offset_deg=11"]), ExitCode::from(2));
    assert_eq!(run(&["--help"]), ExitCode::SUCCESS);
}
