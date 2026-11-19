//! RGB images, the binary PNM files MuPDF writes, and difference metrics.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgb {
    pub w: u32,
    pub h: u32,
    pub data: Vec<u8>,
}

impl Rgb {
    /// Drops alpha (the pages are rendered on white, so every pixel is opaque).
    pub fn from_rgba(w: u32, h: u32, rgba: &[u8]) -> Rgb {
        let data = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect();
        Rgb { w, h, data }
    }

    /// Reads a binary PPM (P6) or PGM (P5) file with 8-bit samples.
    pub fn from_pnm(b: &[u8]) -> Option<Rgb> {
        let mut fields = Vec::new();
        let mut i = 2;
        if b.len() < 2 || (b[..2] != *b"P6" && b[..2] != *b"P5") {
            return None;
        }
        while fields.len() < 3 {
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if b.get(i) == Some(&b'#') {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            let start = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            fields.push(
                std::str::from_utf8(&b[start..i])
                    .ok()?
                    .parse::<u32>()
                    .ok()?,
            );
        }
        i += 1;
        let (w, h, max) = (fields[0], fields[1], fields[2]);
        if max != 255 {
            return None;
        }
        let n = w as usize * h as usize;
        let px = b.get(i..)?;
        let data = if b[1] == b'6' {
            px.get(..n * 3)?.to_vec()
        } else {
            px.get(..n)?.iter().flat_map(|&g| [g, g, g]).collect()
        };
        Some(Rgb { w, h, data })
    }

    fn px(&self, x: u32, y: u32) -> [u8; 3] {
        let o = (y as usize * self.w as usize + x as usize) * 3;
        [self.data[o], self.data[o + 1], self.data[o + 2]]
    }

    /// Box-filtered to fit `max` pixels on its longer side.
    pub fn thumbnail(&self, max: u32) -> Rgb {
        let k = (self.w.max(self.h) as f32 / max as f32).max(1.0);
        let (w, h) = (
            ((self.w as f32 / k) as u32).max(1),
            ((self.h as f32 / k) as u32).max(1),
        );
        let mut data = Vec::with_capacity(w as usize * h as usize * 3);
        for y in 0..h {
            let (y0, y1) = (
                y * self.h / h,
                ((y + 1) * self.h / h).max(y * self.h / h + 1),
            );
            for x in 0..w {
                let (x0, x1) = (
                    x * self.w / w,
                    ((x + 1) * self.w / w).max(x * self.w / w + 1),
                );
                let mut acc = [0u32; 3];
                for sy in y0..y1.min(self.h) {
                    for sx in x0..x1.min(self.w) {
                        let p = self.px(sx, sy);
                        for c in 0..3 {
                            acc[c] += u32::from(p[c]);
                        }
                    }
                }
                let n = ((y1.min(self.h) - y0) * (x1.min(self.w) - x0)).max(1);
                data.extend(acc.map(|v| (v / n) as u8));
            }
        }
        Rgb { w, h, data }
    }

    /// The images left to right on mid grey, 8 pixels apart.
    pub fn side_by_side(imgs: &[Rgb]) -> Rgb {
        const GAP: u32 = 8;
        let w = imgs
            .iter()
            .map(|i| i.w + GAP)
            .sum::<u32>()
            .saturating_sub(GAP)
            .max(1);
        let h = imgs.iter().map(|i| i.h).max().unwrap_or(1);
        let mut data = vec![128u8; w as usize * h as usize * 3];
        let mut x0 = 0usize;
        for img in imgs {
            for y in 0..img.h as usize {
                let src = &img.data[y * img.w as usize * 3..(y + 1) * img.w as usize * 3];
                let at = (y * w as usize + x0) * 3;
                data[at..at + src.len()].copy_from_slice(src);
            }
            x0 += (img.w + GAP) as usize;
        }
        Rgb { w, h, data }
    }

    pub fn png(&self) -> Vec<u8> {
        pagebara_png::encode(self.w, self.h, pagebara_png::ColorType::Rgb, &self.data)
            .unwrap_or_default()
    }
}

/// How two renderings of a page differ, over the area both cover.
#[derive(Debug, Clone, Copy, Default)]
pub struct Metrics {
    /// Mean absolute difference per channel, 0–255.
    pub mae: f64,
    /// Percentage of pixels with a channel differing by more than [`THRESHOLD`].
    pub over: f64,
    /// Mean structural similarity of the grey images over 8 × 8 windows, 0–1.
    pub ssim: f64,
}

pub const THRESHOLD: u8 = 64;

/// The metrics and a difference image (black where equal, brighter where not; red where
/// over the threshold).
pub fn compare(a: &Rgb, b: &Rgb) -> (Metrics, Rgb) {
    let (w, h) = (a.w.min(b.w), a.h.min(b.h));
    let mut diff = Vec::with_capacity(w as usize * h as usize * 3);
    let (mut sum, mut over) = (0u64, 0u64);
    for y in 0..h {
        for x in 0..w {
            let (p, q) = (a.px(x, y), b.px(x, y));
            let d = [
                p[0].abs_diff(q[0]),
                p[1].abs_diff(q[1]),
                p[2].abs_diff(q[2]),
            ];
            sum += d.iter().map(|&v| u64::from(v)).sum::<u64>();
            let m = d[0].max(d[1]).max(d[2]);
            if m > THRESHOLD {
                over += 1;
                diff.extend([255, 0, 0]);
            } else {
                let v = m.saturating_mul(3);
                diff.extend([v, v, v]);
            }
        }
    }
    let n = (u64::from(w) * u64::from(h)).max(1);
    let metrics = Metrics {
        mae: sum as f64 / (n * 3) as f64,
        over: 100.0 * over as f64 / n as f64,
        ssim: ssim(a, b, w, h),
    };
    (metrics, Rgb { w, h, data: diff })
}

fn gray(p: [u8; 3]) -> f64 {
    0.299 * f64::from(p[0]) + 0.587 * f64::from(p[1]) + 0.114 * f64::from(p[2])
}

fn ssim(a: &Rgb, b: &Rgb, w: u32, h: u32) -> f64 {
    const C1: f64 = (0.01 * 255.0) * (0.01 * 255.0);
    const C2: f64 = (0.03 * 255.0) * (0.03 * 255.0);
    let (mut total, mut windows) = (0.0, 0u32);
    for wy in (0..h.saturating_sub(7)).step_by(8) {
        for wx in (0..w.saturating_sub(7)).step_by(8) {
            let (mut sa, mut sb, mut saa, mut sbb, mut sab) = (0.0, 0.0, 0.0, 0.0, 0.0);
            for y in wy..wy + 8 {
                for x in wx..wx + 8 {
                    let (p, q) = (gray(a.px(x, y)), gray(b.px(x, y)));
                    sa += p;
                    sb += q;
                    saa += p * p;
                    sbb += q * q;
                    sab += p * q;
                }
            }
            let n = 64.0;
            let (ma, mb) = (sa / n, sb / n);
            let (va, vb) = (saa / n - ma * ma, sbb / n - mb * mb);
            let cov = sab / n - ma * mb;
            total += ((2.0 * ma * mb + C1) * (2.0 * cov + C2))
                / ((ma * ma + mb * mb + C1) * (va + vb + C2));
            windows += 1;
        }
    }
    if windows == 0 {
        1.0
    } else {
        total / f64::from(windows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, c: [u8; 3]) -> Rgb {
        Rgb {
            w,
            h,
            data: (0..w * h).flat_map(|_| c).collect(),
        }
    }

    #[test]
    fn equal_images_have_no_difference() {
        let a = solid(16, 16, [10, 200, 30]);
        let (m, _) = compare(&a, &a);
        assert_eq!(m.mae, 0.0);
        assert_eq!(m.over, 0.0);
        assert!((m.ssim - 1.0).abs() < 1e-9);
    }

    #[test]
    fn black_against_white() {
        let (m, d) = compare(&solid(16, 8, [0; 3]), &solid(17, 8, [255; 3]));
        assert_eq!((d.w, d.h), (16, 8));
        assert_eq!(m.mae, 255.0);
        assert_eq!(m.over, 100.0);
        assert!(m.ssim < 0.01);
    }

    #[test]
    fn reads_pnm() {
        let mut ppm = b"P6\n# c\n2 1\n255\n".to_vec();
        ppm.extend([1, 2, 3, 4, 5, 6]);
        assert_eq!(Rgb::from_pnm(&ppm).unwrap().data, vec![1, 2, 3, 4, 5, 6]);
        let mut pgm = b"P5 2 1 255\n".to_vec();
        pgm.extend([7, 8]);
        assert_eq!(Rgb::from_pnm(&pgm).unwrap().data, vec![7, 7, 7, 8, 8, 8]);
    }
}
