//! This machine's local time, the way `time.localtime` and `time.mktime` read
//! it. The log names carry the wall clock the operator flew by, not UTC.

/// A broken down local time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Local {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

impl Local {
    /// `time.strftime("%Y-%m-%d_%H-%M")`.
    pub fn stamp(&self) -> String {
        format!(
            "{:04}-{:02}-{:02}_{:02}-{:02}",
            self.year, self.month, self.day, self.hour, self.minute
        )
    }

    /// `time.strftime("%Y-%m-%d %H:%M:%S")`.
    pub fn full(&self) -> String {
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

#[cfg(unix)]
mod sys {
    pub use libc::tm as Tm;

    pub fn localtime(secs: i64) -> Option<Tm> {
        let t = secs as libc::time_t;
        // SAFETY: localtime_r writes only into the struct it is given.
        let mut tm: Tm = unsafe { std::mem::zeroed() };
        if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
            None
        } else {
            Some(tm)
        }
    }

    pub fn mktime(tm: &mut Tm) -> i64 {
        // SAFETY: mktime normalises the struct it is given.
        unsafe { libc::mktime(tm) as i64 }
    }

    pub fn blank() -> Tm {
        // SAFETY: an all zero `tm` is a valid value.
        unsafe { std::mem::zeroed() }
    }
}

#[cfg(windows)]
mod sys {
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Tm {
        pub tm_sec: i32,
        pub tm_min: i32,
        pub tm_hour: i32,
        pub tm_mday: i32,
        pub tm_mon: i32,
        pub tm_year: i32,
        pub tm_wday: i32,
        pub tm_yday: i32,
        pub tm_isdst: i32,
    }

    extern "C" {
        fn _localtime64_s(out: *mut Tm, t: *const i64) -> i32;
        fn _mktime64(tm: *mut Tm) -> i64;
    }

    pub fn blank() -> Tm {
        Tm {
            tm_sec: 0,
            tm_min: 0,
            tm_hour: 0,
            tm_mday: 0,
            tm_mon: 0,
            tm_year: 0,
            tm_wday: 0,
            tm_yday: 0,
            tm_isdst: 0,
        }
    }

    pub fn localtime(secs: i64) -> Option<Tm> {
        let mut tm = blank();
        // SAFETY: _localtime64_s fills the struct it is given.
        if unsafe { _localtime64_s(&mut tm, &secs) } == 0 {
            Some(tm)
        } else {
            None
        }
    }

    pub fn mktime(tm: &mut Tm) -> i64 {
        // SAFETY: _mktime64 normalises the struct it is given.
        unsafe { _mktime64(tm) }
    }
}

/// `time.localtime(epoch)`; `None` where the system cannot say.
pub fn localtime(epoch: i64) -> Option<Local> {
    let tm = sys::localtime(epoch)?;
    Some(Local {
        year: tm.tm_year + 1900,
        month: (tm.tm_mon + 1) as u32,
        day: tm.tm_mday as u32,
        hour: tm.tm_hour as u32,
        minute: tm.tm_min as u32,
        second: tm.tm_sec as u32,
    })
}

/// `time.mktime((y, m, d, H, M, S, 0, 1, -1))`: the epoch of a local time, a
/// field out of range normalised by the C library as Python does. `None` where
/// Python raises.
pub fn mktime(year: i32, month: i32, day: i32, hour: i32, minute: i32, second: i32) -> Option<f64> {
    let mut tm = sys::blank();
    tm.tm_year = year.checked_sub(1900)?;
    tm.tm_mon = month - 1;
    tm.tm_mday = day;
    tm.tm_hour = hour;
    tm.tm_min = minute;
    tm.tm_sec = second;
    tm.tm_wday = -1;
    tm.tm_yday = 1;
    tm.tm_isdst = -1;
    let t = sys::mktime(&mut tm);
    if t == -1 && tm.tm_wday == -1 {
        return None;
    }
    Some(t as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_local_time_round_trips_through_the_epoch() {
        let epoch = mktime(2026, 9, 10, 14, 32, 5).unwrap();
        let back = localtime(epoch as i64).unwrap();
        assert_eq!(back.full(), "2026-09-10 14:32:05");
        assert_eq!(back.stamp(), "2026-09-10_14-32");
    }
}
