//! Client library for the Garmin R10 launch monitor.
//!
//! [`Client`] runs the R10's protocol over any [`Transport`]: MultiLink
//! registration, the GFDI handshake, the shot subscription and the wake,
//! then the device's shots and states as [`Event`]s. `BleTransport`
//! (`ble` feature) finds and connects a paired R10. Nothing blocks:
//! [`Client::poll`] returns `Ok(None)` when there is nothing to read.
//!
//! ```
//! use tenover::{Client, Event, Transport};
//!
//! fn shots(transport: impl Transport) -> Result<(), tenover::Error> {
//!     let mut client = Client::new(transport, 23);
//!     client.start()?;
//!     loop {
//!         match client.poll()? {
//!             Some(Event::Shot(shot)) => {
//!                 if let Some(ball) = shot.ball {
//!                     println!("{:.1} m/s", ball.ball_speed);
//!                 }
//!             }
//!             Some(_) => {}
//!             None => std::thread::sleep(std::time::Duration::from_millis(5)),
//!         }
//!     }
//! }
//! ```
//!
//! # Features
//!
//! - `ble` (default): platform BLE transport (`BleTransport`): BlueZ on
//!   Linux, btleplug elsewhere.
//! - `config`: the driver's settings (`config::Config`), with no BLE stack.
//! - `driver`: the flightrelay `FrpDriver` for the R10 (`driver::R10`);
//!   implies `config` and `ble`.
//! - `frp`: the `tenover-frp` binary, the driver served over the Flight
//!   Relay Protocol; implies `driver`.
//! - `serde`: serialization of shot data types.
//! - `testing`: frame builders with synthetic device identity for
//!   downstream tests (`testing` module). Not for production use.

// "BlueZ" and "btleplug" are brand names, not code identifiers.
#![allow(clippy::doc_markdown)]
#![warn(missing_docs)]

#[cfg(feature = "ble")]
pub mod ble;
#[cfg(ble_backend_bluez)]
pub mod bluez;
#[cfg(ble_backend_btleplug)]
pub mod btleplug;
mod client;
mod cobs;
#[cfg(feature = "config")]
pub mod config;
mod crc;
#[cfg(feature = "driver")]
pub mod driver;
mod error;
pub mod gfdi;
mod multilink;
mod proto;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
#[cfg(test)]
mod wire_golden;

#[cfg(feature = "ble")]
pub use ble::{BleError, BleTransport};
pub use client::{Client, Event, Transport};
#[cfg(feature = "config")]
pub use config::Config;
pub use error::Error;
/// The flightrelay SDK this crate was built with, so callers name its types at the same version.
#[cfg(feature = "config")]
pub use flightrelay;
/// The protobuf library, whose `DecodeError` is in [`Error::Protobuf`].
pub use prost;
pub use proto::{
    BallData, ClubData, DeviceError, DeviceState, Environment, ErrorCode, ErrorSeverity, ShotData,
    ShotKind, SpinCalcType, SwingData,
};
