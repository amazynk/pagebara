//! Text strings (ISO 32000-2 §7.9.2) and dates (§7.9.4).

use std::time::{SystemTime, UNIX_EPOCH};

use pagebara_syntax::text::{pdfdoc_char, pdfdoc_encode};
use pagebara_syntax::{Object, PdfString};

use crate::geom::Color;
use crate::model::Align;

/// Decodes a text string: UTF-16BE or UTF-8 with a byte order mark, else PDFDocEncoding.
pub fn decode(b: &[u8]) -> String {
    if let Some(rest) = b.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
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

/// A default style string (`/DS`, ISO 32000-2 §12.7.4.3) for free text.
pub(crate) fn css_style(family: &str, size: f64, color: Color, align: Align) -> String {
    let [r, g, b] = color
        .to_rgb()
        .map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
    let align = match align {
        Align::Left => "left",
        Align::Center => "center",
        Align::Right => "right",
    };
    format!("font: {family} {size}pt; text-align:{align}; color:#{r:02X}{g:02X}{b:02X}")
}

/// `text` as a rich text string (`/RC`, §12.7.4.3): one XHTML paragraph per line.
pub(crate) fn rich_text(text: &str, style: &str) -> String {
    let mut out = format!(
        "<?xml version=\"1.0\"?><body xmlns=\"http://www.w3.org/1999/xhtml\" \
         xmlns:xfa=\"http://www.xfa.org/schema/xfa-data/1.0/\" xfa:spec=\"2.0.2\" \
         style=\"{}\">",
        escape(style)
    );
    for line in text.split('\n') {
        out.push_str("<p dir=\"ltr\">");
        out.push_str(&escape(line.trim_end_matches('\r')));
        out.push_str("</p>");
    }
    out.push_str("</body>");
    out
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// The current time as a PDF date in UTC, `D:YYYYMMDDHHmmSSZ`.
pub fn now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    date(secs)
}

/// Seconds since the Unix epoch as a PDF date in UTC.
pub fn date(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil(days);
    format!(
        "D:{y:04}{m:02}{d:02}{:02}{:02}{:02}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
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
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(date(0), "D:19700101000000Z");
        assert_eq!(date(1_700_000_000), "D:20231114221320Z");
        assert_eq!(date(951_782_400), "D:20000229000000Z");
    }

    #[test]
    fn strings_round_trip() {
        for s in ["Hello", "Grüße", "日本語", ""] {
            let o = encode(s);
            assert_eq!(decode(o.as_string().unwrap()), s);
        }
        assert_eq!(decode(b"\xEF\xBB\xBFh\xC3\xA9"), "hé");
    }

    #[test]
    fn rich_text_escapes() {
        let style = css_style("Helvetica", 12.0, Color::Rgb(0.0, 0.0, 1.0), Align::Center);
        assert_eq!(
            style,
            "font: Helvetica 12pt; text-align:center; color:#0000FF"
        );
        let rc = rich_text("a<b & \"Ж\"\nnext", &style);
        assert!(
            rc.contains("<p dir=\"ltr\">a&lt;b &amp; &quot;Ж&quot;</p><p dir=\"ltr\">next</p>")
        );
    }
}
