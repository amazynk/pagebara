//! Text encodings: PDFDocEncoding (Annex D.2) and SASLprep for passwords.

use unicode_bidi::{BidiClass, bidi_class};
use unicode_normalization::UnicodeNormalization;

/// Code points of 0x18..=0x1F, then of 0x80..=0xA0 (0x9F is undefined).
const LOW: [char; 8] = [
    '\u{02D8}', '\u{02C7}', '\u{02C6}', '\u{02D9}', '\u{02DD}', '\u{02DB}', '\u{02DA}', '\u{02DC}',
];
const HIGH: [char; 33] = [
    '\u{2022}', '\u{2020}', '\u{2021}', '\u{2026}', '\u{2014}', '\u{2013}', '\u{0192}', '\u{2044}',
    '\u{2039}', '\u{203A}', '\u{2212}', '\u{2030}', '\u{201E}', '\u{201C}', '\u{201D}', '\u{2018}',
    '\u{2019}', '\u{201A}', '\u{2122}', '\u{FB01}', '\u{FB02}', '\u{0141}', '\u{0152}', '\u{0160}',
    '\u{0178}', '\u{017D}', '\u{0131}', '\u{0142}', '\u{0153}', '\u{0161}', '\u{017E}', '\u{FFFD}',
    '\u{20AC}',
];

pub fn pdfdoc_char(b: u8) -> char {
    match b {
        0x18..=0x1F => LOW[usize::from(b - 0x18)],
        0x80..=0xA0 => HIGH[usize::from(b - 0x80)],
        _ => char::from(b),
    }
}

/// `None` if a character has no PDFDocEncoding byte.
pub fn pdfdoc_encode(s: &str) -> Option<Vec<u8>> {
    s.chars()
        .map(|c| match c {
            '\u{18}'..='\u{1F}' | '\u{80}'..='\u{A0}' => None,
            '\0'..='\u{FF}' => Some(c as u8),
            _ => LOW
                .iter()
                .position(|&l| l == c)
                .map(|i| 0x18 + i as u8)
                .or_else(|| {
                    HIGH.iter()
                        .position(|&h| h == c && h != '\u{FFFD}')
                        .map(|i| 0x80 + i as u8)
                }),
        })
        .collect()
}

/// SASLprep (RFC 4013) as a query: mapping, NFKC, prohibited characters and the bidi rule,
/// with current Unicode data and unassigned code points allowed, so passwords with newer
/// characters (emoji) work as in other readers. `None` if the password is not allowed.
pub fn saslprep(s: &str) -> Option<String> {
    let mapped: String = s
        .chars()
        .filter(|&c| !mapped_to_nothing(c))
        .map(|c| if non_ascii_space(c) { ' ' } else { c })
        .collect();
    let out: String = mapped.nfkc().collect();
    if out.chars().any(prohibited) {
        return None;
    }
    let rtl = |c: char| matches!(bidi_class(c), BidiClass::R | BidiClass::AL);
    if out.chars().any(rtl) {
        let (first, last) = (out.chars().next()?, out.chars().next_back()?);
        if out.chars().any(|c| bidi_class(c) == BidiClass::L) || !rtl(first) || !rtl(last) {
            return None;
        }
    }
    Some(out)
}

/// RFC 3454 table B.1.
fn mapped_to_nothing(c: char) -> bool {
    matches!(c, '\u{AD}' | '\u{34F}' | '\u{1806}' | '\u{180B}'..='\u{180D}' | '\u{200B}'..='\u{200D}'
        | '\u{2060}' | '\u{FE00}'..='\u{FE0F}' | '\u{FEFF}')
}

/// Table C.1.2.
fn non_ascii_space(c: char) -> bool {
    matches!(
        c,
        '\u{A0}' | '\u{1680}' | '\u{2000}'..='\u{200B}' | '\u{202F}' | '\u{205F}' | '\u{3000}'
    )
}

/// Tables C.1.2 and C.2.1–C.9.
fn prohibited(c: char) -> bool {
    let n = c as u32;
    non_ascii_space(c)
        || matches!(n, 0..=0x1F | 0x7F..=0x9F | 0x6DD | 0x70F | 0x180E | 0x200C..=0x200F
            | 0x2028..=0x202E | 0x2060..=0x2063 | 0x206A..=0x206F | 0x2FF0..=0x2FFB | 0x340 | 0x341
            | 0xD800..=0xDFFF | 0xE000..=0xF8FF | 0xFDD0..=0xFDEF | 0xFEFF | 0xFFF9..=0xFFFD
            | 0x1D173..=0x1D17A | 0xE0001 | 0xE0020..=0xE007F | 0xF0000..=0x10FFFF)
        || n & 0xFFFE == 0xFFFE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        for b in (0u8..=255).filter(|&b| b != 0x9F) {
            let c = pdfdoc_char(b);
            assert_eq!(pdfdoc_encode(&c.to_string()), Some(vec![b]), "{b:#x}");
        }
        assert_eq!(pdfdoc_encode("€é"), Some(vec![0xA0, 0xE9]));
        assert_eq!(pdfdoc_encode("密码"), None);
    }

    #[test]
    fn saslprep_rfc4013_examples() {
        assert_eq!(saslprep("I\u{AD}X").as_deref(), Some("IX"));
        assert_eq!(saslprep("user").as_deref(), Some("user"));
        assert_eq!(saslprep("USER").as_deref(), Some("USER"));
        assert_eq!(saslprep("\u{AA}").as_deref(), Some("a"));
        assert_eq!(saslprep("\u{2168}").as_deref(), Some("IX"));
        assert_eq!(saslprep("\u{7}"), None);
        assert_eq!(saslprep("\u{627}\u{31}"), None);
        // Allowed here, unlike in RFC 4013's Unicode 3.2 tables.
        assert_eq!(saslprep("🔐").as_deref(), Some("🔐"));
        assert_eq!(
            saslprep("\u{627}\u{644}").as_deref(),
            Some("\u{627}\u{644}")
        );
    }
}
