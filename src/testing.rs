//! Frame builders for tests (`testing` feature).
//!
//! Builds the bytes a device or host puts on the wire, so a downstream
//! crate's tests can drive a [`Transport`](crate::Transport) or an
//! [`IdentityFilter`](crate::capture::IdentityFilter) without knowing the
//! frame layout. All identity values are synthetic.

use crate::gfdi::{self, MSG_DEVICE_INFO};
use crate::multilink;

/// Unit ID carried by [`device_info_frame`]. Synthetic.
pub const DEVICE_INFO_UNIT_ID: u32 = 0x5EED_1D01;

/// Bluetooth friendly name carried by [`device_info_frame`]. Synthetic.
pub const DEVICE_INFO_NAME: &str = "Approach R10 SYNTH";

/// A GFDI frame as it crosses the wire, standard header:
/// `[0x00] COBS([length] [msg_type] [payload] [crc]) [0x00]`.
#[must_use]
pub fn encode_frame(msg_type: u16, payload: &[u8]) -> Vec<u8> {
    gfdi::wrap_cobs(&gfdi::build_frame(msg_type, payload))
}

/// An ACK of `acked_type` with `status` and an optional response payload,
/// as it crosses the wire.
#[must_use]
pub fn ack_frame(acked_type: u16, status: u8, payload: &[u8]) -> Vec<u8> {
    gfdi::wrap_cobs(&gfdi::build_ack(acked_type, status, payload))
}

/// A device information message as the device sends it, carrying
/// [`DEVICE_INFO_UNIT_ID`] and [`DEVICE_INFO_NAME`].
#[must_use]
pub fn device_info_frame() -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&150u16.to_le_bytes()); // protocol version
    p.extend_from_slice(&119u16.to_le_bytes()); // product number
    p.extend_from_slice(&DEVICE_INFO_UNIT_ID.to_le_bytes());
    p.extend_from_slice(&1u16.to_le_bytes()); // software version
    p.extend_from_slice(&512u16.to_le_bytes()); // max packet size
    for name in [DEVICE_INFO_NAME.as_bytes(), b"R10", b"Approach R10"] {
        #[allow(clippy::cast_possible_truncation)]
        p.push(name.len() as u8); // all names < 256 bytes
        p.extend_from_slice(name);
    }
    encode_frame(MSG_DEVICE_INFO, &p)
}

/// `bytes` split into BLE chunks of at most `mtu` bytes, each led by the
/// MultiLink `handle` byte. Empty for an `mtu` below 2.
#[must_use]
pub fn chunks(handle: u8, bytes: &[u8], mtu: usize) -> Vec<Vec<u8>> {
    multilink::chunk_with_handle(bytes, handle, mtu)
}

/// `bytes` as a single BLE chunk led by the MultiLink `handle` byte.
#[must_use]
pub fn with_handle(handle: u8, bytes: &[u8]) -> Vec<u8> {
    [&[handle][..], bytes].concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_info_frame_parses() {
        let f = device_info_frame();
        let frame = gfdi::parse_cobs_frame(&f[1..f.len() - 1]).unwrap();
        assert_eq!(frame.msg_type(), MSG_DEVICE_INFO);
        let info = gfdi::parse_device_info(frame.payload()).unwrap();
        assert_eq!(info.unit_id, DEVICE_INFO_UNIT_ID);
        assert_eq!(info.friendly_name, DEVICE_INFO_NAME);
    }

    #[test]
    fn ack_frame_names_its_type() {
        let f = ack_frame(gfdi::MSG_PROTOBUF_RESPONSE, 0, &[]);
        let frame = gfdi::parse_cobs_frame(&f[1..f.len() - 1]).unwrap();
        assert_eq!(frame.msg_type(), gfdi::MSG_ACK);
        assert_eq!(frame.acked_type(), Some(gfdi::MSG_PROTOBUF_RESPONSE));
        assert_eq!(frame.payload(), [0xB4, 0x13, 0x00]);
    }

    #[test]
    fn chunks_lead_with_the_handle() {
        let c = chunks(0x07, &[1, 2, 3, 4, 5], 3);
        assert_eq!(c, [vec![7, 1, 2], vec![7, 3, 4], vec![7, 5]]);
        assert_eq!(with_handle(0x07, &[9]), [7, 9]);
    }
}
