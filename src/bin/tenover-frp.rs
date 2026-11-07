//! tenover-frp: a Flight Relay Protocol device for the Garmin R10.
//!
//! Finds a paired R10 over BLE, arms it, and serves its shots to FRP
//! controllers, reconnecting whenever the device drops.
//!
//! ```text
//! tenover-frp [--config <path>] [--target <listen-addr | ws://host:port/frp>]
//!             [--set <key>=<value>]... [--log <info|debug|trace>] [--help]
//! ```
//!
//! `--target` defaults to listening on `0.0.0.0:5880`; a `ws://` URL dials a
//! controller such as flighthook instead. `--help` lists every config key.

use std::process::ExitCode;

fn main() -> ExitCode {
    flightrelay::run::<tenover::driver::R10>()
}
