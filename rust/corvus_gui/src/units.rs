//! The display units the operator reads numbers in.
//!
//! Everything is SI underneath and stays that way: this is only the last
//! step before a number reaches the screen, so a unit choice can never change
//! what is sent to the aircraft. Four independent quantities, because real
//! stations mix them (a glider pilot reads feet and knots, a European
//! multirotor crew metres and km/h). The choice is the backend config's
//! `ui.units`, the same one the web interface reads.
//!
//! Numbers are rounded the way the web interface rounds them (`Math.round`,
//! `toFixed`), so the two show the same digits for the same state.

use serde_json::Value;

/// One unit of a quantity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Unit {
    pub id: &'static str,
    pub symbol: &'static str,
    pub per_si: f64,
    /// Distance units only: below this many metres the length unit is used.
    pub short_below: f64,
}

const fn unit(id: &'static str, symbol: &'static str, per_si: f64) -> Unit {
    Unit {
        id,
        symbol,
        per_si,
        short_below: 0.0,
    }
}

const LENGTH: &[Unit] = &[unit("m", "m", 1.0), unit("ft", "ft", 1.0 / 0.3048)];
const DISTANCE: &[Unit] = &[
    Unit {
        id: "km",
        symbol: "km",
        per_si: 1.0 / 1000.0,
        short_below: 1000.0,
    },
    Unit {
        id: "mi",
        symbol: "mi",
        per_si: 1.0 / 1609.344,
        short_below: 160.9344,
    },
    Unit {
        id: "nmi",
        symbol: "NM",
        per_si: 1.0 / 1852.0,
        short_below: 185.2,
    },
];
const SPEED: &[Unit] = &[
    unit("ms", "m/s", 1.0),
    unit("kmh", "km/h", 3.6),
    unit("mph", "mph", 3600.0 / 1609.344),
    unit("kn", "kn", 3600.0 / 1852.0),
];
const TEMPERATURE: &[Unit] = &[unit("c", "°C", 1.0), unit("f", "°F", 1.0)];

/// The units in force.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Units {
    pub length: Unit,
    pub distance: Unit,
    pub speed: Unit,
    pub temperature: Unit,
}

impl Default for Units {
    fn default() -> Self {
        Units {
            length: LENGTH[0],
            distance: DISTANCE[0],
            speed: SPEED[0],
            temperature: TEMPERATURE[0],
        }
    }
}

fn pick(list: &'static [Unit], raw: Option<&Value>) -> Unit {
    let id = raw.and_then(Value::as_str);
    list.iter().find(|u| Some(u.id) == id).copied().unwrap_or(list[0])
}

impl Units {
    /// From a `{length, distance, speed, temperature}` object; an unknown or
    /// missing key falls back to metric, so a config from a newer build never
    /// leaves a quantity without a unit.
    pub fn from_value(v: &Value) -> Units {
        Units {
            length: pick(LENGTH, v.get("length")),
            distance: pick(DISTANCE, v.get("distance")),
            speed: pick(SPEED, v.get("speed")),
            temperature: pick(TEMPERATURE, v.get("temperature")),
        }
    }

    /// From the backend config (`ui.units`), metric when it names none.
    pub fn from_config(cfg: &Value) -> Units {
        match cfg.get("ui").and_then(|u| u.get("units")) {
            Some(u) if u.is_object() => Units::from_value(u),
            _ => Units::default(),
        }
    }

    /// An altitude or other length, e.g. `"120 m"`; `bare` leaves off the symbol.
    pub fn format_length(&self, m: f64, digits: usize, signed: bool, bare: bool) -> String {
        if !m.is_finite() {
            return String::new();
        }
        let s = fixed(m * self.length.per_si, digits, signed);
        if bare {
            s
        } else {
            format!("{s} {}", self.length.symbol)
        }
    }

    /// A trip or a range (`formatDistance`): the length unit below the
    /// distance unit's switch point, else the distance unit to two decimals.
    pub fn format_distance(&self, m: f64) -> String {
        if !m.is_finite() {
            return String::new();
        }
        if m < self.distance.short_below {
            return format!("{} {}", js_round(m * self.length.per_si), self.length.symbol);
        }
        format!("{} {}", js_to_fixed(m * self.distance.per_si, 2), self.distance.symbol)
    }

    /// A speed, one decimal unless told otherwise.
    pub fn format_speed(&self, ms: f64, digits: usize, signed: bool, bare: bool) -> String {
        if !ms.is_finite() {
            return String::new();
        }
        let s = fixed(ms * self.speed.per_si, digits, signed);
        if bare {
            s
        } else {
            format!("{s} {}", self.speed.symbol)
        }
    }

    /// A temperature in the chosen scale.
    #[allow(dead_code)]
    pub fn format_temperature(&self, c: f64, digits: usize) -> String {
        if !c.is_finite() {
            return String::new();
        }
        let v = if self.temperature.id == "f" {
            c * 9.0 / 5.0 + 32.0
        } else {
            c
        };
        format!("{} {}", fixed(v, digits, false), self.temperature.symbol)
    }
}

/// JavaScript's `Math.round`: the nearest integer, a half going up (towards
/// +∞). `v - floor(v)` is exact for every f64 that has a fraction.
pub fn js_round(v: f64) -> f64 {
    let f = v.floor();
    if v - f >= 0.5 {
        f + 1.0
    } else {
        f
    }
}

/// JavaScript's `Number.prototype.toFixed`: the decimal closest to the exact
/// binary value, a tie going to the larger magnitude.
pub fn js_to_fixed(v: f64, digits: usize) -> String {
    if !v.is_finite() {
        return format!("{v}");
    }
    let negative = v < 0.0;
    // Rust prints the exact decimal expansion of an f64 when asked for
    // enough digits; 1100 covers the smallest subnormal.
    let exact = format!("{:.1100}", v.abs());
    let (int_part, frac_part) = exact.split_once('.').unwrap_or((&exact, ""));
    let mut digits_vec: Vec<u8> = int_part
        .bytes()
        .chain(frac_part.bytes().take(digits))
        .map(|b| b - b'0')
        .collect();
    let rest = &frac_part[digits.min(frac_part.len())..];
    let round_up = rest.as_bytes().first().is_some_and(|&d| d >= b'5');
    if round_up {
        let mut i = digits_vec.len();
        loop {
            if i == 0 {
                digits_vec.insert(0, 1);
                break;
            }
            i -= 1;
            if digits_vec[i] == 9 {
                digits_vec[i] = 0;
            } else {
                digits_vec[i] += 1;
                break;
            }
        }
    }
    let split = digits_vec.len() - digits;
    let mut out: String = digits_vec[..split].iter().map(|d| (b'0' + d) as char).collect();
    let out_int = out.trim_start_matches('0');
    out = if out_int.is_empty() {
        "0".to_string()
    } else {
        out_int.to_string()
    };
    if digits > 0 {
        out.push('.');
        out.extend(digits_vec[split..].iter().map(|d| (b'0' + d) as char));
    }
    // JavaScript writes a "-" for any x < 0, even when the digits are all 0.
    if negative {
        format!("-{out}")
    } else {
        out
    }
}

/// JavaScript's `String(number)` for the values a display shows: whole
/// numbers without a fraction, others in their shortest round-trip form.
pub fn js_number(v: f64) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v.fract() == 0.0 && v.abs() < 1e21 {
        return format!("{}", v as i64);
    }
    format!("{v}")
}

/// The web interface's `fixed`: `toFixed(digits)` above zero digits, else
/// `String(Math.round(v))`, with a `+` on a non-negative value when signed.
pub fn fixed(v: f64, digits: usize, signed: bool) -> String {
    let s = if digits > 0 {
        js_to_fixed(v, digits)
    } else {
        let r = js_round(v);
        if r == 0.0 {
            "0".to_string()
        } else {
            format!("{}", r as i64)
        }
    };
    if signed && v >= 0.0 {
        format!("+{s}")
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rounding_matches_javascript() {
        assert_eq!(js_round(2.5), 3.0);
        assert_eq!(js_round(-2.5), -2.0);
        assert_eq!(js_round(-2.6), -3.0);
        assert_eq!(js_round(0.4), 0.0);
        assert_eq!(js_to_fixed(0.25, 1), "0.3");
        assert_eq!(js_to_fixed(1.005, 2), "1.00");
        assert_eq!(js_to_fixed(24.95, 1), "24.9"); // 24.95 is 24.9499... in binary
        assert_eq!(js_to_fixed(-1.25, 1), "-1.3");
        assert_eq!(js_to_fixed(-0.04, 1), "-0.0");
        assert_eq!(js_to_fixed(9.96, 1), "10.0");
        assert_eq!(js_to_fixed(0.0, 2), "0.00");
        assert_eq!(js_to_fixed(123.456, 0), "123");
        assert_eq!(js_number(99.0), "99");
        assert_eq!(js_number(99.5), "99.5");
        assert_eq!(js_number(-0.0), "0");
    }

    #[test]
    fn formatting_as_the_web_interface_does() {
        let metric = Units::default();
        assert_eq!(metric.format_length(120.4, 0, false, false), "120 m");
        assert_eq!(metric.format_length(-0.4, 0, false, true), "0");
        assert_eq!(metric.format_speed(6.04, 1, false, true), "6.0");
        assert_eq!(metric.format_speed(0.0, 1, true, true), "+0.0");
        assert_eq!(metric.format_speed(-1.25, 1, true, true), "-1.3");
        assert_eq!(metric.format_length(f64::NAN, 0, false, false), "");
        let aviation = Units::from_value(&json!({"length": "ft", "distance": "nmi", "speed": "kn"}));
        assert_eq!(aviation.format_length(100.0, 0, false, false), "328 ft");
        assert_eq!(aviation.format_speed(10.0, 1, false, false), "19.4 kn");
        assert_eq!(aviation.temperature.id, "c");
    }

    #[test]
    fn the_config_choice_with_fallbacks() {
        let cfg = json!({"ui": {"units": {"length": "ft", "speed": "warp"}}});
        let u = Units::from_config(&cfg);
        assert_eq!(u.length.id, "ft");
        assert_eq!(u.speed.id, "ms");
        assert_eq!(Units::from_config(&json!({})), Units::default());
    }
}
