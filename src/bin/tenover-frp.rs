//! tenover-frp: the Garmin R10 as a Flight Relay Protocol launch monitor.
//!
//! Finds a paired R10 over BLE, arms it, and publishes its shots on an FRP
//! bus as a launch monitor actor, reconnecting whenever the device drops.
//!
//! ```text
//! tenover-frp [--config <path>] [--target <listen-addr | ws://host:port/frp>]
//!             [--set <key>=<value>]... [--log <info|debug|trace>] [--help]
//! ```
//!
//! `--target` defaults to listening on `0.0.0.0:5880`; a `ws://` URL dials a
//! hub such as flighthook instead. The actor's id is kept in the config
//! file's `[frp]` table. `--help` lists every config key.

use std::process::ExitCode;

fn main() -> ExitCode {
    flightrelay::run::<tenover::driver::R10>()
}
