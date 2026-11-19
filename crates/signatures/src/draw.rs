//! Signatures drawn with a pen, mouse or finger: strokes rendered to an anti-aliased RGBA
//! image, with the width following pressure.

use serde::{Deserialize, Serialize};

use crate::raster::Rgba;

/// A pen sample in canvas units (any scale, y down); `pressure` in 0..=1 when the device
/// reports it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrokePoint {
    pub x: f32,
    pub y: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pressure: Option<f32>,
}

impl StrokePoint {
    pub fn new(x: f32, y: f32) -> Self {
        StrokePoint {
            x,
            y,
            pressure: None,
        }
    }
}

pub type Stroke = Vec<StrokePoint>;

/// Strokes plus how to render them; stored with drawn signatures.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Drawing {
    pub strokes: Vec<Stroke>,
    /// Pen width at full pressure, in canvas units.
    pub pen_width: f32,
}

/// Rendered drawings are at most this wide (pixels).
const MAX_WIDTH: f32 = 1500.0;
/// And at most this many pixels per canvas unit.
const MAX_SCALE: f32 = 4.0;
/// Width at zero pressure, relative to full.
const MIN_PRESSURE_WIDTH: f32 = 0.35;

/// Renders `d` in `color`, cropped to the ink with a small margin.
pub fn render(d: &Drawing, color: [u8; 3]) -> Rgba {
    let pts = || d.strokes.iter().flatten();
    let Some(first) = pts().next() else {
        return Rgba::new(1, 1);
    };
    let (mut x0, mut y0, mut x1, mut y1) = (first.x, first.y, first.x, first.y);
    for p in pts() {
        (x0, y0, x1, y1) = (x0.min(p.x), y0.min(p.y), x1.max(p.x), y1.max(p.y));
    }
    let pad = d.pen_width.max(0.1);
    let (w, h) = (x1 - x0 + 2.0 * pad, y1 - y0 + 2.0 * pad);
    let scale = (MAX_WIDTH / w).min(MAX_SCALE);
    let (pw, ph) = ((w * scale).ceil() as u32 + 2, (h * scale).ceil() as u32 + 2);
    let mut cov = vec![0f32; pw as usize * ph as usize];
    let map = |p: &StrokePoint| {
        let r = d.pen_width / 2.0
            * p.pressure.map_or(1.0, |pr| {
                MIN_PRESSURE_WIDTH + (1.0 - MIN_PRESSURE_WIDTH) * pr.clamp(0.0, 1.0)
            });
        (
            (p.x - x0 + pad) * scale + 1.0,
            (p.y - y0 + pad) * scale + 1.0,
            (r * scale).max(0.5),
        )
    };
    for s in &d.strokes {
        let samples: Vec<(f32, f32, f32)> = s.iter().map(map).collect();
        let dense = densify(&samples);
        if dense.len() == 1 {
            disc(&mut cov, pw, ph, dense[0], dense[0]);
        }
        for seg in dense.windows(2) {
            disc(&mut cov, pw, ph, seg[0], seg[1]);
        }
    }
    let mut img = Rgba::new(pw, ph);
    for (p, c) in img.pixels.as_chunks_mut::<4>().0.iter_mut().zip(&cov) {
        p[..3].copy_from_slice(&color);
        p[3] = (c * 255.0).round() as u8;
    }
    img
}

/// Catmull-Rom interpolation between samples, about one point per pixel.
fn densify(s: &[(f32, f32, f32)]) -> Vec<(f32, f32, f32)> {
    if s.len() < 3 {
        return s.to_vec();
    }
    let mut out = vec![s[0]];
    for i in 0..s.len() - 1 {
        let p0 = s[i.saturating_sub(1)];
        let (p1, p2) = (s[i], s[i + 1]);
        let p3 = s[(i + 2).min(s.len() - 1)];
        let len = (p2.0 - p1.0).hypot(p2.1 - p1.1);
        let n = (len.ceil() as usize).clamp(1, 256);
        for k in 1..=n {
            let t = k as f32 / n as f32;
            let cr = |a: f32, b: f32, c: f32, d: f32| {
                0.5 * (2.0 * b
                    + (c - a) * t
                    + (2.0 * a - 5.0 * b + 4.0 * c - d) * t * t
                    + (3.0 * b - a - 3.0 * c + d) * t * t * t)
            };
            out.push((
                cr(p0.0, p1.0, p2.0, p3.0),
                cr(p0.1, p1.1, p2.1, p3.1),
                p1.2 + (p2.2 - p1.2) * t,
            ));
        }
    }
    out
}

/// Coverage of a segment with round ends and linearly varying radius.
fn disc(cov: &mut [f32], w: u32, h: u32, a: (f32, f32, f32), b: (f32, f32, f32)) {
    let r = a.2.max(b.2) + 1.0;
    let bx0 = (a.0.min(b.0) - r).floor().max(0.0) as u32;
    let by0 = (a.1.min(b.1) - r).floor().max(0.0) as u32;
    let bx1 = ((a.0.max(b.0) + r).ceil() as u32).min(w);
    let by1 = ((a.1.max(b.1) + r).ceil() as u32).min(h);
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    for y in by0..by1 {
        for x in bx0..bx1 {
            let (px, py) = (x as f32 + 0.5 - a.0, y as f32 + 0.5 - a.1);
            let t = if len2 > 0.0 {
                ((px * dx + py * dy) / len2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let d = (px - t * dx).hypot(py - t * dy);
            let rad = a.2 + (b.2 - a.2) * t;
            let c = (rad - d + 0.5).clamp(0.0, 1.0);
            let i = (y * w + x) as usize;
            if c > cov[i] {
                cov[i] = c;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pressure_changes_width() {
        let line = |p: f32| Drawing {
            strokes: vec![
                (0..=10)
                    .map(|i| StrokePoint {
                        x: i as f32 * 10.0,
                        y: 0.0,
                        pressure: Some(p),
                    })
                    .collect(),
            ],
            pen_width: 4.0,
        };
        let ink = |img: &Rgba| {
            img.pixels
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|p| p[3] > 128)
                .count()
        };
        let (thin, thick) = (render(&line(0.0), [0; 3]), render(&line(1.0), [0; 3]));
        assert!(ink(&thick) > ink(&thin) * 2);
        // Anti-aliased: some partial coverage.
        assert!(
            thick
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .any(|p| p[3] > 20 && p[3] < 230)
        );
    }
}
