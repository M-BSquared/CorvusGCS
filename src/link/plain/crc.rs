//! CRC-16/MCRF4XX, the MAVLink frame checksum.

pub fn accumulate(crc: u16, bytes: &[u8]) -> u16 {
    let mut accum = crc;
    for &b in bytes {
        let mut tmp = b ^ (accum & 0xFF) as u8;
        tmp ^= tmp << 4;
        accum = (accum >> 8) ^ ((tmp as u16) << 8) ^ ((tmp as u16) << 3) ^ ((tmp as u16) >> 4);
    }
    accum
}

pub fn x25(bytes: &[u8]) -> u16 {
    accumulate(0xFFFF, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcrf4xx_check_value() {
        // The catalogue check value of CRC-16/MCRF4XX.
        assert_eq!(x25(b"123456789"), 0x6F91);
    }

    #[test]
    fn accumulating_in_pieces_is_the_same_as_at_once() {
        let data = b"MAVLink frames are checked byte by byte";
        let (a, b) = data.split_at(13);
        assert_eq!(accumulate(x25(a), b), x25(data));
    }

    #[test]
    fn the_empty_input_leaves_the_seed() {
        assert_eq!(x25(b""), 0xFFFF);
    }
}
