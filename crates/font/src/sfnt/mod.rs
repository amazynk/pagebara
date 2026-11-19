//! TrueType and OpenType fonts (`sfnt` containers) and TrueType collections.

pub mod cmap;
pub mod glyf;

use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, OnceLock};

use crate::cff::Cff;
use crate::outline::{Matrix, OutlineSink};
use crate::reader::{Reader, i16_at, u16_at, u32_at};
pub use cmap::{Subtable, Variation};

pub type Tag = [u8; 4];

/// The number of fonts in a TrueType collection, 1 for a single font, 0 if not an sfnt.
pub fn collection_count(data: &[u8]) -> u32 {
    match data.get(..4) {
        Some(b"ttcf") => u32_at(data, 8).unwrap_or(0).min(0x10000),
        Some(v) if is_sfnt_version(v) => 1,
        _ => 0,
    }
}

fn is_sfnt_version(v: &[u8]) -> bool {
    matches!(v, [0, 1, 0, 0] | b"OTTO" | b"true" | b"typ1")
}

/// OS/2 metrics and classification.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Os2 {
    pub weight_class: u16,
    pub width_class: u16,
    pub fs_type: u16,
    pub family_class: i16,
    pub panose: [u8; 10],
    pub fs_selection: u16,
    pub typo_ascender: i16,
    pub typo_descender: i16,
    pub win_ascent: u16,
    pub win_descent: u16,
    pub x_height: i16,
    pub cap_height: i16,
    pub unicode_ranges: [u32; 4],
    pub code_page_ranges: [u32; 2],
}

impl Os2 {
    pub fn is_italic(&self) -> bool {
        self.fs_selection & 1 != 0
    }
    pub fn is_bold(&self) -> bool {
        self.fs_selection & 0x20 != 0 || self.weight_class >= 600
    }
    pub fn is_monospaced(&self) -> bool {
        self.panose[0] == 2 && self.panose[3] == 9
    }
    /// From the Panose serif style or the IBM family class.
    pub fn is_serif(&self) -> Option<bool> {
        if self.panose[0] == 2 && self.panose[1] > 1 {
            return Some(!(11..=13).contains(&self.panose[1]));
        }
        match self.family_class >> 8 {
            1..=5 | 7 => Some(true),
            8 => Some(false),
            _ => None,
        }
    }
}

#[derive(Debug)]
enum PostNames {
    None,
    Mac,
    /// Glyph name indices (< 258: Macintosh order) and the custom names' ranges.
    Custom(Vec<u16>, Vec<Range<usize>>),
}

/// A parsed TrueType or OpenType font. Owns its data.
#[derive(Debug)]
pub struct Sfnt {
    data: Arc<[u8]>,
    tables: Vec<(Tag, Range<usize>)>,
    pub(crate) units_per_em: u16,
    bbox: [i16; 4],
    mac_style: u16,
    num_glyphs: u16,
    num_h_metrics: u16,
    num_v_metrics: u16,
    ascender: i16,
    descender: i16,
    line_gap: i16,
    loca_long: bool,
    glyf: Option<(Range<usize>, Range<usize>)>,
    hmtx: Range<usize>,
    vmtx: Range<usize>,
    cmap: Range<usize>,
    subtables: Vec<Subtable>,
    cff: Option<Cff>,
    post: Range<usize>,
    post_names: OnceLock<PostNames>,
    name_index: OnceLock<HashMap<Box<[u8]>, u16>>,
    italic_angle: f32,
    is_fixed_pitch: bool,
}

impl Sfnt {
    /// Parses font `index` of `data` (a single font or a collection).
    pub fn parse(data: impl Into<Arc<[u8]>>, index: u32) -> Option<Sfnt> {
        let data: Arc<[u8]> = data.into();
        let d = &data[..];
        let start = if d.get(..4)? == b"ttcf" {
            let n = u32_at(d, 8)?;
            if index >= n {
                return None;
            }
            u32_at(d, 12 + 4 * index as usize)? as usize
        } else {
            0
        };
        if !is_sfnt_version(d.get(start..start + 4)?) {
            return None;
        }
        let n = usize::from(u16_at(d, start + 4)?);
        let mut tables = Vec::with_capacity(n);
        for i in 0..n {
            let rec = start + 12 + i * 16;
            let Some(tag) = d.get(rec..rec + 4) else {
                break;
            };
            let (Some(off), Some(len)) = (u32_at(d, rec + 8), u32_at(d, rec + 12)) else {
                break;
            };
            let off = off as usize;
            // Clamp tables running past the end; some writers get lengths wrong.
            if off >= d.len() {
                continue;
            }
            let end = off.saturating_add(len as usize).min(d.len());
            tables.push((tag.try_into().ok()?, off..end));
        }
        let mut f = Sfnt {
            data: data.clone(),
            tables,
            units_per_em: 1000,
            bbox: [0; 4],
            mac_style: 0,
            num_glyphs: 0,
            num_h_metrics: 0,
            num_v_metrics: 0,
            ascender: 0,
            descender: 0,
            line_gap: 0,
            loca_long: false,
            glyf: None,
            hmtx: 0..0,
            vmtx: 0..0,
            cmap: 0..0,
            subtables: Vec::new(),
            cff: None,
            post: 0..0,
            post_names: OnceLock::new(),
            name_index: OnceLock::new(),
            italic_angle: 0.0,
            is_fixed_pitch: false,
        };
        let mut head_loca = None;
        if let Some(h) = find_table(d, &f.tables, b"head") {
            let upem = u16_at(h, 18).unwrap_or(0);
            f.units_per_em = if (16..=16384).contains(&upem) {
                upem
            } else {
                1000
            };
            for (i, b) in f.bbox.iter_mut().enumerate() {
                *b = i16_at(h, 36 + 2 * i).unwrap_or(0);
            }
            f.mac_style = u16_at(h, 44).unwrap_or(0);
            head_loca = i16_at(h, 50);
        }
        let maxp_glyphs = find_table(d, &f.tables, b"maxp").and_then(|m| u16_at(m, 4));
        if let Some(h) = find_table(d, &f.tables, b"hhea") {
            f.ascender = i16_at(h, 4).unwrap_or(0);
            f.descender = i16_at(h, 6).unwrap_or(0);
            f.line_gap = i16_at(h, 8).unwrap_or(0);
            f.num_h_metrics = u16_at(h, 34).unwrap_or(0);
        }
        if let Some(h) = find_table(d, &f.tables, b"vhea") {
            f.num_v_metrics = u16_at(h, 34).unwrap_or(0);
        }
        f.hmtx = f.range(b"hmtx");
        f.vmtx = f.range(b"vmtx");
        f.cmap = f.range(b"cmap");
        f.subtables = cmap::subtables(&d[f.cmap.clone()]);
        f.post = f.range(b"post");
        if let Some(p) = find_table(d, &f.tables, b"post") {
            f.italic_angle = u32_at(p, 4).map_or(0.0, |v| v as i32 as f32 / 65536.0);
            f.is_fixed_pitch = u32_at(p, 12).is_some_and(|v| v != 0);
        }
        let (glyf, loca) = (f.range(b"glyf"), f.range(b"loca"));
        if !loca.is_empty() {
            let len = loca.len();
            let n = usize::from(maxp_glyphs.unwrap_or(0));
            // Trust the loca size over a wrong indexToLocFormat.
            f.loca_long = match head_loca {
                Some(1) => !(n > 0 && len == (n + 1) * 2),
                Some(0) => n > 0 && len == (n + 1) * 4,
                _ => n > 0 && len >= (n + 1) * 4,
            };
            f.glyf = Some((glyf, loca));
        }
        if let Some(r) = f
            .tables
            .iter()
            .find(|t| &t.0 == b"CFF ")
            .map(|t| t.1.clone())
        {
            f.cff = Cff::parse_range(data.clone(), r);
        } else if let Some(r) = f
            .tables
            .iter()
            .find(|t| &t.0 == b"CFF2")
            .map(|t| t.1.clone())
        {
            f.cff = Cff::parse_range(data.clone(), r);
        }
        f.num_glyphs = match (maxp_glyphs, &f.glyf, &f.cff) {
            (Some(n), _, _) if n > 0 => n,
            (_, Some(_), _) => f.loca().map_or(0, |l| l.num_glyphs().min(0xFFFF) as u16),
            (_, _, Some(c)) => c.num_glyphs(),
            _ => 0,
        };
        if f.glyf.is_none() && f.cff.is_none() {
            return None;
        }
        Some(f)
    }

    fn range(&self, tag: &Tag) -> Range<usize> {
        self.tables
            .iter()
            .find(|t| &t.0 == tag)
            .map_or(0..0, |t| t.1.clone())
    }

    /// The raw font file.
    pub fn data(&self) -> &Arc<[u8]> {
        &self.data
    }

    pub fn table(&self, tag: &Tag) -> Option<&[u8]> {
        let r = self.tables.iter().find(|t| &t.0 == tag)?.1.clone();
        self.data.get(r)
    }

    pub fn table_tags(&self) -> impl Iterator<Item = &Tag> {
        self.tables.iter().map(|t| &t.0)
    }

    pub(crate) fn loca(&self) -> Option<glyf::Loca<'_>> {
        let (g, l) = self.glyf.as_ref()?;
        Some(glyf::Loca {
            glyf: &self.data[g.clone()],
            loca: &self.data[l.clone()],
            long: self.loca_long,
        })
    }

    /// The CFF or CFF2 table of an OpenType font with PostScript outlines.
    pub fn cff(&self) -> Option<&Cff> {
        self.cff.as_ref()
    }

    pub fn has_glyf(&self) -> bool {
        self.glyf.is_some()
    }

    pub fn num_glyphs(&self) -> u16 {
        self.num_glyphs
    }

    pub fn units_per_em(&self) -> u16 {
        self.units_per_em
    }

    /// Glyph space to text space (em) transform.
    pub fn font_matrix(&self) -> Matrix {
        if self.glyf.is_none()
            && let Some(c) = &self.cff
        {
            return c.font_matrix();
        }
        Matrix::scale(1.0 / f64::from(self.units_per_em))
    }

    /// `[x_min, y_min, x_max, y_max]` from `head`.
    pub fn bbox(&self) -> [i16; 4] {
        self.bbox
    }

    /// Ascender, descender and line gap from `hhea`, else `OS/2`.
    pub fn vertical_metrics(&self) -> (i16, i16, i16) {
        if self.ascender != 0 || self.descender != 0 {
            return (self.ascender, self.descender, self.line_gap);
        }
        match self.os2() {
            Some(o) => (o.typo_ascender, o.typo_descender, 0),
            None => (self.bbox[3], self.bbox[1], 0),
        }
    }

    pub fn italic_angle(&self) -> f32 {
        self.italic_angle
    }

    pub fn is_fixed_pitch(&self) -> bool {
        self.is_fixed_pitch
    }

    /// `head.macStyle`: bit 0 bold, bit 1 italic.
    pub fn mac_style(&self) -> u16 {
        self.mac_style
    }

    /// Draws glyph `gid`; false if it does not exist or is broken. Empty glyphs succeed.
    pub fn outline(&self, gid: u16, sink: &mut dyn OutlineSink) -> bool {
        if gid >= self.num_glyphs.max(1) && self.cff.is_none() {
            return false;
        }
        if let Some(loca) = self.loca() {
            thread_local!(static SCRATCH: std::cell::Cell<glyf::Points> = std::cell::Cell::default());
            // Taken rather than borrowed: the sink may draw another glyph.
            let mut p = SCRATCH.take();
            p.clear();
            if glyf::decode(&loca, gid, &mut p).is_none() {
                SCRATCH.set(p);
                return false;
            }
            // Like FreeType, place the glyph so that its xMin is at the hmtx bearing.
            if let Some(x_min) = loca.glyph(gid).and_then(|g| i16_at(g, 2))
                && let Some(lsb) = self.left_side_bearing(gid)
                && lsb != x_min
            {
                let dx = f32::from(lsb) - f32::from(x_min);
                for q in &mut p.pts {
                    q.0 += dx;
                }
            }
            glyf::emit(&p, sink);
            SCRATCH.set(p);
            return true;
        }
        match &self.cff {
            Some(c) => c.outline(gid, sink),
            None => false,
        }
    }

    /// Advance width in font units.
    pub fn advance(&self, gid: u16) -> Option<u16> {
        metric(&self.data[self.hmtx.clone()], self.num_h_metrics, gid).map(|m| m.0)
    }

    pub fn left_side_bearing(&self, gid: u16) -> Option<i16> {
        metric(&self.data[self.hmtx.clone()], self.num_h_metrics, gid).map(|m| m.1)
    }

    /// Vertical advance and top side bearing from `vmtx`.
    pub fn vertical_advance(&self, gid: u16) -> Option<(u16, i16)> {
        metric(&self.data[self.vmtx.clone()], self.num_v_metrics, gid)
    }

    pub fn cmap_subtables(&self) -> &[Subtable] {
        &self.subtables
    }

    pub fn cmap_lookup(&self, s: &Subtable, code: u32) -> Option<u16> {
        cmap::lookup(&self.data[self.cmap.clone()], s, code)
    }

    /// The subtable for (platform, encoding), preferring formats that cover more.
    pub fn find_subtable(&self, platform: u16, encoding: u16) -> Option<&Subtable> {
        self.subtables
            .iter()
            .filter(|s| s.platform == platform && s.encoding == encoding && s.format != 14)
            .max_by_key(|s| matches!(s.format, 12 | 13 | 10))
    }

    /// The best Unicode subtable.
    pub fn unicode_subtable(&self) -> Option<&Subtable> {
        [
            (3, 10),
            (0, 6),
            (0, 4),
            (3, 1),
            (0, 3),
            (0, 2),
            (0, 1),
            (0, 0),
        ]
        .iter()
        .find_map(|&(p, e)| self.find_subtable(p, e))
    }

    /// The glyph for a Unicode scalar.
    pub fn glyph_for_char(&self, c: char) -> Option<u16> {
        let s = self.unicode_subtable()?;
        self.cmap_lookup(s, c as u32)
    }

    /// The glyph of a variation sequence (cmap format 14).
    pub fn glyph_for_variation(&self, c: char, selector: char) -> Option<u16> {
        let s = self.subtables.iter().find(|s| s.format == 14)?;
        match cmap::variation(&self.data[self.cmap.clone()], s, c as u32, selector as u32)? {
            Variation::Default => self.glyph_for_char(c),
            Variation::Glyph(g) => Some(g),
        }
    }

    /// Calls `f(code, gid)` for each mapping of a subtable.
    pub fn cmap_for_each(&self, s: &Subtable, f: &mut dyn FnMut(u32, u16)) {
        cmap::for_each(&self.data[self.cmap.clone()], s, f)
    }

    fn post_names(&self) -> &PostNames {
        self.post_names.get_or_init(|| {
            let p = &self.data[self.post.clone()];
            match u32_at(p, 0) {
                Some(0x10000) => PostNames::Mac,
                Some(0x20000) => {
                    let mut r = Reader::at(p, 32);
                    let n = r.u16().unwrap_or(0);
                    let mut idx = Vec::with_capacity(usize::from(n));
                    for _ in 0..n {
                        let Some(i) = r.u16() else { break };
                        idx.push(i);
                    }
                    let mut names = Vec::new();
                    let mut pos = r.pos();
                    while let Some(&len) = p.get(pos) {
                        let s = pos + 1;
                        let e = s + usize::from(len);
                        if e > p.len() {
                            break;
                        }
                        names.push(self.post.start + s..self.post.start + e);
                        pos = e;
                    }
                    PostNames::Custom(idx, names)
                }
                _ => PostNames::None,
            }
        })
    }

    /// The glyph name from the `post` table (or the CFF charset).
    pub fn glyph_name(&self, gid: u16) -> Option<&str> {
        match self.post_names() {
            PostNames::Mac => crate::data::mac_glyph_name(gid),
            PostNames::Custom(idx, names) => {
                let i = *idx.get(usize::from(gid))?;
                if i < 258 {
                    crate::data::mac_glyph_name(i)
                } else {
                    let r = names.get(usize::from(i - 258))?.clone();
                    std::str::from_utf8(&self.data[r]).ok()
                }
            }
            PostNames::None => self.cff.as_ref()?.glyph_name(gid),
        }
        .filter(|n| !n.is_empty())
    }

    /// The glyph with a `post` (or CFF) glyph name.
    pub fn glyph_by_name(&self, name: &[u8]) -> Option<u16> {
        if matches!(self.post_names(), PostNames::None) {
            return self.cff.as_ref()?.glyph_by_name(name);
        }
        let map = self.name_index.get_or_init(|| {
            let mut m = HashMap::new();
            for g in (0..self.num_glyphs).rev() {
                if let Some(n) = self.glyph_name(g) {
                    m.insert(n.as_bytes().into(), g);
                }
            }
            m
        });
        map.get(name).copied()
    }

    /// A `name` table string: Windows Unicode records preferred, then Macintosh Roman.
    pub fn name(&self, name_id: u16) -> Option<String> {
        name_string(self.table(b"name")?, name_id)
    }

    /// Typographic family (name 16) or family (name 1).
    pub fn family_name(&self) -> Option<String> {
        self.name(16).or_else(|| self.name(1))
    }

    pub fn subfamily_name(&self) -> Option<String> {
        self.name(17).or_else(|| self.name(2))
    }

    pub fn postscript_name(&self) -> Option<String> {
        self.name(6)
    }

    pub fn full_name(&self) -> Option<String> {
        self.name(4)
    }

    pub fn os2(&self) -> Option<Os2> {
        os2(self.table(b"OS/2")?)
    }

    /// Pair kerning from a format 0 `kern` subtable, in font units.
    pub fn kerning(&self, left: u16, right: u16) -> i16 {
        self.kern_pair(left, right).unwrap_or(0)
    }

    fn kern_pair(&self, left: u16, right: u16) -> Option<i16> {
        let t = self.table(b"kern")?;
        // Version 0 (Microsoft) or 1.0 (Apple) headers.
        let (mut pos, n, apple) = if u16_at(t, 0)? == 0 {
            (4usize, u32::from(u16_at(t, 2)?), false)
        } else {
            (8usize, u32_at(t, 4)?, true)
        };
        let key = (u32::from(left) << 16) | u32::from(right);
        for _ in 0..n.min(64) {
            let (len, format, horizontal, header) = if apple {
                let len = u32_at(t, pos)? as usize;
                let cov = u16_at(t, pos + 4)?;
                (len, cov & 0xFF, cov & 0x8000 == 0, 8)
            } else {
                let len = usize::from(u16_at(t, pos + 2)?);
                let cov = u16_at(t, pos + 4)?;
                (len, cov >> 8, cov & 1 != 0 && cov & 4 == 0, 6)
            };
            if format == 0 && horizontal {
                let s = pos + header;
                let pairs = usize::from(u16_at(t, s)?);
                let (mut lo, mut hi) = (0, pairs.min(t.len().saturating_sub(s + 8) / 6));
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    let p = s + 8 + mid * 6;
                    let k = u32_at(t, p)?;
                    match k.cmp(&key) {
                        std::cmp::Ordering::Less => lo = mid + 1,
                        std::cmp::Ordering::Greater => hi = mid,
                        std::cmp::Ordering::Equal => return i16_at(t, p + 4),
                    }
                }
            }
            if len == 0 {
                break;
            }
            pos = pos.checked_add(len)?;
        }
        None
    }
}

/// A `name` table string: Windows Unicode records preferred, then Macintosh Roman.
pub fn name_string(t: &[u8], name_id: u16) -> Option<String> {
    let count = usize::from(u16_at(t, 2)?);
    let storage = usize::from(u16_at(t, 4)?);
    let mut best: Option<(u8, String)> = None;
    for i in 0..count {
        let rec = 6 + i * 12;
        let (Some(pid), Some(eid), Some(lang), Some(nid), Some(len), Some(off)) = (
            u16_at(t, rec),
            u16_at(t, rec + 2),
            u16_at(t, rec + 4),
            u16_at(t, rec + 6),
            u16_at(t, rec + 8),
            u16_at(t, rec + 10),
        ) else {
            break;
        };
        if nid != name_id {
            continue;
        }
        let s = storage + usize::from(off);
        let Some(b) = t.get(s..s + usize::from(len)) else {
            continue;
        };
        let (rank, text) = match (pid, eid) {
            (3, 0 | 1 | 10) | (0, _) => {
                let units = b.as_chunks::<2>().0.iter().map(|&c| u16::from_be_bytes(c));
                let s: String = char::decode_utf16(units)
                    .map(|c| c.unwrap_or('\u{FFFD}'))
                    .collect();
                (if lang == 0x409 || pid == 0 { 3 } else { 2 }, s)
            }
            (1, 0) => (1, b.iter().map(|&c| mac_roman_char(c)).collect()),
            _ => continue,
        };
        if best.as_ref().is_none_or(|b| b.0 < rank) {
            best = Some((rank, text));
        }
    }
    best.map(|b| b.1).filter(|s| !s.is_empty())
}

/// Parses an `OS/2` table.
pub fn os2(t: &[u8]) -> Option<Os2> {
    let mut o = Os2 {
        weight_class: u16_at(t, 4)?,
        width_class: u16_at(t, 6)?,
        fs_type: u16_at(t, 8)?,
        family_class: i16_at(t, 30)?,
        ..Os2::default()
    };
    o.panose.copy_from_slice(t.get(32..42)?);
    for (i, r) in o.unicode_ranges.iter_mut().enumerate() {
        *r = u32_at(t, 42 + 4 * i)?;
    }
    o.fs_selection = u16_at(t, 62)?;
    o.typo_ascender = i16_at(t, 68).unwrap_or(0);
    o.typo_descender = i16_at(t, 70).unwrap_or(0);
    o.win_ascent = u16_at(t, 74).unwrap_or(0);
    o.win_descent = u16_at(t, 76).unwrap_or(0);
    o.code_page_ranges = [u32_at(t, 78).unwrap_or(0), u32_at(t, 82).unwrap_or(0)];
    o.x_height = i16_at(t, 86).unwrap_or(0);
    o.cap_height = i16_at(t, 88).unwrap_or(0);
    Some(o)
}

fn find_table<'a>(d: &'a [u8], tables: &[(Tag, Range<usize>)], tag: &Tag) -> Option<&'a [u8]> {
    d.get(tables.iter().find(|t| &t.0 == tag)?.1.clone())
}

fn metric(t: &[u8], n: u16, gid: u16) -> Option<(u16, i16)> {
    let n = usize::from(n).min(t.len() / 4);
    if n == 0 {
        return None;
    }
    let g = usize::from(gid);
    if g < n {
        Some((u16_at(t, g * 4)?, i16_at(t, g * 4 + 2)?))
    } else {
        let adv = u16_at(t, (n - 1) * 4)?;
        let lsb = i16_at(t, n * 4 + (g - n) * 2).unwrap_or(0);
        Some((adv, lsb))
    }
}

/// Macintosh Roman to Unicode (for `name` records and (1,0) cmaps).
pub fn mac_roman_char(b: u8) -> char {
    const HIGH: [u16; 128] = [
        0xC4, 0xC5, 0xC7, 0xC9, 0xD1, 0xD6, 0xDC, 0xE1, 0xE0, 0xE2, 0xE4, 0xE3, 0xE5, 0xE7, 0xE9,
        0xE8, 0xEA, 0xEB, 0xED, 0xEC, 0xEE, 0xEF, 0xF1, 0xF3, 0xF2, 0xF4, 0xF6, 0xF5, 0xFA, 0xF9,
        0xFB, 0xFC, 0x2020, 0xB0, 0xA2, 0xA3, 0xA7, 0x2022, 0xB6, 0xDF, 0xAE, 0xA9, 0x2122, 0xB4,
        0xA8, 0x2260, 0xC6, 0xD8, 0x221E, 0xB1, 0x2264, 0x2265, 0xA5, 0xB5, 0x2202, 0x2211, 0x220F,
        0x3C0, 0x222B, 0xAA, 0xBA, 0x3A9, 0xE6, 0xF8, 0xBF, 0xA1, 0xAC, 0x221A, 0x192, 0x2248,
        0x2206, 0xAB, 0xBB, 0x2026, 0xA0, 0xC0, 0xC3, 0xD5, 0x152, 0x153, 0x2013, 0x2014, 0x201C,
        0x201D, 0x2018, 0x2019, 0xF7, 0x25CA, 0xFF, 0x178, 0x2044, 0x20AC, 0x2039, 0x203A, 0xFB01,
        0xFB02, 0x2021, 0xB7, 0x201A, 0x201E, 0x2030, 0xC2, 0xCA, 0xC1, 0xCB, 0xC8, 0xCD, 0xCE,
        0xCF, 0xCC, 0xD3, 0xD4, 0xF8FF, 0xD2, 0xDA, 0xDB, 0xD9, 0x131, 0x2C6, 0x2DC, 0xAF, 0x2D8,
        0x2D9, 0x2DA, 0xB8, 0x2DD, 0x2DB, 0x2C7,
    ];
    if b < 128 {
        b as char
    } else {
        char::from_u32(u32::from(HIGH[usize::from(b - 128)])).unwrap_or('\u{FFFD}')
    }
}
