//! Text strings (ISO 32000-2 §7.9.2).

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

/// A name object's text: UTF-8 when valid, else Latin-1.
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
