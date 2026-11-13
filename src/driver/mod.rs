//! The flightrelay [`DeviceDriver`] for the Garmin R10 (`driver` feature).
//!
//! [`R10`] finds a paired R10 over BLE, runs the handshake, arms the device
//! and turns what it reports into FRP messages. A host drives it:
//! flightrelay's [`FrpServer`](https://docs.rs/flightrelay/0.6/flightrelay/struct.FrpServer.html)
//! (the `tenover-frp` binary), or an application with its own loop. The host
//! publishes the launch monitor actor's `bus.actor`, with the identity
//! (manufacturer "Garmin", model "R10") and `source`
//! ([`Source::Measured`]) in its `device` block.
//!
//! What a session emits:
//!
//! - after the handshake, the `not_armed` blocker;
//! - when the device arms, no blockers and the telemetry key
//!   `device_state=waiting`; as it records and processes a shot,
//!   `device_state=recording` and `processing`;
//! - per shot, all at once as the R10 reports it: `shot.trigger` (without a
//!   timestamp: the R10 has no wall clock), `shot.launch` (see
//!   [`convert::launch`]), `shot.club` when the device tracked the club,
//!   `shot.finished`, then the `not_armed` blocker until it arms again;
//! - a device error as a `bus.alert`; one that carries the device's tilt
//!   sets the attitude (`tilt` is the pitch, `roll` the roll, in degrees);
//! - `shot.raw` frames while the host turns them on: one per GFDI message,
//!   labeled `"<notify|write> <message type, hex>"`. Messages that carry the
//!   device's identity or keys, messages that cannot be classified, and
//!   everything before the handshake finished (the MultiLink registration
//!   included) are never sent; they reach only the host's log, as trace
//!   lines with the hex. While raw frames are off and the host shows no
//!   trace lines, messages are not decoded for capture at all.
//!
//! The R10 has no detection mode, hand or club setting, so the hints of
//! `golfer.turn` are ignored. The configured tee distance
//! ([`Config::range`]) is sent once the device wakes; changing it takes a
//! new session. The aim offset ([`Config::aim_offset_deg`], **beta**)
//! corrects the horizontal angles of the FRP output ([`convert`]); a change
//! reaches the running session and applies from the next shot.
//!
//! The handshake must finish within 10 s of its start, however much the
//! device sends meanwhile. Closing or dropping a session on Windows and
//! macOS (btleplug) disconnects the R10, waiting at most 4 s; on Linux the
//! BlueZ connection is left up.

pub mod convert;
mod raw;

use std::fmt::Write as _;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use flightrelay::driver::{
    Backoff, ConfigError, ConnectCtx, DeviceDriver, DeviceIdentity, DeviceSession, DriverConfig,
    DriverInfo, Events, Level, Live, NoAction, RawProfile, SessionEnd, ShotIds, Stop, Stopped,
};
use flightrelay::{FrpBody, RawFrame, Severity, Source};

use crate::ble::{BleError, BleTransport};
use crate::client::{Client, Event, Transport};
use crate::config::Config;
use crate::proto::{DeviceState, ErrorSeverity};

use raw::Tap;

/// Longest wait from the start of the handshake to its end.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// How often a connect calls `ctx.tick()` while it waits.
const TICK: Duration = Duration::from_millis(100);
/// Sleep between reads while the device is quiet.
const IDLE_SLEEP: Duration = Duration::from_millis(5);
/// Empty polls in a row before a read loop sleeps.
const IDLE_POLLS: u32 = 3;
/// The MTU a session over a test transport uses.
#[cfg(any(test, feature = "testing"))]
const TEST_MTU: usize = 20;

/// Why a connect or a session ended.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The host stopped.
    #[error("stopped")]
    Stopped,
    /// No R10 was found, or the BLE connection could not be set up.
    #[error(transparent)]
    Ble(#[from] BleError),
    /// The connection failed: a transport error, or no handshake in time.
    #[error(transparent)]
    Protocol(#[from] crate::Error),
    /// The thread running the BLE connect could not be started or failed.
    #[error("BLE connect thread: {0}")]
    Thread(String),
}

impl From<Stopped> for Error {
    fn from(_: Stopped) -> Self {
        Self::Stopped
    }
}

/// The Garmin R10 driver. See the [module docs](self).
#[derive(Debug)]
pub struct R10 {
    config: Config,
    ids: ShotIds,
    /// [`Config::aim_offset`], shared with the sessions.
    aim_offset: Live<f64>,
    /// [`HANDSHAKE_TIMEOUT`]; tests shorten it.
    handshake_timeout: Duration,
}

impl R10 {
    /// Run the handshake over `transport`, as [`connect`](DeviceDriver::connect)
    /// does once BLE is up, with `address` as the device id. For tests: a
    /// fake [`Transport`] fed with [`testing`](crate::testing) frames.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Stopped`] after a stop, a transport error, or
    /// [`Error::Protocol`] with a handshake timeout.
    #[cfg(any(test, feature = "testing"))]
    pub fn connect_with<T: Transport>(
        &mut self,
        ctx: &mut ConnectCtx<'_>,
        transport: T,
        address: &str,
    ) -> Result<R10Session<T>, Error> {
        self.establish(ctx, transport, address, TEST_MTU, keep)
    }

    /// Start the client on `transport` and run it until the handshake is
    /// complete, for at most the handshake timeout from now. `release`
    /// frees the device when the session closes.
    fn establish<T: Transport>(
        &mut self,
        ctx: &mut ConnectCtx<'_>,
        transport: T,
        address: &str,
        mtu: usize,
        release: fn(&mut T),
    ) -> Result<R10Session<T>, Error> {
        let deadline = Instant::now() + self.handshake_timeout;
        ctx.out.phase("handshaking");
        let identity = DeviceIdentity::new(address)
            .with_manufacturer("Garmin")
            .with_model("R10")
            .with_label(format!("Garmin R10 ({address})"));
        let mut session = R10Session {
            client: Client::new(Tap::new(transport), mtu),
            identity,
            ids: self.ids.clone(),
            aim_offset: self.aim_offset.clone(),
            tee_distance_yd: self.config.tee_distance_yd(),
            release,
        };
        session.client.start()?;
        let mut ticked = Instant::now();
        let mut idle = 0;
        loop {
            if ticked.elapsed() >= TICK {
                ctx.tick()?;
                ticked = Instant::now();
            }
            if Instant::now() >= deadline {
                return Err(crate::Error::HandshakeTimeout.into());
            }
            // Before the handshake every message is audit-only.
            session
                .client
                .transport_mut()
                .set_enabled(ctx.out.enabled(Level::Trace));
            let result = session.client.poll();
            session.flush_raw(&mut ctx.out, false);
            match result {
                Ok(Some(Event::HandshakeComplete)) => {
                    ctx.out.log(Level::Info, "handshake complete");
                    ctx.out.ready(false);
                    return Ok(session);
                }
                Ok(Some(event)) => {
                    idle = 0;
                    session.handle(event, &mut ctx.out);
                }
                Ok(None) => {
                    idle += 1;
                    if idle >= IDLE_POLLS {
                        thread::sleep(IDLE_SLEEP);
                    }
                }
                Err(e @ crate::Error::Transport(_)) => return Err(e.into()),
                Err(e) => protocol_error(&e, &mut ctx.out),
            }
        }
    }
}

impl DeviceDriver for R10 {
    type Config = Config;
    type Error = Error;
    type Action = NoAction;
    type Session = R10Session;

    /// A BLE connect checks its stop flag at least every 100 ms, and its one
    /// step that cannot be cut short is a D-Bus call with a 3 s timeout,
    /// followed by a disconnect bounded the same way.
    const STOP_WAIT: Duration = Duration::from_secs(6);

    fn info(_: &Config) -> DriverInfo {
        DriverInfo::new("tenover", "Garmin R10", Source::Measured)
            .with_manufacturer("Garmin")
            .exclusive()
            .with_raw(RawProfile::new("r10-ble", 256, 256, 0, no_bulk, false))
    }

    fn new(config: Config) -> Result<Self, ConfigError> {
        config.validate()?;
        Ok(Self {
            aim_offset: Live::new(config.aim_offset()),
            config,
            ids: ShotIds::new(),
            handshake_timeout: HANDSHAKE_TIMEOUT,
        })
    }

    fn config(&self) -> &Config {
        &self.config
    }

    fn connect(&mut self, ctx: &mut ConnectCtx<'_>) -> Result<R10Session, Error> {
        ctx.tick()?;
        ctx.out.phase("discovering");
        ctx.out.log(Level::Info, "searching for Garmin R10");
        let transport = scan(ctx)?;
        let address = transport.device_address().to_owned();
        let mtu = transport.mtu();
        ctx.out.log(
            Level::Info,
            format!("connected to R10 at {address} (mtu={mtu})"),
        );
        self.establish(
            ctx,
            transport,
            &address,
            mtu.into(),
            BleTransport::disconnect,
        )
    }

    /// A warn alert naming the R10 for every failed attempt, then the
    /// backoff.
    fn retry_delay(
        &mut self,
        ended: &SessionEnd<'_, Error>,
        backoff: &mut Backoff,
        out: &mut Events<'_>,
    ) -> Option<Duration> {
        if let Some(error) = ended.error {
            out.alert(Severity::Warn, format!("R10 connection error: {error}"));
        }
        Some(backoff.delay(ended.connected_for))
    }

    fn apply_config(&mut self, new: &Config) {
        self.aim_offset.set(new.aim_offset());
        self.config = new.clone();
    }
}

/// No R10 message is bulk data.
fn no_bulk(_: &RawFrame) -> bool {
    false
}

/// Find and connect the R10 on a helper thread, ticking while it works and
/// forwarding its progress lines as debug logs. A stop cancels the scan;
/// the helper is joined before this returns, so the device is free again.
fn scan(ctx: &mut ConnectCtx<'_>) -> Result<BleTransport, Error> {
    let stop: Stop = ctx.stop.clone();
    let (log_tx, log_rx) = mpsc::channel::<String>();
    let helper = thread::Builder::new()
        .name("tenover-ble-connect".to_owned())
        .spawn(move || {
            let mut log = |line: &str| {
                let _ = log_tx.send(line.to_owned());
            };
            BleTransport::auto_connect_with(stop.flag(), &mut log)
        })
        .map_err(|e| Error::Thread(e.to_string()))?;
    let mut stopped = false;
    let mut ticked = Instant::now();
    loop {
        match log_rx.recv_timeout(TICK) {
            Ok(line) => ctx.out.log(Level::Debug, line),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            // The helper is done: its sender is gone.
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if !stopped && ticked.elapsed() >= TICK {
            stopped = ctx.tick().is_err();
            ticked = Instant::now();
        }
    }
    let result = helper
        .join()
        .map_err(|_| Error::Thread("panicked".to_owned()))?;
    finish_scan(
        result,
        stopped || ctx.stop.is_stopped(),
        BleTransport::disconnect,
    )
}

/// The outcome of a BLE connect. After a stop, a connection that came up
/// anyway is released with `release` and the connect ends as stopped.
fn finish_scan<T>(
    result: Result<T, BleError>,
    stopped: bool,
    release: fn(&mut T),
) -> Result<T, Error> {
    if stopped {
        if let Ok(mut transport) = result {
            release(&mut transport);
        }
        return Err(Error::Stopped);
    }
    Ok(result?)
}

/// A transport with nothing to release.
#[cfg(any(test, feature = "testing"))]
fn keep<T>(_: &mut T) {}

/// Raise a wire or decode error that leaves the session usable.
fn protocol_error(e: &crate::Error, out: &mut Events<'_>) {
    out.log(Level::Warn, format!("protocol error: {e}"));
    out.alert(Severity::Warn, format!("R10 protocol error: {e}"));
}

/// A connected R10. Over BLE it is [`R10`]'s [`DeviceSession`]; over any
/// other [`Transport`] (from `R10::connect_with`, `testing` feature) the same methods are
/// inherent.
pub struct R10Session<T: Transport = BleTransport> {
    client: Client<Tap<T>>,
    identity: DeviceIdentity,
    ids: ShotIds,
    aim_offset: Live<f64>,
    tee_distance_yd: Option<f32>,
    /// Frees the device on close.
    release: fn(&mut T),
}

/// Leaves the identity out: it holds the BLE address.
impl<T: Transport> std::fmt::Debug for R10Session<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("R10Session")
            .field("phase", &self.client.phase())
            .finish_non_exhaustive()
    }
}

impl<T: Transport> R10Session<T> {
    /// [`DeviceSession::poll`]: read and handle what the device sends for
    /// about `timeout`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Protocol`] with the transport error that ended the
    /// connection.
    pub fn poll(&mut self, timeout: Duration, out: &mut Events<'_>) -> Result<(), Error> {
        let deadline = Instant::now() + timeout;
        let capture = out.wants_raw() || out.enabled(Level::Trace);
        self.client.transport_mut().set_enabled(capture);
        let mut idle = 0;
        loop {
            let result = self.client.poll();
            self.flush_raw(out, true);
            match result {
                Ok(Some(event)) => {
                    idle = 0;
                    self.handle(event, out);
                }
                // A read can bring part of a message: sleep only once
                // several polls in a row found nothing.
                Ok(None) => {
                    idle += 1;
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        break;
                    }
                    if idle >= IDLE_POLLS {
                        thread::sleep(left.min(IDLE_SLEEP));
                    }
                }
                Err(e @ crate::Error::Transport(_)) => return Err(e.into()),
                Err(e) => protocol_error(&e, out),
            }
            if Instant::now() >= deadline {
                break;
            }
        }
        self.flush_raw(out, true);
        Ok(())
    }

    /// [`DeviceSession::identity`].
    #[must_use]
    pub fn identity(&self) -> &DeviceIdentity {
        &self.identity
    }

    /// [`DeviceSession::close`]: raise the `not_armed` blocker and release the
    /// device. Over BLE that disconnects on btleplug (bounded at 4 s) and
    /// drops the connection on BlueZ.
    ///
    /// # Errors
    ///
    /// None today; the signature matches the trait.
    pub fn close(mut self, out: &mut Events<'_>) -> Result<(), Error> {
        out.ready(false);
        (self.release)(self.client.transport_mut().inner_mut());
        Ok(())
    }

    /// Emit the messages captured since the last call: as `shot.raw` frames
    /// when raw frames are on and they may leave, else as audit lines.
    fn flush_raw(&mut self, out: &mut Events<'_>, connected: bool) {
        for c in self.client.transport_mut().drain() {
            if !connected || c.private {
                out.audit(c.dir, &c.label, &c.data);
            } else if out.wants_raw() {
                let frame = RawFrame {
                    timestamp: c.at_ms,
                    label: Some(c.label),
                    ..RawFrame::new(c.dir, 0, c.data)
                };
                out.frp(FrpBody::ShotRaw(frame));
            }
        }
    }

    fn handle(&mut self, event: Event, out: &mut Events<'_>) {
        match event {
            Event::Registered { handle } => {
                out.log(
                    Level::Info,
                    format!("MultiLink registered (handle={handle})"),
                );
            }
            Event::HandshakeComplete | Event::Subscribed { success: true } => {}
            // Without the subscription no shot reaches the host.
            Event::Subscribed { success: false } => {
                out.log(Level::Warn, "device refused the shot subscription");
            }
            Event::Ready => {
                out.log(Level::Info, "armed, waiting for shots");
                // A level reading from an earlier error no longer holds.
                out.attitude(None, None);
                out.ready(true);
                out.telemetry([("device_state", "waiting")]);
            }
            Event::Shot(shot) => {
                let key = self.ids.next_key();
                out.log(
                    Level::Info,
                    format!("shot #{} (device shot {})", key.shot_number, shot.shot_id),
                );
                out.shot_trigger(key.clone());
                let aim = self.aim_offset.get();
                if let Some(ball) = &shot.ball {
                    out.launch(key.clone(), convert::launch(ball, aim));
                }
                if let Some(club) = &shot.club {
                    out.club(key.clone(), convert::club_data(club, aim));
                }
                out.shot_finished(key);
                out.attitude(None, None);
                out.ready(false);
            }
            Event::StateChange(state) => {
                out.log(Level::Debug, format!("device state: {state:?}"));
                // Standby and noise check are internal states that flicker
                // while the device retransmits a shot.
                let label = match state {
                    DeviceState::Waiting => Some("waiting"),
                    DeviceState::Recording => Some("recording"),
                    DeviceState::Processing => Some("processing"),
                    _ => None,
                };
                if let Some(label) = label {
                    out.telemetry([("device_state", label)]);
                }
            }
            Event::DeviceError(err) => {
                let severity = match err.severity {
                    ErrorSeverity::Warning => Severity::Warn,
                    ErrorSeverity::Serious | ErrorSeverity::Fatal => Severity::Error,
                };
                let mut message = format!("R10: {:?}", err.code);
                if let Some((roll, pitch)) = err.tilt {
                    let _ = write!(message, " (roll={roll:.1}, pitch={pitch:.1})");
                    out.attitude(degrees(pitch), degrees(roll));
                } else {
                    // An error without a reading takes the last one off.
                    out.attitude(None, None);
                }
                out.log(Level::Warn, message.clone());
                out.alert(severity, message);
            }
            // The device is awake, so it takes its environment settings.
            // Without a configured range nothing is sent, which leaves the
            // device's own tee distance alone.
            Event::WakeResult { .. } => {
                if let Some(yards) = self.tee_distance_yd {
                    let env = crate::proto::Environment {
                        tee_distance_yd: Some(yards),
                        ..crate::proto::Environment::default()
                    };
                    match self.client.send_environment(&env) {
                        Ok(()) => out.log(Level::Debug, format!("sent tee distance {yards:.1} yd")),
                        Err(e) => out.log(Level::Warn, format!("failed to send tee distance: {e}")),
                    }
                }
            }
            Event::EnvironmentResult { success } => {
                if success {
                    out.log(
                        Level::Info,
                        format!(
                            "device accepted tee distance {:.1} yd",
                            self.tee_distance_yd.unwrap_or_default()
                        ),
                    );
                } else {
                    out.log(Level::Warn, "device rejected the tee distance");
                }
            }
        }
    }
}

impl DeviceSession for R10Session {
    type Driver = R10;

    fn poll(&mut self, timeout: Duration, out: &mut Events<'_>) -> Result<(), Error> {
        R10Session::poll(self, timeout, out)
    }

    fn identity(&self) -> &DeviceIdentity {
        R10Session::identity(self)
    }

    fn close(self, out: &mut Events<'_>) -> Result<(), Error> {
        R10Session::close(self, out)
    }
}

/// An angle from the device as `f64`, with the decimal value the `f32`
/// prints as (4.3, not 4.300000190734863).
fn degrees(angle: f32) -> f64 {
    angle.to_string().parse().unwrap_or(f64::from(angle))
}

#[cfg(test)]
mod tests;
