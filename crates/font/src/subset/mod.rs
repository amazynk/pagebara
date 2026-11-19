//! Subsetting and embedding: TrueType and CFF subsets, and the objects of a Type 0 font
//! with `Identity-H` encoding (§9.7) for text drawn with an OpenType font.
//!
//! Codes in content streams are two-byte CIDs: the original glyph id for TrueType (the
//! `CIDToGIDMap` maps it to the subset's glyph) and for name-keyed CFF, or the original
//! CID of CID-keyed CFF fonts.

pub mod blank;
pub mod cff;
pub mod truetype;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;

use pagebara_syntax::{Dict, Document, ObjRef, Object, Stream};

use crate::font::Font;
use crate::sfnt::Sfnt;

pub use truetype::Subset;

/// Collects the glyphs used by text and writes the subset font into a document.
#[derive(Debug)]
pub struct Embedder {
    font: Sfnt,
    /// CID (see the module docs) → (glyph id, text it stands for).
    used: BTreeMap<u16, (u16, String)>,
    scale: f32,
}

impl Embedder {
    /// An embedder for a TrueType or (non-variable) OpenType-CFF font.
    pub fn new(data: impl Into<Arc<[u8]>>, index: u32) -> Option<Embedder> {
        Embedder::from_font(Font::parse_index(data, index)?)
    }

    /// An embedder for a parsed TrueType or (non-variable) OpenType-CFF font.
    pub fn from_font(font: Font) -> Option<Embedder> {
        let Font::Sfnt(font) = font else {
            return None;
        };
        let font = *font;
        if !font.has_glyf() && font.cff().is_none_or(|c| c.is_cff2()) {
            return None;
        }
        let scale = 1000.0 / f32::from(font.units_per_em());
        Some(Embedder {
            font,
            used: BTreeMap::new(),
            scale,
        })
    }

    pub fn font(&self) -> &Sfnt {
        &self.font
    }

    /// The glyph for a character (0 when the font lacks it).
    pub fn glyph(&self, c: char) -> u16 {
        self.font.glyph_for_char(c).unwrap_or(0)
    }

    /// The CID written for a glyph.
    pub fn cid(&self, gid: u16) -> u16 {
        match self.font.cff() {
            Some(c) if !self.font.has_glyf() && c.is_cid() => c.charset_id(gid).unwrap_or(0),
            _ => gid,
        }
    }

    /// The glyph of a CID written by this embedder (the inverse of [`Embedder::cid`]).
    pub fn glyph_of_cid(&self, cid: u16) -> u16 {
        match self.font.cff() {
            Some(c) if !self.font.has_glyf() && c.is_cid() => c.glyph_by_cid(cid).unwrap_or(0),
            _ => cid,
        }
    }

    /// Encodes text as two-byte codes (one glyph per character, no shaping) and records the
    /// glyphs for the subset.
    pub fn encode(&mut self, text: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(text.len() * 2);
        let mut buf = [0; 4];
        for c in text.chars() {
            let cid = self.use_glyph(self.glyph(c), c.encode_utf8(&mut buf));
            out.extend_from_slice(&cid.to_be_bytes());
        }
        out
    }

    /// Records glyph `gid` standing for `text` (for `/ToUnicode`; the first text recorded
    /// for a glyph wins) and returns its CID, for callers that choose glyphs themselves
    /// (contextual forms, ligatures).
    pub fn use_glyph(&mut self, gid: u16, text: &str) -> u16 {
        let cid = self.cid(gid);
        let e = self.used.entry(cid).or_insert_with(|| (gid, String::new()));
        if e.1.is_empty() && gid != 0 {
            e.1.push_str(text);
        }
        cid
    }

    /// Advance width of a glyph in thousandths of the font size.
    pub fn advance(&self, gid: u16) -> f32 {
        let w = self
            .font
            .advance(gid)
            .map(f32::from)
            .or_else(|| self.font.cff().and_then(|c| c.advance(gid)))
            .unwrap_or(0.0);
        w * self.scale
    }

    /// Width of text in thousandths of the font size.
    pub fn text_width(&self, text: &str) -> f32 {
        text.chars().map(|c| self.advance(self.glyph(c))).sum()
    }

    /// Ascent and descent in thousandths of the font size.
    pub fn ascent_descent(&self) -> (f32, f32) {
        let (a, d, _) = self.font.vertical_metrics();
        (f32::from(a) * self.scale, f32::from(d) * self.scale)
    }

    /// The font file and how it is embedded.
    fn program(&self) -> Option<Program> {
        let gids: Vec<u16> = self.used.values().map(|u| u.0).collect();
        if self.font.has_glyf() {
            let cmap: Vec<(char, u16)> = self
                .used
                .values()
                .filter_map(|(g, s)| Some((s.chars().next()?, *g)))
                .collect();
            let sub = truetype::subset(&self.font, gids, &cmap)?;
            // CIDToGIDMap: CID (old gid) → new gid.
            let max = self.used.keys().next_back().copied().unwrap_or(0);
            let mut map = vec![0u8; (usize::from(max) + 1) * 2];
            for &cid in self.used.keys() {
                let new = sub.new_id(cid).unwrap_or(0);
                map[usize::from(cid) * 2..usize::from(cid) * 2 + 2]
                    .copy_from_slice(&new.to_be_bytes());
            }
            Some(Program {
                data: sub.data,
                key: b"FontFile2",
                subtype: None,
                cid_map: Some(map),
            })
        } else {
            let cff = self.font.cff()?;
            match cff::subset(cff, gids) {
                Some(data) => Some(Program {
                    data,
                    key: b"FontFile3",
                    subtype: Some(b"CIDFontType0C"),
                    cid_map: None,
                }),
                None => Some(Program {
                    data: self.font.data().to_vec(),
                    key: b"FontFile3",
                    subtype: Some(b"OpenType"),
                    cid_map: None,
                }),
            }
        }
    }

    /// A six-letter subset tag derived from the glyphs used.
    fn tag(&self) -> String {
        let mut h: u32 = 2166136261;
        for (&cid, (g, _)) in &self.used {
            for b in cid.to_be_bytes().into_iter().chain(g.to_be_bytes()) {
                h = (h ^ u32::from(b)).wrapping_mul(16777619);
            }
        }
        (0..6)
            .map(|i| char::from(b'A' + ((h >> (i * 5)) % 26) as u8))
            .collect()
    }

    /// Adds the font objects to `doc` and returns the Type 0 font dictionary's reference.
    pub fn write(&self, doc: &mut Document) -> Option<ObjRef> {
        let Program {
            data: program,
            key,
            subtype,
            cid_map,
        } = self.program()?;
        let ps: String = self
            .font
            .postscript_name()
            .unwrap_or_else(|| "Font".into())
            .chars()
            .filter(|c| c.is_ascii_graphic() && !"[](){}<>/%#".contains(*c))
            .collect();
        let name = format!(
            "{}+{}",
            self.tag(),
            if ps.is_empty() { "Font" } else { &ps }
        );

        let mut ff = Dict::new();
        if let Some(st) = subtype {
            ff.set(b"Subtype", Object::name(st));
        }
        if key == b"FontFile2" {
            ff.set(b"Length1", Object::Int(program.len() as i64));
        }
        let file = doc.add(flate_stream(ff, &program));

        let s = self.scale;
        let bb = self.font.bbox();
        let (asc, desc) = self.ascent_descent();
        let os2 = self.font.os2();
        let mut flags = 4u32;
        if self.font.is_fixed_pitch() {
            flags |= 1;
        }
        if os2.and_then(|o| o.is_serif()).unwrap_or(false) {
            flags |= 2;
        }
        if self.font.italic_angle() != 0.0 || os2.is_some_and(|o| o.is_italic()) {
            flags |= 64;
        }
        let weight = os2.map_or(400.0, |o| f32::from(o.weight_class));
        let mut fd = Dict::new();
        fd.set(b"Type", Object::name(b"FontDescriptor"));
        fd.set(b"FontName", Object::name(name.as_bytes()));
        fd.set(b"Flags", Object::Int(i64::from(flags)));
        fd.set(
            b"FontBBox",
            Object::Array(bb.iter().map(|&v| num(f32::from(v) * s)).collect()),
        );
        fd.set(b"ItalicAngle", num(self.font.italic_angle()));
        fd.set(b"Ascent", num(asc));
        fd.set(b"Descent", num(desc));
        let cap = os2.map_or(0, |o| o.cap_height);
        fd.set(
            b"CapHeight",
            num(if cap > 0 { f32::from(cap) * s } else { asc }),
        );
        fd.set(b"StemV", num(10.0 + 220.0 * (weight - 50.0) / 900.0));
        fd.set(key, Object::Ref(file));
        let fd = doc.add(Object::Dict(fd));

        let mut cid = Dict::new();
        cid.set(b"Type", Object::name(b"Font"));
        cid.set(
            b"Subtype",
            Object::name(if cid_map.is_some() {
                b"CIDFontType2"
            } else {
                b"CIDFontType0"
            }),
        );
        cid.set(b"BaseFont", Object::name(name.as_bytes()));
        let mut csi = Dict::new();
        csi.set(b"Registry", Object::string(b"Adobe"));
        csi.set(b"Ordering", Object::string(b"Identity"));
        csi.set(b"Supplement", Object::Int(0));
        cid.set(b"CIDSystemInfo", Object::Dict(csi));
        cid.set(b"FontDescriptor", Object::Ref(fd));
        let dw = self.advance(0);
        cid.set(b"DW", num(dw));
        cid.set(b"W", Object::Array(self.widths(dw)));
        if let Some(map) = cid_map {
            let m = doc.add(flate_stream(Dict::new(), &map));
            cid.set(b"CIDToGIDMap", Object::Ref(m));
        }
        let cid = doc.add(Object::Dict(cid));

        let tu = doc.add(flate_stream(Dict::new(), self.to_unicode().as_bytes()));
        let mut t0 = Dict::new();
        t0.set(b"Type", Object::name(b"Font"));
        t0.set(b"Subtype", Object::name(b"Type0"));
        t0.set(b"BaseFont", Object::name(name.as_bytes()));
        t0.set(b"Encoding", Object::name(b"Identity-H"));
        t0.set(b"DescendantFonts", Object::Array(vec![Object::Ref(cid)]));
        t0.set(b"ToUnicode", Object::Ref(tu));
        Some(doc.add(Object::Dict(t0)))
    }

    /// `/W`: runs of consecutive CIDs as `c [w1 w2 …]`, skipping the default width.
    fn widths(&self, dw: f32) -> Vec<Object> {
        let mut out = Vec::new();
        let mut run: Option<(u16, Vec<Object>)> = None;
        for (&cid, &(gid, _)) in &self.used {
            let w = self.advance(gid);
            if (w - dw).abs() < 0.5 {
                continue;
            }
            match &mut run {
                Some((start, ws)) if usize::from(*start) + ws.len() == usize::from(cid) => {
                    ws.push(num(w))
                }
                _ => {
                    if let Some((s, ws)) = run.take() {
                        out.push(Object::Int(i64::from(s)));
                        out.push(Object::Array(ws));
                    }
                    run = Some((cid, vec![num(w)]));
                }
            }
        }
        if let Some((s, ws)) = run {
            out.push(Object::Int(i64::from(s)));
            out.push(Object::Array(ws));
        }
        out
    }

    /// The `/ToUnicode` CMap.
    pub fn to_unicode(&self) -> String {
        let mut s = String::from(
            "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
             /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
             /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
             1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
        );
        let entries: Vec<(u16, &str)> = self
            .used
            .iter()
            .filter(|(_, (_, t))| !t.is_empty())
            .map(|(&c, (_, t))| (c, t.as_str()))
            .collect();
        for chunk in entries.chunks(100) {
            let _ = writeln!(s, "{} beginbfchar", chunk.len());
            for (c, t) in chunk {
                let _ = write!(s, "<{c:04X}> <");
                for u in t.encode_utf16() {
                    let _ = write!(s, "{u:04X}");
                }
                s.push_str(">\n");
            }
            s.push_str("endbfchar\n");
        }
        s.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
        s
    }
}

struct Program {
    data: Vec<u8>,
    key: &'static [u8],
    subtype: Option<&'static [u8]>,
    /// The `CIDToGIDMap` stream data of a TrueType subset.
    cid_map: Option<Vec<u8>>,
}

fn num(v: f32) -> Object {
    let r = v.round();
    if (v - r).abs() < 1e-3 {
        Object::Int(r as i64)
    } else {
        Object::Real(f64::from((v * 1000.0).round() / 1000.0))
    }
}

fn flate_stream(mut dict: Dict, data: &[u8]) -> Object {
    let z = miniz_oxide::deflate::compress_to_vec_zlib(data, 6);
    dict.set(b"Filter", Object::name(b"FlateDecode"));
    Object::Stream(Stream::new(dict, z))
}
