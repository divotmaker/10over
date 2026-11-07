//! Cross-platform BLE transport using btleplug (Windows/macOS/Linux).
//!
//! Runs a tokio runtime in a background thread. Notifications are forwarded
//! via `std::sync::mpsc`; writes are sent to the background thread via
//! `tokio::sync::mpsc` and executed as `peripheral.write()`. Dropping the
//! transport disconnects the peripheral and ends the thread.
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

use std::future::Future;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use btleplug::api::{
    Central, Characteristic, Manager as _, Peripheral as _, ScanFilter, WriteType,
};
use btleplug::platform::{Adapter, Manager, Peripheral};
use tokio::runtime::Runtime;
use tokio::sync::mpsc as tokio_mpsc;
use uuid::Uuid;

use crate::ble::Worker;
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
const SCAN_TIMEOUT: Duration = Duration::from_secs(10);
/// Time to wait for GATT service discovery after connection.
const SERVICE_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);
/// How often a cancelable wait looks at the cancel flag.
const CANCEL_POLL: Duration = Duration::from_millis(100);
/// Longest wait for a disconnect.
const DISCONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// Longest wait for the background thread to disconnect and finish: a
/// write in flight, then the disconnect.
const STOP_TIMEOUT: Duration = Duration::from_secs(4);

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

    /// The caller's cancel flag was set.
    #[error("canceled")]
    Canceled,
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
    /// The background thread; stopping it disconnects the peripheral.
    worker: Worker,
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
        Self::auto_connect_with(&AtomicBool::new(false), &mut |_| {})
    }

    /// [`auto_connect`](Self::auto_connect) that a caller can cut short.
    ///
    /// Setting `cancel` (from another thread) ends the attempt with
    /// [`BtleplugError::Canceled`]: every step (the 10 s scan, the connect,
    /// service discovery, the subscription) is raced against the flag, which
    /// is checked every 100 ms, and a peripheral this call connected is
    /// disconnected again. `log` receives progress lines, including the
    /// peripherals the scan found.
    ///
    /// # Errors
    ///
    /// Returns [`BtleplugError`] if no R10 is found, connection fails,
    /// characteristic discovery fails, or `cancel` was set.
    pub fn auto_connect_with(
        cancel: &AtomicBool,
        log: &mut dyn FnMut(&str),
    ) -> Result<Self, BtleplugError> {
        // Build a temporary runtime for the connection setup (blocking).
        let rt = Runtime::new().map_err(|e| BtleplugError::Runtime(e.to_string()))?;
        let (peripheral, address) = rt.block_on(discover_and_connect(cancel, log))?;
        Self::start_background(peripheral, address, rt, cancel)
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
        Self::start_background(peripheral, addr, rt, &AtomicBool::new(false))
    }

    /// BLE address of the connected device.
    #[must_use]
    pub fn device_address(&self) -> &str {
        &self.device_address
    }

    /// Disconnect the peripheral and end the background thread, waiting at
    /// most 4 s. Later reads and writes fail; later calls return at once.
    pub(crate) fn disconnect(&mut self) {
        let _ = self.worker.stop(STOP_TIMEOUT);
    }

    /// Discover the characteristics, subscribe, and hand the peripheral to
    /// the background thread. A failure or a cancel disconnects it.
    fn start_background(
        peripheral: Peripheral,
        address: String,
        rt: Runtime,
        cancel: &AtomicBool,
    ) -> Result<Self, BtleplugError> {
        let setup = rt.block_on(async {
            // Find characteristics before handing off to background thread.
            cancelable(cancel, peripheral.discover_services()).await??;
            let chars = find_characteristics(&peripheral)?;
            // Subscribe to notifications on 2810.
            cancelable(cancel, peripheral.subscribe(&chars.notify)).await??;
            Ok::<_, BtleplugError>(chars)
        });
        let chars = match setup {
            Ok(chars) => chars,
            Err(e) => {
                rt.block_on(async {
                    let _ = tokio::time::timeout(DISCONNECT_TIMEOUT, peripheral.disconnect()).await;
                });
                return Err(e);
            }
        };

        // Channels: notifications (bg → foreground), writes (foreground → bg).
        let (notify_tx, notify_rx) = mpsc::channel::<Vec<u8>>();
        let (write_tx, write_rx) = tokio_mpsc::channel::<WriteCmd>(WRITE_CHANNEL_CAP);

        let worker = Worker::spawn("tenover-btleplug", move |stop| {
            rt.block_on(background_loop(
                peripheral, chars, notify_tx, write_rx, stop,
            ));
            rt.shutdown_background();
        })?;

        Ok(Self {
            notify_rx,
            write_tx,
            device_address: address,
            worker,
        })
    }
}

impl Drop for BtleplugTransport {
    fn drop(&mut self) {
        self.disconnect();
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
            Err(mpsc::TryRecvError::Disconnected) => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "BLE background thread exited",
            )),
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

/// Background async loop: forwards notifications and drains writes until
/// `stop` is set or the link fails, then disconnects the peripheral.
async fn background_loop(
    peripheral: Peripheral,
    chars: Chars,
    notify_tx: mpsc::Sender<Vec<u8>>,
    write_rx: tokio_mpsc::Receiver<WriteCmd>,
    stop: Arc<AtomicBool>,
) {
    relay(&peripheral, &chars, &notify_tx, write_rx, &stop).await;
    let _ = tokio::time::timeout(DISCONNECT_TIMEOUT, peripheral.disconnect()).await;
}

/// Forward notifications and drain writes until `stop` is set, the
/// foreground is gone, or the link fails.
async fn relay(
    peripheral: &Peripheral,
    chars: &Chars,
    notify_tx: &mpsc::Sender<Vec<u8>>,
    mut write_rx: tokio_mpsc::Receiver<WriteCmd>,
    stop: &AtomicBool,
) {
    use btleplug::api::Peripheral as _;
    use tokio_stream::StreamExt as _;

    let Ok(mut notif_stream) = peripheral.notifications().await else {
        return;
    };

    // Both inputs closed: nothing is left to relay.
    let mut notifying = true;
    let mut writing = true;
    while (notifying || writing) && !stop.load(Ordering::SeqCst) {
        tokio::select! {
            // Wake up now and then to look at `stop`.
            () = tokio::time::sleep(CANCEL_POLL) => {}
            notif = notif_stream.next(), if notifying => {
                let Some(notif) = notif else {
                    notifying = false;
                    continue;
                };
                // Only forward notifications from 2810 (our subscribed char).
                if notif.uuid == CHAR_2810_UUID
                    && notify_tx.send(notif.value).is_err()
                {
                    break; // foreground dropped
                }
            }
            cmd = write_rx.recv(), if writing => {
                let result = match cmd {
                    Some(WriteCmd::Data(data)) => {
                        peripheral
                            .write(&chars.data, &data, WriteType::WithoutResponse)
                            .await
                    }
                    Some(WriteCmd::Register(data)) => {
                        peripheral
                            .write(&chars.register, &data, WriteType::WithResponse)
                            .await
                    }
                    None => {
                        writing = false;
                        continue;
                    }
                };
                if result.is_err() {
                    break; // BLE disconnected
                }
            }
        }
    }
}

/// `fut`, or `Err(Canceled)` as soon as `cancel` is set.
async fn cancelable<F: Future>(cancel: &AtomicBool, fut: F) -> Result<F::Output, BtleplugError> {
    tokio::pin!(fut);
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(BtleplugError::Canceled);
        }
        tokio::select! {
            out = &mut fut => return Ok(out),
            () = tokio::time::sleep(CANCEL_POLL) => {}
        }
    }
}

/// Scan for and connect to a Garmin R10.
async fn discover_and_connect(
    cancel: &AtomicBool,
    log: &mut dyn FnMut(&str),
) -> Result<(Peripheral, String), BtleplugError> {
    let adapter = cancelable(cancel, get_adapter()).await??;

    // Scan with no filter — we'll match by name/manufacturer ourselves.
    log(&format!("starting BLE scan ({SCAN_TIMEOUT:?})"));
    cancelable(cancel, adapter.start_scan(ScanFilter::default())).await??;
    let scanned = cancelable(cancel, tokio::time::sleep(SCAN_TIMEOUT)).await;
    let stopped = tokio::time::timeout(DISCONNECT_TIMEOUT, adapter.stop_scan()).await;
    scanned?;
    if let Ok(result) = stopped {
        result?;
    }

    let peripherals = cancelable(cancel, adapter.peripherals()).await??;
    log(&format!("scan found {} peripherals", peripherals.len()));

    for p in &peripherals {
        let addr = peripheral_address(p);
        let name = cancelable(cancel, p.properties())
            .await?
            .ok()
            .flatten()
            .and_then(|props| props.local_name.clone())
            .unwrap_or_default();
        log(&format!("{addr}  {name:?}"));

        if cancelable(cancel, is_r10(p)).await? {
            log("matched an R10, connecting");
            match cancelable(cancel, p.connect()).await {
                Ok(connected) => connected?,
                Err(canceled) => {
                    let _ = tokio::time::timeout(DISCONNECT_TIMEOUT, p.disconnect()).await;
                    return Err(canceled);
                }
            }
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
    props
        .manufacturer_data
        .contains_key(&GARMIN_MANUFACTURER_ID)
}

/// Extract a string address from a btleplug peripheral.
fn peripheral_address(peripheral: &Peripheral) -> String {
    peripheral.id().to_string()
}
