//! The two clocks the store reads, behind a trait so a test can pin them.

use std::sync::OnceLock;
use std::time::Instant;

pub trait Clock: Send + Sync {
    /// Seconds on a clock that only moves forwards (`time.monotonic()`).
    fn monotonic(&self) -> f64;

    /// The local time as `HH:MM:SS` (`time.strftime("%H:%M:%S")`).
    fn local_hms(&self) -> String;
}

/// The operating system's clocks.
pub struct SystemClock;

static ORIGIN: OnceLock<Instant> = OnceLock::new();

/// Seconds since the first call in this process.
pub fn monotonic() -> f64 {
    ORIGIN.get_or_init(Instant::now).elapsed().as_secs_f64()
}

impl Clock for SystemClock {
    fn monotonic(&self) -> f64 {
        monotonic()
    }

    fn local_hms(&self) -> String {
        let (h, m, s) = local_hms();
        format!("{h:02}:{m:02}:{s:02}")
    }
}

#[cfg(unix)]
fn local_hms() -> (u32, u32, u32) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as libc::time_t)
        .unwrap_or(0);
    // SAFETY: localtime_r writes only into the struct it is given.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let ok = unsafe { !libc::localtime_r(&now, &mut tm).is_null() };
    if ok {
        (tm.tm_hour as u32, tm.tm_min as u32, tm.tm_sec as u32)
    } else {
        utc_hms(now as u64)
    }
}

#[cfg(windows)]
fn local_hms() -> (u32, u32, u32) {
    #[repr(C)]
    #[derive(Default)]
    struct SystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        milliseconds: u16,
    }
    extern "system" {
        fn GetLocalTime(out: *mut SystemTime);
    }
    let mut t = SystemTime::default();
    // SAFETY: GetLocalTime fills the struct it is given.
    unsafe { GetLocalTime(&mut t) };
    (t.hour as u32, t.minute as u32, t.second as u32)
}

/// A calendar time with microseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub micro: u32,
}

/// Days since 1970-01-01 to a civil date (proleptic Gregorian).
fn civil(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = (yoe + era * 400 + i64::from(m <= 2)) as i32;
    (y, m, d)
}

/// Unix seconds as a UTC calendar time.
pub fn utc_datetime(unix_seconds: f64) -> DateTime {
    let whole = unix_seconds.floor();
    let micro = ((unix_seconds - whole) * 1e6).round_ties_even().min(999_999.0) as u32;
    let secs = whole as i64;
    let (year, month, day) = civil(secs.div_euclid(86_400));
    let of_day = secs.rem_euclid(86_400);
    DateTime {
        year,
        month,
        day,
        hour: (of_day / 3600) as u32,
        minute: (of_day % 3600 / 60) as u32,
        second: (of_day % 60) as u32,
        micro,
    }
}

/// The local calendar time now (`datetime.datetime.now()`).
pub fn local_datetime() -> DateTime {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let micro = now.subsec_micros();
    #[cfg(unix)]
    {
        let secs = now.as_secs() as libc::time_t;
        // SAFETY: localtime_r writes only into the struct it is given.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        if unsafe { !libc::localtime_r(&secs, &mut tm).is_null() } {
            return DateTime {
                year: tm.tm_year + 1900,
                month: (tm.tm_mon + 1) as u32,
                day: tm.tm_mday as u32,
                hour: tm.tm_hour as u32,
                minute: tm.tm_min as u32,
                second: tm.tm_sec as u32,
                micro,
            };
        }
    }
    let mut utc = utc_datetime(now.as_secs_f64());
    utc.micro = micro;
    utc
}

#[allow(dead_code)]
fn utc_hms(secs: u64) -> (u32, u32, u32) {
    let day = secs % 86_400;
    ((day / 3600) as u32, (day % 3600 / 60) as u32, (day % 60) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monotonic_only_moves_forward() {
        let a = monotonic();
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(monotonic() > a);
    }

    #[test]
    fn utc_calendar_times() {
        let t = utc_datetime(1_700_000_000.25);
        assert_eq!(
            (t.year, t.month, t.day, t.hour, t.minute, t.second, t.micro),
            (2023, 11, 14, 22, 13, 20, 250_000)
        );
        let epoch = utc_datetime(0.0);
        assert_eq!((epoch.year, epoch.month, epoch.day), (1970, 1, 1));
        let leap = utc_datetime(1_709_164_800.0);
        assert_eq!((leap.year, leap.month, leap.day), (2024, 2, 29));
    }

    #[test]
    fn local_time_has_the_strftime_shape() {
        let s = SystemClock.local_hms();
        assert_eq!(s.len(), 8);
        assert_eq!(&s[2..3], ":");
        assert_eq!(&s[5..6], ":");
        assert_eq!(utc_hms(3 * 3600 + 4 * 60 + 5), (3, 4, 5));
    }
}
