//! A synthetic "scanned signature": off-white paper with uneven lighting and grain, a dark
//! blue pen stroke, and a few specks of dust.

#![allow(dead_code)]

use pagebara_signatures::Rgba;

pub const PAPER: [f32; 3] = [238.0, 232.0, 215.0];
pub const INK: [f32; 3] = [25.0, 35.0, 95.0];

pub struct Scan {
    pub img: Rgba,
    /// Ink coverage per pixel, 0..=1.
    pub coverage: Vec<f32>,
    pub specks: Vec<(u32, u32)>,
    pub radius: f32,
}

pub fn scan(w: u32, h: u32) -> Scan {
    let n = (w * h) as usize;
    let mut coverage = vec![0f32; n];
    let radius = (h as f32 / 90.0).max(2.5);
    let samples = (w * 4) as usize;
    for i in 0..=samples {
        let t = i as f32 / samples as f32;
        let cx = w as f32 * (0.15 + 0.7 * t);
        let cy = h as f32 * (0.5 + 0.2 * (t * std::f32::consts::PI * 5.0).sin() * (1.0 - t * 0.5));
        let r = radius * (0.8 + 0.4 * (t * 9.0).sin().abs());
        let (x0, x1) = (
            (cx - r - 2.0).max(0.0) as u32,
            ((cx + r + 2.0) as u32).min(w - 1),
        );
        let (y0, y1) = (
            (cy - r - 2.0).max(0.0) as u32,
            ((cy + r + 2.0) as u32).min(h - 1),
        );
        for y in y0..=y1 {
            for x in x0..=x1 {
                let d = (x as f32 + 0.5 - cx).hypot(y as f32 + 0.5 - cy);
                let c = (r - d + 0.5).clamp(0.0, 1.0);
                let k = (y * w + x) as usize;
                coverage[k] = coverage[k].max(c);
            }
        }
    }
    let specks: Vec<(u32, u32)> = (0..12)
        .map(|i| (w / 20 + i * w / 14, h / 10 + (i % 3) * h / 40))
        .collect();
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut img = Rgba::new(w, h);
    for y in 0..h {
        for x in 0..w {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let noise = (seed % 13) as f32 - 6.0;
            let light = 1.0 - 0.3 * (x as f32 / w as f32) * (y as f32 / h as f32);
            let k = (y * w + x) as usize;
            let mut c = coverage[k];
            if specks
                .iter()
                .any(|&(sx, sy)| x.abs_diff(sx) <= 1 && y.abs_diff(sy) <= 1)
            {
                c = 0.8;
            }
            let p = &mut img.pixels[k * 4..k * 4 + 4];
            for i in 0..3 {
                let v = (PAPER[i] * (1.0 - c) + INK[i] * c) * light + noise;
                p[i] = v.clamp(0.0, 255.0) as u8;
            }
            p[3] = 255;
        }
    }
    Scan {
        img,
        coverage,
        specks,
        radius,
    }
}

pub fn rgb(img: &Rgba) -> Vec<u8> {
    img.pixels
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect()
}
