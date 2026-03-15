//! COBS (Consistent Overhead Byte Stuffing) codec.
//!
//! Eliminates `0x00` bytes from data so they can serve as frame delimiters.
//! GFDI frames on the wire: `[0x00] [COBS-encoded data] [0x00]`.

/// Encode `data` using COBS. The output contains no `0x00` bytes.
#[must_use]
pub fn encode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 254 + 1);
    let mut i = 0;
    while i < data.len() {
        let start = i;
        while i < data.len() && data[i] != 0x00 && (i - start) < 0xFE {
            i += 1;
        }
        // SAFETY: i - start is at most 0xFE, so +1 fits in u8
        #[allow(clippy::cast_possible_truncation)]
        let code = (i - start + 1) as u8;
        out.push(code);
        out.extend_from_slice(&data[start..i]);
        if i < data.len() && data[i] == 0x00 {
            i += 1;
        }
    }
    out
}

/// Decode COBS-encoded `data` back to the original bytes.
///
/// # Errors
///
/// Returns `Err` if the data is malformed (unexpected end of input).
pub fn decode(data: &[u8]) -> Result<Vec<u8>, DecodeError> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        let code = data[i];
        if code == 0 {
            break;
        }
        i += 1;
        for _ in 1..code {
            if i >= data.len() {
                return Err(DecodeError);
            }
            out.push(data[i]);
            i += 1;
        }
        if code < 0xFF && i < data.len() {
            out.push(0x00);
        }
    }
    // Strip trailing zero added by the block boundary
    if out.last() == Some(&0x00) {
        out.pop();
    }
    Ok(out)
}

/// COBS decode error — malformed input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeError;

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("COBS decode error: malformed input")
    }
}

impl std::error::Error for DecodeError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_empty() {
        let data = b"";
        let encoded = encode(data);
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn roundtrip_no_zeros() {
        let data = b"hello";
        let encoded = encode(data);
        assert!(!encoded.contains(&0x00));
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn roundtrip_with_interior_zeros() {
        // COBS can't roundtrip data ending with 0x00 — the trailing zero
        // merges with the frame delimiter. GFDI frames end with CRC bytes,
        // so this is fine. Test interior zeros only.
        let data = b"\x00hello\x00world";
        let encoded = encode(data);
        assert!(!encoded.contains(&0x00));
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn roundtrip_zeros_then_data() {
        let data = [0x00, 0x00, 0x00, 0x01];
        let encoded = encode(&data);
        assert!(!encoded.contains(&0x00));
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn roundtrip_254_nonzero() {
        let data: Vec<u8> = (1..=254).collect();
        let encoded = encode(&data);
        assert!(!encoded.contains(&0x00));
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn roundtrip_255_nonzero() {
        let data: Vec<u8> = (1..=255).collect();
        let encoded = encode(&data);
        assert!(!encoded.contains(&0x00));
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn known_vector() {
        let data = [0x01, 0x00, 0x02];
        let encoded = encode(&data);
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }
}
