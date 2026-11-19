//! PDF text strings (§7.9.2.2): UTF-16BE or UTF-8 with a byte order mark, else
//! PDFDocEncoding.

use pagebara_syntax::text::pdfdoc_char;

pub fn decode(b: &[u8]) -> String {
    let s = if let Some(utf16) = b.strip_prefix(&[0xFE, 0xFF]) {
        let units = utf16
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| u16::from_be_bytes(c));
        char::decode_utf16(units)
            .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect()
    } else if let Some(utf8) = b.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8_lossy(utf8).into_owned()
    } else {
        b.iter().map(|&c| pdfdoc_char(c)).collect()
    };
    // Language escapes (U+001B … U+001B) and control characters are not displayable.
    let mut out = String::with_capacity(s.len());
    let mut in_escape = false;
    for c in s.chars() {
        match c {
            '\u{1B}' => in_escape = !in_escape,
            _ if in_escape => {}
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::decode;

    #[test]
    fn encodings() {
        assert_eq!(decode(b"Plain \x8D"), "Plain \u{201C}");
        assert_eq!(decode(&[0xFE, 0xFF, 0, 0xC9, 0, b't']), "Ét");
        assert_eq!(decode(b"\xEF\xBB\xBFd\xC3\xA9j\xC3\xA0"), "déjà");
        assert_eq!(
            decode(&[0xFE, 0xFF, 0, 0x1B, 0, b'e', 0, 0x1B, 0, b'x']),
            "x"
        );
        assert_eq!(decode(b"a\nb"), "a b");
    }
}
