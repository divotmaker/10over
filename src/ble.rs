//! Platform-agnostic BLE transport for the Garmin R10.
//!
//! Auto-detects the backend by target: `BlueZTransport` on Linux,
//! `BtleplugTransport` on Windows/macOS. Enabled by default via the
//! `ble` feature.
//!
//! ```ignore
//! use tenover::ble::BleTransport;
//! use tenover::{Client, Event};
//!
//! let transport = BleTransport::auto_connect()?;
//! let mtu = transport.mtu();
//! let mut client = Client::new(transport, mtu.into());
//! client.start()?;
//! ```

use std::io;

use crate::client::Transport;

/// Errors from BLE transport setup.
#[derive(Debug, thiserror::Error)]
pub enum BleError {
    /// BlueZ backend error (Linux).
    #[cfg(ble_backend_bluez)]
    #[error(transparent)]
    BlueZ(#[from] crate::bluez::BlueZError),

    /// btleplug backend error (Windows/macOS).
    #[cfg(ble_backend_btleplug)]
    #[error(transparent)]
    Btleplug(#[from] crate::btleplug::BtleplugError),
}

/// Platform-agnostic BLE transport for the Garmin R10.
///
/// On Linux, delegates to `BlueZTransport` (D-Bus file descriptors).
/// On Windows/macOS, delegates to `BtleplugTransport` (async bridge).
pub enum BleTransport {
    /// BlueZ file-descriptor transport (Linux).
    #[cfg(ble_backend_bluez)]
    BlueZ(crate::bluez::BlueZTransport),

    /// btleplug async-bridge transport (Windows/macOS).
    #[cfg(ble_backend_btleplug)]
    Btleplug(crate::btleplug::BtleplugTransport),
}

impl BleTransport {
    /// Find a paired Garmin R10 and connect.
    ///
    /// Uses the platform-appropriate discovery mechanism. The device must be
    /// pre-paired at the OS level.
    ///
    /// # Errors
    ///
    /// Returns [`BleError`] if no R10 is found or connection fails.
    pub fn auto_connect() -> Result<Self, BleError> {
        #[cfg(ble_backend_bluez)]
        let transport = Self::BlueZ(crate::bluez::BlueZTransport::auto_connect()?);

        #[cfg(ble_backend_btleplug)]
        let transport = Self::Btleplug(crate::btleplug::BtleplugTransport::auto_connect()?);

        Ok(transport)
    }

    /// BLE address of the connected device.
    #[must_use]
    pub fn device_address(&self) -> &str {
        match self {
            #[cfg(ble_backend_bluez)]
            Self::BlueZ(t) => t.device_address(),
            #[cfg(ble_backend_btleplug)]
            Self::Btleplug(t) => t.device_address(),
        }
    }

    /// BLE MTU for the connection.
    ///
    /// On BlueZ, this is the negotiated MTU. On btleplug, returns the
    /// default BLE MTU (23) — the R10 client caps at 20 regardless.
    #[must_use]
    pub fn mtu(&self) -> u16 {
        match self {
            #[cfg(ble_backend_bluez)]
            Self::BlueZ(t) => t.mtu(),
            #[cfg(ble_backend_btleplug)]
            Self::Btleplug(_) => 23,
        }
    }
}

impl Transport for BleTransport {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, io::Error> {
        match self {
            #[cfg(ble_backend_bluez)]
            Self::BlueZ(t) => t.read(buf),
            #[cfg(ble_backend_btleplug)]
            Self::Btleplug(t) => t.read(buf),
        }
    }

    fn write(&mut self, data: &[u8]) -> Result<(), io::Error> {
        match self {
            #[cfg(ble_backend_bluez)]
            Self::BlueZ(t) => t.write(data),
            #[cfg(ble_backend_btleplug)]
            Self::Btleplug(t) => t.write(data),
        }
    }

    fn write_register(&mut self, data: &[u8]) -> Result<(), io::Error> {
        match self {
            #[cfg(ble_backend_bluez)]
            Self::BlueZ(t) => t.write_register(data),
            #[cfg(ble_backend_btleplug)]
            Self::Btleplug(t) => t.write_register(data),
        }
    }
}
