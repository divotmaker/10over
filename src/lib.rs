//! Client library for the Garmin R10 launch monitor.
//!
//! # Features
//!
//! - `ble` (default): platform BLE transport (`BleTransport`).
//! - `frp`: Flight Relay Protocol device server (`FrpDevice`).
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
pub mod crc;
pub mod error;
#[cfg(feature = "frp")]
pub mod frp;
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
#[cfg(feature = "frp")]
pub use frp::FrpDevice;
pub use proto::ShotData;
