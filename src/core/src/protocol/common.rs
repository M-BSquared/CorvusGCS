//! What the command protocols share (`mavlink_common.py`).

pub const TAKEOFF_ALTITUDE_MIN_M: f64 = 1.0;
/// 120 m AGL, the EU open category ceiling the takeoff slider carries too.
pub const TAKEOFF_ALTITUDE_MAX_M: f64 = 120.0;
/// Bounds on a velocity setpoint: past anything the supported vehicles fly, so
/// a typed 300 is refused rather than clamped into a dash at full speed.
pub const VELOCITY_MAX_MS: f64 = 30.0;
pub const YAW_RATE_MAX_DEG_S: f64 = 180.0;

const RESULT_TEXT: &[(i64, &str)] = &[
    (-3, "INTERRUPTED"),
    (-2, "DISCONNECTED"),
    (-1, "ACK_TIMEOUT"),
    (0, "ACCEPTED"),
    (1, "TEMPORARILY_REJECTED"),
    (2, "DENIED"),
    (3, "UNSUPPORTED"),
    (4, "FAILED"),
    (5, "IN_PROGRESS"),
    (6, "CANCELLED"),
    (7, "COMMAND_LONG_ONLY"),
    (8, "COMMAND_INT_ONLY"),
    (9, "UNSUPPORTED_FRAME"),
    (10, "NOT_IN_CONTROL"),
];

/// The words for a MAV_RESULT (or one of Corvus's own negative codes).
pub fn result_text(result: i64) -> String {
    RESULT_TEXT
        .iter()
        .find(|(k, _)| *k == result)
        .map_or_else(|| format!("RESULT_{result}"), |(_, v)| (*v).to_string())
}

/// Python's `f"{value:.{digits}f}"`.
pub fn fixed(value: f64, digits: usize) -> String {
    format!("{value:.digits$}")
}

/// `int(round(degrees * 1e7))`, rounding halves to even as Python does.
pub fn e7(degrees: f64) -> i64 {
    (degrees * 1e7).round_ties_even() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinates_round_half_to_even() {
        assert_eq!(e7(48.1234567), 481234567);
        assert_eq!(e7(-11.00000005), -110000000);
        assert_eq!(e7(0.00000015), 2);
    }

    #[test]
    fn fixed_point_text_matches_python_format() {
        assert_eq!(fixed(10.0, 0), "10");
        assert_eq!(fixed(2.5, 0), "2");
        assert_eq!(fixed(512.45, 1), "512.5");
        assert_eq!(fixed(-0.4, 0), "-0");
    }

    #[test]
    fn results_have_words() {
        assert_eq!(result_text(-2), "DISCONNECTED");
        assert_eq!(result_text(4), "FAILED");
        assert_eq!(result_text(99), "RESULT_99");
    }
}
