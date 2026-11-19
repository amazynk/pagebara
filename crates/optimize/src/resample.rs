//! Separable image downsampling with a windowed kernel, rows in parallel.

use rayon::prelude::*;

/// The kernel used when shrinking images.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Filter {
    /// Catmull-Rom (a = -0.5).
    Bicubic,
    /// Lanczos with three lobes: the sharpest result.
    #[default]
    Lanczos3,
}

impl Filter {
    fn support(self) -> f32 {
        match self {
            Filter::Bicubic => 2.0,
            Filter::Lanczos3 => 3.0,
        }
    }

    fn weight(self, x: f32) -> f32 {
        let x = x.abs();
        match self {
            Filter::Bicubic => {
                let a = -0.5;
                if x < 1.0 {
                    (a + 2.0) * x * x * x - (a + 3.0) * x * x + 1.0
                } else if x < 2.0 {
                    a * x * x * x - 5.0 * a * x * x + 8.0 * a * x - 4.0 * a
                } else {
                    0.0
                }
            }
            Filter::Lanczos3 => {
                if x < 1e-6 {
                    1.0
                } else if x < 3.0 {
                    let px = std::f32::consts::PI * x;
                    3.0 * px.sin() * (px / 3.0).sin() / (px * px)
                } else {
                    0.0
                }
            }
        }
    }
}

/// For each destination index: the first source index and normalised weights.
fn taps(src: usize, dst: usize, f: Filter) -> Vec<(usize, Vec<f32>)> {
    let scale = src as f32 / dst as f32;
    let fscale = scale.max(1.0);
    let support = f.support() * fscale;
    (0..dst)
        .map(|i| {
            let center = (i as f32 + 0.5) * scale;
            let lo = ((center - support).floor().max(0.0)) as usize;
            let hi = ((center + support).ceil() as usize).min(src);
            let mut w: Vec<f32> = (lo..hi)
                .map(|j| f.weight((j as f32 + 0.5 - center) / fscale))
                .collect();
            let sum: f32 = w.iter().sum();
            if sum.abs() > 1e-6 {
                w.iter_mut().for_each(|v| *v /= sum);
            }
            (lo, w)
        })
        .collect()
}

/// Resamples `n`-channel 8-bit pixels from `sw` × `sh` to `dw` × `dh`.
pub(crate) fn resample(
    src: &[u8],
    sw: usize,
    sh: usize,
    n: usize,
    dw: usize,
    dh: usize,
    f: Filter,
) -> Vec<u8> {
    let tx = taps(sw, dw, f);
    let ty = taps(sh, dh, f);
    // Horizontal pass into floats, then vertical.
    let mut mid = vec![0f32; dw * n * sh];
    mid.par_chunks_mut(dw * n).enumerate().for_each(|(y, row)| {
        let s = &src[y * sw * n..(y + 1) * sw * n];
        for (x, (lo, w)) in tx.iter().enumerate() {
            for c in 0..n {
                let mut acc = 0.0;
                for (k, wt) in w.iter().enumerate() {
                    acc += wt * f32::from(s[(lo + k) * n + c]);
                }
                row[x * n + c] = acc;
            }
        }
    });
    let mut out = vec![0u8; dw * n * dh];
    out.par_chunks_mut(dw * n).enumerate().for_each(|(y, row)| {
        let (lo, w) = &ty[y];
        for (i, o) in row.iter_mut().enumerate() {
            let mut acc = 0.0;
            for (k, wt) in w.iter().enumerate() {
                acc += wt * mid[(lo + k) * dw * n + i];
            }
            *o = (acc + 0.5).clamp(0.0, 255.0) as u8;
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_images_stay_flat() {
        for f in [Filter::Bicubic, Filter::Lanczos3] {
            let src = vec![77u8; 40 * 30 * 3];
            let out = resample(&src, 40, 30, 3, 13, 11, f);
            assert!(out.iter().all(|&v| v == 77), "{f:?}");
        }
    }
}
