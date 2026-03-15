//! GFDI (Garmin Framework Device Interface) framing layer.
//!
//! Handles frame construction, parsing, CRC verification, COBS wrapping,
//! and the 5024/5050 handshake sequence.
//!
//! Wire format: `[0x00] [COBS(frame)] [0x00]`
//! Frame: `[length: u16 LE] [header: 2-4 bytes] [payload] [crc: u16 LE]`

use crate::cobs;
use crate::crc::crc16;
use crate::error::Error;

// ── Message types ──

pub(crate) const MSG_ACK: u16 = 5000;
pub(crate) const MSG_DEVICE_INFO: u16 = 5024;
pub(crate) const MSG_FIT_DEFINITION: u16 = 5011;
pub(crate) const MSG_FIT_DATA: u16 = 5012;
pub(crate) const MSG_CONFIGURATION: u16 = 5050;
pub(crate) const MSG_PROTOBUF_REQUEST: u16 = 5043;
pub(crate) const MSG_PROTOBUF_RESPONSE: u16 = 5044;

/// Capability bit for R10 launch monitor (SwingSensor).
pub(crate) const CAP_SWING_SENSOR: usize = 30;

/// Parsed GFDI frame.
#[derive(Debug, Clone)]
pub(crate) struct GfdiFrame {
    pub(crate) msg_type: u16,
    #[allow(dead_code)]
    pub(crate) txn_id: Option<u8>,
    pub(crate) payload: Vec<u8>,
}

/// Device information from the 5024 handshake message.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct DeviceInfo {
    pub protocol_version: u16,
    pub product_number: u16,
    pub unit_id: u32,
    pub software_version: u16,
    pub max_packet_size: u16,
    pub friendly_name: String,
    pub device_name: String,
    pub model_name: String,
}

// ── Stream buffer for reassembling COBS frames from BLE chunks ──

/// Maximum buffer size (64 KB). R10 messages are < 200 bytes; this cap
/// prevents unbounded growth from malformed data.
const STREAM_BUFFER_MAX: usize = 64 * 1024;

/// Accumulates BLE notification data and extracts complete COBS frames.
///
/// GFDI frames are delimited by `0x00` bytes. BLE notifications may split
/// a frame across multiple chunks. This buffer collects chunks and yields
/// complete frames as they arrive.
#[derive(Debug, Default)]
pub(crate) struct StreamBuffer {
    buf: Vec<u8>,
}

impl StreamBuffer {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// Append raw data (handle already stripped).
    ///
    /// Returns `Err` if the buffer would exceed 64 KB (malformed stream).
    pub(crate) fn extend(&mut self, data: &[u8]) -> Result<(), Error> {
        if self.buf.len() + data.len() > STREAM_BUFFER_MAX {
            self.buf.clear();
            return Err(Error::FrameTooShort {
                len: self.buf.len() + data.len(),
                min: 0,
            });
        }
        self.buf.extend_from_slice(data);
        Ok(())
    }

    /// Extract the next complete COBS-delimited frame from the buffer.
    /// Returns `None` if no complete frame is available.
    /// Incomplete data remains in the buffer for future calls.
    pub(crate) fn next_frame(&mut self) -> Option<Result<GfdiFrame, Error>> {
        loop {
            let start = self.buf.iter().position(|&b| b == 0x00)?;
            let rel_end = self.buf[start + 1..].iter().position(|&b| b == 0x00)?;
            let end = start + 1 + rel_end;

            let cobs_data = &self.buf[start + 1..end];
            let consumed = end + 1;

            if cobs_data.is_empty() {
                self.buf.drain(..consumed);
                continue;
            }

            let cobs_owned = cobs_data.to_vec();
            self.buf.drain(..consumed);

            return Some(match cobs::decode(&cobs_owned) {
                Ok(decoded) => parse_frame(&decoded),
                Err(e) => Err(Error::Cobs(e)),
            });
        }
    }

}

// ── Frame parsing ──

/// Parse a decoded (post-COBS) GFDI frame.
///
/// # Errors
///
/// Returns `Err` if the frame is too short or CRC verification fails.
pub(crate) fn parse_frame(frame: &[u8]) -> Result<GfdiFrame, Error> {
    if frame.len() < 6 {
        return Err(Error::FrameTooShort {
            len: frame.len(),
            min: 6,
        });
    }

    // Verify CRC (all bytes except last 2)
    let crc_recv = u16::from_le_bytes([frame[frame.len() - 2], frame[frame.len() - 1]]);
    let crc_calc = crc16(&frame[..frame.len() - 2]);
    if crc_recv != crc_calc {
        return Err(Error::Crc {
            expected: crc_calc,
            actual: crc_recv,
        });
    }

    // length at offset 0 (u16 LE) — we don't need it for parsing
    // let _length = u16::from_le_bytes([frame[0], frame[1]]);

    // Detect header variant: if byte[3] has high bit set, it's a txn_id message
    let (msg_type, txn_id, payload_start) = if frame.len() >= 4 && frame[3] & 0x80 != 0 {
        // Compressed header with transaction ID
        let mt = u16::from(frame[2]) + 5000;
        let tid = frame[3] & 0x7F;
        (mt, Some(tid), 4)
    } else {
        // Standard 4-byte header
        let mt = u16::from_le_bytes([frame[2], frame[3]]);
        (mt, None, 4)
    };

    let payload = frame[payload_start..frame.len() - 2].to_vec();

    Ok(GfdiFrame {
        msg_type,
        txn_id,
        payload,
    })
}

// ── Frame building ──

/// Build a raw GFDI frame with 4-byte header (no transaction ID).
///
/// Format: `[length: u16] [msg_type: u16] [payload] [crc: u16]`
#[must_use]
pub(crate) fn build_frame(msg_type: u16, payload: &[u8]) -> Vec<u8> {
    // length = 2 (length field) + 2 (msg_type) + payload + 2 (crc)
    #[allow(clippy::cast_possible_truncation)]
    let length = (2 + 2 + payload.len() + 2) as u16;
    let mut frame = Vec::with_capacity(usize::from(length));
    frame.extend_from_slice(&length.to_le_bytes());
    frame.extend_from_slice(&msg_type.to_le_bytes());
    frame.extend_from_slice(payload);
    let crc = crc16(&frame);
    frame.extend_from_slice(&crc.to_le_bytes());
    frame
}

/// Build an ACK (type 5000) response frame with 4-byte header.
///
/// Format: `[length: u16] [5000: u16] [orig_type: u16] [status: u8] [payload] [crc: u16]`
#[must_use]
pub(crate) fn build_ack(orig_msg_type: u16, status: u8, payload: &[u8]) -> Vec<u8> {
    let mut ack_payload = Vec::with_capacity(3 + payload.len());
    ack_payload.extend_from_slice(&orig_msg_type.to_le_bytes());
    ack_payload.push(status);
    ack_payload.extend_from_slice(payload);
    build_frame(MSG_ACK, &ack_payload)
}

/// COBS-encode a frame for transmission: `[0x00] [COBS(frame)] [0x00]`.
#[must_use]
pub(crate) fn wrap_cobs(frame: &[u8]) -> Vec<u8> {
    let encoded = cobs::encode(frame);
    let mut out = Vec::with_capacity(2 + encoded.len());
    out.push(0x00);
    out.extend_from_slice(&encoded);
    out.push(0x00);
    out
}

// ── Handshake helpers ──

fn read_length_prefixed_string(data: &[u8], pos: &mut usize) -> String {
    if *pos >= data.len() {
        return String::new();
    }
    let len = data[*pos] as usize;
    *pos += 1;
    if *pos + len > data.len() {
        return String::new();
    }
    let s = String::from_utf8_lossy(&data[*pos..*pos + len]).into_owned();
    *pos += len;
    s
}

/// Parse device information from a 5024 payload.
///
/// # Errors
///
/// Returns `Err` if the payload is too short to contain required fields.
pub(crate) fn parse_device_info(payload: &[u8]) -> Result<DeviceInfo, Error> {
    if payload.len() < 12 {
        return Err(Error::FrameTooShort {
            len: payload.len(),
            min: 12,
        });
    }

    let protocol_version = u16::from_le_bytes([payload[0], payload[1]]);
    let product_number = u16::from_le_bytes([payload[2], payload[3]]);
    let unit_id = u32::from_le_bytes([payload[4], payload[5], payload[6], payload[7]]);
    let software_version = u16::from_le_bytes([payload[8], payload[9]]);
    let max_packet_size = u16::from_le_bytes([payload[10], payload[11]]);

    let mut pos = 12;
    let friendly_name = read_length_prefixed_string(payload, &mut pos);
    let device_name = read_length_prefixed_string(payload, &mut pos);
    let model_name = read_length_prefixed_string(payload, &mut pos);

    Ok(DeviceInfo {
        protocol_version,
        product_number,
        unit_id,
        software_version,
        max_packet_size,
        friendly_name,
        device_name,
        model_name,
    })
}

/// Build the host response payload for a 5024 ACK.
///
/// The response is wrapped in an ACK frame (type 5000) with the host's
/// own device information.
#[must_use]
pub(crate) fn build_device_info_response() -> Vec<u8> {
    let mut host = Vec::with_capacity(48);

    // host_protocol_version = 150
    host.extend_from_slice(&150u16.to_le_bytes());
    // product_number = 0xFFFF
    host.extend_from_slice(&0xFFFFu16.to_le_bytes());
    // unit_id = 0xFFFFFFFF
    host.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    // app_version = 100
    host.extend_from_slice(&100u16.to_le_bytes());
    // max_packet_size = 0xFFFF
    host.extend_from_slice(&0xFFFFu16.to_le_bytes());

    // Length-prefixed strings
    for s in [b"divotmaker" as &[u8], b"Linux", b"Desktop"] {
        #[allow(clippy::cast_possible_truncation)]
        host.push(s.len() as u8); // all strings < 256 bytes
        host.extend_from_slice(s);
    }

    // unknown_flag = 1
    host.push(0x01);

    build_ack(MSG_DEVICE_INFO, 0, &host)
}

/// Parse capability bitmap from a 5050 payload.
///
/// Returns the set of active capability bit indices.
#[must_use]
pub(crate) fn parse_capabilities(payload: &[u8]) -> Vec<usize> {
    if payload.is_empty() {
        return vec![];
    }
    let bitmap_size = usize::from(payload[0]);
    let bitmap = &payload[1..payload.len().min(1 + bitmap_size)];

    let mut caps = Vec::new();
    for (byte_idx, &byte) in bitmap.iter().enumerate() {
        for bit in 0..8 {
            if byte & (1 << bit) != 0 {
                caps.push(byte_idx * 8 + bit);
            }
        }
    }
    caps
}

/// Build a host capabilities 5050 frame with SwingSensor (bit 30) set.
#[must_use]
pub(crate) fn build_host_capabilities() -> Vec<u8> {
    // 13-byte bitmap with bit 30 set
    let mut bitmap = [0u8; 13];
    bitmap[CAP_SWING_SENSOR / 8] |= 1 << (CAP_SWING_SENSOR % 8);

    let mut payload = Vec::with_capacity(1 + bitmap.len());
    #[allow(clippy::cast_possible_truncation)]
    payload.push(bitmap.len() as u8); // always 13
    payload.extend_from_slice(&bitmap);

    build_frame(MSG_CONFIGURATION, &payload)
}

// ── Protobuf fragmentation ──

/// Protobuf fragmentation header (14 bytes before protobuf payload in 5043/5044).
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub(crate) struct FragHeader {
    pub req_id: u16,
    pub offset: u32,
    pub total_len: u32,
    pub chunk_size: u32,
}

/// Parse protobuf fragmentation header from a 5043/5044 payload.
///
/// # Errors
///
/// Returns `Err` if the payload is too short.
pub(crate) fn parse_frag_header(payload: &[u8]) -> Result<(FragHeader, &[u8]), Error> {
    if payload.len() < 14 {
        return Err(Error::FrameTooShort {
            len: payload.len(),
            min: 14,
        });
    }
    let req_id = u16::from_le_bytes([payload[0], payload[1]]);
    let offset = u32::from_le_bytes([payload[2], payload[3], payload[4], payload[5]]);
    let total_len = u32::from_le_bytes([payload[6], payload[7], payload[8], payload[9]]);
    let chunk_size = u32::from_le_bytes([payload[10], payload[11], payload[12], payload[13]]);

    let pb_data = &payload[14..payload.len().min(14 + chunk_size as usize)];

    Ok((
        FragHeader {
            req_id,
            offset,
            total_len,
            chunk_size,
        },
        pb_data,
    ))
}

/// Build a 5043 (protobuf request) frame with fragmentation header.
///
/// Assumes single-frame (no fragmentation needed for R10 messages).
#[must_use]
pub(crate) fn build_protobuf_request(req_id: u16, pb_data: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(14 + pb_data.len());
    payload.extend_from_slice(&req_id.to_le_bytes());
    payload.extend_from_slice(&0u32.to_le_bytes()); // offset = 0
    #[allow(clippy::cast_possible_truncation)]
    let pb_len = pb_data.len() as u32; // R10 messages always < 4GB
    payload.extend_from_slice(&pb_len.to_le_bytes()); // total_len
    payload.extend_from_slice(&pb_len.to_le_bytes()); // chunk_size
    payload.extend_from_slice(pb_data);

    build_frame(MSG_PROTOBUF_REQUEST, &payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_and_parse_frame() {
        let payload = b"test";
        let frame = build_frame(5024, payload);
        let parsed = parse_frame(&frame).unwrap();
        assert_eq!(parsed.msg_type, 5024);
        assert_eq!(parsed.payload, payload);
        assert!(parsed.txn_id.is_none());
    }

    #[test]
    fn build_and_parse_ack() {
        let frame = build_ack(5024, 0, b"");
        let parsed = parse_frame(&frame).unwrap();
        assert_eq!(parsed.msg_type, MSG_ACK);
        // ACK payload: [orig_type u16] [status u8]
        assert_eq!(parsed.payload.len(), 3);
        assert_eq!(
            u16::from_le_bytes([parsed.payload[0], parsed.payload[1]]),
            5024
        );
        assert_eq!(parsed.payload[2], 0);
    }

    #[test]
    fn crc_verified() {
        let frame = build_frame(5024, b"hello");
        // Corrupt one byte
        let mut bad = frame.clone();
        bad[4] ^= 0xFF;
        assert!(parse_frame(&bad).is_err());
    }

    #[test]
    fn cobs_roundtrip() {
        let frame = build_frame(5024, b"test payload with \x00 zeros");
        let wrapped = wrap_cobs(&frame);
        // First and last bytes are 0x00 delimiters
        assert_eq!(wrapped[0], 0x00);
        assert_eq!(wrapped[wrapped.len() - 1], 0x00);
    }

    #[test]
    fn stream_buffer_single_frame() {
        let frame = build_frame(5024, b"test");
        let cobs_frame = wrap_cobs(&frame);

        let mut buf = StreamBuffer::new();
        buf.extend(&cobs_frame);
        let f = buf.next_frame().unwrap().unwrap();
        assert_eq!(f.msg_type, 5024);
        assert_eq!(f.payload, b"test");
        assert!(buf.next_frame().is_none());
    }

    #[test]
    fn stream_buffer_split_chunks() {
        let frame = build_frame(5050, b"payload");
        let cobs_frame = wrap_cobs(&frame);

        let mut buf = StreamBuffer::new();
        // Feed in two halves
        let mid = cobs_frame.len() / 2;
        buf.extend(&cobs_frame[..mid]);
        assert!(buf.next_frame().is_none()); // incomplete
        buf.extend(&cobs_frame[mid..]);
        let f = buf.next_frame().unwrap().unwrap();
        assert_eq!(f.msg_type, 5050);
    }

    #[test]
    fn parse_device_info_r10() {
        // Minimal 5024 payload based on R10 captures
        let mut payload = Vec::new();
        payload.extend_from_slice(&150u16.to_le_bytes()); // protocol_version
        payload.extend_from_slice(&3622u16.to_le_bytes()); // product_number (0x0E26)
        payload.extend_from_slice(&0x12345678u32.to_le_bytes()); // unit_id
        payload.extend_from_slice(&430u16.to_le_bytes()); // software_version (4.30)
        payload.extend_from_slice(&0x021Bu16.to_le_bytes()); // max_packet_size

        // friendly_name: "Approach R10"
        let name = b"Approach R10";
        payload.push(name.len() as u8);
        payload.extend_from_slice(name);

        // device_name: "ApproachR10"
        let dev = b"ApproachR10";
        payload.push(dev.len() as u8);
        payload.extend_from_slice(dev);

        // model_name: empty
        payload.push(0);

        let info = parse_device_info(&payload).unwrap();
        assert_eq!(info.protocol_version, 150);
        assert_eq!(info.product_number, 3622);
        assert_eq!(info.software_version, 430);
        assert_eq!(info.friendly_name, "Approach R10");
        assert_eq!(info.device_name, "ApproachR10");
    }

    #[test]
    fn capabilities_swing_sensor() {
        let caps_frame = build_host_capabilities();
        let parsed = parse_frame(&caps_frame).unwrap();
        assert_eq!(parsed.msg_type, MSG_CONFIGURATION);
        let caps = parse_capabilities(&parsed.payload);
        assert!(caps.contains(&CAP_SWING_SENSOR));
    }

    #[test]
    fn frag_header_roundtrip() {
        let pb_data = b"protobuf payload";
        let frame = build_protobuf_request(42, pb_data);
        let parsed = parse_frame(&frame).unwrap();
        assert_eq!(parsed.msg_type, MSG_PROTOBUF_REQUEST);

        let (hdr, data) = parse_frag_header(&parsed.payload).unwrap();
        assert_eq!(hdr.req_id, 42);
        assert_eq!(hdr.offset, 0);
        assert_eq!(hdr.total_len, pb_data.len() as u32);
        assert_eq!(hdr.chunk_size, pb_data.len() as u32);
        assert_eq!(data, pb_data);
    }

    #[test]
    fn txn_id_detection() {
        // Build a frame that mimics a compressed header with txn_id
        // [length u16] [msg_type-5000 u8] [txn_id|0x80 u8] [payload] [crc u16]
        let mut frame = Vec::new();
        let payload = b"test";
        let length = (2 + 1 + 1 + payload.len() + 2) as u16;
        frame.extend_from_slice(&length.to_le_bytes());
        frame.push(43); // 5043 - 5000
        frame.push(0x81); // txn_id=1 | 0x80
        frame.extend_from_slice(payload);
        let crc = crc16(&frame);
        frame.extend_from_slice(&crc.to_le_bytes());

        let parsed = parse_frame(&frame).unwrap();
        assert_eq!(parsed.msg_type, 5043);
        assert_eq!(parsed.txn_id, Some(1));
        assert_eq!(parsed.payload, payload);
    }
}
