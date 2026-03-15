//! MultiLink transport multiplexer.
//!
//! The R10 uses MultiLink service `6A4E2800` instead of a dedicated GFDI service.
//! GFDI is service ID 1 within MultiLink. A handle byte is prepended to EVERY
//! BLE write/notification chunk for routing.

/// MultiLink service IDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
#[allow(dead_code)]
pub(crate) enum ServiceId {
    Gfdi = 1,
    Nfc = 2,
    RealTimeHr = 6,
    Echo = 15,
    KeepAlive = 22,
}

/// MultiLink register status codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum RegisterStatus {
    Success = 0,
    InvalidServiceId = 1,
    PendingAuth = 2,
    AlreadyInUse = 3,
    Rejected = 4,
}

impl RegisterStatus {
    #[must_use]
    pub(crate) fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Success),
            1 => Some(Self::InvalidServiceId),
            2 => Some(Self::PendingAuth),
            3 => Some(Self::AlreadyInUse),
            4 => Some(Self::Rejected),
            _ => None,
        }
    }
}

/// Build a REGISTER command for a MultiLink service.
///
/// 13 bytes: `[0x00] [0x00] [txn_id: u64 LE] [svc_id: u16 LE] [flags: u8]`
///
/// Written to characteristic `6A4E2810` (bidirectional control + data).
/// No handle byte prefix — this is the control plane.
#[must_use]
pub(crate) fn build_register(txn_id: u64, svc_id: ServiceId) -> [u8; 13] {
    let mut buf = [0u8; 13];
    // buf[0] = 0x00 (reserved)
    // buf[1] = 0x00 (REGISTER command)
    buf[2..10].copy_from_slice(&txn_id.to_le_bytes());
    buf[10..12].copy_from_slice(&(svc_id as u16).to_le_bytes());
    // buf[12] = 0x00 (unreliable)
    buf
}

/// Parse a REGISTER_RESPONSE from a notification on `6A4E2810`.
///
/// Returns `(status, handle, flags)` on success, or `None` if the response
/// is too short or not a REGISTER_RESPONSE.
#[must_use]
pub(crate) fn parse_register_response(data: &[u8]) -> Option<(RegisterStatus, u8, u8)> {
    // Minimum 15 bytes: [reserved][0x01][txn_id 8B][svc_id 2B][status][handle][flags]
    if data.len() < 15 || data[1] != 0x01 {
        return None;
    }
    let status = RegisterStatus::from_u8(data[12])?;
    let handle = data[13];
    let flags = data[14];
    Some((status, handle, flags))
}

/// Strip handle byte from a BLE notification chunk.
///
/// Every notification from `6A4E2810` has the assigned handle as byte 0.
/// Returns the data portion, or `None` if the handle doesn't match.
#[must_use]
pub(crate) fn strip_handle(chunk: &[u8], expected_handle: u8) -> Option<&[u8]> {
    if chunk.first() == Some(&expected_handle) {
        Some(&chunk[1..])
    } else {
        None
    }
}

/// Prepend handle byte and split data into MTU-sized chunks for writing.
///
/// Each BLE write must be `[handle] [up to mtu-1 bytes of data]`.
/// Written to characteristic `6A4E2820` (write-only data channel).
#[must_use]
pub(crate) fn chunk_with_handle(data: &[u8], handle: u8, mtu: usize) -> Vec<Vec<u8>> {
    let payload_per_chunk = mtu.saturating_sub(1);
    if payload_per_chunk == 0 {
        return vec![];
    }
    data.chunks(payload_per_chunk)
        .map(|chunk| {
            let mut buf = Vec::with_capacity(1 + chunk.len());
            buf.push(handle);
            buf.extend_from_slice(chunk);
            buf
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_command_format() {
        let cmd = build_register(1, ServiceId::Gfdi);
        assert_eq!(
            cmd,
            [0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00]
        );
    }

    #[test]
    fn parse_register_success() {
        // Real response from R10: handle=1, flags=0
        let resp = [
            0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x01,
            0x00, 0x00,
        ];
        let (status, handle, flags) = parse_register_response(&resp).unwrap();
        assert_eq!(status, RegisterStatus::Success);
        assert_eq!(handle, 1);
        assert_eq!(flags, 0);
    }

    #[test]
    fn strip_handle_match() {
        let chunk = [0x01, 0xAA, 0xBB];
        assert_eq!(strip_handle(&chunk, 0x01), Some([0xAA, 0xBB].as_slice()));
    }

    #[test]
    fn strip_handle_mismatch() {
        let chunk = [0x02, 0xAA, 0xBB];
        assert_eq!(strip_handle(&chunk, 0x01), None);
    }

    #[test]
    fn chunking_mtu_20() {
        let data = vec![0xAA; 40];
        let chunks = chunk_with_handle(&data, 0x01, 20);
        // 40 bytes / 19 per chunk = 3 chunks (19 + 19 + 2)
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].len(), 20); // handle + 19
        assert_eq!(chunks[0][0], 0x01);
        assert_eq!(chunks[1].len(), 20);
        assert_eq!(chunks[2].len(), 3); // handle + 2
    }
}
