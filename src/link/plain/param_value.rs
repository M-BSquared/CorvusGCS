//! The parameter protocol's constants and value codec: what a PARAM_VALUE's
//! float32 means for each MAV_PARAM_TYPE, and how a write is judged. PX4
//! carries an integer as its own bytes in the float's four bytes (`bytewise`),
//! ArduPilot converts the number (`c_cast`). Plain Rust, shared by the Python
//! engine (`protocol::params`) and `corvus_core::protocol::params`.

pub const PARAM_DOWNLOAD_INACTIVITY_S: f64 = 1.0;
pub const PARAM_DOWNLOAD_MAX_ROUNDS: i64 = 3;
pub const PARAM_DOWNLOAD_TIMEOUT_S: f64 = 30.0;
pub const PARAM_DOWNLOAD_TIMEOUT_S_SERIAL: f64 = 180.0;
pub const PARAM_WATCHDOG_TICK_S: f64 = 0.5;
pub const PARAM_RETRANSMIT_MAX_PER_ROUND_SERIAL: usize = 120;
pub const PARAM_RETRANSMIT_MAX_PER_ROUND_UDP: usize = 250;
pub const PARAM_RETRANSMIT_GAP_S_SERIAL: f64 = 0.02;
pub const PARAM_RETRANSMIT_GAP_S_UDP: f64 = 0.002;
pub const PARAM_CACHE_MAX_ENTRIES: i64 = 10_000;
pub const PARAM_INDEX_NONE: i64 = 0xFFFF;
pub const PARAM_VERIFY_MAX_NAMES: usize = 200;
pub const PARAM_VERIFY_REWRITES: usize = 2;
pub const PARAM_ABSENT_AFTER_MISSES: i64 = 2;
pub const PARAM_ID_MAX_BYTES: usize = 16;
pub const PARAM_HASH_CHECK: &str = "_HASH_CHECK";
pub const PARAM_ENCODING_BYTEWISE: &str = "bytewise";
pub const PARAM_ENCODING_C_CAST: &str = "c_cast";
pub const MAV_PARAM_TYPE_REAL32: i64 = 9;

// ------------------------------------------------------- value encoding ----

/// MAV_PARAM_TYPE -> (bytes, signed, low, high) for the integer types.
pub fn int_type(param_type: i64) -> Option<(usize, bool, i64, i64)> {
    Some(match param_type {
        1 => (1, false, 0, 0xFF),
        2 => (1, true, -0x80, 0x7F),
        3 => (2, false, 0, 0xFFFF),
        4 => (2, true, -0x8000, 0x7FFF),
        5 => (4, false, 0, 0xFFFF_FFFF),
        6 => (4, true, -0x8000_0000, 0x7FFF_FFFF),
        _ => return None,
    })
}

pub fn read_int(raw: &[u8], size: usize, signed: bool) -> f64 {
    match (size, signed) {
        (1, false) => raw[0] as f64,
        (1, true) => raw[0] as i8 as f64,
        (2, false) => u16::from_le_bytes([raw[0], raw[1]]) as f64,
        (2, true) => i16::from_le_bytes([raw[0], raw[1]]) as f64,
        (4, false) => u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) as f64,
        _ => i32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) as f64,
    }
}

pub fn is_integer_type(param_type: i64) -> bool {
    int_type(param_type).is_some()
}

/// The parameter's value from the float32 field it travelled in, preferring
/// the field's own four bytes when the caller has them.
pub fn decode_value(value: f64, param_type: i64, encoding: &str, raw: Option<&[u8]>) -> f64 {
    let Some((size, signed, _, _)) = int_type(param_type) else {
        return value;
    };
    if encoding != PARAM_ENCODING_BYTEWISE {
        return value;
    }
    let bytes = match raw {
        Some(r) if r.len() >= 4 => r[..4].to_vec(),
        _ => (value as f32).to_le_bytes().to_vec(),
    };
    read_int(&bytes, size, signed)
}

/// The float32 PARAM_SET carries so the vehicle stores `value`.
pub fn encode_value(value: f64, param_type: i64, encoding: &str) -> Result<f64, String> {
    let Some((size, signed, low, high)) = int_type(param_type) else {
        return Ok(value);
    };
    if !value.is_finite() || value != value.trunc() {
        return Err("this parameter takes whole numbers only".to_string());
    }
    if value < low as f64 || value > high as f64 {
        return Err(format!("this parameter takes values from {low} to {high}"));
    }
    let whole = value as i64;
    if encoding != PARAM_ENCODING_BYTEWISE {
        return Ok(value);
    }
    let mut packed = [0u8; 4];
    match (size, signed) {
        (1, _) => packed[0] = whole as u8,
        (2, _) => packed[..2].copy_from_slice(&(whole as u16).to_le_bytes()),
        _ => packed.copy_from_slice(&(whole as u32).to_le_bytes()),
    }
    let bits = u32::from_le_bytes(packed);
    if (bits >> 23) & 0xFF == 0xFF && bits & 0x7F_FFFF != 0 && bits & 0x40_0000 == 0 {
        return Err("this value cannot be sent exactly over MAVLink".to_string());
    }
    Ok(f32::from_bits(bits) as f64)
}

/// Exact for integers (rounded half to even, as Python's `round`), float32
/// exact otherwise.
pub fn values_match(param_type: i64, got: f64, wanted: f64) -> bool {
    if !(got.is_finite() && wanted.is_finite()) {
        return false;
    }
    if is_integer_type(param_type) {
        return got.round_ties_even() == wanted.round_ties_even();
    }
    let as_sent = wanted as f32;
    if as_sent.is_infinite() {
        return false;
    }
    let as_sent = as_sent as f64;
    (got - as_sent).abs() <= 1e-6 * f64::max(1.0, as_sent.abs())
}

pub fn echo_confirms(echoed_type: i64, echoed_value: f64, value: f64) -> bool {
    if is_integer_type(echoed_type) {
        return values_match(echoed_type, echoed_value, value);
    }
    (echoed_value - value).abs() <= f64::max(1e-4, 1e-3 * value.abs())
}

#[cfg(test)]
mod tests {
    use super::*;

    const INT32: i64 = 6;
    const UINT8: i64 = 1;
    const REAL32: i64 = 9;

    #[test]
    fn px4_integers_travel_as_their_bytes() {
        let wire = encode_value(4.0, INT32, PARAM_ENCODING_BYTEWISE).unwrap();
        assert_eq!((wire as f32).to_bits(), 4);
        assert_eq!(decode_value(wire, INT32, PARAM_ENCODING_BYTEWISE, None), 4.0);
        let minus_one = encode_value(-1.0, INT32, PARAM_ENCODING_BYTEWISE).unwrap();
        assert!(minus_one.is_nan());
        assert_eq!(
            decode_value(f64::NAN, INT32, PARAM_ENCODING_BYTEWISE, Some(&(-1i32).to_le_bytes())),
            -1.0
        );
    }

    #[test]
    fn ardupilot_integers_travel_as_numbers() {
        assert_eq!(encode_value(4.0, INT32, PARAM_ENCODING_C_CAST).unwrap(), 4.0);
        assert_eq!(decode_value(4.0, INT32, PARAM_ENCODING_C_CAST, None), 4.0);
    }

    #[test]
    fn integers_refuse_fractions_ranges_and_signalling_nans() {
        assert_eq!(
            encode_value(4.5, INT32, PARAM_ENCODING_BYTEWISE).unwrap_err(),
            "this parameter takes whole numbers only"
        );
        assert_eq!(
            encode_value(256.0, UINT8, PARAM_ENCODING_C_CAST).unwrap_err(),
            "this parameter takes values from 0 to 255"
        );
        assert_eq!(
            encode_value(0x7F80_0001u32 as i32 as f64, INT32, PARAM_ENCODING_BYTEWISE).unwrap_err(),
            "this value cannot be sent exactly over MAVLink"
        );
        assert!(encode_value(f64::INFINITY, INT32, PARAM_ENCODING_C_CAST).is_err());
    }

    #[test]
    fn floats_pass_unchanged() {
        assert_eq!(encode_value(0.15, REAL32, PARAM_ENCODING_BYTEWISE).unwrap(), 0.15);
        assert_eq!(decode_value(0.15, REAL32, PARAM_ENCODING_BYTEWISE, None), 0.15);
    }

    #[test]
    fn matching_is_exact_for_integers_and_float32_for_floats() {
        assert!(values_match(INT32, 4.0, 4.0));
        assert!(!values_match(INT32, 1082130432.0, 4.0));
        assert!(values_match(INT32, 2.5, 2.0), "half to even, as Python's round");
        assert!(!values_match(INT32, 3.5, 3.0));
        assert!(values_match(REAL32, 0.15f32 as f64, 0.15));
        assert!(!values_match(REAL32, 0.1501, 0.15));
        assert!(!values_match(REAL32, f64::NAN, f64::NAN));
        assert!(!values_match(REAL32, 1.0, 1e39));
    }

    #[test]
    fn echoes_are_judged_with_a_tolerance_for_floats() {
        assert!(echo_confirms(REAL32, 9.5000001, 9.5));
        assert!(!echo_confirms(REAL32, 9.6, 9.5));
        assert!(echo_confirms(INT32, 6.0, 6.0));
    }
}
