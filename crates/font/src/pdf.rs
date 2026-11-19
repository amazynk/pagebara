//! PDF glyph selection (ISO 32000-2 §9.6.5, §9.6.6, §9.7.4): from character codes (simple
//! fonts) or CIDs (composite fonts) to glyphs of an embedded or substituted font program.

use std::sync::{Arc, OnceLock};

use pagebara_syntax::{Dict, Document, ObjRef, Object};

use crate::data::{self, BaseEncoding};
use crate::fallback::{self, FontDb, FontRequest};
use crate::font::{Font, Format};
use crate::outline::{Matrix, OutlineSink};

/// The PDF font dictionary `/Subtype` (Type 0 fonts by their descendant).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PdfFontType {
    Type1,
    MMType1,
    TrueType,
    /// Type 0 with a `CIDFontType0` descendant.
    CidType0,
    /// Type 0 with a `CIDFontType2` descendant.
    CidType2,
}

impl PdfFontType {
    pub fn is_composite(self) -> bool {
        matches!(self, PdfFontType::CidType0 | PdfFontType::CidType2)
    }
}

/// A simple font's `/Encoding`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SimpleEncoding {
    /// The named encoding or `/BaseEncoding`.
    pub base: Option<BaseEncoding>,
    /// `/Differences` as (code, glyph name).
    pub differences: Vec<(u8, Box<[u8]>)>,
}

impl SimpleEncoding {
    fn difference(&self, code: u8) -> Option<&[u8]> {
        self.differences
            .iter()
            .rev()
            .find(|d| d.0 == code)
            .map(|d| &*d.1)
    }
}

/// `/CIDToGIDMap` of a `CIDFontType2` font.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum CidToGid {
    #[default]
    Identity,
    Map(Arc<[u16]>),
}

impl CidToGid {
    pub fn get(&self, cid: u32) -> u16 {
        match self {
            CidToGid::Identity => u16::try_from(cid).unwrap_or(0),
            CidToGid::Map(m) => m.get(cid as usize).copied().unwrap_or(0),
        }
    }

    /// From the stream data: big-endian GIDs indexed by CID.
    pub fn from_bytes(b: &[u8]) -> CidToGid {
        CidToGid::Map(
            b.as_chunks::<2>()
                .0
                .iter()
                .map(|&c| u16::from_be_bytes(c))
                .collect(),
        )
    }
}

/// What glyph selection needs from the font dictionaries.
#[derive(Clone, Debug, PartialEq)]
pub struct FontSpec {
    pub kind: PdfFontType,
    /// `/BaseFont` without a subset tag.
    pub base_font: Option<String>,
    /// Font descriptor `/Flags`.
    pub flags: u32,
    pub encoding: SimpleEncoding,
    /// Whether the dictionary has an `/Encoding` at all.
    pub has_encoding: bool,
    pub cid_to_gid: CidToGid,
    /// `/CIDSystemInfo` ordering of a composite font.
    pub ordering: Option<String>,
    /// `/FontWeight` of the descriptor.
    pub weight: Option<f32>,
    pub italic_angle: f32,
}

impl FontSpec {
    pub fn new(kind: PdfFontType) -> Self {
        FontSpec {
            kind,
            base_font: None,
            flags: 0,
            encoding: SimpleEncoding::default(),
            has_encoding: false,
            cid_to_gid: CidToGid::Identity,
            ordering: None,
            weight: None,
            italic_angle: 0.0,
        }
    }

    pub fn symbolic(&self) -> bool {
        self.flags & 4 != 0
    }
}

/// A selected glyph.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Glyph {
    pub gid: u16,
    /// The glyph name chosen through the encoding (simple fonts).
    pub name: Option<Box<str>>,
    /// Unicode guessed from the glyph name or the font's cmap.
    pub unicode: Option<Box<str>>,
}

/// A font program with the PDF glyph selection rules applied.
#[derive(Debug)]
pub struct PdfFont {
    font: Arc<Font>,
    spec: FontSpec,
    substitute: bool,
    simple: Option<Box<[Glyph]>>,
    reverse: OnceLock<std::collections::HashMap<u16, char>>,
}

impl PdfFont {
    /// Wraps an embedded font program (shared when several font dictionaries use it).
    pub fn new(font: impl Into<Arc<Font>>, spec: FontSpec) -> PdfFont {
        PdfFont::build(font.into(), spec, false)
    }

    /// Wraps a substitute font for a non-embedded font.
    pub fn substitute(font: impl Into<Arc<Font>>, spec: FontSpec) -> PdfFont {
        PdfFont::build(font.into(), spec, true)
    }

    fn build(font: Arc<Font>, spec: FontSpec, substitute: bool) -> PdfFont {
        let mut f = PdfFont {
            font,
            spec,
            substitute,
            simple: None,
            reverse: OnceLock::new(),
        };
        if !f.spec.kind.is_composite() {
            let table: Box<[Glyph]> = (0..=255u8).map(|c| f.select_simple(c)).collect();
            f.simple = Some(table);
        }
        f
    }

    pub fn font(&self) -> &Font {
        &self.font
    }

    /// The shared font program.
    pub fn program(&self) -> &Arc<Font> {
        &self.font
    }

    pub fn spec(&self) -> &FontSpec {
        &self.spec
    }

    pub fn is_substitute(&self) -> bool {
        self.substitute
    }

    /// Glyph space to text space.
    pub fn font_matrix(&self) -> Matrix {
        self.font.font_matrix()
    }

    /// The glyph of a code (simple fonts) or CID (composite fonts).
    pub fn glyph(&self, code: u32) -> u16 {
        match &self.simple {
            Some(t) => t.get(code as usize).map_or(0, |g| g.gid),
            None => self.cid_glyph(code),
        }
    }

    /// Full selection details of a simple font code.
    pub fn simple_glyph(&self, code: u8) -> Option<&Glyph> {
        self.simple.as_ref()?.get(usize::from(code))
    }

    /// A Unicode guess for a code or CID: the glyph name, else the font's Unicode cmap.
    pub fn unicode(&self, code: u32) -> Option<String> {
        if let Some(g) = self.simple.as_ref().and_then(|t| t.get(code as usize)) {
            return g.unicode.as_deref().map(str::to_owned);
        }
        self.reverse_unicode(self.cid_glyph(code)).map(String::from)
    }

    /// Draws the glyph of a code or CID in glyph space. False if it has no glyph.
    pub fn outline(&self, code: u32, sink: &mut dyn OutlineSink) -> bool {
        let gid = self.glyph(code);
        gid != 0 && self.font.outline(gid, sink)
    }

    /// The glyph for a character, for substitutes of composite fonts (via `/ToUnicode`).
    pub fn glyph_for_char(&self, c: char) -> Option<u16> {
        self.font.glyph_for_char(c)
    }

    /// Advance width of the selected glyph in text space units (per unit font size), for
    /// scaling substitute glyphs to the PDF widths.
    pub fn advance(&self, code: u32) -> Option<f32> {
        let w = self.font.advance(self.glyph(code))?;
        let (x, _) = self.font_matrix().apply(w, 0.0);
        let (o, _) = self.font_matrix().apply(0.0, 0.0);
        Some(x - o)
    }

    fn cid_glyph(&self, cid: u32) -> u16 {
        let Ok(cid16) = u16::try_from(cid) else {
            return 0;
        };
        if self.substitute {
            // Only a CID-keyed substitute of the same character collection maps CIDs; for
            // others the caller goes through Unicode (`glyph_for_char`).
            let same = self.font.as_cff().and_then(|c| c.ros()).is_some_and(|r| {
                self.spec.ordering.as_deref() == Some(r.ordering.as_str())
                    && r.ordering != "Identity"
            });
            return if same {
                self.font.glyph_by_cid(cid16).unwrap_or(0)
            } else {
                0
            };
        }
        match (self.spec.kind, self.font.format()) {
            (_, Format::Cff | Format::OpenTypeCff) => self.font.glyph_by_cid(cid16).unwrap_or(0),
            (_, Format::Type1) => cid16,
            _ => self.spec.cid_to_gid.get(cid),
        }
    }

    fn reverse_unicode(&self, gid: u16) -> Option<char> {
        let map = self.reverse.get_or_init(|| {
            let mut m = std::collections::HashMap::new();
            if let Some(s) = self.font.as_sfnt()
                && let Some(t) = s.unicode_subtable()
            {
                // Prefer unified characters over compatibility forms of the same glyph.
                let compat = |c: char| {
                    matches!(c, '\u{2E80}'..='\u{2FDF}' | '\u{E000}'..='\u{F8FF}' | '\u{F900}'..='\u{FAFF}' | '\u{2F800}'..='\u{2FA1F}')
                };
                s.cmap_for_each(t, &mut |c, g| {
                    if let Some(ch) = char::from_u32(c) {
                        let e = m.entry(g).or_insert(ch);
                        if compat(*e) && !compat(ch) {
                            *e = ch;
                        }
                    }
                });
            }
            m
        });
        map.get(&gid).copied()
    }

    /// The glyph name for `code` from `/Differences` or the base encoding; `None` means
    /// the font's built-in encoding applies.
    fn encoding_name(&self, code: u8) -> Option<&[u8]> {
        if let Some(n) = self.spec.encoding.difference(code) {
            return Some(n);
        }
        let base = self.spec.encoding.base.or_else(|| match self.spec.kind {
            PdfFontType::TrueType if !self.spec.symbolic() => Some(BaseEncoding::WinAnsi),
            PdfFontType::TrueType => None,
            // A non-embedded standard font uses its standard encoding.
            _ if self.substitute => Some(match self.standard_symbol() {
                Some(s) => s,
                None => BaseEncoding::Standard,
            }),
            _ => None,
        })?;
        base.glyph_name(code).map(str::as_bytes)
    }

    fn standard_symbol(&self) -> Option<BaseEncoding> {
        let n = self.spec.base_font.as_deref()?;
        if n.starts_with("Symbol") {
            Some(BaseEncoding::Symbol)
        } else if n.starts_with("ZapfDingbats") || n.starts_with("Dingbats") {
            Some(BaseEncoding::ZapfDingbats)
        } else {
            None
        }
    }

    fn select_simple(&self, code: u8) -> Glyph {
        let name = self.encoding_name(code);
        let mut g = Glyph {
            name: name.map(|n| String::from_utf8_lossy(n).into()),
            ..Glyph::default()
        };
        let mut uni = String::new();
        if let Some(n) = name
            && data::name_to_unicode(n, &mut uni)
        {
            g.unicode = Some(uni.as_str().into());
        }
        g.gid = match self.font.format() {
            Format::TrueType => self.truetype_glyph(code, name),
            _ => self.named_glyph(code, name),
        }
        .unwrap_or(0);
        if g.name.is_none()
            && g.gid != 0
            && let Some(n) = self.font.glyph_name(g.gid)
        {
            g.name = Some(n.into());
            uni.clear();
            if data::name_to_unicode(n.as_bytes(), &mut uni) {
                g.unicode = Some(uni.as_str().into());
            }
        }
        if g.unicode.is_none()
            && g.gid != 0
            && let Some(c) = self.reverse_unicode(g.gid)
            && !('\u{F000}'..='\u{F0FF}').contains(&c)
        {
            g.unicode = Some(c.to_string().into());
        }
        g
    }

    /// Type 1, CFF and OpenType-CFF programs: by glyph name, then the built-in encoding.
    fn named_glyph(&self, code: u8, name: Option<&[u8]>) -> Option<u16> {
        if let Some(n) = name {
            if let Some(g) = self.font.glyph_by_name(n) {
                return Some(g);
            }
            if let Some(c) = data::name_to_char(n)
                && let Some(g) = self.font.as_sfnt().and_then(|s| s.glyph_for_char(c))
            {
                return Some(g);
            }
        }
        if self.spec.encoding.difference(code).is_none()
            && let Some(g) = self.font.builtin_glyph(code)
        {
            return Some(g);
        }
        if self.substitute
            && let Some(c) = name.and_then(data::name_to_char)
        {
            return self.font.glyph_for_char(c);
        }
        None
    }

    /// TrueType programs (§9.6.5.4).
    fn truetype_glyph(&self, code: u8, name: Option<&[u8]>) -> Option<u16> {
        let s = self.font.as_sfnt()?;
        let c = u32::from(code);
        let has_31 = s.find_subtable(3, 1).is_some() || s.find_subtable(3, 10).is_some();
        let has_30 = s.find_subtable(3, 0).is_some();
        let has_10 = s.find_subtable(1, 0).is_some();
        let use_names = name.is_some()
            && (!self.spec.symbolic() || !self.spec.encoding.differences.is_empty() || !has_30);
        if use_names && let Some(n) = name {
            if has_31
                && let Some(ch) = data::name_to_char(n)
                && let Some(g) = s.glyph_for_char(ch)
            {
                return Some(g);
            }
            if has_10
                && let Some(t) = s.find_subtable(1, 0)
                && let Some(mc) = BaseEncoding::MacRoman.code_of(n)
                && let Some(g) = s.cmap_lookup(t, u32::from(mc))
            {
                return Some(g);
            }
            if let Some(g) = s.glyph_by_name(n) {
                return Some(g);
            }
        }
        if let Some(g) = self.font.builtin_glyph(code) {
            return Some(g);
        }
        if let Some(t) = s.unicode_subtable()
            && let Some(g) = s.cmap_lookup(t, c)
        {
            return Some(g);
        }
        if let Some(n) = name
            && let Some(g) = s.glyph_by_name(n)
        {
            return Some(g);
        }
        // Subset fonts without a cmap index glyphs by code.
        if s.cmap_subtables().is_empty() && c < u32::from(s.num_glyphs()) {
            return Some(code.into());
        }
        None
    }
}

/// A subset tag prefix (`ABCDEF+`) removed.
pub fn strip_subset_tag(name: &str) -> &str {
    match name.split_once('+') {
        Some((tag, rest)) if tag.len() == 6 && tag.bytes().all(|b| b.is_ascii_uppercase()) => rest,
        _ => name,
    }
}

/// Builds the [`FontSpec`] of a font dictionary.
pub fn spec_from_dict(doc: &Document, dict: &Dict) -> Option<FontSpec> {
    let subtype = doc.get_in(dict, b"Subtype");
    let (kind, desc_dict) = match subtype.as_name()? {
        b"Type1" => (PdfFontType::Type1, dict.clone()),
        b"MMType1" => (PdfFontType::MMType1, dict.clone()),
        b"TrueType" => (PdfFontType::TrueType, dict.clone()),
        b"Type0" => {
            let df = doc.get_in(dict, b"DescendantFonts");
            let first = df.as_array()?.first()?;
            let d = doc.resolve(first);
            let d = d.as_dict()?.clone();
            let kind = match doc.get_in(&d, b"Subtype").as_name() {
                Some(b"CIDFontType2") => PdfFontType::CidType2,
                _ => PdfFontType::CidType0,
            };
            (kind, d)
        }
        _ => return None,
    };
    let mut spec = FontSpec::new(kind);
    let name = |d: &Dict| {
        doc.get_in(d, b"BaseFont")
            .as_name()
            .map(|n| strip_subset_tag(&String::from_utf8_lossy(n)).to_owned())
    };
    spec.base_font = name(&desc_dict).or_else(|| name(dict));
    let fd = doc.get_in(&desc_dict, b"FontDescriptor");
    if let Some(fd) = fd.as_dict() {
        spec.flags = doc.get_in(fd, b"Flags").as_int().unwrap_or(0) as u32;
        spec.weight = doc.get_in(fd, b"FontWeight").as_f64().map(|v| v as f32);
        spec.italic_angle = doc.get_in(fd, b"ItalicAngle").as_f64().unwrap_or(0.0) as f32;
    }
    if kind.is_composite() {
        match &*doc.get_in(&desc_dict, b"CIDToGIDMap") {
            Object::Stream(s) => {
                if let Ok(d) = doc.decode(s) {
                    spec.cid_to_gid = CidToGid::from_bytes(&d.data);
                }
            }
            _ => spec.cid_to_gid = CidToGid::Identity,
        }
        let csi = doc.get_in(&desc_dict, b"CIDSystemInfo");
        if let Some(csi) = csi.as_dict() {
            spec.ordering = doc
                .get_in(csi, b"Ordering")
                .as_string()
                .map(|s| String::from_utf8_lossy(s).into_owned());
        }
    } else {
        let enc = doc.get_in(dict, b"Encoding");
        spec.has_encoding = !enc.is_null();
        match &*enc {
            Object::Name(n) => spec.encoding.base = BaseEncoding::from_name(n.as_bytes()),
            Object::Dict(d) => {
                spec.encoding.base = d.name(b"BaseEncoding").and_then(BaseEncoding::from_name);
                let diffs = doc.get_in(d, b"Differences");
                let mut code = 0i64;
                for o in diffs.as_array().unwrap_or_default() {
                    match o {
                        Object::Int(i) => code = *i,
                        Object::Real(r) => code = *r as i64,
                        Object::Name(n) => {
                            if let Ok(c) = u8::try_from(code) {
                                spec.encoding.differences.push((c, n.as_bytes().into()));
                            }
                            code += 1;
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    Some(spec)
}

/// The built-in encoding of a simple font's embedded program as glyph names indexed by
/// code (256 entries), for encodings that start from it (§9.6.5).
pub fn builtin_encoding(doc: &Document, dict: &Dict) -> Option<Vec<Option<String>>> {
    let f = embedded_program(doc, dict)?;
    let names = f.builtin_encoding()?;
    Some(names.iter().map(|n| n.map(str::to_owned)).collect())
}

const FONT_FILES: [&[u8]; 3] = [b"FontFile2", b"FontFile3", b"FontFile"];

/// The font descriptor of a font dictionary (of its descendant for Type 0 fonts).
pub fn font_descriptor(doc: &Document, dict: &Dict) -> Option<Dict> {
    let target = match doc.get_in(dict, b"Subtype").as_name() {
        Some(b"Type0") => {
            let df = doc.get_in(dict, b"DescendantFonts");
            let first = df.as_array()?.first()?;
            doc.resolve(first).as_dict()?.clone()
        }
        _ => dict.clone(),
    };
    doc.get_in(&target, b"FontDescriptor").as_dict().cloned()
}

/// The object holding the embedded program of a font dictionary, which identifies a
/// program shared by several fonts (for caching parsed programs).
pub fn font_file_ref(doc: &Document, dict: &Dict) -> Option<ObjRef> {
    let fd = font_descriptor(doc, dict)?;
    FONT_FILES.iter().find_map(|k| fd.get(k)?.as_objref())
}

/// The embedded font program of a font dictionary, if any and parsable.
pub fn embedded_program(doc: &Document, dict: &Dict) -> Option<Font> {
    let fd = font_descriptor(doc, dict)?;
    let fd = &fd;
    for key in FONT_FILES {
        if let Some(s) = doc.get_in(fd, key).as_stream()
            && let Ok(d) = doc.decode(s)
            && let Some(f) = Font::parse(d.data)
        {
            return Some(f);
        }
    }
    None
}

/// Loads a font dictionary: the embedded program, else a substitute from `db`.
pub fn load(doc: &Document, dict: &Dict, db: Option<&FontDb>) -> Option<PdfFont> {
    let spec = spec_from_dict(doc, dict)?;
    if let Some(f) = embedded_program(doc, dict) {
        return Some(PdfFont::new(f, spec));
    }
    let db = db?;
    let req = FontRequest::from_spec(&spec);
    let face = fallback::find(db, &req)?;
    let font = db.load(face)?;
    Some(PdfFont::substitute(font, spec))
}
