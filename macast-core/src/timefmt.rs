//! Calendar arithmetic without a date library.
//!
//! Python reaches for `time.strftime` and gets its answers from libc; the Rust
//! port deliberately has no `chrono` dependency, because every format here is
//! short, is required to be byte-exact against a specification (HTTP dates, UPnP
//! timestamps, log lines, quarantine filenames), and is checked by a unit test.
//!
//! The day-number arithmetic is Howard Hinnant's `civil_from_days` /
//! `days_from_civil`, which is valid for the whole proleptic Gregorian calendar
//! and does not need a lookup table of month lengths (so no leap-year branch in
//! the callers).

use std::time::{SystemTime, UNIX_EPOCH};

/// Whole seconds since the Unix epoch, or `0` if the clock is before it.
pub fn now_secs() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(_) => 0,
    }
}

/// Milliseconds since the Unix epoch, for log timestamps.
pub fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A broken-down calendar instant. `offset` is seconds to add to UTC to get
/// local time; it is `0` for the UTC forms below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Civil {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    /// 0 = Monday, matching Python's `datetime.weekday()`.
    pub weekday: u32,
    pub offset: i64,
}

impl Civil {
    /// Split a Unix timestamp into UTC components.
    pub fn from_utc(secs: i64) -> Civil {
       Civil::from_utc_at(secs, 0)
    }

    /// Split a Unix timestamp after adding `offset` seconds (so passing the
    /// local offset yields local components).
    pub fn from_utc_at(secs: i64, offset: i64) -> Civil {
        let total = secs + offset;
        let days = total.div_euclid(86_400);
        let secs_of_day = total.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days);
        Civil {
            year,
            month,
            day,
            hour: (secs_of_day / 3600) as u32,
            minute: ((secs_of_day % 3600) / 60) as u32,
            second: (secs_of_day % 60) as u32,
            weekday: (days.rem_euclid(7) + 3).rem_euclid(7) as u32,
            offset,
        }
    }

    /// Inverse of [`Civil::from_utc`]; used by the tests to prove the two
    /// directions agree.
    pub fn to_utc(&self) -> i64 {
        let days = days_from_civil(self.year, self.month, self.day);
        days * 86_400 + self.hour as i64 * 3600 + self.minute as i64 * 60 + self.second as i64
            - self.offset
    }
}

/// Days since 1970-01-01 for a civil date.
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Inverse of [`days_from_civil`].
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { y + 1 } else { y }, month, day)
}

pub const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
pub const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `Wed, 06 Oct 2026 11:25:03 GMT` — the one HTTP-date form we write.
///
/// Python uses `time.strftime("%a, %d %b %Y %H:%M:%S GMT", time.gmtime())`.
/// The names are English because libc's C locale is what Python's `gmtime`
/// formatting lands on; SSDP control points parse this header.
pub fn rfc1123_gmt(secs: i64) -> String {
    let c = Civil::from_utc_at(secs, 0);
    format!(
        "{}, {:02} {} {} {:02}:{:02}:{:02} GMT",
        WEEKDAYS[c.weekday as usize],
        c.day,
        MONTHS[(c.month - 1) as usize],
        c.year,
        c.hour,
        c.minute,
        c.second
    )
}

/// `20261006-192503` — the form used for quarantined files and `.trash` dirs.
///
/// Python has `time.strftime('%Y%m%d-%H%M%S')`, i.e. **local** time. Two
/// rejections in the same second must collide, and they do — the caller appends
/// `-1`, `-2` (AGENTS.md §4.2: the copy is named by second while one startup can
/// refuse three different files).
pub fn filename_stamp(secs: i64, local_offset: i64) -> String {
    let c = Civil::from_utc_at(secs, local_offset);
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        c.year, c.month, c.day, c.hour, c.minute, c.second
    )
}

/// `2026-10-06 19:25:03,482` — a Python `logging` line's `asctime`, comma and
/// milliseconds included. Keeping the exact shape means a user's grep for
/// `ERROR` and the tail reader in the settings page both still work.
pub fn log_stamp(secs: i64, millis: u32, local_offset: i64) -> String {
    let c = Civil::from_utc_at(secs, local_offset);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02},{:03}",
        c.year, c.month, c.day, c.hour, c.minute, c.second, millis
    )
}

/// Seconds to add to UTC for local time, asked from libc the way Python's
/// `time.localtime` does it. Falls back to `0` where there is no libc to ask
/// (Windows: the offsets here only ever label log lines and filenames).
pub fn local_offset_secs() -> i64 {
    #[cfg(unix)]
    {
        unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            let t: libc::time_t = libc::time(std::ptr::null_mut());
            if libc::localtime_r(&t, &mut tm).is_null() {
                0
            } else {
                tm.tm_gmtoff as i64
            }
        }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// `H:MM:SS` as DLNA spells a clock position (`0:00:12`, `1:04:05`).
///
/// Negative or non-finite input is clamped to zero rather than rendering a
/// minus sign: a control point that reads `-0:00:03` will not seek anywhere.
pub fn dlna_clock(secs: f64) -> String {
    let s = if secs.is_finite() && secs > 0.0 {
        secs.floor() as i64
    } else {
        0
    };
    format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

/// The other direction: `99:59:59`-ish text from a control point's `Seek`.
pub fn parse_dlna_clock(text: &str) -> Option<f64> {
    let parts: Vec<&str> = text.trim().split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let h: i64 = parts[0].parse().ok()?;
    let m: i64 = parts[1].parse().ok()?;
    let s: f64 = parts[2].parse().ok()?;
    if m < 0 || h < 0 || s < 0.0 {
        return None;
    }
    Some(h as f64 * 3600.0 + m as f64 * 60.0 + s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_is_the_unix_epoch_in_every_form() {
        assert_eq!(rfc1123_gmt(0), "Thu, 01 Jan 1970 00:00:00 GMT");
        let c = Civil::from_utc(0);
        assert_eq!((c.year, c.month, c.day, c.hour, c.weekday), (1970, 1, 1, 0, 3));
        assert_eq!(filename_stamp(0, 0), "19700101-000000");
        assert_eq!(log_stamp(0, 7, 0), "1970-01-01 00:00:00,007");
    }

    #[test]
    fn known_timestamps_match_python() {
        // 2000-02-29T12:34:56Z — a leap day, noon, and the weekday.
        // `days_from_civil` answers in **days**, so it has to be scaled before it
        // is a Unix timestamp; forgetting that makes this line read as
        // `1970-01-01 15:38:32` (11016 "seconds"), which is the bug this test
        // caught the first time it was written.
        let secs = days_from_civil(2000, 2, 29) * 86_400 + 12 * 3600 + 34 * 60 + 56;
        assert_eq!(rfc1123_gmt(secs), "Tue, 29 Feb 2000 12:34:56 GMT");
        assert_eq!(Civil::from_utc(secs).weekday, 1);
        // 2026-10-06T00:00:00Z is a Tuesday too, and 2026 is not a leap year.
        assert_eq!(Civil::from_utc(days_from_civil(2026, 10, 6) * 86_400).weekday, 1);
    }

    #[test]
    fn civil_round_trip_holds_across_the_2038_boundary_and_before_1970() {
        for secs in [-1_000_000_000i64, -1, 0, 1, 2_147_483_647, 4_102_444_800, 253_402_300_799] {
            let c = Civil::from_utc(secs);
            assert_eq!(c.to_utc(), secs, "round trip broke at {}", secs);
        }
    }

    #[test]
    fn local_offset_shifts_the_stamp_not_the_instant() {
        let secs = days_from_civil(2026, 10, 6) * 86_400 + 3600;
        assert_eq!(filename_stamp(secs, 0), "20261006-010000");
        assert_eq!(filename_stamp(secs, 8 * 3600), "20261006-090000");
        assert_eq!(Civil::from_utc_at(secs, 8 * 3600).to_utc(), secs);
        // A negative offset can cross midnight back into the previous day.
        assert_eq!(filename_stamp(secs, -2 * 3600), "20261005-230000");
    }

    #[test]
    fn dlna_clock_matches_python_zero_padding() {
        assert_eq!(dlna_clock(0.0), "0:00:00");
        assert_eq!(dlna_clock(12.0), "0:00:12");
        assert_eq!(dlna_clock(59.9), "0:00:59");
        assert_eq!(dlna_clock(3600.0), "1:00:00");
        assert_eq!(dlna_clock(3725.7), "1:02:05");
        assert_eq!(dlna_clock(-5.0), "0:00:00");
        assert_eq!(dlna_clock(f64::NAN), "0:00:00");
        assert_eq!(parse_dlna_clock("1:02:05"), Some(3725.0));
        assert_eq!(parse_dlna_clock("0:00:12"), Some(12.0));
        assert_eq!(parse_dlna_clock("12"), None);
        assert_eq!(parse_dlna_clock("a:b:c"), None);
        assert_eq!(parse_dlna_clock("-1:00:00"), None);
    }
}
