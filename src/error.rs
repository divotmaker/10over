/// Crate-level error type.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("CRC mismatch: expected {expected:#06x}, got {actual:#06x}")]
    Crc { expected: u16, actual: u16 },

    #[error("COBS decode failed")]
    Cobs(#[from] crate::cobs::DecodeError),

    #[error("frame too short: {len} bytes (minimum {min})")]
    FrameTooShort { len: usize, min: usize },

    #[error("GFDI NAK for message {msg_type}: status {status}")]
    Nak { msg_type: u16, status: u8 },

    #[error("MultiLink registration failed: status {0}")]
    MultiLinkRegister(u8),

    #[error("protobuf decode failed: {0}")]
    Protobuf(#[from] prost::DecodeError),

    #[error("handshake timeout")]
    HandshakeTimeout,

    #[error("transport error: {0}")]
    Transport(#[from] std::io::Error),
}

/// GFDI response status codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
#[allow(dead_code)]
pub(crate) enum ResponseStatus {
    Ack = 0,
    Nak = 1,
    Unsupported = 2,
    CobsError = 3,
    CrcError = 4,
    LengthError = 5,
}

impl ResponseStatus {
    #[must_use]
    #[allow(dead_code)]
    pub(crate) fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Ack),
            1 => Some(Self::Nak),
            2 => Some(Self::Unsupported),
            3 => Some(Self::CobsError),
            4 => Some(Self::CrcError),
            5 => Some(Self::LengthError),
            _ => None,
        }
    }
}
