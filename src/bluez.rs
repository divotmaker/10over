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
use std::thread;
use std::time::Duration;

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
        let conn = Connection::system()?;
        let (device_path, address) = find_r10(&conn)?;
        ensure_connected(&conn, &device_path)?;
        Self::acquire_fds(conn, &device_path, address)
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
        Self::acquire_fds(conn, device_path, address)
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

    fn acquire_fds(conn: Connection, device_path: &str, device_address: String) -> Result<Self, BlueZError> {
        let (path_2810, path_2820) = discover_characteristics(&conn, device_path)?;

        // AcquireNotify on 2810 → notify_fd (all inbound notifications)
        let (notify_fd, notify_mtu) = acquire_fd(&conn, &path_2810, "AcquireNotify")?;

        // AcquireWrite on 2810 → register_fd (MultiLink REGISTER command)
        let (register_fd, _) = acquire_fd(&conn, &path_2810, "AcquireWrite")?;

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
            let address = prop_str(props, "Address")
                .unwrap_or("")
                .to_owned();
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

/// Ensure the device is connected and GATT services are resolved.
fn ensure_connected(conn: &Connection, device_path: &str) -> Result<(), BlueZError> {
    if !get_device_bool(conn, device_path, "Connected")? {
        let proxy = zbus::blocking::Proxy::new(conn, BLUEZ_DEST, device_path, DEVICE_IFACE)?;
        proxy.call_method("Connect", &())?;
    }

    // Wait for GATT service resolution (up to 5s, usually instant from cache)
    for _ in 0..50 {
        if get_device_bool(conn, device_path, "ServicesResolved")? {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
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
