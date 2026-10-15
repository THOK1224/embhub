//! CRC-32（ISO 3309，与 zip/Png 相同的反射表算法）。
//!
//! 纯软件实现，不依赖第三方 crate。每个 LogStore 记录带一个 CRC32，
//! 用于在回放时检测落盘数据是否被篡改或损坏。

const fn build_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut j = 0;
        while j < 8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

const TABLE: [u32; 256] = build_table();

/// 计算一段字节的 CRC32（ISO 3309 反射式，多项式 0xEDB88320）。
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc = (crc >> 8) ^ TABLE[((crc ^ b as u32) & 0xFF) as usize];
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_empty_is_zero() {
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn crc32_check_value() {
        // CRC-32/ISO-HDLC 的标准校验值： "123456789" -> 0xCBF43926
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn crc32_detects_single_bit_flip() {
        let original = b"hello embhub";
        let mut tampered = original.to_vec();
        tampered[3] ^= 0x01;
        assert_ne!(crc32(original), crc32(&tampered));
    }
}
