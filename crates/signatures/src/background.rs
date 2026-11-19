//! Background removal for photographed or scanned signatures: paper becomes transparent,
//! ink stays.
//!
//! [`Prepared`] does the expensive part once per image: a downscaled working copy, a coarse
//! model of the paper colour (bright percentile per grid cell, cells covered by ink filled
//! from their neighbours, bilinear in between, which follows uneven lighting), and every
//! pixel's darkness relative to the paper under it. [`Prepared::apply`] then maps darkness to
//! alpha through a 256-entry table, removes specks, recolours and crops, so slider changes
//! stay interactive.

use std::sync::atomic::{AtomicU32, Ordering};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::raster::Rgba;

/// How ink is coloured after removal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ink {
    /// The photographed ink colour, with the paper's tint taken out.
    Original,
    /// One colour, e.g. dark blue or black.
    Color([u8; 3]),
}

impl Ink {
    pub const BLACK: Ink = Ink::Color([0x10, 0x10, 0x10]);
    pub const DARK_BLUE: Ink = Ink::Color([0x0b, 0x24, 0x7a]);
}

/// The user's choices for one signature; re-applied to the kept original at any time.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub remove_background: bool,
    /// 0 keeps faint strokes, 100 keeps only the darkest ink.
    pub threshold: u8,
    pub ink: Ink,
    pub despeckle: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            remove_background: true,
            threshold: 50,
            ink: Ink::Original,
            despeckle: true,
        }
    }
}

/// Long side of the image interactive previews work on.
pub const PREVIEW_SIDE: u32 = 1024;
/// Long side final processing works on, before cropping.
pub const PROCESS_SIDE: u32 = 3000;
/// Stored signatures are at most this wide (and tall).
pub const STORED_MAX: u32 = 1500;

/// The full pipeline for storing: process at high resolution, crop, then fit
/// [`STORED_MAX`].
pub fn process(original: &Rgba, s: &Settings) -> Rgba {
    Prepared::new(original, PROCESS_SIDE)
        .apply(s)
        .fit_within(STORED_MAX, STORED_MAX)
}

/// An image analysed for background removal.
#[derive(Debug)]
pub struct Prepared {
    img: Rgba,
    /// Paper colour under each pixel.
    paper: Vec<[u8; 3]>,
    /// How much darker than the paper each pixel is, 0..=255.
    dark: Vec<u8>,
    /// Darkness of plain paper (noise, texture) and of solid ink.
    floor: f32,
    ink: f32,
}

impl Prepared {
    /// Analyses `original` scaled to at most `max_side` on its long side.
    pub fn new(original: &Rgba, max_side: u32) -> Prepared {
        let mut img = original.fit_within(max_side, max_side);
        flatten_on_white(&mut img);
        let paper = paper_model(&img);
        let dark: Vec<u8> = img
            .pixels
            .par_chunks_exact(4)
            .zip(paper.par_iter())
            .map(|(p, bg)| {
                let d = (0..3)
                    .map(|c| 1.0 - f32::from(p[c]) / f32::from(bg[c].max(1)))
                    .fold(0.0f32, f32::max);
                (d.clamp(0.0, 1.0) * 255.0).round() as u8
            })
            .collect();
        let hist = dark
            .par_chunks(1 << 16)
            .map(|c| {
                let mut h = [0u32; 256];
                for &d in c {
                    h[usize::from(d)] += 1;
                }
                h
            })
            .reduce(|| [0; 256], |a, b| std::array::from_fn(|i| a[i] + b[i]));
        let pct = |q: f64| {
            let target = (dark.len() as f64 * q) as u64;
            let mut acc = 0u64;
            hist.iter()
                .position(|&n| {
                    acc += u64::from(n);
                    acc > target
                })
                .unwrap_or(255) as f32
                / 255.0
        };
        // Signatures cover little of the picture: high percentiles are still paper, the
        // top one is ink.
        let floor = (pct(0.8) * 1.5 + 0.02).clamp(0.03, 0.35);
        let ink = pct(0.995).max(floor + 0.15).min(1.0);
        Prepared {
            img,
            paper,
            dark,
            floor,
            ink,
        }
    }

    pub fn width(&self) -> u32 {
        self.img.width
    }

    pub fn height(&self) -> u32 {
        self.img.height
    }

    /// The image with `s` applied: transparent paper, cropped to the ink with a margin.
    /// Without background removal, the (scaled) image as is.
    pub fn apply(&self, s: &Settings) -> Rgba {
        if !s.remove_background {
            return self.img.clone();
        }
        let alpha = self.alpha(s);
        let w = self.img.width as usize;
        let Some((x0, y0, x1, y1)) = ink_bounds(&alpha, w) else {
            return Rgba::new(1, 1);
        };
        let mut out = Rgba::new((x1 - x0) as u32, (y1 - y0) as u32);
        out.pixels
            .par_chunks_exact_mut((x1 - x0) * 4)
            .enumerate()
            .for_each(|(row, o)| {
                let start = (y0 + row) * w + x0;
                let src = &self.img.pixels[start * 4..(start + x1 - x0) * 4];
                let pixels = o.as_chunks_mut::<4>().0.iter_mut();
                let inputs = src.as_chunks::<4>().0.iter();
                let bgs = &self.paper[start..start + x1 - x0];
                let alphas = &alpha[start..start + x1 - x0];
                for (((o, p), bg), &a) in pixels.zip(inputs).zip(bgs).zip(alphas) {
                    if a == 0 {
                        continue;
                    }
                    let rgb = match s.ink {
                        Ink::Color(c) => c,
                        Ink::Original => {
                            let af = f32::from(a) / 255.0;
                            std::array::from_fn(|c| {
                                let v = (f32::from(p[c]) - (1.0 - af) * f32::from(bg[c])) / af;
                                v.clamp(0.0, 255.0).round() as u8
                            })
                        }
                    };
                    *o = [rgb[0], rgb[1], rgb[2], a];
                }
            });
        out
    }

    /// The alpha mask `s` gives, at [`Prepared::width`] x [`Prepared::height`] (before
    /// cropping); for overlays that show what will be kept.
    pub fn alpha(&self, s: &Settings) -> Vec<u8> {
        let lut = self.alpha_table(s.threshold);
        let mut alpha: Vec<u8> = self.dark.par_iter().map(|&d| lut[usize::from(d)]).collect();
        if s.despeckle {
            despeckle(&mut alpha, self.img.width, self.img.height);
        }
        alpha
    }

    /// Alpha by darkness: zero up to a cut-off set by the threshold, a soft ramp, then opaque.
    fn alpha_table(&self, threshold: u8) -> [u8; 256] {
        let t = f32::from(threshold.min(100)) / 100.0;
        let low = self.floor + t * 0.6 * (self.ink - self.floor);
        let high = low + ((self.ink - low) * 0.5).max(0.06);
        std::array::from_fn(|i| {
            let x = ((i as f32 / 255.0 - low) / (high - low)).clamp(0.0, 1.0);
            (x * x * (3.0 - 2.0 * x) * 255.0).round() as u8
        })
    }
}

fn flatten_on_white(img: &mut Rgba) {
    img.pixels.par_chunks_exact_mut(4).for_each(|p| {
        let a = u16::from(p[3]);
        if a < 255 {
            for c in &mut p[..3] {
                *c = ((u16::from(*c) * a + 255 * (255 - a) + 127) / 255) as u8;
            }
            p[3] = 255;
        }
    });
}

fn luma(p: &[u8]) -> u32 {
    (u32::from(p[0]) * 77 + u32::from(p[1]) * 150 + u32::from(p[2]) * 29) >> 8
}

/// The paper colour under every pixel.
fn paper_model(img: &Rgba) -> Vec<[u8; 3]> {
    let (w, h) = (img.width as usize, img.height as usize);
    let cell = (w.max(h) / 32).max(8);
    let (gw, gh) = (w.div_ceil(cell), h.div_ceil(cell));
    // Mean colour of the brightest quarter of each cell.
    let mut grid: Vec<Option<[f32; 3]>> = (0..gw * gh)
        .into_par_iter()
        .map(|g| {
            let (cx, cy) = (g % gw * cell, g / gw * cell);
            let mut hist = [0u32; 256];
            let mut n = 0;
            for y in cy..(cy + cell).min(h) {
                for x in cx..(cx + cell).min(w) {
                    let i = (y * w + x) * 4;
                    hist[luma(&img.pixels[i..i + 4]) as usize] += 1;
                    n += 1;
                }
            }
            let mut acc = 0;
            let cut = hist
                .iter()
                .rposition(|&c| {
                    acc += c;
                    acc * 4 >= n
                })
                .unwrap_or(0) as u32;
            let mut sum = [0f32; 3];
            let mut k = 0f32;
            for y in cy..(cy + cell).min(h) {
                for x in cx..(cx + cell).min(w) {
                    let p = &img.pixels[(y * w + x) * 4..(y * w + x) * 4 + 4];
                    if luma(p) >= cut {
                        for c in 0..3 {
                            sum[c] += f32::from(p[c]);
                        }
                        k += 1.0;
                    }
                }
            }
            Some(sum.map(|s| s / k.max(1.0)))
        })
        .collect();
    // Cells mostly covered by ink are much darker than typical paper: rebuild them from
    // their neighbours.
    let lum = |c: &[f32; 3]| 0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2];
    let mut lums: Vec<f32> = grid.iter().flatten().map(lum).collect();
    lums.sort_by(f32::total_cmp);
    let typical = lums.get(lums.len() / 2).copied().unwrap_or(255.0);
    // Darker than a neighbour by more than lighting changes over one cell, or far darker
    // than the page as a whole.
    let lums: Vec<f32> = grid.iter().map(|c| c.as_ref().map_or(0.0, lum)).collect();
    for (i, c) in grid.iter_mut().enumerate() {
        let (gx, gy) = (i % gw, i / gw);
        let mut brightest = 0f32;
        for y in gy.saturating_sub(1)..(gy + 2).min(gh) {
            for x in gx.saturating_sub(1)..(gx + 2).min(gw) {
                brightest = brightest.max(lums[y * gw + x]);
            }
        }
        if lums[i] < brightest * 0.85 || lums[i] < typical * 0.5 {
            *c = None;
        }
    }
    let fill = [typical; 3];
    for _ in 0..gw.max(gh) {
        if grid.iter().all(Option::is_some) {
            break;
        }
        let prev = grid.clone();
        for gy in 0..gh {
            for gx in 0..gw {
                if prev[gy * gw + gx].is_some() {
                    continue;
                }
                grid[gy * gw + gx] = mean_of(&prev, gw, gh, gx, gy);
            }
        }
    }
    let grid: Vec<Option<[f32; 3]>> = grid.into_iter().map(|c| c.or(Some(fill))).collect();
    // Smooth the grid (3x3 mean) to suppress texture.
    let smooth: Vec<[f32; 3]> = (0..gw * gh)
        .map(|i| mean_of(&grid, gw, gh, i % gw, i / gw).unwrap_or(fill))
        .collect();
    // Bilinear between cell centres: the two grid indices and weight along each axis.
    let axis = |i: usize, n: usize| {
        let f = ((i as f32 + 0.5) / cell as f32 - 0.5).clamp(0.0, (n - 1) as f32);
        let i0 = f.floor() as usize;
        (i0, (i0 + 1).min(n - 1), f.fract())
    };
    let columns: Vec<(usize, usize, f32)> = (0..w).map(|x| axis(x, gw)).collect();
    let mut paper = vec![[0u8; 3]; w * h];
    paper.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let (gy0, gy1, ty) = axis(y, gh);
        // This row's colour at each grid column, then along the row.
        let line: Vec<[f32; 3]> = (0..gw)
            .map(|gx| {
                let (a, b) = (smooth[gy0 * gw + gx], smooth[gy1 * gw + gx]);
                std::array::from_fn(|c| a[c] * (1.0 - ty) + b[c] * ty)
            })
            .collect();
        for (px, &(gx0, gx1, tx)) in row.iter_mut().zip(&columns) {
            let (a, b) = (line[gx0], line[gx1]);
            *px = std::array::from_fn(|c| {
                (a[c] * (1.0 - tx) + b[c] * tx).round().clamp(0.0, 255.0) as u8
            });
        }
    });
    paper
}

fn mean_of(g: &[Option<[f32; 3]>], gw: usize, gh: usize, gx: usize, gy: usize) -> Option<[f32; 3]> {
    let mut sum = [0f32; 3];
    let mut n = 0.0;
    for y in gy.saturating_sub(1)..(gy + 2).min(gh) {
        for x in gx.saturating_sub(1)..(gx + 2).min(gw) {
            if let Some(c) = g[y * gw + x] {
                for k in 0..3 {
                    sum[k] += c[k];
                }
                n += 1.0;
            }
        }
    }
    (n > 0.0).then(|| sum.map(|s| s / n))
}

/// Clears 8-connected blobs of visible pixels too small to be ink. Strips of rows are
/// labelled in parallel (union-find over pixel indices), then joined at the strip edges.
fn despeckle(alpha: &mut [u8], w: u32, h: u32) {
    let (w, h) = (w as usize, h as usize);
    if w == 0 || h == 0 {
        return;
    }
    let min_area = ((w * h) as f64 * 3e-5).max(5.0) as u32;
    let strip = h.div_ceil(rayon::current_num_threads() * 2).max(32) * w;
    // 1 + the parent's pixel index; 0 for transparent pixels. Zeroed buffers are cheap to
    // allocate, and only ink pixels are written.
    let mut parent = vec![0u32; w * h];
    parent
        .par_chunks_mut(strip)
        .zip(alpha.par_chunks(strip))
        .enumerate()
        .for_each(|(k, (p, a))| {
            let base = k * strip;
            for i in 0..a.len() {
                if a[i] == 0 {
                    continue;
                }
                p[i] = (base + i + 1) as u32;
                let x = i % w;
                if x > 0 && a[i - 1] != 0 {
                    join(p, base, i, i - 1);
                }
                if i >= w {
                    for n in above(i, x, w).filter(|&n| a[n] != 0) {
                        join(p, base, i, n);
                    }
                }
            }
        });
    for top in (strip..w * h).step_by(strip) {
        for i in top..top + w {
            if alpha[i] != 0 {
                for n in above(i, i - top, w).filter(|&n| alpha[n] != 0) {
                    join(&mut parent, 0, i, n);
                }
            }
        }
    }
    // Blob sizes, counted at their roots.
    let mut area = vec![0u32; w * h];
    let area = AtomicU32::from_mut_slice(&mut area);
    parent.par_iter().enumerate().for_each(|(i, &p)| {
        if p != 0 {
            area[root(&parent, i)].fetch_add(1, Ordering::Relaxed);
        }
    });
    alpha.par_iter_mut().enumerate().for_each(|(i, a)| {
        if parent[i] != 0 && area[root(&parent, i)].load(Ordering::Relaxed) < min_area {
            *a = 0;
        }
    });
}

/// The 8-connected neighbours of pixel `i` (column `x`) in the row above.
fn above(i: usize, x: usize, w: usize) -> impl Iterator<Item = usize> {
    let up = i - w;
    [
        (x > 0).then(|| up - 1),
        Some(up),
        (x + 1 < w).then_some(up + 1),
    ]
    .into_iter()
    .flatten()
}

/// The root of `x` in a forest stored from pixel `base` on, halving the path.
fn find(p: &mut [u32], base: usize, mut x: usize) -> usize {
    let up = |p: &[u32], x: usize| p[x] as usize - 1 - base;
    loop {
        let u = up(p, x);
        if u == x {
            return x;
        }
        let grand = up(p, u);
        p[x] = (grand + base + 1) as u32;
        x = grand;
    }
}

/// Merges the blobs of `a` and `b`, rooted at the lower index.
fn join(p: &mut [u32], base: usize, a: usize, b: usize) {
    let (ra, rb) = (find(p, base, a), find(p, base, b));
    if ra != rb {
        p[ra.max(rb)] = (ra.min(rb) + base + 1) as u32;
    }
}

fn root(p: &[u32], mut x: usize) -> usize {
    while p[x] as usize - 1 != x {
        x = p[x] as usize - 1;
    }
    x
}

/// The box around the visible pixels plus a margin, as `x0, y0, x1, y1`.
fn ink_bounds(alpha: &[u8], w: usize) -> Option<(usize, usize, usize, usize)> {
    let h = alpha.len() / w;
    let (x0, y0, x1, y1) = alpha
        .par_chunks_exact(w)
        .enumerate()
        .filter_map(|(y, row)| {
            let first = row.iter().position(|&a| a != 0)?;
            let last = row.iter().rposition(|&a| a != 0)?;
            Some((first, y, last + 1, y + 1))
        })
        .reduce_with(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))?;
    let m = (w.max(h) / 50).max(4);
    Some((
        x0.saturating_sub(m),
        y0.saturating_sub(m),
        (x1 + m).min(w),
        (y1 + m).min(h),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The single-threaded labelling this replaced, as a reference.
    fn despeckle_reference(alpha: &mut [u8], w: usize, h: usize) {
        let min_area = ((w * h) as f64 * 3e-5).max(5.0) as u32;
        let mut seen = vec![false; w * h];
        for start in 0..w * h {
            if alpha[start] == 0 || seen[start] {
                continue;
            }
            let mut blob = vec![start];
            seen[start] = true;
            let mut k = 0;
            while k < blob.len() {
                let (x, y) = ((blob[k] % w) as i64, (blob[k] / w) as i64);
                for (dx, dy) in (-1..=1).flat_map(|dy| (-1..=1).map(move |dx| (dx, dy))) {
                    let (nx, ny) = (x + dx, y + dy);
                    if (0..w as i64).contains(&nx) && (0..h as i64).contains(&ny) {
                        let n = ny as usize * w + nx as usize;
                        if alpha[n] != 0 && !seen[n] {
                            seen[n] = true;
                            blob.push(n);
                        }
                    }
                }
                k += 1;
            }
            if (blob.len() as u32) < min_area {
                for i in blob {
                    alpha[i] = 0;
                }
            }
        }
    }

    #[test]
    fn despeckle_matches_flood_fill() {
        let (w, h) = (257, 700);
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut alpha: Vec<u8> = (0..w * h)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                // Sparse noise that forms blobs of all sizes, many across strip edges.
                if seed % 100 < 38 { 200 } else { 0 }
            })
            .collect();
        // A long diagonal joined only through corners, spanning every strip.
        for y in 0..h {
            alpha[y * w + y * (w - 1) / h] = 255;
        }
        let mut expected = alpha.clone();
        despeckle_reference(&mut expected, w, h);
        despeckle(&mut alpha, w as u32, h as u32);
        assert_eq!(alpha, expected);
        assert!(alpha.iter().any(|&a| a != 0) && alpha.contains(&0));
    }
}
