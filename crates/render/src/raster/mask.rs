//! Coverage masks: clip paths, soft masks, glyphs.

use std::simd::prelude::*;

use pagebara_content::display::FillRule;

use super::{Edges, IRect, Rasterizer, Span};

/// 8-bit coverage over `rect`, rows of `rect.width()` bytes; zero outside.
#[derive(Clone, Debug, Default)]
pub struct Mask {
    pub rect: IRect,
    pub data: Vec<u8>,
}

impl Mask {
    pub fn new(rect: IRect) -> Self {
        Mask {
            rect,
            data: vec![0; rect.area()],
        }
    }

    pub fn filled(rect: IRect, v: u8) -> Self {
        Mask {
            rect,
            data: vec![v; rect.area()],
        }
    }

    /// The coverage of a fill inside `clip`, cropped to what it covers.
    pub fn from_fill(r: &mut Rasterizer, e: &Edges, rule: FillRule, clip: IRect) -> Self {
        let rect = e.pixel_bounds().intersect(&clip);
        let mut m = Mask::new(rect);
        r.fill(e, rule, rect, &mut |y, x, s| {
            let row = m.row_mut(y);
            let dst = &mut row[(x - rect.x0) as usize..][..s.len()];
            match s {
                Span::Cov(c) => dst.copy_from_slice(c),
                Span::Run(_, v) => dst.fill(v),
            }
        });
        m
    }

    #[inline]
    pub fn row(&self, y: i32) -> &[u8] {
        let w = self.rect.width() as usize;
        let o = (y - self.rect.y0) as usize * w;
        &self.data[o..o + w]
    }

    #[inline]
    pub fn row_mut(&mut self, y: i32) -> &mut [u8] {
        let w = self.rect.width() as usize;
        let o = (y - self.rect.y0) as usize * w;
        &mut self.data[o..o + w]
    }

    /// The value at a pixel (0 outside).
    pub fn get(&self, x: i32, y: i32) -> u8 {
        let r = self.rect;
        if x < r.x0 || x >= r.x1 || y < r.y0 || y >= r.y1 {
            return 0;
        }
        self.row(y)[(x - r.x0) as usize]
    }

    /// Multiplies `cov` (pixels `x..x + cov.len()` of row `y`) by the mask.
    #[inline]
    pub fn apply(&self, y: i32, x: i32, cov: &mut [u8]) {
        let r = self.rect;
        if y < r.y0 || y >= r.y1 {
            cov.fill(0);
            return;
        }
        let row = self.row(y);
        let a = x;
        let lo = (r.x0 - a).clamp(0, cov.len() as i32) as usize;
        let hi = (r.x1 - a).clamp(0, cov.len() as i32) as usize;
        cov[..lo].fill(0);
        cov[hi..].fill(0);
        if lo < hi {
            let m = &row[(a + lo as i32 - r.x0) as usize..][..hi - lo];
            mul_in_place(&mut cov[lo..hi], m);
        }
    }

    /// Intersects with another mask (pixel-wise product), cropping to the overlap.
    pub fn intersect(&self, o: &Mask) -> Mask {
        let rect = self.rect.intersect(&o.rect);
        let mut m = Mask::new(rect);
        for y in rect.y0..rect.y1 {
            let a = &self.row(y)[(rect.x0 - self.rect.x0) as usize..][..rect.width() as usize];
            let row = m.row_mut(y);
            row.copy_from_slice(a);
            o.apply(y, rect.x0, row);
        }
        m
    }
}

/// `a = a * b / 255`, rounded.
#[inline]
pub fn mul_in_place(a: &mut [u8], b: &[u8]) {
    let (ac, ar) = a.as_chunks_mut::<16>();
    let (bc, br) = b.as_chunks::<16>();
    for (x, y) in ac.iter_mut().zip(bc) {
        let p = u8x16::from_array(*x).cast::<u16>() * u8x16::from_array(*y).cast::<u16>();
        *x = div255(p).cast::<u8>().to_array();
    }
    for (x, y) in ar.iter_mut().zip(br) {
        *x = mul8(*x, *y);
    }
}

#[inline]
pub fn mul8(a: u8, b: u8) -> u8 {
    let p = u16::from(a) * u16::from(b) + 128;
    ((p + (p >> 8)) >> 8) as u8
}

/// `v / 255` rounded, for `v <= 255 * 255`.
#[inline]
pub fn div255<const N: usize>(v: Simd<u16, N>) -> Simd<u16, N> {
    let v = v + Simd::splat(128);
    (v + (v >> 8)) >> 8
}
