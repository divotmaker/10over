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
use std::sync::atomic::AtomicBool;

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

    /// [`auto_connect`](Self::auto_connect) that a caller can cut short.
    ///
    /// Setting `cancel` from another thread ends the attempt with the
    /// backend's `Canceled` error within about 6 s, and a connection this
    /// call opened is closed again. `log` receives progress lines (on
    /// btleplug, also the address and name of every peripheral the scan
    /// found).
    ///
    /// # Errors
    ///
    /// Returns [`BleError`] if no R10 is found, connection fails, or `cancel`
    /// was set.
    pub fn auto_connect_with(
        cancel: &AtomicBool,
        log: &mut dyn FnMut(&str),
    ) -> Result<Self, BleError> {
        #[cfg(ble_backend_bluez)]
        let transport = Self::BlueZ(crate::bluez::BlueZTransport::auto_connect_with(
            cancel, log,
        )?);

        #[cfg(ble_backend_btleplug)]
        let transport = Self::Btleplug(crate::btleplug::BtleplugTransport::auto_connect_with(
            cancel, log,
        )?);

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

    /// Release the device. On btleplug this disconnects the peripheral and
    /// stops the background thread, waiting at most 4 s; dropping the
    /// transport does the same. On BlueZ it does nothing: the connection
    /// stays up at the BlueZ level when the transport is dropped. Reads and
    /// writes after a disconnect fail.
    pub fn disconnect(&mut self) {
        match self {
            #[cfg(ble_backend_bluez)]
            Self::BlueZ(_) => {}
            #[cfg(ble_backend_btleplug)]
            Self::Btleplug(t) => t.disconnect(),
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

/// A background thread that runs until asked to stop, and that a caller can
/// wait for with a bound.
#[cfg(any(test, ble_backend_btleplug))]
pub(crate) struct Worker {
    stop: std::sync::Arc<AtomicBool>,
    /// Disconnects when the thread has finished (its sender is dropped).
    done: std::sync::mpsc::Receiver<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(any(test, ble_backend_btleplug))]
impl Worker {
    /// Run `work` on a new thread named `name`. `work` gets the stop flag
    /// and should return soon after it is set.
    pub(crate) fn spawn(
        name: &str,
        work: impl FnOnce(std::sync::Arc<AtomicBool>) + Send + 'static,
    ) -> io::Result<Self> {
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let (done_tx, done) = std::sync::mpsc::channel::<()>();
        let flag = std::sync::Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                let _done = done_tx;
                work(flag);
            })?;
        Ok(Self {
            stop,
            done,
            thread: Some(thread),
        })
    }

    /// Set the stop flag and wait up to `wait` for the thread to finish.
    /// Returns whether it did; one that has not is left to finish on its
    /// own. Later calls return at once.
    pub(crate) fn stop(&mut self, wait: std::time::Duration) -> bool {
        use std::sync::atomic::Ordering;
        use std::sync::mpsc::RecvTimeoutError;

        self.stop.store(true, Ordering::SeqCst);
        let Some(thread) = self.thread.take() else {
            return true;
        };
        match self.done.recv_timeout(wait) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                let _ = thread.join();
                true
            }
            Err(RecvTimeoutError::Timeout) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn a_worker_stops_when_asked() {
        let mut worker = Worker::spawn("test-worker", |stop| {
            while !stop.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(5));
            }
        })
        .expect("spawn");
        let started = Instant::now();
        assert!(worker.stop(Duration::from_secs(5)));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(
            worker.stop(Duration::from_secs(5)),
            "a second stop returns at once"
        );
    }

    #[test]
    fn a_stuck_worker_is_waited_for_only_up_to_the_bound() {
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let mut worker = Worker::spawn("test-worker", move |_| {
            let _ = release_rx.recv();
        })
        .expect("spawn");
        let started = Instant::now();
        assert!(!worker.stop(Duration::from_millis(100)));
        let waited = started.elapsed();
        assert!(waited >= Duration::from_millis(100) && waited < Duration::from_secs(2));
        drop(release_tx);
    }

    #[test]
    fn a_finished_worker_stops_at_once() {
        let mut worker = Worker::spawn("test-worker", |_| {}).expect("spawn");
        let started = Instant::now();
        assert!(worker.stop(Duration::from_secs(5)));
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
