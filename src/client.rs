//! Poll-based R10 client (modeled on ironsight `BinaryClient`).
//!
//! The caller provides BLE transport; the client handles MultiLink registration,
//! GFDI handshake, protobuf subscribe/wakeup, and shot data decoding.
//!
//! ```ignore
//! let mut client = Client::new(transport, 20);
//! loop {
//!     match client.poll()? {
//!         Some(Event::Shot(shot)) => println!("Shot: {shot:?}"),
//!         Some(Event::Ready) => println!("Waiting for shot..."),
//!         Some(event) => println!("{event:?}"),
//!         None => {}  // no data available
//!     }
//! }
//! ```

use std::io;
use std::time::Instant;

use crate::error::Error;
use crate::gfdi::{self, GfdiFrame, StreamBuffer};
use crate::multilink;
use crate::proto::{self, DeviceError, DeviceState, ShotData, SmartEvent};

/// How long to remember shot IDs for deduplication (seconds).
/// R10 replays shots at 6x, and back-to-back shots can overlap.
const SHOT_DEDUP_WINDOW_SECS: u64 = 60;

/// Transport trait — the caller provides the BLE read/write implementation.
///
/// The library does NOT depend on any specific BLE crate. Implement this trait
/// to bridge to bluer, btleplug, or raw file descriptors.
pub trait Transport {
    /// Read available data from the BLE notification channel.
    ///
    /// Returns one BLE notification chunk (including the handle byte prefix
    /// from MultiLink). Non-blocking: returns `Err(WouldBlock)` when no
    /// data is available.
    ///
    /// # Errors
    ///
    /// Returns `WouldBlock` when no data is available, or a transport error.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, io::Error>;

    /// Write data to the BLE write channel.
    ///
    /// `data` is a single BLE write chunk (including handle byte prefix).
    ///
    /// # Errors
    ///
    /// Returns a transport error on write failure.
    fn write(&mut self, data: &[u8]) -> Result<(), io::Error>;

    /// Write to the MultiLink register channel (characteristic `6A4E2810`).
    ///
    /// This is separate from `write` because registration goes to the
    /// bidirectional characteristic, not the write-only one.
    ///
    /// # Errors
    ///
    /// Returns a transport error on write failure.
    fn write_register(&mut self, data: &[u8]) -> Result<(), io::Error>;
}

/// Events emitted by the client.
#[derive(Debug, Clone)]
pub enum Event {
    /// MultiLink registration succeeded, GFDI handshake starting.
    Registered { handle: u8 },
    /// GFDI handshake complete, protobuf session starting.
    HandshakeComplete,
    /// Device is armed and waiting for a shot.
    Ready,
    /// Shot data received.
    Shot(ShotData),
    /// Device state changed.
    StateChange(DeviceState),
    /// Device reported an error.
    DeviceError(DeviceError),
    /// Subscribe response received.
    Subscribed { success: bool },
    /// WakeUp response received.
    WakeUpResponse { status: i32 },
}

/// Client state machine phases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Waiting for MultiLink REGISTER_RESPONSE.
    Registering,
    /// Waiting for 5024 device info.
    WaitDeviceInfo,
    /// Waiting for 5050 capabilities.
    WaitCapabilities,
    /// Waiting for ACK of our 5050 (handshake complete).
    WaitCapabilitiesAck,
    /// Subscribing to LAUNCH_MONITOR alerts.
    Subscribing,
    /// Sending WakeUp request.
    WakingUp,
    /// Normal operation — listening for shots.
    Active,
}

/// Poll-based R10 client.
pub struct Client<T: Transport> {
    transport: T,
    mtu: usize,
    handle: u8,
    phase: Phase,
    stream_buf: StreamBuffer,
    req_id_counter: u16,
    /// Recent shot IDs with timestamps for dedup across overlapping replays.
    recent_shots: Vec<(u32, Instant)>,
    last_state: Option<DeviceState>,
    read_buf: Vec<u8>,
    pending_event: Option<Event>,
}

impl<T: Transport> Client<T> {
    /// Create a new client.
    ///
    /// `mtu` is the BLE ATT_MTU (typically 23 for unmodified connections, up to 515
    /// after negotiation). The effective write payload is `mtu - 3` (ATT header),
    /// capped at 20 for most R10 connections.
    ///
    /// After construction, call `start()` to begin the MultiLink registration,
    /// then poll in a loop.
    #[must_use]
    pub fn new(transport: T, mtu: usize) -> Self {
        Self {
            transport,
            mtu: mtu.min(20), // R10 typically uses MTU 23 → 20 usable
            handle: 0,
            phase: Phase::Registering,
            stream_buf: StreamBuffer::new(),
            req_id_counter: 1,
            recent_shots: Vec::new(),
            last_state: None,
            read_buf: vec![0u8; 512],
            pending_event: None,
        }
    }

    /// Send the MultiLink REGISTER command to start the connection.
    ///
    /// # Errors
    ///
    /// Returns `Err` on transport write failure.
    #[must_use = "start() returns an error if the REGISTER command fails"]
    pub fn start(&mut self) -> Result<(), Error> {
        let cmd = multilink::build_register(1, multilink::ServiceId::Gfdi);
        self.transport.write_register(&cmd)?;
        self.phase = Phase::Registering;
        Ok(())
    }

    /// Poll for the next event. Returns `None` if no data is available.
    ///
    /// Call this in a loop. The client progresses through connection phases
    /// automatically: registration → handshake → subscribe → wakeup → active.
    ///
    /// # Errors
    ///
    /// Returns `Err` on protocol errors (CRC mismatch, NAK, decode failure)
    /// or transport errors.
    #[must_use = "poll() returns events that must be handled"]
    pub fn poll(&mut self) -> Result<Option<Event>, Error> {
        // Drain any pending event before reading more data.
        if let Some(event) = self.pending_event.take() {
            return Ok(Some(event));
        }

        // Read available data from transport
        match self.transport.read(&mut self.read_buf) {
            Ok(0) => {}
            Ok(n) => {
                let chunk = self.read_buf[..n].to_vec();

                // In registration phase, first response is the register response
                if self.phase == Phase::Registering {
                    return self.handle_register_response(&chunk);
                }

                // Strip handle byte and feed to stream buffer
                if let Some(data) = multilink::strip_handle(&chunk, self.handle) {
                    self.stream_buf.extend(data)?;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(Error::Transport(e)),
        }

        // Process the next complete GFDI frame (one per poll to avoid dropping
        // frames when returning an event early).
        if let Some(frame_result) = self.stream_buf.next_frame() {
            let frame = frame_result?;
            if let Some(event) = self.dispatch_frame(&frame)? {
                return Ok(Some(event));
            }
        }

        Ok(None)
    }

    /// Access the underlying transport.
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Access the underlying transport mutably.
    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    /// Current connection phase.
    #[must_use]
    pub fn phase(&self) -> &str {
        match self.phase {
            Phase::Registering => "registering",
            Phase::WaitDeviceInfo => "wait_device_info",
            Phase::WaitCapabilities => "wait_capabilities",
            Phase::WaitCapabilitiesAck => "wait_capabilities_ack",
            Phase::Subscribing => "subscribing",
            Phase::WakingUp => "waking_up",
            Phase::Active => "active",
        }
    }

    fn handle_register_response(&mut self, chunk: &[u8]) -> Result<Option<Event>, Error> {
        match multilink::parse_register_response(chunk) {
            Some((multilink::RegisterStatus::Success, handle, _flags)) => {
                self.handle = handle;
                self.phase = Phase::WaitDeviceInfo;
                Ok(Some(Event::Registered { handle }))
            }
            Some((status, _, _)) => Err(Error::MultiLinkRegister(status as u8)),
            None => {
                // Might be GFDI data arriving alongside/after registration
                // (device sends 5024 immediately). Feed to stream buffer.
                if let Some(data) = multilink::strip_handle(chunk, self.handle) {
                    self.stream_buf.extend(data)?;
                }
                Ok(None)
            }
        }
    }

    fn dispatch_frame(&mut self, frame: &GfdiFrame) -> Result<Option<Event>, Error> {
        match frame.msg_type {
            gfdi::MSG_DEVICE_INFO => self.handle_device_info(&frame.payload),
            gfdi::MSG_CONFIGURATION => self.handle_configuration(&frame.payload),
            gfdi::MSG_ACK => self.handle_ack(&frame.payload),
            gfdi::MSG_FIT_DEFINITION | gfdi::MSG_FIT_DATA => {
                self.send_ack(frame.msg_type, 0, &[0x00])?;
                Ok(None)
            }
            gfdi::MSG_PROTOBUF_REQUEST | gfdi::MSG_PROTOBUF_RESPONSE => {
                let event = self.handle_protobuf(&frame.payload, frame.msg_type)?;
                self.send_ack(frame.msg_type, 0, &[])?;
                Ok(event)
            }
            _ => Ok(None),
        }
    }

    fn handle_device_info(&mut self, payload: &[u8]) -> Result<Option<Event>, Error> {
        let _info = gfdi::parse_device_info(payload)?;
        let response = gfdi::build_device_info_response();
        self.send_frame(&response)?;
        self.phase = Phase::WaitCapabilities;
        Ok(None)
    }

    fn handle_configuration(&mut self, payload: &[u8]) -> Result<Option<Event>, Error> {
        let _caps = gfdi::parse_capabilities(payload);
        // ACK the device's 5050
        self.send_ack(gfdi::MSG_CONFIGURATION, 0, &[])?;
        // Send host capabilities with SwingSensor (bit 30)
        let host_caps = gfdi::build_host_capabilities();
        self.send_frame(&host_caps)?;
        self.phase = Phase::WaitCapabilitiesAck;
        Ok(None)
    }

    fn handle_ack(&mut self, payload: &[u8]) -> Result<Option<Event>, Error> {
        if payload.len() < 3 {
            return Ok(None);
        }
        let orig_type = u16::from_le_bytes([payload[0], payload[1]]);
        let status = payload[2];

        if status != 0 {
            return Err(Error::Nak {
                msg_type: orig_type,
                status,
            });
        }

        // Handshake completes when device ACKs our 5050
        if orig_type == gfdi::MSG_CONFIGURATION && self.phase == Phase::WaitCapabilitiesAck {
            self.phase = Phase::Subscribing;
            self.send_subscribe()?;
            return Ok(Some(Event::HandshakeComplete));
        }

        Ok(None)
    }

    fn handle_protobuf(
        &mut self,
        payload: &[u8],
        _msg_type: u16,
    ) -> Result<Option<Event>, Error> {
        let (_hdr, pb_data) = gfdi::parse_frag_header(payload)?;
        let smart_event = proto::decode_smart(pb_data)?;

        match smart_event {
            SmartEvent::SubscribeResponse { success } => {
                if self.phase == Phase::Subscribing {
                    self.phase = Phase::WakingUp;
                    self.send_wakeup()?;
                }
                Ok(Some(Event::Subscribed { success }))
            }
            SmartEvent::WakeUpResponse { status } => {
                if self.phase == Phase::WakingUp {
                    self.phase = Phase::Active;
                }
                // status 1 = ALREADY_AWAKE: device won't send state transitions,
                // it's already in WAITING from a previous session.
                // Emit WakeUpResponse now, queue Ready for next poll().
                if status == 1 {
                    self.last_state = Some(DeviceState::Waiting);
                    self.pending_event = Some(Event::Ready);
                }
                Ok(Some(Event::WakeUpResponse { status }))
            }
            SmartEvent::StateChange(state) => {
                let changed = self.last_state != Some(state);
                self.last_state = Some(state);
                if changed {
                    if state == DeviceState::Waiting {
                        return Ok(Some(Event::Ready));
                    }
                    // Device went to Standby while active — re-arm it.
                    if state == DeviceState::Standby && self.phase == Phase::Active {
                        self.send_wakeup()?;
                    }
                    return Ok(Some(Event::StateChange(state)));
                }
                Ok(None)
            }
            SmartEvent::Shot(shot) => {
                let now = Instant::now();
                // Prune shots older than the dedup window.
                self.recent_shots.retain(|(_, t)| {
                    now.duration_since(*t).as_secs() < SHOT_DEDUP_WINDOW_SECS
                });
                // Check if we've already seen this shot ID.
                if self.recent_shots.iter().any(|(id, _)| *id == shot.shot_id) {
                    return Ok(None); // duplicate retransmit
                }
                self.recent_shots.push((shot.shot_id, now));
                Ok(Some(Event::Shot(shot)))
            }
            SmartEvent::Error(err) => Ok(Some(Event::DeviceError(err))),
            SmartEvent::CalibrationStatus { .. }
            | SmartEvent::LaunchMonitorResponse
            | SmartEvent::Unknown => Ok(None),
        }
    }

    fn send_subscribe(&mut self) -> Result<(), Error> {
        let pb_data = proto::build_subscribe_request();
        let frame = gfdi::build_protobuf_request(self.next_req_id(), &pb_data);
        self.send_frame(&frame)
    }

    fn send_wakeup(&mut self) -> Result<(), Error> {
        let pb_data = proto::build_wakeup_request();
        let frame = gfdi::build_protobuf_request(self.next_req_id(), &pb_data);
        self.send_frame(&frame)
    }

    fn send_ack(&mut self, orig_type: u16, status: u8, payload: &[u8]) -> Result<(), Error> {
        let frame = gfdi::build_ack(orig_type, status, payload);
        self.send_frame(&frame)
    }

    fn send_frame(&mut self, frame: &[u8]) -> Result<(), Error> {
        let cobs_data = gfdi::wrap_cobs(frame);
        let chunks = multilink::chunk_with_handle(&cobs_data, self.handle, self.mtu);
        for chunk in &chunks {
            self.transport.write(chunk)?;
        }
        Ok(())
    }

    fn next_req_id(&mut self) -> u16 {
        let id = self.req_id_counter;
        self.req_id_counter = self.req_id_counter.wrapping_add(1);
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    /// Mock transport for testing.
    struct MockTransport {
        rx: Rc<RefCell<VecDeque<Vec<u8>>>>,
        tx: Rc<RefCell<Vec<Vec<u8>>>>,
        reg_tx: Rc<RefCell<Vec<Vec<u8>>>>,
    }

    impl MockTransport {
        fn new() -> Self {
            Self {
                rx: Rc::new(RefCell::new(VecDeque::new())),
                tx: Rc::new(RefCell::new(Vec::new())),
                reg_tx: Rc::new(RefCell::new(Vec::new())),
            }
        }

        fn push_rx(&self, data: Vec<u8>) {
            self.rx.borrow_mut().push_back(data);
        }
    }

    impl Transport for MockTransport {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, io::Error> {
            match self.rx.borrow_mut().pop_front() {
                Some(data) => {
                    let len = data.len().min(buf.len());
                    buf[..len].copy_from_slice(&data[..len]);
                    Ok(len)
                }
                None => Err(io::Error::from(io::ErrorKind::WouldBlock)),
            }
        }

        fn write(&mut self, data: &[u8]) -> Result<(), io::Error> {
            self.tx.borrow_mut().push(data.to_vec());
            Ok(())
        }

        fn write_register(&mut self, data: &[u8]) -> Result<(), io::Error> {
            self.reg_tx.borrow_mut().push(data.to_vec());
            Ok(())
        }
    }

    #[test]
    fn client_starts_registration() {
        let transport = MockTransport::new();
        let reg_tx = Rc::clone(&transport.reg_tx);
        let mut client = Client::new(transport, 20);
        client.start().unwrap();
        let sent = reg_tx.borrow();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].len(), 13);
        assert_eq!(sent[0][0], 0x00); // reserved
        assert_eq!(sent[0][1], 0x00); // REGISTER command
    }

    #[test]
    fn client_handles_register_response() {
        let transport = MockTransport::new();
        let rx = Rc::clone(&transport.rx);
        let mut client = Client::new(transport, 20);
        client.start().unwrap();

        // Feed register response: success, handle=1
        rx.borrow_mut().push_back(vec![
            0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x01,
            0x00, 0x00,
        ]);

        let event = client.poll().unwrap();
        assert!(matches!(event, Some(Event::Registered { handle: 1 })));
        assert_eq!(client.phase(), "wait_device_info");
    }
}
