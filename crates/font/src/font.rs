//! [`Font`]: any supported font program behind one interface.

use std::sync::Arc;

use crate::cff::Cff;
use crate::data;
use crate::outline::{Matrix, OutlineSink};
use crate::sfnt::{self, Sfnt};
use crate::type1::Type1;

/// The container format of a font program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// `sfnt` with `glyf` outlines.
    TrueType,
    /// `sfnt` with a `CFF ` or `CFF2` table.
    OpenTypeCff,
    /// A bare CFF font (PDF `FontFile3` /Type1C or /CIDFontType0C).
    Cff,
    Type1,
}

/// A parsed font program.
#[derive(Debug)]
pub enum Font {
    Sfnt(Box<Sfnt>),
    Cff(Box<Cff>),
    Type1(Box<Type1>),
}

impl Font {
    /// Detects the format and parses the first font of `data`.
    pub fn parse(data: impl Into<Arc<[u8]>>) -> Option<Font> {
        Font::parse_index(data, 0)
    }

    /// Parses font `index` of a collection (or the only font).
    pub fn parse_index(data: impl Into<Arc<[u8]>>, index: u32) -> Option<Font> {
        let data: Arc<[u8]> = data.into();
        if sfnt::collection_count(&data) > 0 {
            return Sfnt::parse(data, index).map(|s| Font::Sfnt(Box::new(s)));
        }
        match data.first()? {
            1 | 2 if data.get(1).is_some_and(|&m| m < 16) => {
                Cff::parse(data).map(|c| Font::Cff(Box::new(c)))
            }
            _ => Type1::parse(&data).map(|t| Font::Type1(Box::new(t))),
        }
    }

    pub fn format(&self) -> Format {
        match self {
            Font::Sfnt(s) if s.has_glyf() => Format::TrueType,
            Font::Sfnt(_) => Format::OpenTypeCff,
            Font::Cff(_) => Format::Cff,
            Font::Type1(_) => Format::Type1,
        }
    }

    pub fn as_sfnt(&self) -> Option<&Sfnt> {
        match self {
            Font::Sfnt(s) => Some(s),
            _ => None,
        }
    }

    /// The CFF data: a bare CFF font or the `CFF ` table of an OpenType font.
    pub fn as_cff(&self) -> Option<&Cff> {
        match self {
            Font::Cff(c) => Some(c),
            Font::Sfnt(s) if !s.has_glyf() => s.cff(),
            _ => None,
        }
    }

    pub fn as_type1(&self) -> Option<&Type1> {
        match self {
            Font::Type1(t) => Some(t),
            _ => None,
        }
    }

    pub fn num_glyphs(&self) -> u16 {
        match self {
            Font::Sfnt(s) => s.num_glyphs(),
            Font::Cff(c) => c.num_glyphs(),
            Font::Type1(t) => t.num_glyphs(),
        }
    }

    /// Glyph space to text space: outlines are in glyph space.
    pub fn font_matrix(&self) -> Matrix {
        match self {
            Font::Sfnt(s) => s.font_matrix(),
            Font::Cff(c) => c.font_matrix(),
            Font::Type1(t) => t.font_matrix(),
        }
    }

    /// Draws glyph `gid` in glyph space. Returns false for missing or broken glyphs.
    pub fn outline(&self, gid: u16, sink: &mut dyn OutlineSink) -> bool {
        match self {
            Font::Sfnt(s) => s.outline(gid, sink),
            Font::Cff(c) => c.outline(gid, sink),
            Font::Type1(t) => t.outline(gid, sink),
        }
    }

    /// Advance width in glyph space units.
    pub fn advance(&self, gid: u16) -> Option<f32> {
        match self {
            Font::Sfnt(s) => s
                .advance(gid)
                .map(f32::from)
                .or_else(|| s.cff().and_then(|c| c.advance(gid))),
            Font::Cff(c) => c.advance(gid),
            Font::Type1(t) => t.advance(gid),
        }
    }

    pub fn postscript_name(&self) -> Option<String> {
        match self {
            Font::Sfnt(s) => s
                .postscript_name()
                .or_else(|| s.cff().and_then(|c| c.name().map(str::to_owned))),
            Font::Cff(c) => c.name().map(str::to_owned),
            Font::Type1(t) => t.name().map(str::to_owned),
        }
    }

    pub fn glyph_name(&self, gid: u16) -> Option<&str> {
        match self {
            Font::Sfnt(s) => s.glyph_name(gid),
            Font::Cff(c) => c.glyph_name(gid),
            Font::Type1(t) => t.glyph_name(gid),
        }
    }

    pub fn glyph_by_name(&self, name: &[u8]) -> Option<u16> {
        match self {
            Font::Sfnt(s) => s.glyph_by_name(name),
            Font::Cff(c) => c.glyph_by_name(name),
            Font::Type1(t) => t.glyph_by_name(name),
        }
    }

    /// The glyph for a character: the Unicode cmap, else the AGL glyph name.
    pub fn glyph_for_char(&self, c: char) -> Option<u16> {
        if let Font::Sfnt(s) = self
            && let Some(g) = s.glyph_for_char(c)
        {
            return Some(g);
        }
        self.glyph_by_name(data::unicode_to_name(c).as_bytes())
    }

    /// The glyph of a CID: the CFF charset of CID-keyed fonts, otherwise CID = GID.
    pub fn glyph_by_cid(&self, cid: u16) -> Option<u16> {
        match self.as_cff() {
            Some(c) => c.glyph_by_cid(cid),
            None => (cid < self.num_glyphs()).then_some(cid),
        }
    }

    /// The glyph of `code` in the font's built-in encoding: the Type 1 `/Encoding`, the CFF
    /// encoding (also of OpenType-CFF), or else the (3,0) symbol cmap (also at U+F000 +
    /// code) or the (1,0) Macintosh cmap.
    pub fn builtin_glyph(&self, code: u8) -> Option<u16> {
        match self {
            Font::Type1(t) => t.encoding_glyph(code),
            Font::Cff(c) => c.encoding_glyph(code),
            Font::Sfnt(s) => {
                if !s.has_glyf()
                    && let Some(g) = s.cff().and_then(|c| c.encoding_glyph(code))
                {
                    return Some(g);
                }
                if let Some(t) = s.find_subtable(3, 0) {
                    for base in [0u32, 0xF000, 0xF100, 0xF200] {
                        if let Some(g) = s.cmap_lookup(t, base + u32::from(code)) {
                            return Some(g);
                        }
                    }
                }
                if let Some(t) = s.find_subtable(1, 0)
                    && let Some(g) = s.cmap_lookup(t, u32::from(code))
                {
                    return Some(g);
                }
                None
            }
        }
    }

    /// The built-in encoding as glyph names (code → name), when the font has one.
    pub fn builtin_encoding(&self) -> Option<[Option<&str>; 256]> {
        let mut any = false;
        let names = std::array::from_fn(|c| {
            let n = self.builtin_glyph(c as u8).and_then(|g| self.glyph_name(g));
            any |= n.is_some();
            n
        });
        any.then_some(names)
    }
}
