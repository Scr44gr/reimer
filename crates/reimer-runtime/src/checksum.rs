//! Checksums over validated caller-owned byte regions.

/// ABI symbol for CRC-32/ISO-HDLC.
pub const CHECKSUM_CRC32_SYMBOL: &str = "checksum_crc32";

const CRC32_TABLES: [[u32; 256]; 8] = build_crc32_tables();

/// Calculates CRC-32/ISO-HDLC for one bounded byte region.
///
/// # Safety
///
/// `data` must be null with a zero `length`, or point to `length` readable bytes.
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn checksum_crc32(data: *const u8, length: usize) -> u32 {
    let bytes = if length == 0 {
        &[]
    } else if data.is_null() {
        return u32::MAX;
    } else {
        // SAFETY: The ABI contract requires a readable region of exactly `length` bytes.
        unsafe { std::slice::from_raw_parts(data, length) }
    };
    let mut checksum = u32::MAX;
    let mut chunks = bytes.chunks_exact(8);
    for chunk in &mut chunks {
        let first = checksum ^ u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        let second = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);
        checksum = CRC32_TABLES[7][low_byte_index(first)]
            ^ CRC32_TABLES[6][low_byte_index(first >> 8)]
            ^ CRC32_TABLES[5][low_byte_index(first >> 16)]
            ^ CRC32_TABLES[4][low_byte_index(first >> 24)]
            ^ CRC32_TABLES[3][low_byte_index(second)]
            ^ CRC32_TABLES[2][low_byte_index(second >> 8)]
            ^ CRC32_TABLES[1][low_byte_index(second >> 16)]
            ^ CRC32_TABLES[0][low_byte_index(second >> 24)];
    }
    for byte in chunks.remainder() {
        let index = low_byte_index(checksum ^ u32::from(*byte));
        checksum = (checksum >> 8) ^ CRC32_TABLES[0][index];
    }
    !checksum
}

fn low_byte_index(value: u32) -> usize {
    usize::from(value.to_le_bytes()[0])
}

const fn build_crc32_tables() -> [[u32; 256]; 8] {
    let mut tables = [[0_u32; 256]; 8];
    let mut index = 0_usize;
    let mut code = 0_u32;
    while index < tables[0].len() {
        let mut value = code;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 1 == 0 {
                value >> 1
            } else {
                (value >> 1) ^ 0xedb8_8320
            };
            bit += 1;
        }
        tables[0][index] = value;
        index += 1;
        code += 1;
    }
    let mut table = 1_usize;
    while table < tables.len() {
        index = 0;
        while index < tables[table].len() {
            let previous = tables[table - 1][index];
            let lookup = previous.to_le_bytes()[0] as usize;
            tables[table][index] = (previous >> 8) ^ tables[0][lookup];
            index += 1;
        }
        table += 1;
    }
    tables
}

#[cfg(test)]
mod tests {
    use super::checksum_crc32;

    #[test]
    fn crc32_should_match_the_standard_check_value() {
        let bytes = b"123456789";
        // SAFETY: The static byte string is readable for its complete length.
        let checksum = unsafe { checksum_crc32(bytes.as_ptr(), bytes.len()) };
        assert_eq!(checksum, 0xcbf4_3926);
    }
}
