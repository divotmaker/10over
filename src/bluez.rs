//! BlueZ D-Bus transport for Linux.
//!
//! Uses `AcquireNotify`/`AcquireWrite` to get raw file descriptors from BlueZ,
//! then does non-blocking `read(2)`/`write(2)` — no async runtime needed.
//!
//! # Usage
//!
//! ```ignore
//! use tenover::bluez::BlueZTransport;
//! use tenover::{Client, Event};
//!
//! // Auto-discover a paired R10 and connect
//! let transport = BlueZTransport::auto_connect()?;
//! let mtu = transport.mtu();
//! let mut client = Client::new(transport, mtu.into());
//! client.start()?;
//! loop {
//!     match client.poll()? {
//!         Some(Event::Shot(shot)) => println!("{shot:?}"),
//!         Some(event) => println!("{event:?}"),
//!         None => {}
//!     }
//! }
//! ```

use std::collections::HashMap;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use zbus::blocking::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::client::Transport;

/// Full UUID for characteristic 6A4E2810 (bidirectional: register + notifications).
const CHAR_2810_UUID: &str = "6a4e2810-667b-11e3-949a-0800200c9a66";
/// Full UUID for characteristic 6A4E2820 (write-only: GFDI data).
const CHAR_2820_UUID: &str = "6a4e2820-667b-11e3-949a-0800200c9a66";

const BLUEZ_DEST: &str = "org.bluez";
const DEVICE_IFACE: &str = "org.bluez.Device1";
const GATT_CHAR_IFACE: &str = "org.bluez.GattCharacteristic1";
const DBUS_PROPS_IFACE: &str = "org.freedesktop.DBus.Properties";
const OBJECT_MANAGER_IFACE: &str = "org.freedesktop.DBus.ObjectManager";

/// R10 device name as reported by BLE advertisement.
const R10_DEVICE_NAME: &str = "Approach R10";

/// Longest any one D-Bus call may take on the connection
/// [`BlueZTransport::auto_connect_with`] uses.
const METHOD_TIMEOUT: Duration = Duration::from_secs(3);
/// How long a connect may go on after its `Connect` call timed out: BlueZ
/// keeps connecting, and the device shows as connected once it is.
const CONNECT_WAIT: Duration = Duration::from_secs(30);
/// How often a wait looks at the device and the cancel flag.
const POLL_SLICE: Duration = Duration::from_millis(100);
/// Polls of `ServicesResolved` before giving up (5 s).
const SERVICES_POLLS: u32 = 50;

/// Errors from BlueZ transport setup.
#[derive(Debug, thiserror::Error)]
pub enum BlueZError {
    /// D-Bus communication error.
    #[error("D-Bus error: {0}")]
    DBus(#[from] zbus::Error),

    /// No paired Garmin R10 found in BlueZ.
    #[error("no paired Garmin R10 found — pair with `bluetoothctl pair <address>`")]
    DeviceNotFound,

    /// Device found but GATT services did not resolve in time.
    #[error("timeout waiting for GATT services — try `bluetoothctl disconnect` then retry")]
    ServicesTimeout,

    /// Required GATT characteristic not found under the device.
    #[error("characteristic {0} not found — is the R10 connected and paired?")]
    CharacteristicNotFound(&'static str),

    /// I/O error during fd setup.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    /// The caller's cancel flag was set.
    #[error("canceled")]
    Canceled,
}

/// BlueZ file-descriptor transport for the Garmin R10.
///
/// Holds three file descriptors acquired from BlueZ via D-Bus:
/// - `notify_fd` — notifications from char `6A4E2810` (all inbound data)
/// - `write_fd` — writes to char `6A4E2820` (GFDI data with handle prefix)
/// - `register_fd` — writes to char `6A4E2810` (MultiLink REGISTER command)
///
/// All reads are non-blocking (`WouldBlock` when no data).
/// Writes are blocking (BLE flow control).
pub struct BlueZTransport {
    notify_fd: OwnedFd,
    write_fd: OwnedFd,
    register_fd: OwnedFd,
    mtu: u16,
    /// BLE address of the connected device (e.g. `F5:D1:88:F6:90:5D`).
    device_address: String,
    /// Keep the D-Bus connection alive — dropping it invalidates acquired fds.
    _conn: Connection,
}

impl BlueZTransport {
    /// Find a paired Garmin R10, connect if needed, and acquire BLE file descriptors.
    ///
    /// Scans BlueZ for a paired device named "Approach R10". If the device is not
    /// connected, initiates a BLE connection and waits for GATT service resolution.
    ///
    /// # Errors
    ///
    /// Returns [`BlueZError`] if no paired R10 is found, the connection times out,
    /// or fd acquisition fails.
    pub fn auto_connect() -> Result<Self, BlueZError> {
        Self::auto_connect_with(&AtomicBool::new(false), &mut |_| {})
    }

    /// [`auto_connect`](Self::auto_connect) that a caller can cut short.
    ///
    /// Setting `cancel` (from another thread) ends the attempt with
    /// [`BlueZError::Canceled`] within about 6 s: every D-Bus call has a 3 s
    /// timeout, the flag is checked between calls and every 100 ms while
    /// waiting, and a connect this call started is disconnected again. A
    /// `Connect` that times out is waited for, up to 30 s, while BlueZ keeps
    /// connecting. `log` receives progress lines.
    ///
    /// # Errors
    ///
    /// Returns [`BlueZError`] if no paired R10 is found, the connection times
    /// out, fd acquisition fails, or `cancel` was set.
    pub fn auto_connect_with(
        cancel: &AtomicBool,
        log: &mut dyn FnMut(&str),
    ) -> Result<Self, BlueZError> {
        let conn = zbus::blocking::connection::Builder::system()?
            .method_timeout(METHOD_TIMEOUT)
            .build()?;
        check(cancel)?;
        let (device_path, address) = find_r10(&conn)?;
        log("found a paired R10");
        check(cancel)?;
        let device = DbusDevice {
            conn: &conn,
            path: &device_path,
        };
        let started = ensure_connected(&device, cancel, log)?;
        let transport = Self::acquire_fds(conn.clone(), &device_path, address, cancel);
        if started && transport.is_err() {
            disconnect(&conn, &device_path);
        }
        transport
    }

    /// Connect to a paired R10 at a known BlueZ D-Bus path.
    ///
    /// `device_path` is the BlueZ D-Bus object path for the device, e.g.
    /// `/org/bluez/hci0/dev_F5_D1_88_F6_90_5D`. The device must already be
    /// paired, trusted, and connected.
    ///
    /// # Errors
    ///
    /// Returns [`BlueZError`] if the D-Bus connection fails, characteristics are
    /// not found, or fd acquisition fails.
    pub fn connect(device_path: &str) -> Result<Self, BlueZError> {
        let conn = Connection::system()?;
        let address = address_from_path(device_path);
        Self::acquire_fds(conn, device_path, address, &AtomicBool::new(false))
    }

    /// BLE MTU reported by BlueZ (minimum of notify and write MTUs).
    #[must_use]
    pub fn mtu(&self) -> u16 {
        self.mtu
    }

    /// BLE address of the connected device (e.g. `F5:D1:88:F6:90:5D`).
    #[must_use]
    pub fn device_address(&self) -> &str {
        &self.device_address
    }

    fn acquire_fds(
        conn: Connection,
        device_path: &str,
        device_address: String,
        cancel: &AtomicBool,
    ) -> Result<Self, BlueZError> {
        let (path_2810, path_2820) = discover_characteristics(&conn, device_path)?;
        check(cancel)?;

        // AcquireNotify on 2810 → notify_fd (all inbound notifications)
        let (notify_fd, notify_mtu) = acquire_fd(&conn, &path_2810, "AcquireNotify")?;
        check(cancel)?;

        // AcquireWrite on 2810 → register_fd (MultiLink REGISTER command)
        let (register_fd, _) = acquire_fd(&conn, &path_2810, "AcquireWrite")?;
        check(cancel)?;

        // AcquireWrite on 2820 → write_fd (GFDI data with handle prefix)
        let (write_fd, write_mtu) = acquire_fd(&conn, &path_2820, "AcquireWrite")?;

        // Notify fd must be non-blocking for poll-based Client
        set_nonblocking(&notify_fd)?;

        let mtu = notify_mtu.min(write_mtu);

        Ok(Self {
            notify_fd,
            write_fd,
            register_fd,
            mtu,
            device_address,
            _conn: conn,
        })
    }
}

impl Transport for BlueZTransport {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, io::Error> {
        // SAFETY: notify_fd is a valid open file descriptor from BlueZ AcquireNotify.
        let n = unsafe {
            libc::read(
                self.notify_fd.as_raw_fd(),
                buf.as_mut_ptr().cast(),
                buf.len(),
            )
        };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(n.cast_unsigned())
    }

    fn write(&mut self, data: &[u8]) -> Result<(), io::Error> {
        write_all_fd(&self.write_fd, data)
    }

    fn write_register(&mut self, data: &[u8]) -> Result<(), io::Error> {
        write_all_fd(&self.register_fd, data)
    }
}

/// Find a paired Garmin R10 in BlueZ's device list.
/// Returns `(device_path, ble_address)`.
fn find_r10(conn: &Connection) -> Result<(String, String), BlueZError> {
    let proxy = zbus::blocking::Proxy::new(conn, BLUEZ_DEST, "/", OBJECT_MANAGER_IFACE)?;
    let reply = proxy.call_method("GetManagedObjects", &())?;
    let objects: HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>> =
        reply.body().deserialize()?;

    for (path, interfaces) in &objects {
        let Some(props) = interfaces.get(DEVICE_IFACE) else {
            continue;
        };

        let name = prop_str(props, "Name").unwrap_or("");
        let paired = prop_bool(props, "Paired");

        if name == R10_DEVICE_NAME && paired {
            let address = prop_str(props, "Address").unwrap_or("").to_owned();
            return Ok((path.as_str().to_owned(), address));
        }
    }

    Err(BlueZError::DeviceNotFound)
}

/// Extract BLE address from a BlueZ D-Bus path.
/// `/org/bluez/hci0/dev_F5_D1_88_F6_90_5D` → `F5:D1:88:F6:90:5D`
fn address_from_path(path: &str) -> String {
    path.rsplit('/')
        .next()
        .and_then(|s| s.strip_prefix("dev_"))
        .map(|s| s.replace('_', ":"))
        .unwrap_or_default()
}

/// `Err(Canceled)` once `cancel` is set.
fn check(cancel: &AtomicBool) -> Result<(), BlueZError> {
    if cancel.load(Ordering::SeqCst) {
        Err(BlueZError::Canceled)
    } else {
        Ok(())
    }
}

/// Whether a D-Bus call failed because it hit the connection's method
/// timeout.
fn is_timeout(e: &zbus::Error) -> bool {
    matches!(e, zbus::Error::InputOutput(io) if io.kind() == io::ErrorKind::TimedOut)
}

/// Ask BlueZ to drop the device's connection. Best effort: used to undo a
/// connect that is no longer wanted.
fn disconnect(conn: &Connection, device_path: &str) {
    if let Ok(proxy) = zbus::blocking::Proxy::new(conn, BLUEZ_DEST, device_path, DEVICE_IFACE) {
        let _ = proxy.call_method("Disconnect", &());
    }
}

/// The BlueZ calls [`ensure_connected`] makes on one device.
trait Device {
    /// The device's `Connected` property.
    fn connected(&self) -> Result<bool, BlueZError>;
    /// The device's `ServicesResolved` property.
    fn services_resolved(&self) -> Result<bool, BlueZError>;
    /// `Device1.Connect`.
    fn connect(&self) -> Result<(), zbus::Error>;
    /// `Device1.Disconnect`, best effort.
    fn disconnect(&self);
    /// Wait between two looks at the device.
    fn pause(&self) {
        thread::sleep(POLL_SLICE);
    }
}

/// A BlueZ device on a D-Bus connection.
struct DbusDevice<'a> {
    conn: &'a Connection,
    path: &'a str,
}

impl Device for DbusDevice<'_> {
    fn connected(&self) -> Result<bool, BlueZError> {
        get_device_bool(self.conn, self.path, "Connected")
    }

    fn services_resolved(&self) -> Result<bool, BlueZError> {
        get_device_bool(self.conn, self.path, "ServicesResolved")
    }

    fn connect(&self) -> Result<(), zbus::Error> {
        let proxy = zbus::blocking::Proxy::new(self.conn, BLUEZ_DEST, self.path, DEVICE_IFACE)?;
        proxy.call_method("Connect", &()).map(drop)
    }

    fn disconnect(&self) {
        disconnect(self.conn, self.path);
    }
}

/// Ensure the device is connected and GATT services are resolved. Returns
/// whether this call started the connection. A connect it started is
/// disconnected again when anything after it fails: `cancel` is set, the
/// connect or the services wait times out, or a D-Bus call fails.
fn ensure_connected(
    device: &impl Device,
    cancel: &AtomicBool,
    log: &mut dyn FnMut(&str),
) -> Result<bool, BlueZError> {
    if device.connected()? {
        wait_services(device, cancel)?;
        return Ok(false);
    }
    log("connecting");
    let result = connect(device, cancel, log).and_then(|()| wait_services(device, cancel));
    if result.is_err() {
        device.disconnect();
    }
    result.map(|()| true)
}

/// `Connect`, and when the call times out, wait up to [`CONNECT_WAIT`] for
/// BlueZ to finish connecting.
fn connect(
    device: &impl Device,
    cancel: &AtomicBool,
    log: &mut dyn FnMut(&str),
) -> Result<(), BlueZError> {
    match device.connect() {
        Ok(()) => Ok(()),
        Err(e) if is_timeout(&e) => {
            log("still connecting");
            let deadline = Instant::now() + CONNECT_WAIT;
            loop {
                check(cancel)?;
                if device.connected()? {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err(e.into());
                }
                device.pause();
            }
        }
        Err(e) => Err(e.into()),
    }
}

/// Wait for GATT service resolution (up to 5 s, usually instant from cache).
fn wait_services(device: &impl Device, cancel: &AtomicBool) -> Result<(), BlueZError> {
    for _ in 0..SERVICES_POLLS {
        check(cancel)?;
        if device.services_resolved()? {
            return Ok(());
        }
        device.pause();
    }
    Err(BlueZError::ServicesTimeout)
}

/// Discover the D-Bus paths of the two MultiLink characteristics under the device.
fn discover_characteristics(
    conn: &Connection,
    device_path: &str,
) -> Result<(String, String), BlueZError> {
    let proxy = zbus::blocking::Proxy::new(conn, BLUEZ_DEST, "/", OBJECT_MANAGER_IFACE)?;

    let reply = proxy.call_method("GetManagedObjects", &())?;
    let objects: HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>> =
        reply.body().deserialize()?;

    let mut path_2810 = None;
    let mut path_2820 = None;

    for (path, interfaces) in &objects {
        if !path.as_str().starts_with(device_path) {
            continue;
        }
        let Some(props) = interfaces.get(GATT_CHAR_IFACE) else {
            continue;
        };
        let Some(uuid) = prop_str(props, "UUID") else {
            continue;
        };
        if uuid.eq_ignore_ascii_case(CHAR_2810_UUID) {
            path_2810 = Some(path.as_str().to_owned());
        } else if uuid.eq_ignore_ascii_case(CHAR_2820_UUID) {
            path_2820 = Some(path.as_str().to_owned());
        }
    }

    let path_2810 = path_2810.ok_or(BlueZError::CharacteristicNotFound(CHAR_2810_UUID))?;
    let path_2820 = path_2820.ok_or(BlueZError::CharacteristicNotFound(CHAR_2820_UUID))?;

    Ok((path_2810, path_2820))
}

/// Call `AcquireNotify` or `AcquireWrite` on a GATT characteristic, returning
/// the raw file descriptor and MTU.
fn acquire_fd(
    conn: &Connection,
    char_path: &str,
    method: &str,
) -> Result<(OwnedFd, u16), BlueZError> {
    let proxy = zbus::blocking::Proxy::new(conn, BLUEZ_DEST, char_path, GATT_CHAR_IFACE)?;

    let empty_opts: HashMap<&str, Value<'_>> = HashMap::new();
    let reply = proxy.call_method(method, &empty_opts)?;
    let (fd, mtu): (zbus::zvariant::OwnedFd, u16) = reply.body().deserialize()?;

    Ok((fd.into(), mtu))
}

/// Set a file descriptor to non-blocking mode.
fn set_nonblocking(fd: &OwnedFd) -> io::Result<()> {
    // SAFETY: fd is a valid open file descriptor. fcntl F_GETFL/F_SETFL are
    // standard POSIX operations that cannot cause UB on a valid fd.
    unsafe {
        let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Write all bytes to a file descriptor, retrying on short writes.
fn write_all_fd(fd: &OwnedFd, mut data: &[u8]) -> io::Result<()> {
    while !data.is_empty() {
        // SAFETY: fd is a valid open file descriptor from BlueZ AcquireWrite.
        let n = unsafe { libc::write(fd.as_raw_fd(), data.as_ptr().cast(), data.len()) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        data = &data[n.cast_unsigned()..];
    }
    Ok(())
}

// ── D-Bus property helpers ──

/// Read a boolean property from `org.bluez.Device1` via `Properties.Get`.
fn get_device_bool(conn: &Connection, device_path: &str, prop: &str) -> Result<bool, BlueZError> {
    let proxy = zbus::blocking::Proxy::new(conn, BLUEZ_DEST, device_path, DBUS_PROPS_IFACE)?;
    let reply = proxy.call_method("Get", &(DEVICE_IFACE, prop))?;
    let val: OwnedValue = reply.body().deserialize()?;
    Ok(matches!(&*val, Value::Bool(true)))
}

/// Extract a string property from a `GetManagedObjects` property map.
fn prop_str<'a>(props: &'a HashMap<String, OwnedValue>, key: &str) -> Option<&'a str> {
    props.get(key).and_then(|v| match &**v {
        Value::Str(s) => Some(s.as_str()),
        _ => None,
    })
}

/// Extract a boolean property from a `GetManagedObjects` property map.
fn prop_bool(props: &HashMap<String, OwnedValue>, key: &str) -> bool {
    props
        .get(key)
        .is_some_and(|v| matches!(&**v, Value::Bool(true)))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::sync::Arc;

    use super::*;

    /// A device whose answers the test sets, counting disconnects.
    #[derive(Default)]
    struct Fake {
        connected: Cell<bool>,
        resolves: bool,
        connect_times_out: bool,
        /// Set the cancel flag on the first pause.
        cancel_on_pause: Option<Arc<AtomicBool>>,
        disconnects: Cell<u32>,
    }

    impl Device for Fake {
        fn connected(&self) -> Result<bool, BlueZError> {
            Ok(self.connected.get())
        }

        fn services_resolved(&self) -> Result<bool, BlueZError> {
            Ok(self.resolves)
        }

        fn connect(&self) -> Result<(), zbus::Error> {
            if self.connect_times_out {
                return Err(zbus::Error::InputOutput(Arc::new(
                    io::ErrorKind::TimedOut.into(),
                )));
            }
            self.connected.set(true);
            Ok(())
        }

        fn disconnect(&self) {
            self.disconnects.set(self.disconnects.get() + 1);
        }

        fn pause(&self) {
            if let Some(cancel) = &self.cancel_on_pause {
                cancel.store(true, Ordering::SeqCst);
            }
        }
    }

    fn run(device: &Fake, cancel: &AtomicBool) -> Result<bool, BlueZError> {
        ensure_connected(device, cancel, &mut |_| {})
    }

    #[test]
    fn a_services_timeout_undoes_a_connect_this_call_started() {
        let device = Fake::default();
        let err = run(&device, &AtomicBool::new(false)).unwrap_err();
        assert!(matches!(err, BlueZError::ServicesTimeout), "{err}");
        assert_eq!(device.disconnects.get(), 1);
    }

    #[test]
    fn a_services_timeout_leaves_an_existing_connection_alone() {
        let device = Fake {
            connected: Cell::new(true),
            ..Fake::default()
        };
        let err = run(&device, &AtomicBool::new(false)).unwrap_err();
        assert!(matches!(err, BlueZError::ServicesTimeout), "{err}");
        assert_eq!(device.disconnects.get(), 0);
    }

    #[test]
    fn a_resolved_connect_is_kept() {
        let device = Fake {
            resolves: true,
            ..Fake::default()
        };
        assert!(run(&device, &AtomicBool::new(false)).expect("connected"));
        assert_eq!(device.disconnects.get(), 0);
        let device = Fake {
            connected: Cell::new(true),
            resolves: true,
            ..Fake::default()
        };
        assert!(!run(&device, &AtomicBool::new(false)).expect("connected"));
    }

    #[test]
    fn a_cancel_while_a_timed_out_connect_goes_on_undoes_it() {
        let cancel = Arc::new(AtomicBool::new(false));
        let device = Fake {
            connect_times_out: true,
            cancel_on_pause: Some(Arc::clone(&cancel)),
            ..Fake::default()
        };
        let err = run(&device, &cancel).unwrap_err();
        assert!(matches!(err, BlueZError::Canceled), "{err}");
        assert_eq!(device.disconnects.get(), 1);
    }
}
