//! Client library for the Garmin R10 launch monitor.
//!
//! # Features
//!
//! - `ble` (default): platform BLE transport (`BleTransport`).
//! - `config`: the driver's settings (`config::Config`), with no BLE stack.
//! - `driver`: the flightrelay `DeviceDriver` for the R10 (`driver::R10`);
//!   implies `config` and `ble`.
//! - `frp`: the `tenover-frp` binary, the driver served over the Flight
//!   Relay Protocol; implies `driver`.
//! - `serde`: serialization of shot data types.
//! - `testing`: frame builders with synthetic device identity for
//!   downstream tests (`testing` module). Not for production use.

// "BlueZ" and "btleplug" are brand names, not code identifiers.
#![allow(clippy::doc_markdown)]

#[cfg(feature = "ble")]
pub mod ble;
#[cfg(ble_backend_bluez)]
pub mod bluez;
#[cfg(ble_backend_btleplug)]
pub mod btleplug;
pub mod capture;
pub mod client;
pub mod cobs;
#[cfg(feature = "config")]
pub mod config;
pub mod crc;
#[cfg(feature = "driver")]
pub mod driver;
pub mod error;
pub mod gfdi;
pub mod multilink;
pub mod proto;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
#[cfg(test)]
mod wire_golden;

#[cfg(feature = "ble")]
pub use ble::{BleError, BleTransport};
pub use client::{Client, Event, Transport};
pub use error::Error;
pub use proto::ShotData;
