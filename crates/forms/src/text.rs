//! Text strings (ISO 32000-2 §7.9.2) and dates (§7.9.4).

use pagebara_syntax::text::{pdfdoc_char, pdfdoc_encode};
use pagebara_syntax::{Object, PdfString};

/// Decodes a text string: UTF-16BE or UTF-8 with a byte order mark, else PDFDocEncoding.
pub fn decode(b: &[u8]) -> String {
    if let Some(rest) = b.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_be_bytes(*c))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if let Some(rest) = b.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    b.iter().map(|&c| pdfdoc_char(c)).collect()
}

/// A text string object: PDFDocEncoding when it can hold `s`, else UTF-16BE.
pub fn encode(s: &str) -> Object {
    let bytes = pdfdoc_encode(s).unwrap_or_else(|| {
        let mut v = vec![0xFE, 0xFF];
        v.extend(s.encode_utf16().flat_map(u16::to_be_bytes));
        v
    });
    Object::String(PdfString { bytes, hex: false })
}

/// A name object's text: UTF-8 when valid (PDF 2.0), else Latin-1.
pub fn name_text(b: &[u8]) -> String {
    match std::str::from_utf8(b) {
        Ok(s) => s.to_owned(),
        Err(_) => b.iter().map(|&c| char::from(c)).collect(),
    }
}

/// The text of a string or name object.
pub fn text_of(o: &Object) -> Option<String> {
    match o {
        Object::String(s) => Some(decode(&s.bytes)),
        Object::Name(n) => Some(name_text(n.as_bytes())),
        _ => None,
    }
}

/// A calendar date and time with its UTC offset in minutes, if given.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct DateTime {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub offset_minutes: Option<i16>,
}

impl DateTime {
    /// Parses `D:YYYYMMDDHHmmSSOHH'mm'` with every part after the year optional.
    pub fn parse_pdf(s: &str) -> Option<DateTime> {
        let s = s.trim();
        let s = s.strip_prefix("D:").unwrap_or(s);
        let digits = |from: usize, n: usize| -> Option<u32> {
            let p = s.get(from..from + n)?;
            p.bytes()
                .all(|b| b.is_ascii_digit())
                .then(|| p.parse().ok())?
        };
        let year = digits(0, 4)? as i32;
        let part = |from, default| digits(from, 2).map_or(default, |v| v as u8);
        let mut d = DateTime {
            year,
            month: part(4, 1).clamp(1, 12),
            day: part(6, 1).clamp(1, 31),
            hour: part(8, 0).min(23),
            minute: part(10, 0).min(59),
            second: part(12, 0).min(59),
            offset_minutes: None,
        };
        let digits_end = s.bytes().take_while(u8::is_ascii_digit).count().min(14);
        let rest = &s[digits_end..];
        match rest.as_bytes().first() {
            Some(b'Z') => d.offset_minutes = Some(0),
            Some(&sign @ (b'+' | b'-')) => {
                let t: String = rest[1..].chars().filter(char::is_ascii_digit).collect();
                let h: i16 = t.get(0..2).and_then(|v| v.parse().ok()).unwrap_or(0);
                let m: i16 = t.get(2..4).and_then(|v| v.parse().ok()).unwrap_or(0);
                let off = h * 60 + m;
                d.offset_minutes = Some(if sign == b'-' { -off } else { off });
            }
            _ => {}
        }
        Some(d)
    }

    /// ISO 8601, as `2024-03-05T14:30:00+01:00` (no offset when unknown).
    pub fn to_iso(&self) -> String {
        let mut s = format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        );
        match self.offset_minutes {
            Some(0) => s.push('Z'),
            Some(o) => {
                let sign = if o < 0 { '-' } else { '+' };
                let o = o.unsigned_abs();
                s.push_str(&format!("{sign}{:02}:{:02}", o / 60, o % 60));
            }
            None => {}
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for s in ["plain", "Grüße", "日本語", ""] {
            let o = encode(s);
            assert_eq!(text_of(&o).unwrap(), s);
        }
        assert_eq!(decode(b"\xEF\xBB\xBFa\xC3\xA9"), "aé");
    }

    #[test]
    fn dates() {
        let d = DateTime::parse_pdf("D:20240305143000+01'00'").unwrap();
        assert_eq!(d.to_iso(), "2024-03-05T14:30:00+01:00");
        let d = DateTime::parse_pdf("D:2023").unwrap();
        assert_eq!(d.to_iso(), "2023-01-01T00:00:00");
        assert_eq!(
            DateTime::parse_pdf("D:20231231235959Z").unwrap().to_iso(),
            "2023-12-31T23:59:59Z"
        );
        assert!(DateTime::parse_pdf("garbage").is_none());
    }
}
