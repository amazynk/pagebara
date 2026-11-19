//! Signed-area accumulation: each line adds the exact area it sweeps to the cells it crosses
//! and its cover to the cell right of it; a prefix sum along a row then gives the winding
//! integral over every pixel. Lines mark the 8-pixel blocks they touch, so the runs between
//! edges come out as constant spans without per-pixel work.

use std::simd::StdFloat;
use std::simd::prelude::*;

use pagebara_content::display::FillRule;

use super::{Edges, IRect, Line};

/// Rows processed together; bounds the accumulation buffer for large paths.
const BAND: i32 = 32;
/// Pixels per mark bit.
const BLOCK: usize = 8;

/// Coverage of one row segment.
#[derive(Clone, Copy, Debug)]
pub enum Span<'a> {
    /// Per-pixel coverage.
    Cov(&'a [u8]),
    /// `n` pixels of one coverage.
    Run(u32, u8),
}

impl Span<'_> {
    pub fn len(&self) -> usize {
        match self {
            Span::Cov(c) => c.len(),
            Span::Run(n, _) => *n as usize,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The accumulation cells of one band and their block marks.
#[derive(Debug, Default)]
struct Grid {
    acc: Vec<f32>,
    marks: Vec<u64>,
    /// Cells per row (a whole number of blocks, with room right of the clip).
    stride: usize,
    /// Mark words per row.
    wpr: usize,
    /// Clip width in pixels.
    w: f32,
}

impl Grid {
    /// Sizes the grid. Cells and marks are all zero between fills (resolving clears
    /// them), so only growth needs initialising.
    fn reset(&mut self, w: usize, rows: usize) {
        self.stride = (w + 2).next_multiple_of(BLOCK);
        self.wpr = (self.stride / BLOCK).div_ceil(64);
        self.w = w as f32;
        let (cells, words) = (self.stride * rows, self.wpr * rows);
        if self.acc.len() < cells {
            self.acc.resize(cells, 0.0);
        }
        if self.marks.len() < words {
            self.marks.resize(words, 0);
        }
    }

    #[inline]
    fn mark(&mut self, y: usize, c0: usize, c1: usize) {
        let row = &mut self.marks[y * self.wpr..(y + 1) * self.wpr];
        for b in c0 / BLOCK..=c1 / BLOCK {
            row[b / 64] |= 1 << (b % 64);
        }
    }

    /// Adds a line clipped to `0..w` × `0..h`: parts left of the buffer become vertical
    /// lines on its left edge (they still change the winding), parts right of it are dropped.
    fn clip_line(&mut self, l: Line, h: f32) {
        let w = self.w;
        let (mut x0, mut y0, mut x1, mut y1, dir) = if l.y0 < l.y1 {
            (l.x0, l.y0, l.x1, l.y1, 1.0)
        } else {
            (l.x1, l.y1, l.x0, l.y0, -1.0)
        };
        if y1 <= 0.0 || y0 >= h || !(x0.is_finite() && x1.is_finite()) {
            return;
        }
        let dxdy = (x1 - x0) / (y1 - y0);
        if y0 < 0.0 {
            x0 -= y0 * dxdy;
            y0 = 0.0;
        }
        if y1 > h {
            x1 -= (y1 - h) * dxdy;
            y1 = h;
        }
        if x0.min(x1) >= 0.0 && x0.max(x1) <= w {
            return self.line(x0, y0, x1, y1, dir);
        }
        // Split where the line crosses x = 0 and x = w.
        let mut ts = [0.0f32, 1.0, 1.0, 1.0];
        let mut n = 1;
        let dx = x1 - x0;
        for edge in [0.0, w] {
            let t = (edge - x0) / dx;
            if t > 0.0 && t < 1.0 {
                ts[n] = t;
                n += 1;
            }
        }
        ts[n] = 1.0;
        ts[1..n].sort_unstable_by(f32::total_cmp);
        let dy = y1 - y0;
        for k in 0..n {
            let (ta, tb) = (ts[k], ts[k + 1]);
            let (ya, yb) = (y0 + dy * ta, y0 + dy * tb);
            if yb <= ya {
                continue;
            }
            let mid = x0 + dx * (ta + tb) * 0.5;
            if mid >= w {
                continue;
            }
            if mid <= 0.0 {
                self.line(0.0, ya, 0.0, yb, dir);
            } else {
                let xa = (x0 + dx * ta).clamp(0.0, w);
                let xb = (x0 + dx * tb).clamp(0.0, w);
                self.line(xa, ya, xb, yb, dir);
            }
        }
    }

    /// Exact area coverage of a line with `y0 < y1`, both ends inside the buffer.
    #[inline]
    fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, dir: f32) {
        let dxdy = (x1 - x0) / (y1 - y0);
        let (w, stride) = (self.w, self.stride);
        let mut x = x0;
        let ys = y0 as usize;
        let ye = (y1.ceil() as usize).min(self.acc.len() / stride);
        for y in ys..ye {
            let row = &mut self.acc[y * stride..(y + 1) * stride];
            let fy = y as f32;
            let dy = (fy + 1.0).min(y1) - fy.max(y0);
            let xnext = (x + dxdy * dy).clamp(0.0, w);
            let d = dy * dir;
            let (xa, xb) = if x < xnext { (x, xnext) } else { (xnext, x) };
            let xaf = xa.floor();
            let xai = xaf as usize;
            let xbi = xb.ceil() as usize;
            if xbi <= xai + 1 {
                let xmf = 0.5 * (x + xnext) - xaf;
                row[xai] += d - d * xmf;
                row[xai + 1] += d * xmf;
                self.mark(y, xai, xai + 1);
            } else {
                let s = 1.0 / (xb - xa);
                let x0f = xa - xaf;
                let a0 = 0.5 * s * (1.0 - x0f) * (1.0 - x0f);
                let x1f = xb - xb.ceil() + 1.0;
                let am = 0.5 * s * x1f * x1f;
                row[xai] += d * a0;
                if xbi == xai + 2 {
                    row[xai + 1] += d * (1.0 - a0 - am);
                } else {
                    let a1 = s * (1.5 - x0f);
                    row[xai + 1] += d * (a1 - a0);
                    for c in &mut row[xai + 2..xbi - 1] {
                        *c += d * s;
                    }
                    let a2 = a1 + (xbi - xai - 3) as f32 * s;
                    row[xbi - 1] += d * (1.0 - a2 - am);
                }
                row[xbi] += d * am;
                self.mark(y, xai, xbi);
            }
            x = xnext;
        }
    }

    /// Resolves row `j` into spans of the `w` pixels from `x0`, clearing it.
    fn resolve(
        &mut self,
        j: usize,
        rule: FillRule,
        y: i32,
        x0: i32,
        out: &mut [u8],
        sink: &mut dyn FnMut(i32, i32, Span<'_>),
    ) {
        let w = self.w as usize;
        let nb = w.div_ceil(BLOCK);
        let acc = &mut self.acc[j * self.stride..(j + 1) * self.stride];
        let marks = &mut self.marks[j * self.wpr..(j + 1) * self.wpr];
        let marked = |b: usize| marks[b / 64] >> (b % 64) & 1 != 0;
        let mut sum = 0.0f32;
        let mut b = 0;
        while b < nb {
            let start = b;
            if !marked(b) {
                b = next_mark(marks, b, true).min(nb);
                let c = coverage(sum, rule);
                if c != 0 {
                    let (s, e) = (start * BLOCK, (b * BLOCK).min(w));
                    sink(y, x0 + s as i32, Span::Run((e - s) as u32, c));
                }
                continue;
            }
            let end = next_mark(marks, b, false).min(nb);
            while b < end {
                let o = b * BLOCK;
                let cells: &mut [f32; BLOCK] = (&mut acc[o..o + BLOCK]).try_into().expect("block");
                let dst: &mut [u8; BLOCK] = (&mut out[o..o + BLOCK]).try_into().expect("block");
                sum = block(cells, dst, sum, rule);
                b += 1;
            }
            let (s, e) = (start * BLOCK, (b * BLOCK).min(w));
            let seg = &out[s..e];
            if let Some(first) = seg.iter().position(|&c| c != 0) {
                let last = seg.iter().rposition(|&c| c != 0).unwrap_or(first);
                sink(y, x0 + (s + first) as i32, Span::Cov(&seg[first..=last]));
            }
        }
        // Cells right of the clip may hold contributions of lines on its edge.
        acc[nb * BLOCK..].fill(0.0);
        marks.fill(0);
    }
}

/// The first block at or after `b` that is marked (`set`) or unmarked (`!set`).
#[inline]
fn next_mark(marks: &[u64], b: usize, set: bool) -> usize {
    let mut i = b / 64;
    let flip = if set { 0 } else { u64::MAX };
    let mut word = (marks[i] ^ flip) >> (b % 64);
    if word != 0 {
        return b + word.trailing_zeros() as usize;
    }
    loop {
        i += 1;
        if i >= marks.len() {
            return marks.len() * 64;
        }
        word = marks[i] ^ flip;
        if word != 0 {
            return i * 64 + word.trailing_zeros() as usize;
        }
    }
}

#[inline]
fn coverage(sum: f32, rule: FillRule) -> u8 {
    let a = sum.abs();
    let cov = match rule {
        FillRule::NonZero => a.min(1.0),
        FillRule::EvenOdd => {
            let m = a % 2.0;
            if m > 1.0 { 2.0 - m } else { m }
        }
    };
    (cov * 255.0 + 0.5) as u8
}

/// Prefix sum of one block from `sum` into coverage; clears the cells. Returns the sum.
#[inline]
fn block(cells: &mut [f32; BLOCK], out: &mut [u8; BLOCK], sum: f32, rule: FillRule) -> f32 {
    let mut v = f32x8::from_array(*cells);
    // In-register prefix sum: log2(8) shifted adds.
    v += simd_swizzle!(v, f32x8::splat(0.0), [8, 0, 1, 2, 3, 4, 5, 6]);
    v += simd_swizzle!(v, f32x8::splat(0.0), [8, 9, 0, 1, 2, 3, 4, 5]);
    v += simd_swizzle!(v, f32x8::splat(0.0), [8, 9, 10, 11, 0, 1, 2, 3]);
    v += f32x8::splat(sum);
    *cells = [0.0; BLOCK];
    let a = v.abs();
    let (half, one, two) = (f32x8::splat(0.5), f32x8::splat(1.0), f32x8::splat(2.0));
    let cov = match rule {
        FillRule::NonZero => a.simd_min(one),
        FillRule::EvenOdd => {
            let m = a - (a * half).floor() * two;
            m.simd_gt(one).select(two - m, m)
        }
    };
    *out = (cov * f32x8::splat(255.0) + half).cast::<u8>().to_array();
    v[BLOCK - 1]
}

/// Reusable scratch for scan conversion (one per thread).
#[derive(Debug, Default)]
pub struct Rasterizer {
    grid: Grid,
    row: Vec<u8>,
    order: Vec<(f32, u32)>,
    active: Vec<u32>,
}

impl Rasterizer {
    pub fn new() -> Self {
        Rasterizer::default()
    }

    /// Calls `sink(y, x, span)` for the covered spans of each row of the fill inside `clip`,
    /// left to right.
    pub fn fill(
        &mut self,
        e: &Edges,
        rule: FillRule,
        clip: IRect,
        sink: &mut dyn FnMut(i32, i32, Span<'_>),
    ) {
        let r = e.pixel_bounds().intersect(&clip);
        if r.is_empty() || e.lines.is_empty() {
            return;
        }
        if let Some(b) = rect_of(e) {
            return fill_rect(b, r, sink);
        }
        let w = r.width() as usize;
        let bh = BAND.min(r.height()) as usize;
        self.grid.reset(w, bh);
        if self.row.len() < self.grid.stride {
            self.row.resize(self.grid.stride, 0);
        }
        self.order.clear();
        self.order.extend(
            e.lines
                .iter()
                .enumerate()
                .map(|(i, l)| (l.y0.min(l.y1), i as u32)),
        );
        self.order.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
        self.active.clear();
        let mut next = 0;
        let fx = r.x0 as f32;
        let mut by = r.y0;
        while by < r.y1 {
            let h = (r.y1 - by).min(bh as i32);
            let band_end = (by + h) as f32;
            while next < self.order.len() && self.order[next].0 < band_end {
                self.active.push(self.order[next].1);
                next += 1;
            }
            let fy = by as f32;
            let mut k = 0;
            while k < self.active.len() {
                let l = e.lines[self.active[k] as usize];
                if l.y0.max(l.y1) <= fy {
                    self.active.swap_remove(k);
                    continue;
                }
                let l = Line {
                    x0: l.x0 - fx,
                    y0: l.y0 - fy,
                    x1: l.x1 - fx,
                    y1: l.y1 - fy,
                };
                self.grid.clip_line(l, h as f32);
                k += 1;
            }
            for j in 0..h as usize {
                self.grid
                    .resolve(j, rule, by + j as i32, r.x0, &mut self.row, sink);
            }
            by += h;
        }
    }
}

/// An axis-aligned rectangle: partial edge pixels and a constant run between.
fn fill_rect(b: [f32; 4], r: IRect, sink: &mut dyn FnMut(i32, i32, Span<'_>)) {
    let cover = |lo: f32, hi: f32, p: i32| {
        let p = p as f32;
        (hi.min(p + 1.0) - lo.max(p)).clamp(0.0, 1.0)
    };
    let (x0, x1) = (r.x0, r.x1);
    for y in r.y0..r.y1 {
        let v = cover(b[1], b[3], y);
        if v <= 0.0 {
            continue;
        }
        let c = |x: i32| (cover(b[0], b[2], x) * v * 255.0 + 0.5) as u8;
        if x1 - x0 <= 2 {
            let row: Vec<u8> = (x0..x1).map(c).collect();
            sink(y, x0, Span::Cov(&row));
            continue;
        }
        let (l, rr) = (c(x0), c(x1 - 1));
        let mid = c(x0 + 1);
        if l != 0 {
            sink(y, x0, Span::Cov(&[l]));
        }
        if mid != 0 {
            sink(y, x0 + 1, Span::Run((x1 - x0 - 2) as u32, mid));
        }
        if rr != 0 {
            sink(y, x1 - 1, Span::Cov(&[rr]));
        }
    }
}

/// The rectangle if the edges are exactly one axis-aligned rectangle.
fn rect_of(e: &Edges) -> Option<[f32; 4]> {
    let b = e.as_rect()?;
    let ok = e.lines.iter().all(|l| {
        let (lo, hi) = (l.y0.min(l.y1), l.y0.max(l.y1));
        lo == b[1] && hi == b[3]
    });
    let (a, c) = (e.lines[0], e.lines[1]);
    (ok && (a.y1 - a.y0).signum() != (c.y1 - c.y0).signum()).then_some(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raster::Flattener;
    use pagebara_content::{Matrix, Point};

    fn cov(e: &Edges, rule: FillRule, clip: IRect) -> Vec<Vec<u8>> {
        let mut out = vec![vec![0u8; clip.width() as usize]; clip.height() as usize];
        Rasterizer::new().fill(e, rule, clip, &mut |y, x, s| {
            let row = &mut out[(y - clip.y0) as usize];
            let dst = &mut row[(x - clip.x0) as usize..][..s.len()];
            match s {
                Span::Cov(c) => dst.copy_from_slice(c),
                Span::Run(_, v) => dst.fill(v),
            }
        });
        out
    }

    fn poly(pts: &[(f32, f32)]) -> Edges {
        let mut e = Edges::new();
        let mut f = Flattener::new(&mut e, Matrix::IDENTITY);
        f.move_to(Point::new(pts[0].0, pts[0].1));
        for p in &pts[1..] {
            f.line_to(Point::new(p.0, p.1));
        }
        f.close();
        e
    }

    #[test]
    fn half_pixel_square() {
        let e = poly(&[(0.5, 0.0), (2.0, 0.0), (2.0, 1.0), (0.5, 1.0)]);
        let c = cov(&e, FillRule::NonZero, IRect::new(0, 0, 3, 1));
        assert_eq!(c[0], vec![128, 255, 0]);
    }

    #[test]
    fn triangle_area_sums() {
        let e = poly(&[(0.0, 0.0), (10.0, 0.0), (0.0, 10.0)]);
        let c = cov(&e, FillRule::NonZero, IRect::new(0, 0, 10, 10));
        let total: f32 = c.iter().flatten().map(|&v| v as f32 / 255.0).sum();
        assert!((total - 50.0).abs() < 0.2, "{total}");
    }

    #[test]
    fn clipped_left_keeps_winding() {
        let e = poly(&[
            (-5.0, 0.0),
            (5.0, 0.0),
            (5.0, 4.0),
            (-5.0, 4.0),
            (-3.0, 2.0),
        ]);
        let c = cov(&e, FillRule::NonZero, IRect::new(0, 0, 8, 4));
        assert!(
            c.iter()
                .all(|r| r[..5].iter().all(|&v| v == 255) && r[5] == 0)
        );
    }

    #[test]
    fn even_odd_hole() {
        let mut e = poly(&[(0.0, 0.0), (6.0, 0.0), (6.0, 6.0), (0.0, 6.0)]);
        let inner = poly(&[(2.0, 2.0), (4.0, 2.0), (4.0, 4.0), (2.0, 4.0)]);
        e.lines.extend(inner.lines);
        let c = cov(&e, FillRule::EvenOdd, IRect::new(0, 0, 6, 6));
        assert_eq!(c[3], vec![255, 255, 0, 0, 255, 255]);
        let c = cov(&e, FillRule::NonZero, IRect::new(0, 0, 6, 6));
        assert_eq!(c[3], vec![255; 6]);
    }

    #[test]
    fn bands_agree() {
        let mut e = Edges::new();
        let mut f = Flattener::new(&mut e, Matrix::IDENTITY);
        f.move_to(Point::new(3.3, 1.7));
        f.cubic_to(
            Point::new(90.0, -20.0),
            Point::new(140.0, 160.0),
            Point::new(10.0, 99.0),
        );
        f.close();
        let full = cov(&e, FillRule::NonZero, IRect::new(0, 0, 150, 150));
        let part = cov(&e, FillRule::NonZero, IRect::new(40, 37, 90, 120));
        for y in 37..120 {
            assert_eq!(&full[y][40..90], &part[y - 37][..]);
        }
    }
}
