#![allow(clippy::doc_markdown)]
//! tenover-frp — FlightRelay Protocol device server for Garmin R10.
//!
//! Connects to a Garmin R10 over BLE, arms it, and serves shot data over
//! FRP (WebSocket on port 5880) to any connected controller.

use std::io::Write;
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use tenover::ble::BleTransport;
use tenover::frp::FrpServer;
use tenover::{Client, Event};

fn main() -> ExitCode {
    let frp_addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "0.0.0.0:5880".to_owned());

    // Bind FRP server first so controllers can connect while we scan
    let mut frp = match FrpServer::bind(&frp_addr) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("tenover-frp: failed to bind FRP server: {e}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!("tenover-frp: FRP server listening on {frp_addr}");

    // Accept controller connection (blocking)
    eprintln!("tenover-frp: waiting for FRP controller...");
    if let Err(e) = frp.accept() {
        eprintln!("tenover-frp: controller accept failed: {e}");
        return ExitCode::FAILURE;
    }
    eprintln!("tenover-frp: controller connected");

    // Connect to R10
    eprintln!("tenover-frp: searching for Garmin R10...");
    let transport = match BleTransport::auto_connect() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("tenover-frp: connection failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    let addr = transport.device_address().to_owned();
    let mtu = transport.mtu();
    eprintln!("tenover-frp: connected  {addr}  mtu={mtu}");

    frp.set_device_name(&format!("Garmin R10 {addr}"));
    if let Err(e) = frp.send_device_info() {
        eprintln!("tenover-frp: send device_info failed: {e}");
    }

    let mut client = Client::new(transport, mtu.into());
    if let Err(e) = client.start() {
        eprintln!("tenover-frp: failed to send REGISTER: {e}");
        return ExitCode::FAILURE;
    }

    let mut shot_count = 0u32;
    let mut ready_printed = false;

    loop {
        match client.poll() {
            Ok(Some(event)) => {
                match &event {
                    Event::Registered { handle } => {
                        eprintln!("tenover-frp: registered  handle={handle}");
                    }
                    Event::HandshakeComplete => {
                        eprintln!("tenover-frp: handshake complete");
                    }
                    Event::Ready => {
                        if !ready_printed {
                            eprintln!("tenover-frp: ready");
                            ready_printed = true;
                        }
                    }
                    Event::StateChange(_) => {
                        ready_printed = false;
                    }
                    Event::Shot(shot) => {
                        ready_printed = false;
                        shot_count += 1;
                        let ball_mph = shot
                            .ball
                            .as_ref()
                            .map_or(0.0, |b| b.ball_speed * 2.237);
                        eprintln!(
                            "tenover-frp: shot #{shot_count} — {ball_mph:.1} mph"
                        );
                    }
                    Event::DeviceError(err) => {
                        eprintln!("tenover-frp: device error: {err:?}");
                    }
                    _ => {}
                }

                if let Err(e) = frp.handle_event(&event) {
                    eprintln!("tenover-frp: FRP send error: {e}");
                }

                // Check for controller commands
                if let Some(mode) = frp.check_controller() {
                    eprintln!("tenover-frp: mode request: {mode:?} (R10 does not support mode switching)");
                }
            }
            Ok(None) => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(tenover::Error::Transport(e)) => {
                eprintln!("tenover-frp: transport error: {e}");
                return ExitCode::FAILURE;
            }
            Err(e) => {
                eprintln!("tenover-frp: warning: {e}");
            }
        }

        let _ = std::io::stderr().flush();
    }
}
