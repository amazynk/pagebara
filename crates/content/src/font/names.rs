//! Glyph names (Adobe Glyph List), the standard simple-font encodings and Unicode for
//! glyph names.

use super::data;

/// Index of a glyph name in the embedded list.
pub type GlyphId = u16;
pub const NO_GLYPH: GlyphId = u16::MAX;

pub fn glyph_name(id: GlyphId) -> &'static str {
    let i = usize::from(id);
    &data::NAMES[data::OFFSETS[i] as usize..data::OFFSETS[i + 1] as usize]
}

pub fn glyph_id(name: &[u8]) -> Option<GlyphId> {
    let n = data::UNICODE.len();
    let (mut lo, mut hi) = (0, n);
    while lo < hi {
        let mid = (lo + hi) / 2;
        match glyph_name(mid as GlyphId).as_bytes().cmp(name) {
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Greater => hi = mid,
            std::cmp::Ordering::Equal => return Some(mid as GlyphId),
        }
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BaseEncoding {
    Standard,
    MacRoman,
    WinAnsi,
    PdfDoc,
    MacExpert,
    Symbol,
    ZapfDingbats,
}

impl BaseEncoding {
    pub fn from_name(n: &[u8]) -> Option<Self> {
        Some(match n {
            b"StandardEncoding" => BaseEncoding::Standard,
            b"MacRomanEncoding" => BaseEncoding::MacRoman,
            b"WinAnsiEncoding" => BaseEncoding::WinAnsi,
            b"PDFDocEncoding" => BaseEncoding::PdfDoc,
            b"MacExpertEncoding" => BaseEncoding::MacExpert,
            _ => return None,
        })
    }

    pub fn table(self) -> &'static [GlyphId; 256] {
        match self {
            BaseEncoding::Standard => &data::STANDARD,
            BaseEncoding::MacExpert => &data::MAC_EXPERT,
            BaseEncoding::MacRoman => &data::MAC_ROMAN,
            BaseEncoding::WinAnsi => &data::WIN_ANSI,
            BaseEncoding::PdfDoc => &data::PDF_DOC,
            BaseEncoding::Symbol => &data::SYMBOL,
            BaseEncoding::ZapfDingbats => &data::ZAPF_DINGBATS,
        }
    }
}

/// Unicode of a glyph name: list entries, `uniXXXX…`, `uXXXX[XX]`, suffixes (`a.sc`) and
/// ligatures (`f_f_i`).
pub fn name_to_unicode(name: &[u8], out: &mut String) -> bool {
    let base = match name.iter().position(|&b| b == b'.') {
        Some(0) => return false,
        Some(p) => &name[..p],
        None => name,
    };
    let start = out.len();
    for part in base.split(|&b| b == b'_') {
        if !component(part, out) {
            out.truncate(start);
            return false;
        }
    }
    out.len() > start
}

fn component(n: &[u8], out: &mut String) -> bool {
    if let Some(id) = glyph_id(n) {
        let u = data::UNICODE[usize::from(id)];
        if let Some(c) = char::from_u32(u32::from(u)) {
            out.push(c);
            return true;
        }
    }
    let hex = |s: &[u8]| {
        std::str::from_utf8(s)
            .ok()
            .filter(|s| s.bytes().all(|b| b.is_ascii_hexdigit()))
            .and_then(|s| u32::from_str_radix(s, 16).ok())
    };
    if let Some(rest) = n.strip_prefix(b"uni")
        && !rest.is_empty()
        && rest.len() % 4 == 0
    {
        let mut units = Vec::with_capacity(rest.len() / 4);
        for c in rest.chunks(4) {
            let Some(u) = hex(c) else { return false };
            units.push(u as u16);
        }
        let s: String = char::decode_utf16(units)
            .map(|c| c.unwrap_or('\u{FFFD}'))
            .collect();
        out.push_str(&s);
        return true;
    }
    if let Some(rest) = n.strip_prefix(b"u")
        && (4..=6).contains(&rest.len())
        && let Some(c) = hex(rest).and_then(char::from_u32)
    {
        out.push(c);
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uni(n: &str) -> Option<String> {
        let mut s = String::new();
        name_to_unicode(n.as_bytes(), &mut s).then_some(s)
    }

    #[test]
    fn names() {
        assert_eq!(glyph_name(glyph_id(b"Aacute").unwrap()), "Aacute");
        assert_eq!(glyph_id(b"nonexistent-glyph"), None);
        assert_eq!(uni("A").as_deref(), Some("A"));
        assert_eq!(uni("eacute").as_deref(), Some("é"));
        assert_eq!(uni("f_i").as_deref(), Some("fi"));
        assert_eq!(uni("uni00410042").as_deref(), Some("AB"));
        assert_eq!(uni("u1F600").as_deref(), Some("😀"));
        assert_eq!(uni("a.sc").as_deref(), Some("a"));
        assert_eq!(uni("a20").as_deref(), Some("✔"));
        assert_eq!(uni("g123"), None);
        assert_eq!(uni(".notdef"), None);
    }

    #[test]
    fn encodings() {
        let name = |e: BaseEncoding, c: u8| glyph_name(e.table()[usize::from(c)]);
        assert_eq!(name(BaseEncoding::WinAnsi, 0x80), "Euro");
        assert_eq!(name(BaseEncoding::Standard, 0x27), "quoteright");
        assert_eq!(name(BaseEncoding::MacRoman, 0x8E), "eacute");
        assert_eq!(name(BaseEncoding::Symbol, 0x61), "alpha");
        assert_eq!(name(BaseEncoding::PdfDoc, 0xA0), "Euro");
        assert_eq!(BaseEncoding::Standard.table()[0x80], NO_GLYPH);
        assert_eq!(name(BaseEncoding::MacExpert, 0x61), "Asmall");
        assert_eq!(name(BaseEncoding::MacExpert, 0x30), "zerooldstyle");
        assert_eq!(name(BaseEncoding::MacExpert, 0x56), "ff");
        assert_eq!(BaseEncoding::MacExpert.table()[0x41], NO_GLYPH);
    }

    #[test]
    fn expert_glyphs_read_plain() {
        assert_eq!(uni("Asmall").as_deref(), Some("a"));
        assert_eq!(uni("Aacutesmall").as_deref(), Some("á"));
        assert_eq!(uni("sevenoldstyle").as_deref(), Some("7"));
        assert_eq!(uni("dollarsuperior").as_deref(), Some("$"));
        assert_eq!(uni("onesuperior").as_deref(), Some("¹"));
    }
}
