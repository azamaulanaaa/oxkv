//! CRC32C for snapshot page integrity.

/// The reflected Castagnoli polynomial.
const POLYNOMIAL: u32 = 0x82F6_3B78;

/// Computes the CRC32C value of one byte slice.
pub(crate) fn crc32c(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFF_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 0 {
                crc >> 1
            } else {
                (crc >> 1) ^ POLYNOMIAL
            };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::crc32c;

    #[test]
    fn the_standard_check_value_matches() {
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
    }

    #[test]
    fn an_empty_slice_is_zero() {
        assert_eq!(crc32c(b""), 0);
    }
}
