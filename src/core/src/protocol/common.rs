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

/// The words for a MAV_RESULT, if it has any.
pub fn known_result_text(result: i64) -> Option<&'static str> {
    RESULT_TEXT.iter().find(|(k, _)| *k == result).map(|(_, v)| *v)
}

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

/// Python's `f"{value:g}"`: six significant digits, trailing zeros dropped,
/// an exponent only below 1e-4 or from 1e6.
pub fn py_g(value: f64) -> String {
    if value.is_nan() {
        return "nan".into();
    }
    if value.is_infinite() {
        return if value > 0.0 { "inf" } else { "-inf" }.into();
    }
    if value == 0.0 {
        return if value.is_sign_negative() { "-0" } else { "0" }.into();
    }
    let sci = format!("{value:.5e}");
    let (mantissa, exp) = sci.split_once('e').expect("exponent form");
    let exp: i32 = exp.parse().expect("exponent");
    let strip = |text: &str| -> String {
        if text.contains('.') {
            text.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            text.to_string()
        }
    };
    if !(-4..6).contains(&exp) {
        let sign = if exp < 0 { '-' } else { '+' };
        return format!("{}e{sign}{:02}", strip(mantissa), exp.abs());
    }
    let decimals = (5 - exp).max(0) as usize;
    strip(&format!("{value:.decimals$}"))
}

/// `round(x, digits)`: correctly rounded, exact ties to even, non-finite
/// values unchanged.
pub fn py_round(x: f64, digits: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.digits$}").parse().unwrap_or(x)
}

/// Python 3.12's `sum()` of floats: the first term added to the int 0, the
/// rest with Neumaier's compensation.
pub fn py_sum(values: &[f64]) -> f64 {
    let Some((first, rest)) = values.split_first() else {
        return 0.0;
    };
    let mut total = 0.0 + first;
    let mut c = 0.0;
    for &x in rest {
        let t = total + x;
        if total.abs() >= x.abs() {
            c += (total - t) + x;
        } else {
            c += (x - t) + total;
        }
        total = t;
    }
    if c != 0.0 && c.is_finite() {
        total += c;
    }
    total
}

/// `min(a, b)`: `a` unless `b` is smaller, so a NaN `b` loses.
pub fn py_min(a: f64, b: f64) -> f64 {
    if b < a {
        b
    } else {
        a
    }
}

/// `max(a, b)`: `a` unless `b` is larger.
pub fn py_max(a: f64, b: f64) -> f64 {
    if b > a {
        b
    } else {
        a
    }
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
    fn general_format_matches_python() {
        let cases = [
            (0.0, "0"),
            (50.0, "50"),
            (12.5, "12.5"),
            (100.0, "100"),
            (0.5, "0.5"),
            (3.5, "3.5"),
            (1234567.0, "1.23457e+06"),
            (123456.0, "123456"),
            (0.0001, "0.0001"),
            (0.00001234, "1.234e-05"),
            (2.0 / 3.0, "0.666667"),
            (-7.25, "-7.25"),
            (1e16, "1e+16"),
            (99.99999, "100"),
        ];
        for (value, want) in cases {
            assert_eq!(py_g(value), want, "{value}");
        }
    }

    #[test]
    fn round_ties_go_to_even_on_exact_values() {
        assert_eq!(py_round(0.25, 1), 0.2);
        assert_eq!(py_round(0.35, 1), 0.3);
        assert_eq!(py_round(2.675, 2), 2.67);
        assert_eq!(py_round(-0.04, 1).to_bits(), (-0.0f64).to_bits());
        assert!(py_round(f64::NAN, 1).is_nan());
    }

    #[test]
    fn the_sum_is_compensated() {
        assert_eq!(py_sum(&[0.1; 10]), 1.0);
        assert_eq!(py_sum(&[1e100, 1.0, -1e100, 1.0]), 2.0);
        assert_eq!(py_sum(&[]), 0.0);
    }

    #[test]
    fn results_have_words() {
        assert_eq!(result_text(-2), "DISCONNECTED");
        assert_eq!(result_text(4), "FAILED");
        assert_eq!(result_text(99), "RESULT_99");
    }
}

/// A latch a thread can wait on with a timeout (`threading.Event`).
#[derive(Debug, Default)]
pub struct Event {
    flag: std::sync::Mutex<bool>,
    changed: std::sync::Condvar,
}

impl Event {
    pub fn new() -> Event {
        Event::default()
    }

    fn flag(&self) -> std::sync::MutexGuard<'_, bool> {
        self.flag.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set(&self) {
        *self.flag() = true;
        self.changed.notify_all();
    }

    pub fn clear(&self) {
        *self.flag() = false;
    }

    pub fn is_set(&self) -> bool {
        *self.flag()
    }

    /// Waits up to `timeout` seconds for the latch; true when it is set.
    pub fn wait(&self, timeout: f64) -> bool {
        let mut flag = self.flag();
        if *flag {
            return true;
        }
        let limit = std::time::Duration::try_from_secs_f64(timeout.max(0.0)).unwrap_or(std::time::Duration::MAX);
        let (guard, _) = self
            .changed
            .wait_timeout_while(flag, limit, |set| !*set)
            .unwrap_or_else(|e| e.into_inner());
        flag = guard;
        *flag
    }
}

/// Python's `str.capitalize()`: the first character upper case, the rest lower.
pub fn py_capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
        None => String::new(),
    }
}
