//! Cross-platform BLE transport using btleplug (Windows/macOS/Linux).
//!
//! Runs a tokio runtime in a background thread. Notifications are forwarded
//! via `std::sync::mpsc`; writes are sent to the background thread via
//! `tokio::sync::mpsc` and executed as `peripheral.write()`.
//!
//! The device must be pre-paired at the OS level — btleplug has no pairing API.
//!
//! # Usage
//!
//! ```ignore
//! use tenover::btleplug::BtleplugTransport;
//! use tenover::{Client, Event};
//!
//! let transport = BtleplugTransport::auto_connect()?;
//! let mut client = Client::new(transport, 20);
//! client.start()?;
//! loop {
//!     match client.poll()? {
//!         Some(Event::Shot(shot)) => println!("{shot:?}"),
//!         Some(event) => println!("{event:?}"),
//!         None => {}
//!     }
//! }
//! ```

use std::io;
use std::sync::mpsc;
use std::thread;

use btleplug::api::{
    Central, Characteristic, Manager as _, Peripheral as _, ScanFilter, WriteType,
};
use btleplug::platform::{Adapter, Manager, Peripheral};
use tokio::runtime::Runtime;
use tokio::sync::mpsc as tokio_mpsc;
use uuid::Uuid;

use crate::client::Transport;

/// Full UUID for characteristic 6A4E2810 (bidirectional: register + notifications).
const CHAR_2810_UUID: Uuid = uuid::uuid!("6a4e2810-667b-11e3-949a-0800200c9a66");
/// Full UUID for characteristic 6A4E2820 (write-only: GFDI data).
const CHAR_2820_UUID: Uuid = uuid::uuid!("6a4e2820-667b-11e3-949a-0800200c9a66");
/// R10 device name as reported by BLE advertisement.
const R10_DEVICE_NAME: &str = "Approach R10";
/// Garmin manufacturer ID in BLE advertisements.
const GARMIN_MANUFACTURER_ID: u16 = 0x0087;

/// Discovery timeout.
const SCAN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// Time to wait for GATT service discovery after connection.
const SERVICE_DISCOVERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Write channel capacity — small buffer, writes are fast.
const WRITE_CHANNEL_CAP: usize = 32;

/// Errors from btleplug transport setup.
#[derive(Debug, thiserror::Error)]
pub enum BtleplugError {
    /// btleplug library error.
    #[error("btleplug: {0}")]
    Btleplug(#[from] btleplug::Error),

    /// No BLE adapter found on the system.
    #[error("no BLE adapter found")]
    NoAdapter,

    /// No paired Garmin R10 found.
    #[error("no Garmin R10 found — pair at OS level first")]
    DeviceNotFound,

    /// Required GATT characteristic not found.
    #[error("characteristic {0} not found — is the R10 connected and paired?")]
    CharacteristicNotFound(&'static str),

    /// Background runtime failed to start.
    #[error("runtime error: {0}")]
    Runtime(String),

    /// I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
}

/// A write command sent to the background thread.
enum WriteCmd {
    /// Write to the data characteristic (6A4E2820).
    Data(Vec<u8>),
    /// Write to the register characteristic (6A4E2810).
    Register(Vec<u8>),
}

/// Cross-platform BLE transport for the Garmin R10.
///
/// Internally spawns a background thread running a tokio runtime that handles
/// async BLE operations. The synchronous `Transport` trait methods use channels
/// to communicate with the background thread.
pub struct BtleplugTransport {
    /// Inbound notifications from the background thread.
    notify_rx: mpsc::Receiver<Vec<u8>>,
    /// Outbound write commands to the background thread.
    write_tx: tokio_mpsc::Sender<WriteCmd>,
    /// BLE address of the connected device.
    device_address: String,
    /// Keep the background thread alive.
    _bg_thread: thread::JoinHandle<()>,
}

impl BtleplugTransport {
    /// Find a paired Garmin R10, connect, and set up the BLE transport.
    ///
    /// Scans for a device named "Approach R10" or with Garmin manufacturer ID
    /// (0x0087). The device must be pre-paired at the OS level.
    ///
    /// # Errors
    ///
    /// Returns [`BtleplugError`] if no R10 is found, connection fails, or
    /// characteristic discovery fails.
    pub fn auto_connect() -> Result<Self, BtleplugError> {
        // Build a temporary runtime for the connection setup (blocking).
        let rt = Runtime::new().map_err(|e| BtleplugError::Runtime(e.to_string()))?;
        let (peripheral, address) = rt.block_on(discover_and_connect())?;
        Self::start_background(peripheral, address, rt)
    }

    /// Connect to a known device by BLE address string (e.g. `F5:D1:88:F6:90:5D`).
    ///
    /// The device must already be paired and connectable.
    ///
    /// # Errors
    ///
    /// Returns [`BtleplugError`] if the device is not found or connection fails.
    pub fn connect(address: &str) -> Result<Self, BtleplugError> {
        let rt = Runtime::new().map_err(|e| BtleplugError::Runtime(e.to_string()))?;
        let (peripheral, addr) = rt.block_on(connect_by_address(address))?;
        Self::start_background(peripheral, addr, rt)
    }

    /// BLE address of the connected device.
    #[must_use]
    pub fn device_address(&self) -> &str {
        &self.device_address
    }

    fn start_background(
        peripheral: Peripheral,
        address: String,
        rt: Runtime,
    ) -> Result<Self, BtleplugError> {
        // Find characteristics before handing off to background thread.
        let chars = rt.block_on(async {
            peripheral.discover_services().await?;
            find_characteristics(&peripheral)
        })?;

        // Subscribe to notifications on 2810.
        rt.block_on(peripheral.subscribe(&chars.notify))?;

        // Channels: notifications (bg → foreground), writes (foreground → bg).
        let (notify_tx, notify_rx) = mpsc::channel::<Vec<u8>>();
        let (write_tx, write_rx) = tokio_mpsc::channel::<WriteCmd>(WRITE_CHANNEL_CAP);

        let bg_thread = thread::spawn(move || {
            rt.block_on(background_loop(peripheral, chars, notify_tx, write_rx));
        });

        Ok(Self {
            notify_rx,
            write_tx,
            device_address: address,
            _bg_thread: bg_thread,
        })
    }
}

impl Transport for BtleplugTransport {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, io::Error> {
        match self.notify_rx.try_recv() {
            Ok(data) => {
                let len = data.len().min(buf.len());
                buf[..len].copy_from_slice(&data[..len]);
                Ok(len)
            }
            Err(mpsc::TryRecvError::Empty) => Err(io::Error::from(io::ErrorKind::WouldBlock)),
            Err(mpsc::TryRecvError::Disconnected) => {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "BLE background thread exited"))
            }
        }
    }

    fn write(&mut self, data: &[u8]) -> Result<(), io::Error> {
        self.write_tx
            .blocking_send(WriteCmd::Data(data.to_vec()))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "BLE background thread exited"))
    }

    fn write_register(&mut self, data: &[u8]) -> Result<(), io::Error> {
        self.write_tx
            .blocking_send(WriteCmd::Register(data.to_vec()))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "BLE background thread exited"))
    }
}

/// Resolved characteristic handles for the two MultiLink channels.
struct Chars {
    /// 6A4E2810 — notifications (subscribe) and register writes.
    notify: Characteristic,
    /// 6A4E2810 — same char, used for register writes.
    register: Characteristic,
    /// 6A4E2820 — data writes.
    data: Characteristic,
}

/// Find the two required characteristics on the peripheral.
fn find_characteristics(peripheral: &Peripheral) -> Result<Chars, BtleplugError> {
    let chars = peripheral.characteristics();

    let char_2810 = chars
        .iter()
        .find(|c| c.uuid == CHAR_2810_UUID)
        .ok_or(BtleplugError::CharacteristicNotFound("6A4E2810"))?
        .clone();

    let char_2820 = chars
        .iter()
        .find(|c| c.uuid == CHAR_2820_UUID)
        .ok_or(BtleplugError::CharacteristicNotFound("6A4E2820"))?
        .clone();

    Ok(Chars {
        notify: char_2810.clone(),
        register: char_2810,
        data: char_2820,
    })
}

/// Background async loop: forwards notifications and drains writes.
async fn background_loop(
    peripheral: Peripheral,
    chars: Chars,
    notify_tx: mpsc::Sender<Vec<u8>>,
    mut write_rx: tokio_mpsc::Receiver<WriteCmd>,
) {
    use btleplug::api::Peripheral as _;
    use tokio_stream::StreamExt as _;

    let Ok(mut notif_stream) = peripheral.notifications().await else {
        return;
    };

    loop {
        tokio::select! {
            Some(notif) = notif_stream.next() => {
                // Only forward notifications from 2810 (our subscribed char).
                if notif.uuid == CHAR_2810_UUID
                    && notify_tx.send(notif.value).is_err()
                {
                    break; // foreground dropped
                }
            }
            Some(cmd) = write_rx.recv() => {
                let result = match cmd {
                    WriteCmd::Data(data) => {
                        peripheral
                            .write(&chars.data, &data, WriteType::WithoutResponse)
                            .await
                    }
                    WriteCmd::Register(data) => {
                        peripheral
                            .write(&chars.register, &data, WriteType::WithResponse)
                            .await
                    }
                };
                if result.is_err() {
                    break; // BLE disconnected
                }
            }
            else => break,
        }
    }
}

/// Scan for and connect to a Garmin R10.
async fn discover_and_connect() -> Result<(Peripheral, String), BtleplugError> {
    let adapter = get_adapter().await?;

    // Scan with no filter — we'll match by name/manufacturer ourselves.
    eprintln!("  starting BLE scan ({SCAN_TIMEOUT:?})...");
    adapter.start_scan(ScanFilter::default()).await?;
    tokio::time::sleep(SCAN_TIMEOUT).await;
    adapter.stop_scan().await?;

    let peripherals = adapter.peripherals().await?;
    eprintln!("  scan found {} peripherals", peripherals.len());

    for p in &peripherals {
        let addr = peripheral_address(p);
        let name = p
            .properties()
            .await
            .ok()
            .flatten()
            .and_then(|props| props.local_name.clone())
            .unwrap_or_default();
        eprintln!("    {addr}  {name:?}");

        if is_r10(p).await {
            eprintln!("  → matched R10, connecting...");
            p.connect().await?;
            return Ok((p.clone(), addr));
        }
    }

    Err(BtleplugError::DeviceNotFound)
}

/// Connect to a peripheral by BLE address string.
async fn connect_by_address(target: &str) -> Result<(Peripheral, String), BtleplugError> {
    let adapter = get_adapter().await?;

    // Peripherals may be cached from prior connections.
    let peripherals = adapter.peripherals().await?;
    for p in &peripherals {
        let address = peripheral_address(p);
        if address.eq_ignore_ascii_case(target) {
            p.connect().await?;
            return Ok((p.clone(), address));
        }
    }

    // Try a short scan if the device wasn't cached.
    adapter.start_scan(ScanFilter::default()).await?;
    tokio::time::sleep(SERVICE_DISCOVERY_TIMEOUT).await;
    adapter.stop_scan().await?;

    let peripherals = adapter.peripherals().await?;
    for p in &peripherals {
        let address = peripheral_address(p);
        if address.eq_ignore_ascii_case(target) {
            p.connect().await?;
            return Ok((p.clone(), address));
        }
    }

    Err(BtleplugError::DeviceNotFound)
}

/// Get the first available BLE adapter.
async fn get_adapter() -> Result<Adapter, BtleplugError> {
    let manager = Manager::new().await?;
    let adapters = manager.adapters().await?;
    adapters.into_iter().next().ok_or(BtleplugError::NoAdapter)
}

/// Check if a peripheral looks like a Garmin R10.
async fn is_r10(peripheral: &Peripheral) -> bool {
    let Ok(Some(props)) = peripheral.properties().await else {
        return false;
    };

    // Match by name.
    if props.local_name.as_deref() == Some(R10_DEVICE_NAME) {
        return true;
    }

    // Match by Garmin manufacturer ID.
    props.manufacturer_data.contains_key(&GARMIN_MANUFACTURER_ID)
}

/// Extract a string address from a btleplug peripheral.
fn peripheral_address(peripheral: &Peripheral) -> String {
    peripheral.id().to_string()
}
