//! TrueType `glyf` outlines (quadratic B-splines) and composite glyphs.

use crate::outline::OutlineSink;
use crate::reader::{Reader, i16_at, u16_at, u32_at};

const MAX_DEPTH: u32 = 8;
const MAX_POINTS: usize = 0x10000;

const ARG_WORDS: u16 = 0x0001;
const ARGS_XY: u16 = 0x0002;
const SCALE: u16 = 0x0008;
const MORE: u16 = 0x0020;
const XY_SCALE: u16 = 0x0040;
const TWO_BY_TWO: u16 = 0x0080;
const SCALED_OFFSET: u16 = 0x0800;

#[derive(Clone, Copy, Debug)]
pub struct Loca<'a> {
    pub loca: &'a [u8],
    pub glyf: &'a [u8],
    pub long: bool,
}

impl<'a> Loca<'a> {
    pub fn glyph(&self, gid: u16) -> Option<&'a [u8]> {
        let i = usize::from(gid);
        let (a, b) = if self.long {
            (
                u32_at(self.loca, i * 4)? as usize,
                u32_at(self.loca, i * 4 + 4)? as usize,
            )
        } else {
            (
                usize::from(u16_at(self.loca, i * 2)?) * 2,
                usize::from(u16_at(self.loca, i * 2 + 2)?) * 2,
            )
        };
        if a >= b {
            return Some(&[]);
        }
        // Some fonts have a last offset past the table end.
        self.glyf.get(a..b.min(self.glyf.len()))
    }

    pub fn num_glyphs(&self) -> usize {
        (self.loca.len() / if self.long { 4 } else { 2 }).saturating_sub(1)
    }
}

/// A decoded glyph: points with on-curve flags and contour end indices.
#[derive(Default, Debug)]
pub struct Points {
    pub pts: Vec<(f32, f32, bool)>,
    pub ends: Vec<usize>,
    flags: Vec<u8>,
}

impl Points {
    pub fn clear(&mut self) {
        self.pts.clear();
        self.ends.clear();
    }
}

pub fn decode(loca: &Loca, gid: u16, out: &mut Points) -> Option<()> {
    decode_at(loca, gid, out, 0, &mut 4096)
}

fn decode_at(loca: &Loca, gid: u16, out: &mut Points, depth: u32, budget: &mut u32) -> Option<()> {
    if depth > MAX_DEPTH || *budget == 0 || out.pts.len() > MAX_POINTS {
        return None;
    }
    *budget -= 1;
    let g = loca.glyph(gid)?;
    if g.len() < 10 {
        return Some(());
    }
    let contours = i16_at(g, 0)?;
    if contours >= 0 {
        simple(g, contours as usize, out)
    } else {
        composite(loca, g, out, depth, budget)
    }
}

fn simple(g: &[u8], contours: usize, out: &mut Points) -> Option<()> {
    let mut r = Reader::at(g, 10);
    let base = out.pts.len();
    let mut n = 0usize;
    let ends_start = out.ends.len();
    for _ in 0..contours {
        let e = usize::from(r.u16()?) + 1;
        if e < n {
            return None;
        }
        n = e;
        out.ends.push(base + e - 1);
    }
    if n == 0 {
        out.ends.truncate(ends_start);
        return Some(());
    }
    if base + n > MAX_POINTS {
        return None;
    }
    let ilen = usize::from(r.u16()?);
    r.skip(ilen)?;
    let mut flags = std::mem::take(&mut out.flags);
    flags.clear();
    while flags.len() < n {
        let f = r.u8()?;
        flags.push(f);
        if f & 8 != 0 {
            let rep = r.u8()?;
            for _ in 0..rep {
                if flags.len() == n {
                    break;
                }
                flags.push(f);
            }
        }
    }
    out.pts.reserve(n);
    let mut x = 0i32;
    for &f in &flags {
        if f & 2 != 0 {
            let d = i32::from(r.u8()?);
            x += if f & 0x10 != 0 { d } else { -d };
        } else if f & 0x10 == 0 {
            x += i32::from(r.i16()?);
        }
        out.pts.push((x as f32, 0.0, f & 1 != 0));
    }
    let mut y = 0i32;
    for (i, &f) in flags.iter().enumerate() {
        if f & 4 != 0 {
            let d = i32::from(r.u8()?);
            y += if f & 0x20 != 0 { d } else { -d };
        } else if f & 0x20 == 0 {
            y += i32::from(r.i16()?);
        }
        out.pts[base + i].1 = y as f32;
    }
    out.flags = flags;
    Some(())
}

fn composite(loca: &Loca, g: &[u8], out: &mut Points, depth: u32, budget: &mut u32) -> Option<()> {
    let mut r = Reader::at(g, 10);
    loop {
        let flags = r.u16()?;
        let gid = r.u16()?;
        let (a1, a2) = if flags & ARG_WORDS != 0 {
            if flags & ARGS_XY != 0 {
                (i32::from(r.i16()?), i32::from(r.i16()?))
            } else {
                (i32::from(r.u16()?), i32::from(r.u16()?))
            }
        } else if flags & ARGS_XY != 0 {
            (i32::from(r.i8()?), i32::from(r.i8()?))
        } else {
            (i32::from(r.u8()?), i32::from(r.u8()?))
        };
        let f2 = |v: i16| f32::from(v) / 16384.0;
        let (mut a, mut b, mut c, mut d) = (1.0, 0.0, 0.0, 1.0);
        if flags & SCALE != 0 {
            a = f2(r.i16()?);
            d = a;
        } else if flags & XY_SCALE != 0 {
            a = f2(r.i16()?);
            d = f2(r.i16()?);
        } else if flags & TWO_BY_TWO != 0 {
            a = f2(r.i16()?);
            b = f2(r.i16()?);
            c = f2(r.i16()?);
            d = f2(r.i16()?);
        }
        let start = out.pts.len();
        let ends_start = out.ends.len();
        if decode_at(loca, gid, out, depth + 1, budget).is_none() {
            out.pts.truncate(start);
            out.ends.truncate(ends_start);
        }
        for p in &mut out.pts[start..] {
            let (x, y) = (p.0, p.1);
            p.0 = a * x + c * y;
            p.1 = b * x + d * y;
        }
        let (dx, dy) = if flags & ARGS_XY != 0 {
            let (mut dx, mut dy) = (a1 as f32, a2 as f32);
            if flags & SCALED_OFFSET != 0 {
                (dx, dy) = (a * dx + c * dy, b * dx + d * dy);
            }
            (dx, dy)
        } else {
            // Point matching: parent point a1 meets component point a2.
            let p = out.pts.get(a1 as usize).filter(|_| (a1 as usize) < start);
            let q = out.pts.get(start + a2 as usize);
            match (p, q) {
                (Some(p), Some(q)) => (p.0 - q.0, p.1 - q.1),
                _ => (0.0, 0.0),
            }
        };
        if dx != 0.0 || dy != 0.0 {
            for p in &mut out.pts[start..] {
                p.0 += dx;
                p.1 += dy;
            }
        }
        if flags & MORE == 0 {
            break;
        }
    }
    Some(())
}

/// Emits the contours of `p` as move/line/quad/close.
pub fn emit(p: &Points, sink: &mut dyn OutlineSink) {
    let mut s = 0;
    for &e in &p.ends {
        if e < s || e >= p.pts.len() {
            break;
        }
        contour(&p.pts[s..=e], sink);
        s = e + 1;
    }
}

fn contour(c: &[(f32, f32, bool)], sink: &mut dyn OutlineSink) {
    let n = c.len();
    if n == 0 {
        return;
    }
    let mid = |a: (f32, f32, bool), b: (f32, f32, bool)| ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
    // Start at an on-curve point, or the midpoint of the first two off-curve points.
    let first_on = c.iter().position(|p| p.2);
    let (start, offset) = match first_on {
        Some(i) => ((c[i].0, c[i].1), i),
        None => (mid(c[n - 1], c[0]), 0),
    };
    sink.move_to(start.0, start.1);
    let mut ctrl: Option<(f32, f32)> = None;
    let count = if first_on.is_some() { n - 1 } else { n };
    for k in 1..=count {
        let p = c[(offset + k) % n];
        if p.2 {
            match ctrl.take() {
                Some(q) => sink.quad_to(q.0, q.1, p.0, p.1),
                None => sink.line_to(p.0, p.1),
            }
        } else {
            if let Some(q) = ctrl {
                let m = ((q.0 + p.0) / 2.0, (q.1 + p.1) / 2.0);
                sink.quad_to(q.0, q.1, m.0, m.1);
            }
            ctrl = Some((p.0, p.1));
        }
    }
    if let Some(q) = ctrl {
        sink.quad_to(q.0, q.1, start.0, start.1);
    }
    sink.close();
}

/// The glyphs a composite glyph refers to.
pub fn components(g: &[u8]) -> Vec<u16> {
    let mut out = Vec::new();
    if i16_at(g, 0).is_none_or(|c| c >= 0) {
        return out;
    }
    let mut r = Reader::at(g, 10);
    while let (Some(flags), Some(gid)) = (r.u16(), r.u16()) {
        out.push(gid);
        let mut skip = if flags & ARG_WORDS != 0 { 4 } else { 2 };
        if flags & SCALE != 0 {
            skip += 2;
        } else if flags & XY_SCALE != 0 {
            skip += 4;
        } else if flags & TWO_BY_TWO != 0 {
            skip += 8;
        }
        if r.skip(skip).is_none() || flags & MORE == 0 || out.len() > 1024 {
            break;
        }
    }
    out
}
