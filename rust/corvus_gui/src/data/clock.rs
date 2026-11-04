//! Calendar dates from Unix times, without a time zone database: UTC, and the
//! local wall clock through this machine's offset.

/// `(year, month, day, hour, minute, second)` of a Unix time in UTC.
pub fn civil(secs: i64) -> (i64, i64, i64, i64, i64, i64) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // days since 1970-01-01 to a civil date (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d, rem / 3600, rem % 3600 / 60, rem % 60)
}

/// `YYYY-MM-DD HH:MM` (with `:SS` when asked) of a Unix time, `offset`
/// seconds east of UTC (`formatStamp` of `src/js/analysis.js`).
pub fn stamp(secs: i64, offset: i64, with_seconds: bool) -> String {
    let (y, m, d, hh, mm, ss) = civil(secs + offset);
    let base = format!("{y}-{m:02}-{d:02} {hh:02}:{mm:02}");
    if with_seconds {
        format!("{base}:{ss:02}")
    } else {
        base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_unix_time_becomes_a_calendar_date() {
        assert_eq!(civil(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(civil(1_000_000_000), (2001, 9, 9, 1, 46, 40));
        assert_eq!(civil(951_782_400 + 86_399), (2000, 2, 29, 23, 59, 59));
        assert_eq!(civil(-1), (1969, 12, 31, 23, 59, 59));
    }

    #[test]
    fn the_stamp_follows_the_offset() {
        assert_eq!(stamp(1_000_000_000, 0, false), "2001-09-09 01:46");
        assert_eq!(stamp(1_000_000_000, 7200, true), "2001-09-09 03:46:40");
        assert_eq!(stamp(1_000_000_000, -2 * 3600, false), "2001-09-08 23:46");
    }
}
