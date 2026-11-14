/// Crate-level error type.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A GFDI frame failed its CRC check.
    #[error("CRC mismatch: expected {expected:#06x}, got {actual:#06x}")]
    Crc {
        /// The CRC computed over the frame.
        expected: u16,
        /// The CRC the frame carried.
        actual: u16,
    },

    /// A frame's COBS encoding is malformed.
    #[error("COBS decode failed")]
    Cobs,

    /// A frame or payload is shorter than its header needs.
    #[error("frame too short: {len} bytes (minimum {min})")]
    FrameTooShort {
        /// Bytes received.
        len: usize,
        /// Bytes needed.
        min: usize,
    },

    /// The device refused a message.
    #[error("GFDI NAK for message {msg_type}: status {status}")]
    Nak {
        /// The GFDI message type the device refused.
        msg_type: u16,
        /// The device's status code.
        status: u8,
    },

    /// The device refused the MultiLink registration, with this status.
    #[error("MultiLink registration failed: status {0}")]
    MultiLinkRegister(u8),

    /// A protobuf envelope failed to decode.
    #[error("protobuf decode failed: {0}")]
    Protobuf(#[from] prost::DecodeError),

    /// The handshake did not finish in time.
    #[error("handshake timeout")]
    HandshakeTimeout,

    /// The transport failed: the connection is gone.
    #[error("transport error: {0}")]
    Transport(#[from] std::io::Error),
}

impl From<crate::cobs::DecodeError> for Error {
    fn from(_: crate::cobs::DecodeError) -> Self {
        Self::Cobs
    }
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
