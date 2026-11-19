//! Static tables: glyph names (Adobe Glyph List), the standard simple-font encodings, the
//! CFF standard strings and charsets and the Macintosh glyph order.

mod tables;

use tables as t;

/// The standard simple-font encodings of ISO 32000-2 Annex D.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

    /// The glyph name of `code`.
    pub fn glyph_name(self, code: u8) -> Option<&'static str> {
        let table = match self {
            BaseEncoding::Standard => &t::STANDARD,
            BaseEncoding::MacExpert => return expert_name(code),
            BaseEncoding::MacRoman => &t::MAC_ROMAN,
            BaseEncoding::WinAnsi => &t::WIN_ANSI,
            BaseEncoding::PdfDoc => &t::PDF_DOC,
            BaseEncoding::Symbol => &t::SYMBOL,
            BaseEncoding::ZapfDingbats => &t::ZAPF_DINGBATS,
        };
        agl_name(table[usize::from(code)])
    }

    /// The code of glyph `name`, if the encoding has it.
    pub fn code_of(self, name: &[u8]) -> Option<u8> {
        (0..=255u8).find(|&c| self.glyph_name(c).is_some_and(|n| n.as_bytes() == name))
    }
}

fn agl_name(id: u16) -> Option<&'static str> {
    let i = usize::from(id);
    let (a, b) = (*t::OFFSETS.get(i)?, *t::OFFSETS.get(i + 1)?);
    t::NAMES.get(a as usize..b as usize)
}

fn agl_id(name: &[u8]) -> Option<usize> {
    let (mut lo, mut hi) = (0, t::UNICODE.len());
    while lo < hi {
        let mid = (lo + hi) / 2;
        match agl_name(mid as u16)?.as_bytes().cmp(name) {
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Greater => hi = mid,
            std::cmp::Ordering::Equal => return Some(mid),
        }
    }
    None
}

/// A glyph name for a Unicode scalar from the glyph list, else `uniXXXX`/`uXXXXXX`.
pub fn unicode_to_name(c: char) -> String {
    let u = c as u32;
    if u <= 0xFFFF
        && let Some(i) = t::UNICODE.iter().position(|&x| u32::from(x) == u)
        && let Some(n) = agl_name(i as u16)
    {
        return n.to_owned();
    }
    if u <= 0xFFFF {
        format!("uni{u:04X}")
    } else {
        format!("u{u:06X}")
    }
}

/// Unicode of a glyph name: list entries, `uniXXXX…`, `uXXXX[XX]`, suffixes (`a.sc`) and
/// ligatures (`f_f_i`). Appends to `out`.
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

/// The single character a glyph name stands for.
pub fn name_to_char(name: &[u8]) -> Option<char> {
    if let Some(i) = agl_id(name) {
        return char::from_u32(u32::from(t::UNICODE[i]));
    }
    let mut s = String::new();
    name_to_unicode(name, &mut s);
    let mut it = s.chars();
    it.next().filter(|_| it.next().is_none())
}

fn component(n: &[u8], out: &mut String) -> bool {
    if let Some(i) = agl_id(n)
        && let Some(c) = char::from_u32(u32::from(t::UNICODE[i]))
    {
        out.push(c);
        return true;
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
        out.extend(char::decode_utf16(units).map(|c| c.unwrap_or('\u{FFFD}')));
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

/// CFF standard string `sid` (0..391).
pub fn cff_standard_string(sid: u16) -> Option<&'static str> {
    let i = usize::from(sid);
    let (a, b) = (
        *t::CFF_STRINGS_OFFSETS.get(i)?,
        *t::CFF_STRINGS_OFFSETS.get(i + 1)?,
    );
    t::CFF_STRINGS.get(usize::from(a)..usize::from(b))
}

pub const CFF_STANDARD_STRINGS: u16 = 391;

pub fn cff_expert_charset() -> &'static [u16] {
    &t::EXPERT_CHARSET
}

pub fn cff_expert_subset_charset() -> &'static [u16] {
    &t::EXPERT_SUBSET_CHARSET
}

/// The Macintosh standard order glyph `i` (post table formats 1 and 2).
pub fn mac_glyph_name(i: u16) -> Option<&'static str> {
    let i = usize::from(i);
    let (a, b) = (
        *t::MAC_GLYPHS_OFFSETS.get(i)?,
        *t::MAC_GLYPHS_OFFSETS.get(i + 1)?,
    );
    t::MAC_GLYPHS.get(usize::from(a)..usize::from(b))
}

/// The CFF Expert encoding (TN 5176 appendix B), also used for MacExpertEncoding.
fn expert_name(code: u8) -> Option<&'static str> {
    const LOW: [&str; 95] = [
        "space",
        "exclamsmall",
        "Hungarumlautsmall",
        "",
        "dollaroldstyle",
        "dollarsuperior",
        "ampersandsmall",
        "Acutesmall",
        "parenleftsuperior",
        "parenrightsuperior",
        "twodotenleader",
        "onedotenleader",
        "comma",
        "hyphen",
        "period",
        "fraction",
        "zerooldstyle",
        "oneoldstyle",
        "twooldstyle",
        "threeoldstyle",
        "fouroldstyle",
        "fiveoldstyle",
        "sixoldstyle",
        "sevenoldstyle",
        "eightoldstyle",
        "nineoldstyle",
        "colon",
        "semicolon",
        "commasuperior",
        "threequartersemdash",
        "periodsuperior",
        "questionsmall",
        "",
        "asuperior",
        "bsuperior",
        "centsuperior",
        "dsuperior",
        "esuperior",
        "",
        "",
        "",
        "isuperior",
        "",
        "",
        "lsuperior",
        "msuperior",
        "nsuperior",
        "osuperior",
        "",
        "",
        "rsuperior",
        "ssuperior",
        "tsuperior",
        "",
        "ff",
        "fi",
        "fl",
        "ffi",
        "ffl",
        "parenleftinferior",
        "",
        "parenrightinferior",
        "Circumflexsmall",
        "hyphensuperior",
        "Gravesmall",
        "Asmall",
        "Bsmall",
        "Csmall",
        "Dsmall",
        "Esmall",
        "Fsmall",
        "Gsmall",
        "Hsmall",
        "Ismall",
        "Jsmall",
        "Ksmall",
        "Lsmall",
        "Msmall",
        "Nsmall",
        "Osmall",
        "Psmall",
        "Qsmall",
        "Rsmall",
        "Ssmall",
        "Tsmall",
        "Usmall",
        "Vsmall",
        "Wsmall",
        "Xsmall",
        "Ysmall",
        "Zsmall",
        "colonmonetary",
        "onefitted",
        "rupiah",
        "Tildesmall",
    ];
    const HIGH: [&str; 95] = [
        "exclamdownsmall",
        "centoldstyle",
        "Lslashsmall",
        "",
        "",
        "Scaronsmall",
        "Zcaronsmall",
        "Dieresissmall",
        "Brevesmall",
        "Caronsmall",
        "",
        "Dotaccentsmall",
        "",
        "",
        "Macronsmall",
        "",
        "",
        "figuredash",
        "hypheninferior",
        "",
        "",
        "Ogoneksmall",
        "Ringsmall",
        "Cedillasmall",
        "",
        "",
        "",
        "onequarter",
        "onehalf",
        "threequarters",
        "questiondownsmall",
        "oneeighth",
        "threeeighths",
        "fiveeighths",
        "seveneighths",
        "onethird",
        "twothirds",
        "",
        "",
        "zerosuperior",
        "onesuperior",
        "twosuperior",
        "threesuperior",
        "foursuperior",
        "fivesuperior",
        "sixsuperior",
        "sevensuperior",
        "eightsuperior",
        "ninesuperior",
        "zeroinferior",
        "oneinferior",
        "twoinferior",
        "threeinferior",
        "fourinferior",
        "fiveinferior",
        "sixinferior",
        "seveninferior",
        "eightinferior",
        "nineinferior",
        "centinferior",
        "dollarinferior",
        "periodinferior",
        "commainferior",
        "Agravesmall",
        "Aacutesmall",
        "Acircumflexsmall",
        "Atildesmall",
        "Adieresissmall",
        "Aringsmall",
        "AEsmall",
        "Ccedillasmall",
        "Egravesmall",
        "Eacutesmall",
        "Ecircumflexsmall",
        "Edieresissmall",
        "Igravesmall",
        "Iacutesmall",
        "Icircumflexsmall",
        "Idieresissmall",
        "Ethsmall",
        "Ntildesmall",
        "Ogravesmall",
        "Oacutesmall",
        "Ocircumflexsmall",
        "Otildesmall",
        "Odieresissmall",
        "OEsmall",
        "Oslashsmall",
        "Ugravesmall",
        "Uacutesmall",
        "Ucircumflexsmall",
        "Udieresissmall",
        "Yacutesmall",
        "Thornsmall",
        "Ydieresissmall",
    ];
    let n = match code {
        32..=126 => LOW[usize::from(code - 32)],
        161..=255 => HIGH[usize::from(code - 161)],
        _ => "",
    };
    (!n.is_empty()).then_some(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables() {
        assert_eq!(BaseEncoding::WinAnsi.glyph_name(0x80), Some("Euro"));
        assert_eq!(BaseEncoding::Standard.glyph_name(0x27), Some("quoteright"));
        assert_eq!(BaseEncoding::Standard.glyph_name(0x80), None);
        assert_eq!(BaseEncoding::MacExpert.glyph_name(0x61), Some("Asmall"));
        assert_eq!(
            BaseEncoding::MacExpert.glyph_name(0xFF),
            Some("Ydieresissmall")
        );
        assert_eq!(BaseEncoding::WinAnsi.code_of(b"Euro"), Some(0x80));
        assert_eq!(cff_standard_string(0), Some(".notdef"));
        assert_eq!(cff_standard_string(390), Some("Semibold"));
        assert_eq!(mac_glyph_name(3), Some("space"));
        assert_eq!(mac_glyph_name(257), Some("dcroat"));
        assert_eq!(mac_glyph_name(258), None);
        assert_eq!(name_to_char(b"eacute"), Some('é'));
        assert_eq!(name_to_char(b"uni20AC"), Some('€'));
        assert_eq!(name_to_char(b"f_i"), None);
        assert_eq!(unicode_to_name('A'), "A");
        assert_eq!(unicode_to_name('\u{E000}'), "uniE000");
        let mut s = String::new();
        assert!(name_to_unicode(b"f_f_i", &mut s));
        assert_eq!(s, "ffi");
    }
}
