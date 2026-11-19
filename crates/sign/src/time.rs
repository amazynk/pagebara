//! Points in time as Unix seconds (UTC).

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::asn1::{civil, days_from_civil};

/// A point in time, in seconds since 1970-01-01 UTC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Time(pub i64);

impl Time {
    pub fn now() -> Time {
        Time::from(SystemTime::now())
    }

    pub fn from_ymd_hms(y: i64, mo: i64, d: i64, h: i64, mi: i64, s: i64) -> Time {
        Time(days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + s)
    }

    /// ISO 8601, e.g. `2024-03-05T13:30:00Z`.
    pub fn to_iso(self) -> String {
        let (y, mo, d, h, mi, s) = civil(self.0);
        format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
    }

    /// A PDF date string, e.g. `D:20240305133000Z`.
    pub fn to_pdf(self) -> String {
        let (y, mo, d, h, mi, s) = civil(self.0);
        format!("D:{y:04}{mo:02}{d:02}{h:02}{mi:02}{s:02}Z")
    }

    /// From a PDF date string (`D:YYYYMMDDHHmmSSOHH'mm`), fields after the year optional.
    pub fn parse_pdf(s: &str) -> Option<Time> {
        let s = s.strip_prefix("D:").unwrap_or(s);
        let num = |a: usize, b: usize, d: i64| -> Option<i64> {
            match s.get(a..b) {
                Some(t) if t.bytes().all(|c| c.is_ascii_digit()) => t.parse().ok(),
                Some(_) => None,
                None => Some(d),
            }
        };
        let y = num(0, 4, -1).filter(|&y| y >= 0)?;
        let (mo, d) = (num(4, 6, 1)?, num(6, 8, 1)?);
        let (h, mi, sec) = (num(8, 10, 0)?, num(10, 12, 0)?, num(12, 14, 0)?);
        let offset = match s.as_bytes().get(14) {
            Some(&c @ (b'+' | b'-')) => {
                let oh: i64 = s.get(15..17)?.parse().ok()?;
                let om: i64 = s
                    .get(18..20)
                    .or_else(|| s.get(17..19).filter(|t| !t.contains('\'')))
                    .and_then(|t| t.parse().ok())
                    .unwrap_or(0);
                let o = oh * 3600 + om * 60;
                if c == b'+' { o } else { -o }
            }
            _ => 0,
        };
        Some(Time(Time::from_ymd_hms(y, mo, d, h, mi, sec).0 - offset))
    }
}

impl From<SystemTime> for Time {
    fn from(t: SystemTime) -> Time {
        match t.duration_since(UNIX_EPOCH) {
            Ok(d) => Time(d.as_secs() as i64),
            Err(e) => Time(-(e.duration().as_secs() as i64)),
        }
    }
}

impl fmt::Display for Time {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_iso())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdf_dates() {
        let t = Time::parse_pdf("D:20240305143000+01'00'").unwrap();
        assert_eq!(t.to_iso(), "2024-03-05T13:30:00Z");
        assert_eq!(Time::parse_pdf(&t.to_pdf()), Some(t));
        assert_eq!(
            Time::parse_pdf("D:2024").unwrap().to_iso(),
            "2024-01-01T00:00:00Z"
        );
    }
}
