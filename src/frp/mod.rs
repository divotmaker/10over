//! FRP device server — bridges 10over's R10 protocol to the
//! [Flight Relay Protocol](https://github.com/flightrelay/spec).
//!
//! Maps [`Event`]s from a connected R10 to FRP envelopes and streams them
//! to any connected FRP controller over WebSocket (port 5880).
//!
//! Requires the `frp` feature.

mod convert;

use flightrelay::{
    FrpConnection, FrpEnvelope, FrpEvent, FrpListener, FrpMessage, FrpProtocolMessage, ShotKey,
    SPEC_VERSION,
};

use crate::client::Event;

pub use convert::{ball_flight, club_data};

/// An FRP device server backed by an R10 connection.
///
/// Manages the FRP listener and converts [`Event`]s into FRP envelopes.
/// The caller drives both the `Client` poll loop and this server in the
/// same thread.
pub struct FrpServer {
    listener: FrpListener,
    conn: Option<FrpConnection>,
    device: String,
    shot_number: u32,
}

impl FrpServer {
    /// Bind the FRP listener on the given address (e.g. `"0.0.0.0:5880"`).
    ///
    /// # Errors
    ///
    /// Returns an error if the TCP listener cannot bind.
    pub fn bind(addr: &str) -> Result<Self, flightrelay::FrpError> {
        let listener = FrpListener::bind(addr, &[SPEC_VERSION])?;
        Ok(Self {
            listener,
            conn: None,
            device: String::new(),
            shot_number: 0,
        })
    }

    /// Set the device name (e.g. `"Garmin R10 F5:D1:88:F6:90:5D"`).
    pub fn set_device_name(&mut self, name: &str) {
        name.clone_into(&mut self.device);
    }

    /// Accept a controller connection (blocking).
    ///
    /// Replaces any existing connection.
    ///
    /// # Errors
    ///
    /// Returns an error if the WebSocket handshake fails.
    pub fn accept(&mut self) -> Result<(), flightrelay::FrpError> {
        let conn = self.listener.accept()?;
        conn.set_nonblocking(true)?;
        self.conn = Some(conn);
        Ok(())
    }

    /// Whether a controller is currently connected.
    #[must_use]
    pub fn has_controller(&self) -> bool {
        self.conn.is_some()
    }

    /// Try to accept a new controller if none is connected (non-blocking).
    ///
    /// `FrpListener` does not support non-blocking accept, so this is a
    /// no-op stub. For controller reconnect, restart the server or call
    /// `accept()` from a background thread.
    pub fn try_accept(&mut self) {
        // FrpListener wraps TcpListener without exposing set_nonblocking,
        // so non-blocking accept isn't possible without upstream changes.
    }

    /// Poll for incoming controller commands (non-blocking).
    ///
    /// Returns a [`DetectionMode`](flightrelay::DetectionMode) if the
    /// controller sent `set_detection_mode`. The R10 does not support
    /// mode switching, so the caller can log and ignore this.
    pub fn check_controller(&mut self) -> Option<flightrelay::DetectionMode> {
        let conn = self.conn.as_mut()?;
        match conn.try_recv() {
            Ok(Some(FrpMessage::Protocol(FrpProtocolMessage::SetDetectionMode {
                mode, ..
            }))) => mode,
            Err(flightrelay::FrpError::Closed) => {
                self.conn = None;
                None
            }
            _ => None,
        }
    }

    /// Send a device info envelope identifying the R10.
    ///
    /// # Errors
    ///
    /// Returns an error if the send fails.
    pub fn send_device_info(&mut self) -> Result<(), flightrelay::FrpError> {
        let Some(conn) = self.conn.as_mut() else {
            return Ok(());
        };

        let mut telemetry = std::collections::HashMap::new();
        telemetry.insert("ready".to_owned(), "true".to_owned());

        let env = FrpEnvelope {
            device: self.device.clone(),
            event: FrpEvent::DeviceTelemetry {
                manufacturer: Some("Garmin".to_owned()),
                model: Some("Approach R10".to_owned()),
                firmware: None,
                telemetry: Some(telemetry),
            },
        };

        conn.send_envelope(&env).or_else(|e| {
            if matches!(e, flightrelay::FrpError::Closed) {
                self.conn = None;
                Ok(())
            } else {
                Err(e)
            }
        })
    }

    /// Process a client [`Event`] and send any resulting FRP envelopes.
    ///
    /// The R10 delivers all shot data atomically in a single `Event::Shot`.
    /// This emits the full FRP sequence: `ShotTrigger → BallFlight →
    /// ClubPath → ShotFinished`.
    ///
    /// # Errors
    ///
    /// Returns an error if a send fails (other than connection close).
    pub fn handle_event(&mut self, event: &Event) -> Result<(), flightrelay::FrpError> {
        let shot = match event {
            Event::Ready => return self.send_ready(true),
            Event::StateChange(_) => return self.send_ready(false),
            Event::Shot(shot) => shot,
            _ => return Ok(()),
        };

        // Send ready=false before the shot sequence
        self.send_ready(false)?;

        self.shot_number += 1;
        let key = ShotKey {
            shot_id: uuid_v4(),
            shot_number: self.shot_number,
        };

        let mut events = vec![FrpEvent::ShotTrigger { key: key.clone() }];

        if let Some(ref ball) = shot.ball {
            events.push(FrpEvent::BallFlight {
                key: key.clone(),
                ball: convert::ball_flight(ball),
            });
        }

        if let Some(ref club) = shot.club {
            events.push(FrpEvent::ClubPath {
                key: key.clone(),
                club: convert::club_data(club),
            });
        }

        events.push(FrpEvent::ShotFinished { key });

        self.send_events(&events)
    }

    fn send_ready(&mut self, ready: bool) -> Result<(), flightrelay::FrpError> {
        let Some(conn) = self.conn.as_mut() else {
            return Ok(());
        };

        let mut telemetry = std::collections::HashMap::new();
        telemetry.insert("ready".to_owned(), ready.to_string());

        let env = FrpEnvelope {
            device: self.device.clone(),
            event: FrpEvent::DeviceTelemetry {
                manufacturer: Some("Garmin".to_owned()),
                model: Some("Approach R10".to_owned()),
                firmware: None,
                telemetry: Some(telemetry),
            },
        };

        conn.send_envelope(&env).or_else(|e| {
            if matches!(e, flightrelay::FrpError::Closed) {
                self.conn = None;
                Ok(())
            } else {
                Err(e)
            }
        })
    }

    fn send_events(&mut self, events: &[FrpEvent]) -> Result<(), flightrelay::FrpError> {
        let Some(conn) = self.conn.as_mut() else {
            return Ok(());
        };

        for event in events {
            let env = FrpEnvelope {
                device: self.device.clone(),
                event: event.clone(),
            };
            match conn.send_envelope(&env) {
                Ok(()) => {}
                Err(flightrelay::FrpError::Closed) => {
                    self.conn = None;
                    return Ok(());
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

/// Generate a UUID v4 string without pulling in the `uuid` crate.
fn uuid_v4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seed = t.as_nanos();

    // xorshift128+ with time-based seed
    #[allow(clippy::cast_possible_truncation)]
    let mut s0 = seed as u64;
    #[allow(clippy::cast_possible_truncation)]
    let mut s1 = seed.wrapping_mul(6_364_136_223_846_793_005) as u64;
    if s0 == 0 {
        s0 = 0x1234_5678_9abc_def0;
    }
    if s1 == 0 {
        s1 = 0xfedc_ba98_7654_3210;
    }

    let mut bytes = [0u8; 16];
    for chunk in bytes.chunks_exact_mut(8) {
        let mut x = s0;
        let y = s1;
        s0 = y;
        x ^= x << 23;
        x ^= x >> 17;
        x ^= y;
        x ^= y >> 26;
        s1 = x;
        let val = s0.wrapping_add(s1);
        chunk.copy_from_slice(&val.to_le_bytes());
    }

    // Set version 4 and variant bits
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;

    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    )
}
