//! Python's text for numbers.

pub fn py_float_repr(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf" } else { "-inf" }.into();
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0.0" } else { "0.0" }.into();
    }
    let sci = format!("{:e}", x.abs());
    let (mantissa, exp) = sci.split_once('e').expect("exponent form");
    let exp: i32 = exp.parse().expect("exponent");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let sign = if x < 0.0 { "-" } else { "" };
    if (-4..16).contains(&exp) {
        if exp >= 0 {
            let split = exp as usize + 1;
            let (int, frac) = if digits.len() > split {
                (digits[..split].to_string(), digits[split..].to_string())
            } else {
                (format!("{digits}{}", "0".repeat(split - digits.len())), "0".to_string())
            };
            format!("{sign}{int}.{frac}")
        } else {
            format!("{sign}0.{}{digits}", "0".repeat((-exp - 1) as usize))
        }
    } else {
        let rest = &digits[1..];
        let dot = if rest.is_empty() {
            String::new()
        } else {
            format!(".{rest}")
        };
        format!(
            "{sign}{}{dot}e{}{:02}",
            &digits[..1],
            if exp < 0 { '-' } else { '+' },
            exp.abs()
        )
    }
}

/// `str(OSError)`: `[Errno 2] No such file or directory: 'path'`.
pub fn os_error_text(e: &std::io::Error, path: Option<&std::path::Path>) -> String {
    let full = e.to_string();
    let strerror = crate::peripherals::util::os_error_text(e);
    match (e.raw_os_error(), path) {
        (Some(code), Some(p)) => format!("[Errno {code}] {strerror}: '{}'", p.display()),
        (Some(code), None) => format!("[Errno {code}] {strerror}"),
        (None, _) => full,
    }
}
