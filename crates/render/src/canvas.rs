//! Premultiplied RGBA8 pixmaps and span compositing with the PDF blend modes (§11.3.5).

use std::simd::prelude::*;

use pagebara_content::display::BlendMode;

use crate::raster::IRect;
use crate::raster::mask::div255;

#[derive(Clone, Debug, Default)]
pub struct Pixmap {
    pub rect: IRect,
    /// Premultiplied RGBA, rows of `rect.width() * 4` bytes.
    pub data: Vec<u8>,
}

impl Pixmap {
    pub fn new(rect: IRect) -> Self {
        Pixmap {
            rect,
            data: vec![0; rect.area() * 4],
        }
    }

    pub fn filled(rect: IRect, c: [u8; 4]) -> Self {
        let mut p = Pixmap::new(rect);
        for px in p.data.as_chunks_mut::<4>().0 {
            *px = c;
        }
        p
    }

    pub fn width(&self) -> usize {
        self.rect.width() as usize
    }

    /// Pixels `x..x + n` of row `y` (device coordinates), which must be inside.
    #[inline]
    pub fn span_mut(&mut self, y: i32, x: i32, n: usize) -> &mut [u8] {
        let w = self.width();
        let o = ((y - self.rect.y0) as usize * w + (x - self.rect.x0) as usize) * 4;
        &mut self.data[o..o + n * 4]
    }

    #[inline]
    pub fn span(&self, y: i32, x: i32, n: usize) -> &[u8] {
        let w = self.width();
        let o = ((y - self.rect.y0) as usize * w + (x - self.rect.x0) as usize) * 4;
        &self.data[o..o + n * 4]
    }

    /// A copy of the pixels inside `r` (which must be inside the pixmap).
    pub fn crop(&self, r: IRect) -> Pixmap {
        let mut p = Pixmap::new(r);
        let n = r.width() as usize;
        for y in r.y0..r.y1 {
            p.span_mut(y, r.x0, n)
                .copy_from_slice(self.span(y, r.x0, n));
        }
        p
    }

    /// The pixel at a device position, transparent outside.
    pub fn get(&self, x: i32, y: i32) -> [u8; 4] {
        let r = self.rect;
        if x < r.x0 || y < r.y0 || x >= r.x1 || y >= r.y1 {
            return [0; 4];
        }
        let s = self.span(y, x, 1);
        [s[0], s[1], s[2], s[3]]
    }
}

/// Composites premultiplied `src` pixels onto `dst` with per-pixel coverage `cov`.
#[inline]
pub fn composite(dst: &mut [u8], src: &[u8], cov: &[u8], blend: BlendMode) {
    if blend == BlendMode::Normal {
        over(dst, src, cov);
    } else {
        blend_span(dst, src, cov, blend);
    }
}

/// Source-over of a solid premultiplied colour.
#[inline]
pub fn composite_solid(dst: &mut [u8], c: [u8; 4], cov: &[u8], blend: BlendMode) {
    if blend != BlendMode::Normal {
        let mut src = [0u8; 4 * 64];
        for (d, cv) in dst.chunks_mut(4 * 64).zip(cov.chunks(64)) {
            let n = cv.len();
            for px in src[..n * 4].as_chunks_mut::<4>().0 {
                *px = c;
            }
            blend_span(d, &src[..n * 4], cv, blend);
        }
        return;
    }
    let opaque = c[3] == 255;
    let cv = u16x16::from_array([
        c[0] as u16,
        c[1] as u16,
        c[2] as u16,
        c[3] as u16,
        c[0] as u16,
        c[1] as u16,
        c[2] as u16,
        c[3] as u16,
        c[0] as u16,
        c[1] as u16,
        c[2] as u16,
        c[3] as u16,
        c[0] as u16,
        c[1] as u16,
        c[2] as u16,
        c[3] as u16,
    ]);
    let (dc, dr) = dst.as_chunks_mut::<16>();
    let (cc, cr) = cov.as_chunks::<4>();
    let word = u32::from_ne_bytes(c);
    for (d, k) in dc.iter_mut().zip(cc) {
        if opaque && u32::from_ne_bytes(*k) == u32::MAX {
            for px in d.as_chunks_mut::<4>().0 {
                *px = word.to_ne_bytes();
            }
            continue;
        }
        if u32::from_ne_bytes(*k) == 0 {
            continue;
        }
        let k16 = expand_cov(*k);
        let s = div255(cv * k16);
        let sa = simd_swizzle!(s, [3, 3, 3, 3, 7, 7, 7, 7, 11, 11, 11, 11, 15, 15, 15, 15]);
        let dv = u8x16::from_array(*d).cast::<u16>();
        let r = s + div255(dv * (u16x16::splat(255) - sa));
        *d = r.cast::<u8>().to_array();
    }
    for (d, &k) in dr.as_chunks_mut::<4>().0.iter_mut().zip(cr) {
        over_px(d, c, k);
    }
}

#[inline]
fn expand_cov(k: [u8; 4]) -> u16x16 {
    let k = u8x4::from_array(k).cast::<u16>();
    simd_swizzle!(k, [0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3])
}

#[inline]
fn over_px(d: &mut [u8; 4], s: [u8; 4], k: u8) {
    if k == 0 {
        return;
    }
    let m = |v: u8| ((u32::from(v) * u32::from(k) + 127) / 255) as u8;
    let s = if k == 255 { s } else { s.map(m) };
    let inv = 255 - u32::from(s[3]);
    for i in 0..4 {
        d[i] = (u32::from(s[i]) + (u32::from(d[i]) * inv + 127) / 255).min(255) as u8;
    }
}

/// Source-over of premultiplied pixels.
#[inline]
pub fn over(dst: &mut [u8], src: &[u8], cov: &[u8]) {
    let (dc, dr) = dst.as_chunks_mut::<16>();
    let (sc, sr) = src.as_chunks::<16>();
    let (cc, cr) = cov.as_chunks::<4>();
    for ((d, s), k) in dc.iter_mut().zip(sc).zip(cc) {
        let kk = u32::from_ne_bytes(*k);
        if kk == 0 {
            continue;
        }
        let sv = u8x16::from_array(*s).cast::<u16>();
        let s = if kk == u32::MAX {
            sv
        } else {
            div255(sv * expand_cov(*k))
        };
        let sa = simd_swizzle!(s, [3, 3, 3, 3, 7, 7, 7, 7, 11, 11, 11, 11, 15, 15, 15, 15]);
        let dv = u8x16::from_array(*d).cast::<u16>();
        let r = s + div255(dv * (u16x16::splat(255) - sa));
        *d = r.simd_min(u16x16::splat(255)).cast::<u8>().to_array();
    }
    for ((d, s), &k) in dr
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(sr.as_chunks::<4>().0)
        .zip(cr)
    {
        over_px(d, *s, k);
    }
}

/// Blend modes other than Normal, per pixel in floating point.
pub fn blend_span(dst: &mut [u8], src: &[u8], cov: &[u8], mode: BlendMode) {
    for ((d, s), &k) in dst
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(src.as_chunks::<4>().0)
        .zip(cov)
    {
        if k == 0 || s[3] == 0 {
            continue;
        }
        let kf = k as f32 / 255.0;
        let sa = s[3] as f32 / 255.0 * kf;
        let da = d[3] as f32 / 255.0;
        let sc = [s[0], s[1], s[2]].map(|v| v as f32 / 255.0 * kf);
        let dc = [d[0], d[1], d[2]].map(|v| v as f32 / 255.0);
        let r = blend_px(sc, sa, dc, da, mode);
        for i in 0..4 {
            d[i] = (r[i].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        }
    }
}

/// One premultiplied pixel blended: `s` and `d` are premultiplied colours.
#[inline]
pub fn blend_px(s: [f32; 3], sa: f32, d: [f32; 3], da: f32, mode: BlendMode) -> [f32; 4] {
    let ra = sa + da - sa * da;
    let unp = |c: [f32; 3], a: f32| {
        if a > 0.0 {
            c.map(|v| (v / a).min(1.0))
        } else {
            [0.0; 3]
        }
    };
    let cs = unp(s, sa);
    let cb = unp(d, da);
    let b = blend_fn(cs, cb, mode);
    let mut out = [0f32; 4];
    for i in 0..3 {
        out[i] = s[i] * (1.0 - da) + d[i] * (1.0 - sa) + sa * da * b[i];
    }
    out[3] = ra;
    out
}

/// The blend function `B(cb, cs)` on unpremultiplied colours.
pub fn blend_fn(cs: [f32; 3], cb: [f32; 3], mode: BlendMode) -> [f32; 3] {
    use BlendMode::*;
    let sep = |f: fn(f32, f32) -> f32| [f(cb[0], cs[0]), f(cb[1], cs[1]), f(cb[2], cs[2])];
    match mode {
        Normal => cs,
        Multiply => sep(|b, s| b * s),
        Screen => sep(|b, s| b + s - b * s),
        Overlay => sep(|b, s| hard_light(s, b)),
        Darken => sep(f32::min),
        Lighten => sep(f32::max),
        ColorDodge => sep(|b, s| {
            if b == 0.0 {
                0.0
            } else if b >= 1.0 - s {
                1.0
            } else {
                b / (1.0 - s)
            }
        }),
        ColorBurn => sep(|b, s| {
            if b == 1.0 {
                1.0
            } else if 1.0 - b >= s {
                0.0
            } else {
                1.0 - (1.0 - b) / s
            }
        }),
        HardLight => sep(hard_light),
        SoftLight => sep(|b, s| {
            if s <= 0.5 {
                b - (1.0 - 2.0 * s) * b * (1.0 - b)
            } else {
                let d = if b <= 0.25 {
                    ((16.0 * b - 12.0) * b + 4.0) * b
                } else {
                    b.sqrt()
                };
                b + (2.0 * s - 1.0) * (d - b)
            }
        }),
        Difference => sep(|b, s| (b - s).abs()),
        Exclusion => sep(|b, s| b + s - 2.0 * b * s),
        Hue => set_lum(set_sat(cs, sat(cb)), lum(cb)),
        Saturation => set_lum(set_sat(cb, sat(cs)), lum(cb)),
        Color => set_lum(cs, lum(cb)),
        Luminosity => set_lum(cb, lum(cs)),
    }
}

fn hard_light(b: f32, s: f32) -> f32 {
    if s <= 0.5 {
        b * 2.0 * s
    } else {
        let s2 = 2.0 * s - 1.0;
        b + s2 - b * s2
    }
}

fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

fn clip_color(c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    let mut c = c;
    if n < 0.0 && l - n != 0.0 {
        c = c.map(|v| l + (v - l) * l / (l - n));
    }
    if x > 1.0 && x - l != 0.0 {
        c = c.map(|v| l + (v - l) * (1.0 - l) / (x - l));
    }
    c
}

fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    clip_color(c.map(|v| v + d))
}

fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    let mut idx = [0usize, 1, 2];
    idx.sort_by(|&a, &b| c[a].total_cmp(&c[b]));
    let (mn, md, mx) = (idx[0], idx[1], idx[2]);
    let mut out = [0f32; 3];
    if c[mx] > c[mn] {
        out[md] = (c[md] - c[mn]) * s / (c[mx] - c[mn]);
        out[mx] = s;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solid_over() {
        let mut d = vec![255u8; 4 * 5];
        composite_solid(
            &mut d,
            [0, 0, 255, 255],
            &[255, 128, 0, 255, 64],
            BlendMode::Normal,
        );
        assert_eq!(&d[0..4], &[0, 0, 255, 255]);
        assert_eq!(&d[4..8], &[127, 127, 255, 255]);
        assert_eq!(&d[8..12], &[255, 255, 255, 255]);
        assert_eq!(&d[16..20], &[191, 191, 255, 255]);
    }

    #[test]
    fn multiply_white_is_identity() {
        let mut d = vec![255u8; 4];
        composite_solid(&mut d, [10, 20, 30, 255], &[255], BlendMode::Multiply);
        assert_eq!(d, vec![10, 20, 30, 255]);
    }
}
