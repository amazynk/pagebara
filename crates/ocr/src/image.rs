//! The small amount of image processing the models need: an RGB buffer, area resizing and
//! sampling a rotated rectangle.

/// An 8-bit RGB image, rows top to bottom.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RgbImage {
    pub width: usize,
    pub height: usize,
    /// `width * height * 3` bytes.
    pub data: Vec<u8>,
}

impl RgbImage {
    /// From opaque RGBA8 (alpha is dropped).
    pub fn from_rgba(width: usize, height: usize, rgba: &[u8]) -> RgbImage {
        let mut data = Vec::with_capacity(width * height * 3);
        for p in rgba.as_chunks::<4>().0.iter().take(width * height) {
            data.extend_from_slice(&p[..3]);
        }
        data.resize(width * height * 3, 255);
        RgbImage {
            width,
            height,
            data,
        }
    }

    /// From 8-bit gray.
    pub fn from_gray(width: usize, height: usize, gray: &[u8]) -> RgbImage {
        let mut data = Vec::with_capacity(width * height * 3);
        for &g in gray.iter().take(width * height) {
            data.extend_from_slice(&[g, g, g]);
        }
        data.resize(width * height * 3, 255);
        RgbImage {
            width,
            height,
            data,
        }
    }

    fn at(&self, x: usize, y: usize) -> [f32; 3] {
        let i = (y * self.width + x) * 3;
        [
            f32::from(self.data[i]),
            f32::from(self.data[i + 1]),
            f32::from(self.data[i + 2]),
        ]
    }

    /// Bilinear sample at `(x, y)` (pixel centres at half-integers), clamped to the edges.
    pub fn sample(&self, x: f32, y: f32) -> [f32; 3] {
        if self.width == 0 || self.height == 0 {
            return [255.0; 3];
        }
        let fx = (x - 0.5).clamp(0.0, (self.width - 1) as f32);
        let fy = (y - 0.5).clamp(0.0, (self.height - 1) as f32);
        let (x0, y0) = (fx as usize, fy as usize);
        let (x1, y1) = ((x0 + 1).min(self.width - 1), (y0 + 1).min(self.height - 1));
        let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
        let (a, b, c, d) = (
            self.at(x0, y0),
            self.at(x1, y0),
            self.at(x0, y1),
            self.at(x1, y1),
        );
        std::array::from_fn(|i| {
            let top = a[i] + (b[i] - a[i]) * tx;
            let bot = c[i] + (d[i] - c[i]) * tx;
            top + (bot - top) * ty
        })
    }

    /// Resized to `width` × `height`: area averaging when shrinking, bilinear when growing.
    pub fn resize(&self, width: usize, height: usize) -> RgbImage {
        let sx = self.width as f32 / width.max(1) as f32;
        let sy = self.height as f32 / height.max(1) as f32;
        let mut data = Vec::with_capacity(width * height * 3);
        if sx <= 1.0 && sy <= 1.0 {
            for y in 0..height {
                for x in 0..width {
                    let p = self.sample((x as f32 + 0.5) * sx, (y as f32 + 0.5) * sy);
                    data.extend(p.map(|v| v.round() as u8));
                }
            }
        } else {
            let xs = spans(self.width, width);
            let ys = spans(self.height, height);
            for &(y0, y1) in &ys {
                for &(x0, x1) in &xs {
                    let mut sum = [0u32; 3];
                    for yy in y0..y1 {
                        let row =
                            &self.data[(yy * self.width + x0) * 3..(yy * self.width + x1) * 3];
                        for p in row.as_chunks::<3>().0 {
                            for i in 0..3 {
                                sum[i] += u32::from(p[i]);
                            }
                        }
                    }
                    let n = ((y1 - y0) * (x1 - x0)).max(1) as u32;
                    data.extend(sum.map(|s| ((s + n / 2) / n) as u8));
                }
            }
        }
        RgbImage {
            width,
            height,
            data,
        }
    }
}

impl RgbImage {
    /// Turned `quarter` quarter turns clockwise.
    pub fn turned(&self, quarter: u8) -> RgbImage {
        let (w, h) = (self.width, self.height);
        let q = quarter % 4;
        if q == 0 {
            return self.clone();
        }
        let (nw, nh) = if q == 2 { (w, h) } else { (h, w) };
        let mut data = vec![0; nw * nh * 3];
        for y in 0..nh {
            for x in 0..nw {
                let (sx, sy) = match q {
                    1 => (y, h - 1 - x),
                    2 => (w - 1 - x, h - 1 - y),
                    _ => (w - 1 - y, x),
                };
                let s = (sy * w + sx) * 3;
                data[(y * nw + x) * 3..][..3].copy_from_slice(&self.data[s..s + 3]);
            }
        }
        RgbImage {
            width: nw,
            height: nh,
            data,
        }
    }
}

/// Source index ranges, at least one pixel wide, covering `src` in `dst` parts.
fn spans(src: usize, dst: usize) -> Vec<(usize, usize)> {
    (0..dst)
        .map(|i| {
            let a = i * src / dst;
            let b = ((i + 1) * src / dst).max(a + 1).min(src);
            (a.min(src.saturating_sub(1)), b)
        })
        .collect()
}

/// A rectangle in image pixels, possibly rotated: `origin` is its top-left corner, `u` runs
/// along the top edge and `v` down the left edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RotRect {
    pub origin: [f32; 2],
    pub u: [f32; 2],
    pub v: [f32; 2],
}

impl RotRect {
    pub fn width(&self) -> f32 {
        self.u[0].hypot(self.u[1])
    }

    pub fn height(&self) -> f32 {
        self.v[0].hypot(self.v[1])
    }

    /// The point at fractions `(s, t)` along `u` and `v`.
    pub fn at(&self, s: f32, t: f32) -> [f32; 2] {
        [
            self.origin[0] + self.u[0] * s + self.v[0] * t,
            self.origin[1] + self.u[1] * s + self.v[1] * t,
        ]
    }

    /// Corners: top-left, top-right, bottom-right, bottom-left.
    pub fn corners(&self) -> [[f32; 2]; 4] {
        [
            self.at(0.0, 0.0),
            self.at(1.0, 0.0),
            self.at(1.0, 1.0),
            self.at(0.0, 1.0),
        ]
    }

    /// The rectangle in an image of `width` × `height` that was turned `quarter` quarter
    /// turns clockwise to give the one `self` is in.
    pub fn unturned(&self, quarter: u8, width: f32, height: f32) -> RotRect {
        let (w, h) = (width, height);
        let point = |[x, y]: [f32; 2]| match quarter % 4 {
            0 => [x, y],
            1 => [y, h - x],
            2 => [w - x, h - y],
            _ => [w - y, x],
        };
        let vector = |[x, y]: [f32; 2]| match quarter % 4 {
            0 => [x, y],
            1 => [y, -x],
            2 => [-x, -y],
            _ => [-y, x],
        };
        RotRect {
            origin: point(self.origin),
            u: vector(self.u),
            v: vector(self.v),
        }
    }

    /// The same area read the other way round (for upside-down text): the far corner
    /// becomes the origin, and the axes stay.
    pub fn reversed_in(&self, width: f32, height: f32) -> RotRect {
        let [x, y] = self.at(1.0, 1.0);
        RotRect {
            origin: [width - x, height - y],
            u: self.u,
            v: self.v,
        }
    }

    /// Scaled by `k` about the image origin (to the same area of an image `k` times as
    /// large).
    pub fn scaled(&self, k: f32) -> RotRect {
        RotRect {
            origin: [self.origin[0] * k, self.origin[1] * k],
            u: [self.u[0] * k, self.u[1] * k],
            v: [self.v[0] * k, self.v[1] * k],
        }
    }

    /// The part between fractions `s0..s1` along `u`.
    pub fn slice(&self, s0: f32, s1: f32) -> RotRect {
        RotRect {
            origin: self.at(s0, 0.0),
            u: [self.u[0] * (s1 - s0), self.u[1] * (s1 - s0)],
            v: self.v,
        }
    }
}

/// Samples `r` into a `width` × `height` image, averaging `k` × `k` samples per pixel when
/// shrinking by `k` or more.
pub fn warp(src: &RgbImage, r: &RotRect, width: usize, height: usize) -> RgbImage {
    let k = (r.height() / height as f32).max(r.width() / width.max(1) as f32);
    let k = (k.ceil() as usize).clamp(1, 4);
    let inv = 1.0 / (k * k) as f32;
    let mut data = Vec::with_capacity(width * height * 3);
    for y in 0..height {
        for x in 0..width {
            let mut acc = [0.0f32; 3];
            for j in 0..k {
                for i in 0..k {
                    let s = (x as f32 + (i as f32 + 0.5) / k as f32) / width as f32;
                    let t = (y as f32 + (j as f32 + 0.5) / k as f32) / height as f32;
                    let [px, py] = r.at(s, t);
                    let p = src.sample(px, py);
                    for c in 0..3 {
                        acc[c] += p[c];
                    }
                }
            }
            data.extend(acc.map(|v| (v * inv).round().clamp(0.0, 255.0) as u8));
        }
    }
    RgbImage {
        width,
        height,
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_averages() {
        let img = RgbImage::from_gray(4, 2, &[0, 255, 0, 255, 0, 255, 0, 255]);
        let r = img.resize(2, 1);
        assert_eq!(r.data, [128, 128, 128, 128, 128, 128]);
        let up = img.resize(8, 4);
        assert_eq!((up.width, up.height, up.data.len()), (8, 4, 96));
    }

    #[test]
    fn turns_round_trip() {
        let img = RgbImage::from_gray(3, 2, &[1, 2, 3, 4, 5, 6]);
        let t = img.turned(1);
        assert_eq!((t.width, t.height), (2, 3));
        // Clockwise: the bottom-left pixel comes to the top-left.
        assert_eq!(t.data[0], 4);
        assert_eq!(t.turned(3), img);
        assert_eq!(img.turned(2).turned(2), img);
        // A box in the turned image maps back onto the same pixels.
        let r = RotRect {
            origin: [0.0, 0.0],
            u: [1.0, 0.0],
            v: [0.0, 1.0],
        };
        for q in 0..4 {
            let t = img.turned(q);
            let back = r.unturned(q, 3.0, 2.0);
            let c = back.at(0.5, 0.5);
            let v = t.data[0];
            let i = (c[1] as usize * 3 + c[0] as usize) * 3;
            assert_eq!(img.data[i], v, "quarter {q}");
        }
    }

    #[test]
    fn warp_identity() {
        let img = RgbImage::from_gray(3, 2, &[10, 20, 30, 40, 50, 60]);
        let r = RotRect {
            origin: [0.0, 0.0],
            u: [3.0, 0.0],
            v: [0.0, 2.0],
        };
        assert_eq!(warp(&img, &r, 3, 2), img);
        let s = r.slice(0.5, 1.0);
        assert_eq!(s.corners()[0], [1.5, 0.0]);
        assert_eq!(s.width(), 1.5);
    }
}
