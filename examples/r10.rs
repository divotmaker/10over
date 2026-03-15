//! Connect to a Garmin R10 and print shots.
//!
//! Usage:
//!   cargo run --example r10
//!
//! Auto-discovers a paired R10 and connects. The device must be paired
//! at the OS level.

use std::thread;
use std::time::Duration;

use tenover::ble::BleTransport;
use tenover::{Client, Event};

const MS_TO_MPH: f32 = 2.237;

fn main() {
    eprintln!("searching for Garmin R10...");
    let transport = match BleTransport::auto_connect() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("connection failed: {e}");
            std::process::exit(1);
        }
    };

    let mtu = transport.mtu();
    eprintln!("connected  {}  mtu={mtu}", transport.device_address());

    let mut client = Client::new(transport, mtu.into());
    client.start().expect("failed to send REGISTER");

    let mut shot_count = 0u32;
    let mut ready_printed = false;

    loop {
        match client.poll() {
            Ok(Some(event)) => match event {
                Event::Registered { handle } => {
                    eprintln!("registered  handle={handle}");
                }
                Event::HandshakeComplete => {
                    eprintln!("handshake complete");
                }
                Event::Subscribed { .. } | Event::WakeUpResponse { .. } => {}
                Event::Ready => {
                    if !ready_printed {
                        eprintln!("READY — waiting for shot");
                        ready_printed = true;
                    }
                }
                Event::StateChange(_) => {
                    ready_printed = false;
                }
                Event::DeviceError(err) => {
                    eprintln!("DEVICE ERROR: {err:?}");
                }
                Event::Shot(shot) => {
                    ready_printed = false;
                    shot_count += 1;
                    println!("\n── Shot #{shot_count} (id={}) ──", shot.shot_id);

                    if let Some(b) = &shot.ball {
                        println!(
                            "  Ball: {:.1} mph  LA {:.1}°  Dir {:.1}°",
                            b.ball_speed * MS_TO_MPH,
                            b.launch_angle,
                            b.launch_direction,
                        );
                        println!(
                            "  Spin: {:.0} RPM  axis {:.1}°  (back {:.0}, side {:.0})  [{:?}]",
                            b.total_spin, b.spin_axis, b.backspin, b.sidespin, b.spin_calc_type,
                        );
                    }

                    if let Some(c) = &shot.club {
                        println!(
                            "  Club: {:.1} mph  face {:.1}°  path {:.1}°  AoA {:.1}°",
                            c.club_head_speed * MS_TO_MPH,
                            c.face_angle,
                            c.path_angle,
                            c.attack_angle,
                        );
                    }

                    if let Some(s) = &shot.swing {
                        let tempo = s.downswing_start.saturating_sub(s.backswing_start);
                        let down = s.impact.saturating_sub(s.downswing_start);
                        println!("  Tempo: backswing {tempo}ms  downswing {down}ms");
                    }
                }
            },
            Ok(None) => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(tenover::Error::Transport(e)) => {
                eprintln!("transport error: {e}");
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("warning: {e}");
            }
        }
    }
}
