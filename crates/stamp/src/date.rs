//! Dates for header tokens, formatted with a `strftime` subset.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// The moment `{date}` and `{time}` show: seconds since the Unix epoch and the UTC offset
/// in minutes to show them in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clock {
    pub secs: i64,
    pub offset_minutes: i32,
}

impl Clock {
    /// Now, shown in the system's local time zone (its UTC offset at this moment; UTC
    /// when the zone cannot be found).
    pub fn local() -> Clock {
        let now = jiff::Timestamp::now();
        let offset = jiff::tz::TimeZone::try_system()
            .map(|tz| tz.to_offset(now).seconds() / 60)
            .unwrap_or(0);
        Clock {
            secs: now.as_second(),
            offset_minutes: offset,
        }
    }

    pub fn now(offset_minutes: i32) -> Clock {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        Clock {
            secs,
            offset_minutes,
        }
    }
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const DAYS: [&str; 7] = [
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
];

/// `%Y %y %m %d %e %H %I %M %S %p %B %b %A %a %j %%`; other characters as they are.
pub fn format_date(c: Clock, fmt: &str) -> String {
    let t = c.secs + i64::from(c.offset_minutes) * 60;
    let (days, rem) = (t.div_euclid(86_400), t.rem_euclid(86_400));
    let (y, m, d) = civil(days);
    let (hh, mm, ss) = (rem / 3600, rem / 60 % 60, rem % 60);
    let yday = days - days_from_civil(y, 1, 1) + 1;
    let wd = days.rem_euclid(7) as usize;
    let mut out = String::new();
    let mut it = fmt.chars();
    while let Some(ch) = it.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }
        let s = match it.next() {
            Some('Y') => format!("{y:04}"),
            Some('y') => format!("{:02}", y.rem_euclid(100)),
            Some('m') => format!("{m:02}"),
            Some('d') => format!("{d:02}"),
            Some('e') => d.to_string(),
            Some('H') => format!("{hh:02}"),
            Some('I') => format!("{:02}", (hh + 11) % 12 + 1),
            Some('M') => format!("{mm:02}"),
            Some('S') => format!("{ss:02}"),
            Some('p') => (if hh < 12 { "AM" } else { "PM" }).into(),
            Some('B') => MONTHS[m as usize - 1].into(),
            Some('b') => MONTHS[m as usize - 1][..3].into(),
            Some('A') => DAYS[wd].into(),
            Some('a') => DAYS[wd][..3].into(),
            Some('j') => format!("{yday:03}"),
            Some('%') => "%".into(),
            Some(o) => format!("%{o}"),
            None => "%".into(),
        };
        out.push_str(&s);
    }
    out
}

/// Days since 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's algorithm).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        // 2024-02-29 13:05:09 UTC, a Thursday.
        let c = Clock {
            secs: 1_709_211_909,
            offset_minutes: 0,
        };
        assert_eq!(format_date(c, "%Y-%m-%d %H:%M:%S"), "2024-02-29 13:05:09");
        assert_eq!(
            format_date(c, "%e %B %Y, %a %I %p"),
            "29 February 2024, Thu 01 PM"
        );
        assert_eq!(format_date(c, "%j %%"), "060 %");
        let east = Clock {
            offset_minutes: 11 * 60,
            ..c
        };
        assert_eq!(format_date(east, "%d %H"), "01 00");
        // The local clock is now, in some zone within a day of UTC.
        let l = Clock::local();
        assert!(l.offset_minutes.abs() <= 14 * 60);
        assert!((l.secs - Clock::now(0).secs).abs() < 5);
    }
}
