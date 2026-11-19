//! Fonts for field appearances: the form's own simple fonts from `/DR` (so filled text
//! keeps the form's typeface), the caller's [`TextFont`] for other scripts, and Helvetica
//! as the last resort. Also ZapfDingbats metrics for check marks.

use std::sync::Arc;

use pagebara_annot::{Glyph, Helvetica, TextFont};
use pagebara_syntax::{Dict, Document, Object};

/// Glyph names of codes 0x20..=0x7E and 0xA0..=0xFF in WinAnsiEncoding.
#[rustfmt::skip]
const LATIN: [&str; 95 + 96] = [
    "space", "exclam", "quotedbl", "numbersign", "dollar", "percent", "ampersand", "quotesingle",
    "parenleft", "parenright", "asterisk", "plus", "comma", "hyphen", "period", "slash", "zero",
    "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "colon", "semicolon",
    "less", "equal", "greater", "question", "at", "A", "B", "C", "D", "E", "F", "G", "H", "I",
    "J", "K", "L", "M", "N", "O", "P", "Q", "R", "S", "T", "U", "V", "W", "X", "Y", "Z",
    "bracketleft", "backslash", "bracketright", "asciicircum", "underscore", "grave", "a", "b",
    "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r", "s", "t",
    "u", "v", "w", "x", "y", "z", "braceleft", "bar", "braceright", "asciitilde",
    "nbspace", "exclamdown", "cent", "sterling", "currency", "yen", "brokenbar", "section",
    "dieresis", "copyright", "ordfeminine", "guillemotleft", "logicalnot", "sfthyphen",
    "registered", "macron", "degree", "plusminus", "twosuperior", "threesuperior", "acute", "mu",
    "paragraph", "periodcentered", "cedilla", "onesuperior", "ordmasculine", "guillemotright",
    "onequarter", "onehalf", "threequarters", "questiondown", "Agrave", "Aacute", "Acircumflex",
    "Atilde", "Adieresis", "Aring", "AE", "Ccedilla", "Egrave", "Eacute", "Ecircumflex",
    "Edieresis", "Igrave", "Iacute", "Icircumflex", "Idieresis", "Eth", "Ntilde", "Ograve",
    "Oacute", "Ocircumflex", "Otilde", "Odieresis", "multiply", "Oslash", "Ugrave", "Uacute",
    "Ucircumflex", "Udieresis", "Yacute", "Thorn", "germandbls", "agrave", "aacute",
    "acircumflex", "atilde", "adieresis", "aring", "ae", "ccedilla", "egrave", "eacute",
    "ecircumflex", "edieresis", "igrave", "iacute", "icircumflex", "idieresis", "eth", "ntilde",
    "ograve", "oacute", "ocircumflex", "otilde", "odieresis", "divide", "oslash", "ugrave",
    "uacute", "ucircumflex", "udieresis", "yacute", "thorn", "ydieresis",
];

/// Other glyph names of WinAnsiEncoding and PDFDocEncoding.
const EXTRA: &[(&str, char)] = &[
    ("Euro", '€'),
    ("quotesinglbase", '‚'),
    ("florin", 'ƒ'),
    ("quotedblbase", '„'),
    ("ellipsis", '…'),
    ("dagger", '†'),
    ("daggerdbl", '‡'),
    ("circumflex", 'ˆ'),
    ("perthousand", '‰'),
    ("Scaron", 'Š'),
    ("guilsinglleft", '‹'),
    ("OE", 'Œ'),
    ("Zcaron", 'Ž'),
    ("quoteleft", '‘'),
    ("quoteright", '’'),
    ("quotedblleft", '“'),
    ("quotedblright", '”'),
    ("bullet", '•'),
    ("endash", '–'),
    ("emdash", '—'),
    ("tilde", '˜'),
    ("trademark", '™'),
    ("scaron", 'š'),
    ("guilsinglright", '›'),
    ("oe", 'œ'),
    ("zcaron", 'ž'),
    ("Ydieresis", 'Ÿ'),
    ("breve", '˘'),
    ("caron", 'ˇ'),
    ("dotaccent", '˙'),
    ("hungarumlaut", '˝'),
    ("ogonek", '˛'),
    ("ring", '˚'),
    ("fraction", '⁄'),
    ("minus", '−'),
    ("fi", 'ﬁ'),
    ("fl", 'ﬂ'),
    ("Lslash", 'Ł'),
    ("lslash", 'ł'),
    ("dotlessi", 'ı'),
    ("space", ' '),
    ("hyphen", '-'),
    ("periodcentered", '·'),
];

/// The character a glyph name stands for (the common Latin names and `uniXXXX`).
pub(crate) fn glyph_char(name: &str) -> Option<char> {
    if let Some(i) = LATIN.iter().position(|&n| n == name) {
        let code = if i < 95 { 0x20 + i } else { 0xA0 + i - 95 };
        return char::from_u32(code as u32);
    }
    if let Some(&(_, c)) = EXTRA.iter().find(|(n, _)| *n == name) {
        return Some(c);
    }
    let hex = name.strip_prefix("uni").filter(|h| h.len() == 4)?;
    char::from_u32(u32::from_str_radix(hex, 16).ok()?)
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Metrics {
    Helvetica,
    Courier,
}

/// A simple (single-byte) font from the form's resources, measured by its `/Widths` or,
/// for unembedded standard fonts, built-in metrics.
#[derive(Debug, Clone)]
pub struct SimpleFont {
    name: String,
    object: Object,
    /// The character of each code.
    chars: Vec<(u8, char)>,
    first: u32,
    widths: Vec<f64>,
    builtin: Option<Metrics>,
    missing_width: f64,
    ascent: f64,
}

impl SimpleFont {
    /// The font `font` (a font dictionary or a reference to one) under resource `name`, if
    /// it is a simple font whose encoding we understand.
    pub fn from_resource(doc: &Document, name: &str, font: &Object) -> Option<SimpleFont> {
        let d = doc.resolve(font);
        let d = d.as_dict()?;
        match d.name(b"Subtype")? {
            b"Type1" | b"MMType1" | b"TrueType" => {}
            _ => return None,
        }
        let base = d.name(b"BaseFont").map(crate::text::name_text);
        let base = base.as_deref().unwrap_or("");
        // Subset prefixes (ABCDEF+Arial) do not change the family.
        let family = base.split_once('+').map_or(base, |(_, b)| b);
        let builtin = if family.starts_with("Courier") {
            Some(Metrics::Courier)
        } else if ["Helvetica", "Arial", "Times", "Liberation"]
            .iter()
            .any(|p| family.starts_with(p))
        {
            Some(Metrics::Helvetica)
        } else {
            None
        };
        if matches!(family, "Symbol" | "ZapfDingbats") {
            return None;
        }
        let desc = doc.get_in(d, b"FontDescriptor");
        let desc = desc.as_dict();
        let flags = desc.and_then(|f| f.int(b"Flags")).unwrap_or(32);
        let enc = doc.get_in(d, b"Encoding");
        let chars = encoding(doc, &enc, flags & 4 != 0)?;
        let widths: Vec<f64> = doc
            .get_in(d, b"Widths")
            .as_array()
            .unwrap_or_default()
            .iter()
            .map(|w| doc.resolve(w).as_f64().unwrap_or(0.0) / 1000.0)
            .collect();
        if widths.is_empty() && builtin.is_none() {
            return None;
        }
        let num = |k: &[u8]| desc.and_then(|f| doc.get_in(f, k).as_f64());
        let ascent = num(b"Ascent").filter(|a| *a > 100.0 && *a < 1500.0).map_or(
            if builtin == Some(Metrics::Courier) {
                0.629
            } else {
                0.718
            },
            |a| a / 1000.0,
        );
        Some(SimpleFont {
            name: name.to_owned(),
            object: font.clone(),
            chars,
            first: doc.get_in(d, b"FirstChar").as_int().unwrap_or(0).max(0) as u32,
            widths,
            builtin,
            missing_width: num(b"MissingWidth").map_or(0.5, |w| w / 1000.0),
            ascent,
        })
    }

    fn width(&self, code: u8, c: char) -> f64 {
        let i = u32::from(code).checked_sub(self.first);
        if let Some(&w) = i.and_then(|i| self.widths.get(i as usize))
            && w > 0.0
        {
            return w;
        }
        match self.builtin {
            Some(Metrics::Courier) => 0.6,
            Some(Metrics::Helvetica) => Helvetica.glyph(c).map_or(0.556, |g| g.width),
            None => self.missing_width,
        }
    }
}

/// Code-to-character pairs of a simple font's encoding: WinAnsi as the base for
/// non-symbolic fonts (StandardEncoding agrees on the letters and digits), with
/// `/Differences` applied.
fn encoding(doc: &Document, enc: &Object, symbolic: bool) -> Option<Vec<(u8, char)>> {
    let (base, diffs) = match enc {
        Object::Name(n) => (Some(n.as_bytes().to_vec()), None),
        Object::Dict(d) => (
            d.name(b"BaseEncoding").map(<[u8]>::to_vec),
            doc.get_in(d, b"Differences").as_array().map(<[_]>::to_vec),
        ),
        Object::Null => (None, None),
        _ => return None,
    };
    let mut chars: Vec<(u8, char)> = Vec::new();
    match base.as_deref() {
        Some(b"WinAnsiEncoding") => chars.extend(win_ansi()),
        Some(b"MacRomanEncoding") | Some(b"StandardEncoding") => {
            chars.extend((0x20u8..0x7F).map(|b| (b, char::from(b))));
            if base.as_deref() == Some(b"StandardEncoding") {
                chars.retain(|(b, _)| *b != 0x27 && *b != 0x60);
                chars.extend([(0x27, '’'), (0x60, '‘')]);
            }
        }
        // Symbolic fonts use their built-in encoding, which only differences can explain.
        None if symbolic => {}
        None => chars.extend(win_ansi()),
        Some(_) => return None,
    }
    let mut code: Option<u8> = None;
    for o in diffs.unwrap_or_default() {
        match o {
            Object::Int(i) => code = u8::try_from(i).ok(),
            Object::Name(n) => {
                let Some(c) = code else { continue };
                chars.retain(|(b, _)| *b != c);
                if let Some(ch) = glyph_char(&crate::text::name_text(n.as_bytes())) {
                    chars.insert(0, (c, ch));
                }
                code = c.checked_add(1);
            }
            _ => {}
        }
    }
    (!chars.is_empty()).then_some(chars)
}

fn win_ansi() -> impl Iterator<Item = (u8, char)> {
    (0x20u32..=0xFF)
        .filter_map(char::from_u32)
        .chain(EXTRA.iter().map(|(_, c)| *c))
        .filter_map(|c| Some((Helvetica::code(c)?, c)))
        .filter(|(b, _)| *b >= 0x20)
}

impl TextFont for SimpleFont {
    fn resource_name(&self) -> &str {
        &self.name
    }

    fn family(&self) -> &str {
        &self.name
    }

    fn glyph(&self, c: char) -> Option<Glyph> {
        // A non-breaking space shows as a space where the font lacks it.
        let find = |c: char| self.chars.iter().find(|(_, x)| *x == c).map(|(b, _)| *b);
        let code = find(c).or_else(|| (c == '\u{a0}').then(|| find(' ')).flatten())?;
        Some(Glyph {
            code: u32::from(code),
            len: 1,
            width: self.width(code, c),
            face: 0,
        })
    }

    fn fallback(&self) -> Glyph {
        self.glyph('?').unwrap_or(Glyph {
            code: u32::from(b'?'),
            len: 1,
            width: 0.556,
            face: 0,
        })
    }

    fn ascent(&self) -> f64 {
        self.ascent
    }

    fn resource(&self, _: &mut Document, _: &[(char, Glyph)]) -> Object {
        self.object.clone()
    }
}

/// A shareable text font.
pub type SharedFont = Arc<dyn TextFont + Send + Sync>;

/// The font named `name` in the resource dictionary `dr`, if usable.
pub(crate) fn from_dr(doc: &Document, dr: &Dict, name: &str) -> Option<SimpleFont> {
    let fonts = doc.get_in(dr, b"Font");
    let f = fonts.as_dict()?.get(name.as_bytes())?;
    SimpleFont::from_resource(doc, name, f)
}

/// Advance widths (size 1) of the ZapfDingbats glyphs used for check marks.
pub(crate) fn dingbat_width(c: u8) -> f64 {
    match c {
        b'4' => 0.846,
        b'l' => 0.791,
        b'8' => 0.776,
        b'u' => 0.788,
        b'n' => 0.761,
        b'H' => 0.816,
        _ => 0.8,
    }
}

/// The ZapfDingbats font dictionary.
pub(crate) fn dingbats() -> Object {
    let mut d = Dict::new();
    d.set(b"Type", Object::name(b"Font"));
    d.set(b"Subtype", Object::name(b"Type1"));
    d.set(b"BaseFont", Object::name(b"ZapfDingbats"));
    Object::Dict(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_names() {
        assert_eq!(glyph_char("A"), Some('A'));
        assert_eq!(glyph_char("ydieresis"), Some('ÿ'));
        assert_eq!(glyph_char("Euro"), Some('€'));
        assert_eq!(glyph_char("uni0416"), Some('Ж'));
        assert_eq!(glyph_char("nonsense"), None);
        assert_eq!(glyph_char("nbspace"), Some('\u{a0}'));
    }
}
