//! CRC-16/ARC (also CRC-16/LHA, CRC-IBM).
//!
//! Nibble-based lookup table implementation matching the Garmin GFDI CRC.
//! Poly 0x8005 reflected, init=0, no final XOR.
//!
//! Equivalent to the `crc` crate's `CRC_16_ARC` preset but implemented
//! directly to avoid an external dependency for 20 lines of code.

const TABLE: [u16; 16] = [
    0x0000, 0xCC01, 0xD801, 0x1400, 0xF001, 0x3C00, 0x2800, 0xE401,
    0xA001, 0x6C00, 0x7800, 0xB401, 0x5000, 0x9C01, 0x8801, 0x4400,
];

/// Compute CRC-16/ARC over `data`.
#[must_use]
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &b in data {
        // Low nibble
        let tmp = ((crc >> 4) & 0x0FFF) ^ TABLE[(crc & 0xF) as usize] ^ TABLE[usize::from(b & 0xF)];
        // High nibble
        crc = ((tmp >> 4) & 0x0FFF) ^ TABLE[(tmp & 0xF) as usize] ^ TABLE[usize::from(b >> 4)];
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Standard CRC-16/ARC check value for "123456789".
    #[test]
    fn check_value() {
        assert_eq!(crc16(b"123456789"), 0xBB3D);
    }

    #[test]
    fn empty() {
        assert_eq!(crc16(b""), 0);
    }

    #[test]
    fn single_byte() {
        assert_eq!(crc16(&[0x01]), 0xC0C1);
    }
}
