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
    fn local_time_has_the_strftime_shape() {
        let s = SystemClock.local_hms();
        assert_eq!(s.len(), 8);
        assert_eq!(&s[2..3], ":");
        assert_eq!(&s[5..6], ":");
        assert_eq!(utc_hms(3 * 3600 + 4 * 60 + 5), (3, 4, 5));
    }
}
