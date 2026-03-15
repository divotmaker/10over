// "BlueZ" and "btleplug" are brand names, not code identifiers.
#![allow(clippy::doc_markdown)]

#[cfg(feature = "ble")]
pub mod ble;
#[cfg(ble_backend_bluez)]
pub mod bluez;
#[cfg(ble_backend_btleplug)]
pub mod btleplug;
pub mod client;
pub mod cobs;
pub mod crc;
pub mod error;
#[cfg(feature = "frp")]
pub mod frp;
pub mod gfdi;
pub mod multilink;
pub mod proto;

#[cfg(feature = "ble")]
pub use ble::{BleError, BleTransport};
pub use client::{Client, Event, Transport};
pub use error::Error;
#[cfg(feature = "frp")]
pub use frp::FrpServer;
pub use proto::ShotData;
