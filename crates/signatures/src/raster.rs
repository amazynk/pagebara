//! RGBA images and the few geometric operations signatures need.

use rayon::prelude::*;

/// 8-bit RGBA, rows top to bottom, straight (not premultiplied) alpha.
#[derive(Clone, PartialEq, Eq)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl std::fmt::Debug for Rgba {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Rgba({}x{})", self.width, self.height)
    }
}

impl Rgba {
    pub fn new(width: u32, height: u32) -> Self {
        Rgba {
            width,
            height,
            pixels: vec![0; width as usize * height as usize * 4],
        }
    }

    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [
            self.pixels[i],
            self.pixels[i + 1],
            self.pixels[i + 2],
            self.pixels[i + 3],
        ]
    }

    pub fn as_annot(&self) -> pagebara_annot::RgbaImage<'_> {
        pagebara_annot::RgbaImage {
            width: self.width,
            height: self.height,
            pixels: &self.pixels,
        }
    }

    /// Scaled down (area average, alpha-weighted) so neither side exceeds `max_w` x
    /// `max_h`, aspect preserved; unchanged when it already fits.
    pub fn fit_within(&self, max_w: u32, max_h: u32) -> Rgba {
        let s = (f64::from(max_w) / f64::from(self.width))
            .min(f64::from(max_h) / f64::from(self.height));
        if s >= 1.0 {
            return self.clone();
        }
        let w = ((f64::from(self.width) * s).round() as u32).max(1);
        let h = ((f64::from(self.height) * s).round() as u32).max(1);
        self.downscale(w, h)
    }

    /// Area-average downscale to `w` x `h` (each no larger than the source).
    fn downscale(&self, w: u32, h: u32) -> Rgba {
        let (sw, sh) = (self.width as usize, self.height as usize);
        let fx = sw as f64 / f64::from(w);
        let fy = sh as f64 / f64::from(h);
        let span = |i: usize, f: f64, n: usize| {
            let a = (i as f64 * f) as usize;
            (a, (((i + 1) as f64 * f).ceil() as usize).clamp(a + 1, n))
        };
        let columns: Vec<(usize, usize)> = (0..w as usize).map(|x| span(x, fx, sw)).collect();
        let mut out = Rgba::new(w, h);
        out.pixels
            .par_chunks_mut(w as usize * 4)
            .enumerate()
            .for_each(|(y, row)| {
                let (y0, y1) = span(y, fy, sh);
                for (px, &(x0, x1)) in row.as_chunks_mut::<4>().0.iter_mut().zip(&columns) {
                    let mut acc = [0u64; 4];
                    for sy in y0..y1 {
                        let src = &self.pixels[(sy * sw + x0) * 4..(sy * sw + x1) * 4];
                        for p in src.as_chunks::<4>().0 {
                            let a = u64::from(p[3]);
                            acc[0] += u64::from(p[0]) * a;
                            acc[1] += u64::from(p[1]) * a;
                            acc[2] += u64::from(p[2]) * a;
                            acc[3] += a;
                        }
                    }
                    let n = ((y1 - y0) * (x1 - x0)) as u64;
                    for c in 0..3 {
                        px[c] = (acc[c] + acc[3] / 2).checked_div(acc[3]).unwrap_or(0) as u8;
                    }
                    px[3] = ((acc[3] + n / 2) / n) as u8;
                }
            });
        out
    }

    /// The sub-image `x, y, w, h` (clamped to the image).
    pub fn crop(&self, x: u32, y: u32, w: u32, h: u32) -> Rgba {
        let x = x.min(self.width);
        let y = y.min(self.height);
        let w = w.min(self.width - x);
        let h = h.min(self.height - y);
        let mut out = Vec::with_capacity(w as usize * h as usize * 4);
        for row in y..y + h {
            let s = (row as usize * self.width as usize + x as usize) * 4;
            out.extend_from_slice(&self.pixels[s..s + w as usize * 4]);
        }
        Rgba {
            width: w,
            height: h,
            pixels: out,
        }
    }

    /// Applies an Exif orientation (1..=8) so the image is upright.
    pub fn oriented(self, orientation: u16) -> Rgba {
        if !(2..=8).contains(&orientation) {
            return self;
        }
        let (w, h) = (self.width, self.height);
        let swap = orientation >= 5;
        let (ow, oh) = if swap { (h, w) } else { (w, h) };
        let mut out = Rgba::new(ow, oh);
        for y in 0..oh {
            for x in 0..ow {
                // Source pixel for output (x, y).
                let (sx, sy) = match orientation {
                    2 => (w - 1 - x, y),
                    3 => (w - 1 - x, h - 1 - y),
                    4 => (x, h - 1 - y),
                    5 => (y, x),
                    6 => (y, h - 1 - x),
                    7 => (w - 1 - y, h - 1 - x),
                    _ => (w - 1 - y, x),
                };
                let s = (sy as usize * w as usize + sx as usize) * 4;
                let d = (y as usize * ow as usize + x as usize) * 4;
                out.pixels[d..d + 4].copy_from_slice(&self.pixels[s..s + 4]);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downscale_averages() {
        let mut img = Rgba::new(4, 2);
        for (i, p) in img.pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let v = if i % 2 == 0 { 0 } else { 200 };
            p.copy_from_slice(&[v, v, v, 255]);
        }
        let s = img.fit_within(2, 2);
        assert_eq!((s.width, s.height), (2, 1));
        assert_eq!(s.pixel(0, 0), [100, 100, 100, 255]);
    }

    #[test]
    fn orientation_6_rotates_clockwise() {
        let mut img = Rgba::new(2, 1);
        img.pixels = vec![1, 1, 1, 255, 2, 2, 2, 255];
        let r = img.oriented(6);
        assert_eq!((r.width, r.height), (1, 2));
        assert_eq!(r.pixel(0, 0)[0], 1);
        assert_eq!(r.pixel(0, 1)[0], 2);
    }
}
