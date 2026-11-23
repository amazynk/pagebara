//! Compact Font Format (Adobe TN 5176) and CFF2: name-keyed and CID-keyed fonts.

pub(crate) mod charstring;

use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, OnceLock};

use crate::data;
use crate::outline::{Matrix, NullSink, OutlineSink, Pen};
use crate::reader::{Reader, i16_at, u8_at, u16_at, u32_at};

/// An INDEX: `count` objects; positions are relative to the CFF table.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Index {
    pub count: u32,
    off_size: u8,
    offsets: usize,
    data: usize,
    /// Position just past the INDEX.
    pub end: usize,
}

impl Index {
    pub fn parse(d: &[u8], pos: usize, cff2: bool) -> Option<Index> {
        let mut r = Reader::at(d, pos);
        let count = if cff2 { r.u32()? } else { u32::from(r.u16()?) };
        if count == 0 {
            return Some(Index {
                end: r.pos(),
                ..Index::default()
            });
        }
        let off_size = r.u8()?;
        if !(1..=4).contains(&off_size) {
            return None;
        }
        let offsets = r.pos();
        let table = (count as usize + 1).checked_mul(usize::from(off_size))?;
        let data = (offsets + table).checked_sub(1)?;
        let last =
            Reader::at(d, offsets + count as usize * usize::from(off_size)).uint(off_size)?;
        let end = data.checked_add(last as usize)?;
        if end > d.len() {
            return None;
        }
        Some(Index {
            count,
            off_size,
            offsets,
            data,
            end,
        })
    }

    pub fn range(&self, d: &[u8], i: u32) -> Option<Range<usize>> {
        if i >= self.count {
            return None;
        }
        let at = self.offsets + i as usize * usize::from(self.off_size);
        let mut r = Reader::at(d, at);
        let a = r.uint(self.off_size)? as usize;
        let b = r.uint(self.off_size)? as usize;
        if a == 0 || b < a {
            return None;
        }
        let range = self.data + a..(self.data + b).min(self.end);
        (range.start <= range.end).then_some(range)
    }

    pub fn get<'d>(&self, d: &'d [u8], i: u32) -> Option<&'d [u8]> {
        d.get(self.range(d, i)?)
    }
}

/// Parses DICT data, calling `f(operator, operands)`; two-byte operators are `1200 + b1`.
pub(crate) fn parse_dict(d: &[u8], mut f: impl FnMut(u16, &[f64])) {
    let mut ops: Vec<f64> = Vec::with_capacity(48);
    let mut i = 0;
    while i < d.len() {
        let b = d[i];
        i += 1;
        let v = match b {
            28 => {
                let Some(v) = i16_at(d, i) else { return };
                i += 2;
                f64::from(v)
            }
            29 => {
                let Some(v) = u32_at(d, i) else { return };
                i += 4;
                f64::from(v as i32)
            }
            30 => {
                let (v, n) = real(&d[i..]);
                i += n;
                v
            }
            32..=246 => f64::from(i32::from(b) - 139),
            247..=250 => {
                let Some(&w) = d.get(i) else { return };
                i += 1;
                f64::from((i32::from(b) - 247) * 256 + i32::from(w) + 108)
            }
            251..=254 => {
                let Some(&w) = d.get(i) else { return };
                i += 1;
                f64::from(-(i32::from(b) - 251) * 256 - i32::from(w) - 108)
            }
            12 => {
                let Some(&o) = d.get(i) else { return };
                i += 1;
                f(1200 + u16::from(o), &ops);
                ops.clear();
                continue;
            }
            23 => {
                // CFF2 blend: keep the default values.
                let n = ops.pop().unwrap_or(0.0).max(0.0) as usize;
                if n > 0 && n <= ops.len() {
                    let k = ops.len() / n;
                    let start = ops.len() - n * k;
                    ops.truncate(start + n);
                }
                continue;
            }
            0..=27 => {
                f(u16::from(b), &ops);
                ops.clear();
                continue;
            }
            _ => {
                ops.clear();
                continue;
            }
        };
        if ops.len() < 513 {
            ops.push(v);
        }
    }
}

fn real(d: &[u8]) -> (f64, usize) {
    let mut s = String::new();
    let mut n = 0;
    'outer: for &b in d {
        n += 1;
        for nib in [b >> 4, b & 15] {
            match nib {
                0..=9 => s.push(char::from(b'0' + nib)),
                0xA => s.push('.'),
                0xB => s.push('E'),
                0xC => s.push_str("E-"),
                0xE => s.push('-'),
                0xF => break 'outer,
                _ => {}
            }
        }
        if s.len() > 64 {
            break;
        }
    }
    (s.parse().unwrap_or(0.0), n)
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Private {
    pub subrs: Index,
    pub default_width: f32,
    pub nominal_width: f32,
    pub vsindex: u16,
    /// The private DICT bytes (relative to the table).
    pub range: Range<usize>,
}

#[derive(Clone, Debug)]
pub(crate) struct Fd {
    pub private: Private,
    /// Glyph units to the font's matrix when it differs from it.
    pub transform: Option<Matrix>,
    pub dict: Range<usize>,
}

#[derive(Clone, Debug)]
enum Charset {
    IsoAdobe,
    Expert,
    ExpertSubset,
    /// SID (or CID) per glyph.
    Custom(Vec<u16>),
}

#[derive(Clone, Copy, Debug)]
enum FdSelect {
    None,
    F0(usize),
    F3(usize, u32),
    F4(usize, u32),
}

/// Registry, ordering and supplement of a CID-keyed font.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ros {
    pub registry: String,
    pub ordering: String,
    pub supplement: i32,
}

/// A parsed CFF or CFF2 font (the first font of the FontSet).
#[derive(Debug)]
pub struct Cff {
    data: Arc<[u8]>,
    table: Range<usize>,
    pub(crate) cff2: bool,
    name: Option<String>,
    pub(crate) strings: Index,
    pub(crate) gsubrs: Index,
    pub(crate) charstrings: Index,
    charset: Charset,
    encoding: Option<Box<[u16; 256]>>,
    pub(crate) private: Private,
    pub(crate) fds: Vec<Fd>,
    fd_select: FdSelect,
    ros: Option<Ros>,
    matrix: Matrix,
    bbox: [f32; 4],
    regions: Vec<u16>,
    pub(crate) top_dict: Range<usize>,
    names: OnceLock<HashMap<Box<[u8]>, u16>>,
    cids: OnceLock<Vec<u16>>,
}

#[derive(Default)]
struct Top {
    charset: Option<usize>,
    encoding: Option<usize>,
    charstrings: Option<usize>,
    private: Option<(usize, usize)>,
    matrix: Option<[f64; 6]>,
    ros: Option<(u16, u16, i32)>,
    fd_array: Option<usize>,
    fd_select: Option<usize>,
    vstore: Option<usize>,
    bbox: [f32; 4],
}

fn top_dict(d: &[u8]) -> Top {
    let mut t = Top::default();
    let off = |a: &[f64], i: usize| a.get(i).filter(|v| **v >= 0.0).map(|v| *v as usize);
    parse_dict(d, |op, a| match op {
        15 => t.charset = off(a, 0),
        16 => t.encoding = off(a, 0),
        17 => t.charstrings = off(a, 0),
        18 => t.private = off(a, 0).zip(off(a, 1)),
        5 if a.len() == 4 => t.bbox = [a[0] as f32, a[1] as f32, a[2] as f32, a[3] as f32],
        24 => t.vstore = off(a, 0),
        1207 if a.len() == 6 => t.matrix = a.try_into().ok(),
        1230 if a.len() >= 3 => t.ros = Some((a[0] as u16, a[1] as u16, a[2] as i32)),
        1236 => t.fd_array = off(a, 0),
        1237 => t.fd_select = off(a, 0),
        _ => {}
    });
    t
}

fn private_dict(d: &[u8], range: Range<usize>, cff2: bool) -> Private {
    let mut p = Private {
        range: range.clone(),
        ..Private::default()
    };
    let Some(pd) = d.get(range.clone()) else {
        return p;
    };
    let mut subrs = None;
    parse_dict(pd, |op, a| match op {
        19 => subrs = a.first().filter(|v| **v > 0.0).map(|v| *v as usize),
        20 => p.default_width = a.first().copied().unwrap_or(0.0) as f32,
        21 => p.nominal_width = a.first().copied().unwrap_or(0.0) as f32,
        22 => p.vsindex = a.first().copied().unwrap_or(0.0) as u16,
        _ => {}
    });
    if let Some(s) = subrs
        && let Some(pos) = range.start.checked_add(s)
    {
        p.subrs = Index::parse(d, pos, cff2).unwrap_or_default();
    }
    p
}

impl Cff {
    /// Parses a bare CFF or CFF2 font program.
    pub fn parse(data: impl Into<Arc<[u8]>>) -> Option<Cff> {
        let data: Arc<[u8]> = data.into();
        let n = data.len();
        Cff::parse_range(data, 0..n)
    }

    /// Parses the CFF table at `table` within `data`.
    pub fn parse_range(data: Arc<[u8]>, table: Range<usize>) -> Option<Cff> {
        let d = data.get(table.clone())?;
        let major = u8_at(d, 0)?;
        let hdr = usize::from(u8_at(d, 2)?);
        let cff2 = major == 2;
        if !(1..=2).contains(&major) {
            return None;
        }
        let (name, top_range, gsubrs, strings) = if cff2 {
            let len = usize::from(u16_at(d, 3)?);
            let top = hdr..hdr.checked_add(len)?;
            d.get(top.clone())?;
            let gsubrs = Index::parse(d, top.end, true)?;
            (None, top, gsubrs, Index::default())
        } else {
            let names = Index::parse(d, hdr, false)?;
            let tops = Index::parse(d, names.end, false)?;
            let strings = Index::parse(d, tops.end, false)?;
            let gsubrs = Index::parse(d, strings.end, false).unwrap_or(Index {
                end: strings.end,
                ..Index::default()
            });
            let name = names
                .get(d, 0)
                .map(|b| String::from_utf8_lossy(b).into_owned());
            (name, tops.range(d, 0)?, gsubrs, strings)
        };
        let top = top_dict(&d[top_range.clone()]);
        let charstrings = Index::parse(d, top.charstrings?, cff2)?;
        let num_glyphs = charstrings.count.min(0xFFFF) as u16;
        let top_matrix = top.matrix.map(Matrix);
        let default = Matrix::scale(0.001);
        let mut f = Cff {
            data: data.clone(),
            table: table.clone(),
            cff2,
            name,
            strings,
            gsubrs,
            charstrings,
            charset: Charset::IsoAdobe,
            encoding: None,
            private: Private::default(),
            fds: Vec::new(),
            fd_select: FdSelect::None,
            ros: None,
            matrix: top_matrix.unwrap_or(default),
            bbox: top.bbox,
            regions: Vec::new(),
            top_dict: top_range,
            names: OnceLock::new(),
            cids: OnceLock::new(),
        };
        if let Some((size, off)) = top.private {
            f.private = private_dict(d, off..off.saturating_add(size), cff2);
        }
        if let Some(fda) = top.fd_array {
            let fdi = Index::parse(d, fda, cff2)?;
            let mut matrices = Vec::new();
            for i in 0..fdi.count.min(256) {
                let Some(r) = fdi.range(d, i) else {
                    break;
                };
                let fd = top_dict(&d[r.clone()]);
                let private = fd
                    .private
                    .map(|(size, off)| private_dict(d, off..off.saturating_add(size), cff2))
                    .unwrap_or_default();
                matrices.push(fd.matrix.map(Matrix));
                f.fds.push(Fd {
                    private,
                    transform: None,
                    dict: r,
                });
            }
            // The effective matrix of each font DICT; the font's is the first one's.
            let eff: Vec<Matrix> = matrices
                .iter()
                .map(|m| match (m, top_matrix) {
                    (Some(m), Some(t)) if !cff2 => m.then(&t),
                    (Some(m), _) => *m,
                    (None, t) => t.unwrap_or(default),
                })
                .collect();
            if let Some(first) = eff.first() {
                f.matrix = *first;
                let inv = first.invert();
                for (fd, m) in f.fds.iter_mut().zip(&eff) {
                    if m != first {
                        fd.transform = inv.map(|inv| m.then(&inv));
                    }
                }
            }
            if let Some(fs) = top.fd_select {
                f.fd_select = match u8_at(d, fs) {
                    Some(0) => FdSelect::F0(fs + 1),
                    Some(3) => FdSelect::F3(fs + 3, u32::from(u16_at(d, fs + 1).unwrap_or(0))),
                    Some(4) => FdSelect::F4(fs + 5, u32_at(d, fs + 1).unwrap_or(0)),
                    _ => FdSelect::None,
                };
            }
        }
        if let Some((reg, ord, sup)) = top.ros {
            f.ros = Some(Ros {
                registry: f.string(reg).unwrap_or_default().to_owned(),
                ordering: f.string(ord).unwrap_or_default().to_owned(),
                supplement: sup,
            });
        }
        if let Some(vs) = top.vstore {
            f.regions = regions(d, vs).unwrap_or_default();
        }
        if !cff2 {
            f.charset = match top.charset.unwrap_or(0) {
                0 => Charset::IsoAdobe,
                1 => Charset::Expert,
                2 => Charset::ExpertSubset,
                off => parse_charset(d, off, num_glyphs).unwrap_or(Charset::IsoAdobe),
            };
            if f.ros.is_none() {
                f.encoding = Some(f.parse_encoding(d, top.encoding.unwrap_or(0)));
            }
        }
        Some(f)
    }

    fn d(&self) -> &[u8] {
        &self.data[self.table.clone()]
    }

    pub(crate) fn table_data(&self) -> &[u8] {
        self.d()
    }

    pub fn is_cff2(&self) -> bool {
        self.cff2
    }

    /// The font's name from the Name INDEX.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn num_glyphs(&self) -> u16 {
        self.charstrings.count.min(0xFFFF) as u16
    }

    pub fn font_matrix(&self) -> Matrix {
        self.matrix
    }

    pub fn bbox(&self) -> [f32; 4] {
        self.bbox
    }

    /// Registry-ordering-supplement of a CID-keyed font.
    pub fn ros(&self) -> Option<&Ros> {
        self.ros.as_ref()
    }

    pub fn is_cid(&self) -> bool {
        self.ros.is_some()
    }

    /// String `sid` (standard or from the String INDEX).
    pub fn string(&self, sid: u16) -> Option<&str> {
        if sid < data::CFF_STANDARD_STRINGS {
            return data::cff_standard_string(sid);
        }
        let b = self
            .strings
            .get(self.d(), u32::from(sid - data::CFF_STANDARD_STRINGS))?;
        std::str::from_utf8(b).ok()
    }

    /// The SID (name-keyed) or CID (CID-keyed) of a glyph.
    pub fn charset_id(&self, gid: u16) -> Option<u16> {
        if gid >= self.num_glyphs() {
            return None;
        }
        match &self.charset {
            Charset::IsoAdobe => (gid < 229).then_some(gid),
            Charset::Expert => data::cff_expert_charset().get(usize::from(gid)).copied(),
            Charset::ExpertSubset => data::cff_expert_subset_charset()
                .get(usize::from(gid))
                .copied(),
            Charset::Custom(v) => v.get(usize::from(gid)).copied(),
        }
    }

    pub fn glyph_name(&self, gid: u16) -> Option<&str> {
        if self.is_cid() || self.cff2 {
            return None;
        }
        self.string(self.charset_id(gid)?)
    }

    pub fn glyph_by_name(&self, name: &[u8]) -> Option<u16> {
        if self.is_cid() || self.cff2 {
            return None;
        }
        let m = self.names.get_or_init(|| {
            let mut m = HashMap::with_capacity(usize::from(self.num_glyphs()));
            for g in (0..self.num_glyphs()).rev() {
                if let Some(n) = self.glyph_name(g) {
                    m.insert(n.as_bytes().into(), g);
                }
            }
            m
        });
        m.get(name).copied()
    }

    /// The glyph of a CID (CID-keyed fonts); other fonts use CID = GID.
    pub fn glyph_by_cid(&self, cid: u16) -> Option<u16> {
        if !self.is_cid() {
            return (cid < self.num_glyphs()).then_some(cid);
        }
        let v = self.cids.get_or_init(|| {
            let mut v = Vec::new();
            for g in 0..self.num_glyphs() {
                let Some(c) = self.charset_id(g) else { break };
                let c = usize::from(c);
                if c >= v.len() {
                    v.resize(c + 1, u16::MAX);
                }
                if v[c] == u16::MAX {
                    v[c] = g;
                }
            }
            v
        });
        v.get(usize::from(cid)).copied().filter(|&g| g != u16::MAX)
    }

    /// The glyph of `code` in the font's built-in encoding.
    pub fn encoding_glyph(&self, code: u8) -> Option<u16> {
        self.encoding
            .as_ref()
            .map(|e| e[usize::from(code)])
            .filter(|&g| g != 0)
    }

    fn parse_encoding(&self, d: &[u8], off: usize) -> Box<[u16; 256]> {
        let mut e = Box::new([0u16; 256]);
        let by_name = |n: &str| self.glyph_by_name(n.as_bytes()).unwrap_or(0);
        match off {
            0 | 1 => {
                let enc = if off == 0 {
                    data::BaseEncoding::Standard
                } else {
                    data::BaseEncoding::MacExpert
                };
                for c in 0..=255u8 {
                    if let Some(n) = enc.glyph_name(c) {
                        e[usize::from(c)] = by_name(n);
                    }
                }
            }
            _ => {
                let mut r = Reader::at(d, off);
                let Some(format) = r.u8() else { return e };
                let n = r.u8().unwrap_or(0);
                let mut gid = 1u16;
                match format & 0x7F {
                    0 => {
                        for _ in 0..n {
                            let Some(c) = r.u8() else { break };
                            e[usize::from(c)] = gid;
                            gid += 1;
                        }
                    }
                    1 => {
                        for _ in 0..n {
                            let (Some(first), Some(left)) = (r.u8(), r.u8()) else {
                                break;
                            };
                            for c in u16::from(first)..=u16::from(first) + u16::from(left) {
                                if c < 256 {
                                    e[usize::from(c)] = gid;
                                }
                                gid += 1;
                            }
                        }
                    }
                    _ => return e,
                }
                if format & 0x80 != 0 {
                    let ns = r.u8().unwrap_or(0);
                    for _ in 0..ns {
                        let (Some(c), Some(sid)) = (r.u8(), r.u16()) else {
                            break;
                        };
                        if let Some(n) = self.string(sid) {
                            e[usize::from(c)] = by_name(n);
                        }
                    }
                }
                for g in &mut e[..] {
                    if *g >= self.num_glyphs() {
                        *g = 0;
                    }
                }
            }
        }
        e
    }

    /// The private DICT and transform that apply to `gid`.
    pub(crate) fn fd_index(&self, gid: u16) -> usize {
        let d = self.d();
        let g = u32::from(gid);
        let fd = match self.fd_select {
            FdSelect::None => 0,
            FdSelect::F0(p) => usize::from(u8_at(d, p + usize::from(gid)).unwrap_or(0)),
            FdSelect::F3(p, n) => ranges(
                n,
                |i| {
                    Some((
                        u32::from(u16_at(d, p + i * 3)?),
                        u16::from(u8_at(d, p + i * 3 + 2)?),
                    ))
                },
                g,
            ),
            FdSelect::F4(p, n) => ranges(
                n,
                |i| Some((u32_at(d, p + i * 6)?, u16_at(d, p + i * 6 + 4)?)),
                g,
            ),
        };
        if fd < self.fds.len() { fd } else { 0 }
    }

    fn private_for(&self, gid: u16) -> (&Private, Option<Matrix>) {
        match self.fds.get(self.fd_index(gid)) {
            Some(fd) => (&fd.private, fd.transform),
            None => (&self.private, None),
        }
    }

    fn run(
        &self,
        gid: u16,
        pen: &mut Pen,
        origin: (f32, f32),
        depth: u8,
    ) -> Option<charstring::Result> {
        let d = self.d();
        let cs = self.charstrings.get(d, u32::from(gid))?;
        let (p, _) = self.private_for(gid);
        let ctx = charstring::Ctx {
            data: d,
            gsubrs: &self.gsubrs,
            lsubrs: &p.subrs,
            cff2: self.cff2,
            regions: &self.regions,
            vsindex: p.vsindex,
        };
        let r = charstring::run(&ctx, cs, pen, origin)?;
        if let Some(s) = r.seac
            && depth == 0
        {
            let std = data::BaseEncoding::Standard;
            let glyph = |c: u8| {
                std.glyph_name(c)
                    .and_then(|n| self.glyph_by_name(n.as_bytes()))
            };
            if let Some(b) = glyph(s.base) {
                self.run(b, pen, (0.0, 0.0), 1)?;
            }
            if let Some(a) = glyph(s.accent) {
                self.run(a, pen, (s.adx, s.ady), 1)?;
            }
        }
        Some(r)
    }

    /// Draws glyph `gid` in font units (see [`Cff::font_matrix`]).
    pub fn outline(&self, gid: u16, sink: &mut dyn OutlineSink) -> bool {
        let (_, m) = self.private_for(gid);
        let mut pen = Pen::new(sink, m);
        self.run(gid, &mut pen, (0.0, 0.0), 0).is_some()
    }

    /// The advance width from the charstring (CFF only).
    pub fn advance(&self, gid: u16) -> Option<f32> {
        if self.cff2 {
            return None;
        }
        let mut sink = NullSink;
        let mut pen = Pen::new(&mut sink, None);
        let r = self.run(gid, &mut pen, (0.0, 0.0), 1)?;
        let (p, _) = self.private_for(gid);
        Some(r.width.map_or(p.default_width, |w| w + p.nominal_width))
    }
}

fn ranges(n: u32, at: impl Fn(usize) -> Option<(u32, u16)>, g: u32) -> usize {
    let (mut lo, mut hi) = (0usize, n as usize);
    while lo < hi {
        let mid = (lo + hi) / 2;
        let Some((first, fd)) = at(mid) else { return 0 };
        let next = at(mid + 1).map_or(u32::MAX, |r| r.0);
        if g < first {
            hi = mid;
        } else if g >= next {
            lo = mid + 1;
        } else {
            return usize::from(fd);
        }
    }
    0
}

fn parse_charset(d: &[u8], off: usize, n: u16) -> Option<Charset> {
    let mut r = Reader::at(d, off);
    let format = r.u8()?;
    let mut v = Vec::with_capacity(usize::from(n));
    v.push(0);
    while v.len() < usize::from(n) {
        match format {
            0 => match r.u16() {
                Some(s) => v.push(s),
                None => break,
            },
            1 | 2 => {
                let (Some(first), Some(left)) = (
                    r.u16(),
                    if format == 1 {
                        r.u8().map(u16::from)
                    } else {
                        r.u16()
                    },
                ) else {
                    break;
                };
                for i in 0..=u32::from(left) {
                    if v.len() >= usize::from(n) {
                        break;
                    }
                    v.push((u32::from(first) + i).min(0xFFFF) as u16);
                }
            }
            _ => return None,
        }
    }
    Some(Charset::Custom(v))
}

/// Region counts of each ItemVariationData (CFF2 `vstore`, an offset from the font's
/// data, which may be anything).
fn regions(d: &[u8], vstore: usize) -> Option<Vec<u16>> {
    // The ItemVariationStore follows the store's two-byte length. Within the data, the
    // additions below cannot overflow.
    let ivs = vstore.checked_add(2).filter(|&o| o <= d.len())?;
    let n = u16_at(d, ivs + 6)?;
    let mut out = Vec::with_capacity(usize::from(n));
    for i in 0..usize::from(n) {
        let off = u32_at(d, ivs + 8 + i * 4)? as usize;
        out.push(u16_at(d, ivs.checked_add(off)?.checked_add(4)?)?);
    }
    Some(out)
}
