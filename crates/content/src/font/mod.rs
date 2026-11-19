//! What the interpreter and text extraction need of fonts (§9.6–9.10): splitting strings
//! into codes, glyph widths and vertical metrics, and the Unicode of codes. Glyph outlines
//! belong to the renderer.

pub mod cmap;
mod data;
pub mod names;
pub mod program;
pub mod std14;

use std::sync::{Arc, OnceLock};

use pagebara_syntax::{Dict, Document, Name, Object};

use crate::geom::{Matrix, Point, Rect};
use cmap::{CMap, Ordering};
use data::Std14;
use names::{BaseEncoding, GlyphId, NO_GLYPH};
pub use program::{FontProgram, ProgramLoader, embedded_programs};

/// Composite fonts' code tables read from embedded CMaps are capped at this nesting.
const MAX_USECMAP: u32 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontKind {
    Type1,
    TrueType,
    Type3,
    /// Type 0 with a `CIDFontType0` (CFF) descendant.
    Cid0,
    /// Type 0 with a `CIDFontType2` (TrueType) descendant.
    Cid2,
}

#[derive(Debug)]
pub struct Font {
    pub kind: FontKind,
    pub base_font: Option<Name>,
    /// Glyph space to text space: 1/1000 except for Type 3 fonts.
    pub font_matrix: Matrix,
    /// Ascent and descent in text space units per unit of font size.
    pub ascent: f32,
    pub descent: f32,
    /// Font bounding box in glyph space.
    pub bbox: Rect,
    /// `/Flags` of the font descriptor.
    pub flags: u32,
    pub vertical: bool,
    /// The font dictionary, for the renderer (font programs, Type 3 `/CharProcs`).
    pub dict: Dict,
    /// The CID font dictionary of a Type 0 font.
    pub descendant: Option<Dict>,
    /// The standard font this font is or falls back to (non-embedded simple fonts).
    pub standard: Option<&'static str>,
    /// The character collection of a composite font's CIDs.
    pub ordering: Ordering,
    /// The font program's own facts, when a [`ProgramLoader`] supplied them.
    pub program: Option<Arc<dyn FontProgram>>,
    codes: Option<Arc<CMap>>,
    widths: Widths,
    encoding: Option<Box<Encoding>>,
    to_unicode: Option<Object>,
    unicode: OnceLock<Option<CMap>>,
    /// The Unicode of every code of a simple font, worked out on first use.
    simple_text: OnceLock<CodeText>,
}

/// Text of the 256 codes of a simple font: code `c` maps to `text[ends[c]..ends[c + 1]]`.
#[derive(Debug)]
struct CodeText {
    text: String,
    ends: [u32; 257],
    mapped: [bool; 256],
}

#[derive(Debug)]
enum Widths {
    Simple {
        first: u32,
        widths: Box<[f32]>,
        missing: f32,
        std14: Option<&'static Std14>,
    },
    Cid {
        ranges: Vec<(u32, u32, f32)>,
        default: f32,
        vertical: Vec<(u32, u32, [f32; 3])>,
        vertical_default: [f32; 2],
    },
}

/// Glyph names of the 256 codes of a simple font.
#[derive(Debug)]
struct Encoding {
    glyphs: [GlyphId; 256],
    /// `/Differences` names that are not in the glyph list.
    custom: Vec<(u8, Box<[u8]>)>,
}

impl Encoding {
    fn set(&mut self, code: u8, name: &[u8]) {
        self.custom.retain(|(c, _)| *c != code);
        match names::glyph_id(name) {
            Some(g) => self.glyphs[usize::from(code)] = g,
            None => {
                self.glyphs[usize::from(code)] = NO_GLYPH;
                self.custom.push((code, name.into()));
            }
        }
    }

    fn name(&self, code: u8) -> Option<&[u8]> {
        if let Some((_, n)) = self.custom.iter().find(|(c, _)| *c == code) {
            return Some(n);
        }
        match self.glyphs[usize::from(code)] {
            NO_GLYPH => None,
            g => Some(names::glyph_name(g).as_bytes()),
        }
    }
}

impl Font {
    /// Reads a font dictionary. Never fails: missing parts fall back to defaults.
    pub fn load(doc: &Document, dict: &Dict) -> Font {
        Font::load_with(doc, dict, None)
    }

    /// [`Font::load`] with the font program's facts from `programs`.
    pub fn load_with(doc: &Document, dict: &Dict, programs: Option<&ProgramLoader>) -> Font {
        let kind = match dict.name(b"Subtype") {
            Some(b"TrueType") => FontKind::TrueType,
            Some(b"Type3") => FontKind::Type3,
            Some(b"Type0") => FontKind::Cid0,
            _ => FontKind::Type1,
        };
        let mut f = Font {
            kind,
            base_font: dict
                .get(b"BaseFont")
                .and_then(Object::as_name)
                .map(Name::new),
            font_matrix: Matrix::scale(0.001, 0.001),
            ascent: 0.0,
            descent: 0.0,
            bbox: Rect::default(),
            flags: 0,
            vertical: false,
            dict: dict.clone(),
            descendant: None,
            standard: None,
            ordering: Ordering::Other,
            program: programs.and_then(|p| p(doc, dict)),
            codes: None,
            widths: Widths::Simple {
                first: 0,
                widths: Box::default(),
                missing: 0.0,
                std14: None,
            },
            encoding: None,
            to_unicode: dict.get(b"ToUnicode").cloned(),
            unicode: OnceLock::new(),
            simple_text: OnceLock::new(),
        };
        if kind == FontKind::Cid0 {
            f.load_composite(doc, dict);
        } else {
            f.load_simple(doc, dict);
        }
        f
    }

    fn load_simple(&mut self, doc: &Document, dict: &Dict) {
        let desc = doc.get_in(dict, b"FontDescriptor");
        let desc = desc.as_dict();
        self.flags = desc.and_then(|d| d.int(b"Flags")).unwrap_or(0) as u32;
        if self.kind == FontKind::Type3 {
            self.font_matrix =
                Matrix::from_object(&doc.get_in(dict, b"FontMatrix")).unwrap_or(self.font_matrix);
        }
        let base = self.base_font.as_ref().map_or(&[][..], |n| n.as_bytes());
        let embedded = desc.is_some_and(|d| {
            d.contains(b"FontFile") || d.contains(b"FontFile2") || d.contains(b"FontFile3")
        });
        let widths = doc.get_in(dict, b"Widths");
        let widths: Box<[f32]> = widths
            .as_array()
            .unwrap_or_default()
            .iter()
            .map(|w| doc.resolve(w).as_f64().unwrap_or(0.0) as f32)
            .collect();
        let mut std14 = (self.kind != FontKind::Type3)
            .then(|| std14::lookup(base))
            .flatten();
        if std14.is_none() && widths.is_empty() && !embedded && self.kind != FontKind::Type3 {
            std14 = Some(std14::fallback(base, self.flags));
        }
        self.standard = std14.map(|s| s.name);
        let missing = desc
            .and_then(|d| d.get(b"MissingWidth"))
            .and_then(|o| doc.resolve(o).as_f64())
            .unwrap_or(0.0) as f32;
        self.widths = Widths::Simple {
            first: doc
                .get_in(dict, b"FirstChar")
                .as_int()
                .unwrap_or(0)
                .clamp(0, 255) as u32,
            widths,
            missing,
            std14,
        };
        self.encoding = Some(Box::new(self.simple_encoding(doc, dict, std14)));
        self.metrics(doc, desc, std14);
    }

    fn simple_encoding(&self, doc: &Document, dict: &Dict, std14: Option<&Std14>) -> Encoding {
        let symbolic = self.flags & 4 != 0;
        let builtin = match std14.map(|s| s.name) {
            Some("Symbol") => Some(BaseEncoding::Symbol),
            Some("ZapfDingbats") => Some(BaseEncoding::ZapfDingbats),
            _ => None,
        };
        let enc = doc.get_in(dict, b"Encoding");
        let (base_name, diffs) = match &*enc {
            Object::Name(n) => (Some(n.as_bytes()), None),
            Object::Dict(d) => (d.name(b"BaseEncoding"), d.get(b"Differences")),
            _ => (None, None),
        };
        let base = base_name
            .and_then(BaseEncoding::from_name)
            .or(builtin)
            .unwrap_or(match self.kind {
                FontKind::TrueType if !symbolic => BaseEncoding::WinAnsi,
                _ => BaseEncoding::Standard,
            });
        let mut e = Encoding {
            glyphs: *base.table(),
            custom: Vec::new(),
        };
        // Symbolic TrueType fonts without an encoding index their cmap by code.
        if self.kind == FontKind::TrueType && symbolic && base_name.is_none() && diffs.is_none() {
            e.glyphs = [NO_GLYPH; 256];
        }
        // Without a base encoding, embedded programs (and symbolic TrueType ones) bring their own.
        if let Some(p) = &self.program
            && base_name.is_none()
            && (self.kind != FontKind::TrueType || symbolic)
        {
            for c in 0..=255u8 {
                if let Some(n) = p.glyph_name(u32::from(c)) {
                    e.set(c, n);
                }
            }
        }
        if let Some(diffs) = diffs {
            let diffs = doc.resolve(diffs);
            let mut code = 0i64;
            for o in diffs.as_array().unwrap_or_default() {
                match o {
                    Object::Int(_) | Object::Real(_) => code = o.as_f64().unwrap_or(0.0) as i64,
                    Object::Name(n) => {
                        if let Ok(c) = u8::try_from(code) {
                            e.set(c, n.as_bytes());
                        }
                        code += 1;
                    }
                    _ => {}
                }
            }
        }
        e
    }

    fn load_composite(&mut self, doc: &Document, dict: &Dict) {
        let enc = doc.get_in(dict, b"Encoding");
        let cmap = match &*enc {
            Object::Name(n) => CMap::predefined(n.as_bytes()),
            Object::Stream(s) => Some(Arc::new(embedded_cmap(doc, s, 0))),
            _ => None,
        }
        .or_else(|| CMap::predefined(b"Identity-H"))
        .unwrap_or_default();
        self.vertical = cmap.vertical;
        self.ordering = cmap.ordering;
        self.codes = Some(cmap);
        let df = doc.get_in(dict, b"DescendantFonts");
        let cid_font = df
            .as_array()
            .and_then(|a| a.first())
            .map(|o| doc.resolve(o))
            .and_then(|o| o.as_dict().cloned())
            .unwrap_or_default();
        if cid_font.name(b"Subtype") == Some(b"CIDFontType2") {
            self.kind = FontKind::Cid2;
        }
        let info = doc.get_in(&cid_font, b"CIDSystemInfo");
        if let Some(info) = info.as_dict()
            && doc.get_in(info, b"Registry").as_string() == Some(b"Adobe")
            && let Some(o) = doc.get_in(info, b"Ordering").as_string()
            && Ordering::from_name(o) != Ordering::Other
        {
            self.ordering = Ordering::from_name(o);
        }
        let desc = doc.get_in(&cid_font, b"FontDescriptor");
        let desc = desc.as_dict();
        self.flags = desc.and_then(|d| d.int(b"Flags")).unwrap_or(0) as u32;
        let num = |o: &Object| doc.resolve(o).as_f64().map(|v| v as f32);
        let default = cid_font.get(b"DW").and_then(num).unwrap_or(1000.0);
        let w = doc.get_in(&cid_font, b"W");
        let ranges = cid_widths::<1>(doc, w.as_array().unwrap_or_default())
            .into_iter()
            .map(|(lo, hi, v)| (lo, hi, v[0]))
            .collect();
        let dw2 = doc.get_in(&cid_font, b"DW2");
        let vertical_default = match dw2.as_array() {
            Some([vy, w1]) => [num(vy).unwrap_or(880.0), num(w1).unwrap_or(-1000.0)],
            _ => [880.0, -1000.0],
        };
        let w2 = doc.get_in(&cid_font, b"W2");
        let vertical = cid_widths::<3>(doc, w2.as_array().unwrap_or_default());
        self.widths = Widths::Cid {
            ranges,
            default,
            vertical,
            vertical_default,
        };
        self.metrics(doc, desc, None);
        self.descendant = Some(cid_font);
    }

    fn metrics(&mut self, doc: &Document, desc: Option<&Dict>, std14: Option<&Std14>) {
        let num = |k: &[u8]| {
            desc.and_then(|d| d.get(k))
                .and_then(|o| doc.resolve(o).as_f64())
                .map(|v| v as f32)
        };
        let bbox = if self.kind == FontKind::Type3 {
            Rect::from_object(&doc.get_in(&self.dict, b"FontBBox"))
        } else {
            desc.and_then(|d| d.get(b"FontBBox"))
                .and_then(|o| Rect::from_object(&doc.resolve(o)))
        };
        let std_bbox = std14.map(|s| {
            let b = s.bbox.map(f32::from);
            Rect::new(b[0], b[1], b[2], b[3])
        });
        self.bbox = bbox
            .or(std_bbox)
            .unwrap_or(Rect::new(0.0, -200.0, 1000.0, 800.0));
        let fm = self.font_matrix;
        let (mut asc, mut desc) = match (num(b"Ascent"), num(b"Descent")) {
            (Some(a), Some(d)) if a != 0.0 && a > d => (a, d),
            _ => match std14 {
                Some(s) if bbox.is_none() => (f32::from(s.ascent), f32::from(s.descent)),
                _ => (self.bbox.y1, self.bbox.y0),
            },
        };
        if asc <= desc {
            (asc, desc) = (800.0, -200.0);
        }
        self.ascent = asc * fm.d.abs().max(fm.b.abs());
        self.descent = desc * fm.d.abs().max(fm.b.abs());
        if self.ascent - self.descent <= 0.0 || !(self.ascent - self.descent).is_finite() {
            (self.ascent, self.descent) = (0.8, -0.2);
        }
    }

    /// Splits the next code off `bytes` (non-empty): its value and length.
    #[inline]
    pub fn next_code(&self, bytes: &[u8]) -> (u32, usize) {
        match &self.codes {
            None => (u32::from(bytes[0]), 1),
            Some(c) => c.next_code(bytes),
        }
    }

    /// Whether codes are composite (CIDs through a CMap).
    pub fn is_composite(&self) -> bool {
        self.codes.is_some()
    }

    /// The CID of a code of a composite font (0 when unmapped), or the code itself.
    pub fn cid(&self, code: u32) -> u32 {
        match &self.codes {
            Some(c) => c.cid(code).unwrap_or(0),
            None => code,
        }
    }

    /// Horizontal advance of a code in text space units per unit of font size.
    pub fn width(&self, code: u32) -> f32 {
        let w = match &self.widths {
            Widths::Simple {
                first,
                widths,
                missing,
                std14,
            } => match code
                .checked_sub(*first)
                .and_then(|i| widths.get(i as usize))
            {
                Some(&w) => w,
                None if widths.is_empty() => {
                    let program = self.program.as_ref().and_then(|p| p.width(code));
                    match (program, std14) {
                        (Some(w), _) => w,
                        (None, Some(s)) => self.std14_width(s, code).unwrap_or(*missing),
                        (None, None) => *missing,
                    }
                }
                None => *missing,
            },
            Widths::Cid {
                ranges, default, ..
            } => find(ranges, self.cid(code)).map_or(*default, |r| r.2),
        };
        w * self.font_matrix.a
    }

    fn std14_width(&self, s: &Std14, code: u32) -> Option<f32> {
        if s.widths.is_empty() {
            return Some(600.0);
        }
        let name = self.glyph_name(code)?;
        let id = names::glyph_id(name)?;
        let i = s.widths.binary_search_by_key(&id, |w| w.0).ok()?;
        Some(f32::from(s.widths[i].1))
    }

    /// Vertical displacement `w1y` and the position vector `v` from the horizontal origin
    /// to the vertical one, in text space units per unit of font size.
    pub fn vertical_metrics(&self, code: u32) -> (f32, Point) {
        let Widths::Cid {
            vertical,
            vertical_default,
            ..
        } = &self.widths
        else {
            return (-1.0, Point::new(self.width(code) / 2.0, 0.88));
        };
        let s = self.font_matrix.d;
        match find(vertical, self.cid(code)) {
            Some((_, _, [w1, vx, vy])) => (w1 * s, Point::new(vx * s, vy * s)),
            None => (
                vertical_default[1] * s,
                Point::new(self.width(code) / 2.0, vertical_default[0] * s),
            ),
        }
    }

    /// The glyph name of a code of a simple font.
    pub fn glyph_name(&self, code: u32) -> Option<&[u8]> {
        self.encoding.as_ref()?.name(u8::try_from(code).ok()?)
    }

    /// The glyph index of a code, when the font program is known.
    pub fn glyph_id(&self, code: u32) -> Option<u32> {
        self.program.as_ref()?.glyph_id(self.cid(code))
    }

    /// Appends the Unicode text of a code; false if it has no known mapping. Sources in
    /// order: `/ToUnicode`, Unicode-coded CMaps, glyph names, the character collection of
    /// the CIDs, the font program, and for symbolic simple fonts the code itself.
    pub fn unicode(&self, doc: &Document, code: u32, out: &mut String) -> bool {
        if self.codes.is_none()
            && let Ok(c) = u8::try_from(code)
        {
            let t = self.simple_text.get_or_init(|| self.code_text(doc));
            let c = usize::from(c);
            out.push_str(&t.text[t.ends[c] as usize..t.ends[c + 1] as usize]);
            return t.mapped[c];
        }
        self.lookup_unicode(doc, code, out)
    }

    fn code_text(&self, doc: &Document) -> CodeText {
        let mut t = CodeText {
            text: String::new(),
            ends: [0; 257],
            mapped: [false; 256],
        };
        for c in 0..256 {
            t.mapped[c] = self.lookup_unicode(doc, c as u32, &mut t.text);
            t.ends[c + 1] = t.text.len() as u32;
        }
        t
    }

    fn lookup_unicode(&self, doc: &Document, code: u32, out: &mut String) -> bool {
        if let Some(m) = self.unicode.get_or_init(|| self.load_to_unicode(doc))
            && m.unicode(code, out)
        {
            return true;
        }
        if let Some(c) = &self.codes {
            if c.unicode(code, out) {
                return true;
            }
            if let Some(cid) = c.cid(code)
                && self.ordering.unicode(cid, out)
            {
                return true;
            }
        } else if let Some(n) = self.glyph_name(code)
            && names::name_to_unicode(n, out)
        {
            return true;
        }
        if let Some(p) = &self.program
            && p.unicode(self.cid(code), out)
        {
            return true;
        }
        if self.codes.is_none() && (self.flags & 4 != 0 || self.encoding.is_none()) {
            let c = match code {
                0xF020..=0xF0FF => code - 0xF000,
                _ => code,
            };
            if let Some(c) = char::from_u32(c).filter(|c| !c.is_control()) {
                out.push(c);
                return true;
            }
        }
        false
    }

    fn load_to_unicode(&self, doc: &Document) -> Option<CMap> {
        let o = doc.resolve(self.to_unicode.as_ref()?);
        let s = o.as_stream()?;
        let data = doc.decode(s).ok()?.data;
        let m = CMap::parse(&data);
        (!m.is_empty()).then_some(m)
    }
}

/// Reads an embedded CMap stream with its `usecmap` or `/UseCMap` parent.
fn embedded_cmap(doc: &Document, s: &pagebara_syntax::Stream, depth: u32) -> CMap {
    let mut m = doc
        .decode(s)
        .map(|d| CMap::parse(&d.data))
        .unwrap_or_default();
    m.vertical |= s.dict.int(b"WMode") == Some(1);
    let parent = match &*doc.get_in(&s.dict, b"UseCMap") {
        Object::Stream(p) if depth < MAX_USECMAP => {
            Some(Arc::new(embedded_cmap(doc, p, depth + 1)))
        }
        Object::Name(n) => CMap::predefined(n.as_bytes()),
        _ => m.use_cmap.as_deref().and_then(CMap::predefined),
    };
    if let Some(p) = parent {
        m.inherit(&p);
    }
    if !m.has_codespace()
        && let Some(id) = CMap::predefined(b"Identity-H")
    {
        m.inherit(&id);
    }

    m
}

/// `/W` (`n = 1`) or `/W2` (`n = 3`) arrays as sorted CID ranges.
fn cid_widths<const N: usize>(doc: &Document, a: &[Object]) -> Vec<(u32, u32, [f32; N])> {
    let num = |o: &Object| doc.resolve(o).as_f64().map(|v| v as f32);
    let mut out = Vec::new();
    let mut i = 0;
    while i < a.len() {
        let Some(first) = num(&a[i]).filter(|v| *v >= 0.0).map(|v| v as u32) else {
            i += 1;
            continue;
        };
        let next = doc.resolve(a.get(i + 1).unwrap_or(&Object::Null));
        if let Some(list) = next.as_array() {
            for (k, c) in list.as_chunks::<N>().0.iter().enumerate() {
                let mut v = [0.0; N];
                for (d, o) in v.iter_mut().zip(c) {
                    *d = num(o).unwrap_or(0.0);
                }
                let cid = first.saturating_add(k as u32);
                out.push((cid, cid, v));
            }
            i += 2;
        } else {
            let last = num(&next).map_or(first, |v| v.max(0.0) as u32);
            let mut v = [0.0; N];
            for (k, d) in v.iter_mut().enumerate() {
                *d = a.get(i + 2 + k).and_then(num).unwrap_or(0.0);
            }
            if first <= last {
                out.push((first, last, v));
            }
            i += 2 + N;
        }
    }
    out.sort_by_key(|r| r.0);
    out
}

fn find<T>(ranges: &[(u32, u32, T)], cid: u32) -> Option<&(u32, u32, T)> {
    let end = ranges.partition_point(|r| r.0 <= cid);
    ranges[..end].iter().rev().take(8).find(|r| cid <= r.1)
}
