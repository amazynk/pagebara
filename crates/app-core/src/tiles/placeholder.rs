//! Stand-in pages for tests: white paper with a faint half-inch grid, the page number, each
//! tile's coordinates, and an artificial render delay.

use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::{Job, PageRenderer};

const GRID_PT: f64 = 36.0;
const PAPER: [u8; 4] = [255, 255, 255, 255];
const GRID: [u8; 4] = [228, 228, 231, 255];
const NUMBER: [u8; 4] = [212, 212, 216, 255];
const LABEL: [u8; 4] = [37, 99, 235, 255];

#[derive(Debug, Clone)]
pub struct PlaceholderRenderer {
    /// Simulated render time per job, in milliseconds.
    pub delay_ms: Range<u64>,
}

impl Default for PlaceholderRenderer {
    fn default() -> Self {
        PlaceholderRenderer { delay_ms: 5..21 }
    }
}

impl PageRenderer for PlaceholderRenderer {
    fn render(&self, job: &Job, cancel: &AtomicBool, out: &mut [u8]) -> bool {
        let mut c = Canvas {
            out,
            w: job.rect.w,
            h: job.rect.h,
        };
        c.fill(0, 0, c.w, c.h, PAPER);
        let s = job.scale;
        let (x0, y0) = (job.rect.x, job.rect.y);
        // Grid lines: the first device pixel of each 36 pt cell.
        let cell = |p: u32| (f64::from(p) / s / GRID_PT).floor() as i64;
        for px in 0..c.w {
            if px + x0 > 0 && cell(px + x0) != cell(px + x0 - 1) {
                c.fill(px, 0, 1, c.h, GRID);
            }
        }
        for py in 0..c.h {
            if py + y0 > 0 && cell(py + y0) != cell(py + y0 - 1) {
                c.fill(0, py, c.w, 1, GRID);
            }
        }
        // The page number, 72 pt tall, an inch below the top edge, centred on the grid.
        let digits = (job.page + 1).to_string();
        let unit = 72.0 / 7.0 * s;
        let text_w = text_width(&digits) as f64 * unit;
        let origin = (
            (job.page_size.0 * s - text_w) / 2.0 - f64::from(x0),
            72.0 * s - f64::from(y0),
        );
        c.text(&digits, origin, unit, NUMBER);
        if !job.key.is_preview() {
            let label = format!("{},{} z{}", job.key.x, job.key.y, job.key.level);
            c.text(&label, (6.0, 6.0), 2.0, LABEL);
        }
        let span = self.delay_ms.end.saturating_sub(self.delay_ms.start).max(1);
        let ms = self.delay_ms.start + hash(job) % span;
        std::thread::sleep(Duration::from_millis(ms));
        !cancel.load(Ordering::Relaxed)
    }
}

fn hash(job: &Job) -> u64 {
    let k = job.key;
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for v in [
        u64::from(k.page),
        k.level as u64,
        u64::from(k.x),
        u64::from(k.y),
    ] {
        h = (h ^ v).wrapping_mul(0x0100_0000_01b3);
    }
    h
}

struct Canvas<'a> {
    out: &'a mut [u8],
    w: u32,
    h: u32,
}

impl Canvas<'_> {
    fn fill(&mut self, x: u32, y: u32, w: u32, h: u32, rgba: [u8; 4]) {
        let (x1, y1) = ((x + w).min(self.w), (y + h).min(self.h));
        for row in y.min(self.h)..y1 {
            let start = ((row * self.w + x.min(x1)) * 4) as usize;
            let end = ((row * self.w + x1) * 4) as usize;
            for px in self.out[start..end].as_chunks_mut::<4>().0 {
                *px = rgba;
            }
        }
    }

    /// Fills the rectangle `[x0, x1) × [y0, y1)` given in fractional pixels, clipped.
    fn fill_f(&mut self, x0: f64, y0: f64, x1: f64, y1: f64, rgba: [u8; 4]) {
        let clip = |v: f64, max: u32| v.round().clamp(0.0, f64::from(max)) as u32;
        let (ax, bx) = (clip(x0, self.w), clip(x1, self.w));
        let (ay, by) = (clip(y0, self.h), clip(y1, self.h));
        if bx > ax && by > ay {
            self.fill(ax, ay, bx - ax, by - ay, rgba);
        }
    }

    fn text(&mut self, s: &str, (x, y): (f64, f64), unit: f64, rgba: [u8; 4]) {
        let mut pen = x;
        for ch in s.chars() {
            let rows = glyph(ch);
            for (r, bits) in rows.iter().enumerate() {
                for col in 0..5 {
                    if bits & (0x10 >> col) != 0 {
                        let gx = pen + f64::from(col) * unit;
                        let gy = y + r as f64 * unit;
                        self.fill_f(gx, gy, gx + unit, gy + unit, rgba);
                    }
                }
            }
            pen += 6.0 * unit;
        }
    }
}

fn text_width(s: &str) -> usize {
    (s.chars().count() * 6).saturating_sub(1)
}

/// 5 × 7 glyphs, one byte per row, high bit on the left.
fn glyph(c: char) -> [u8; 7] {
    match c {
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        ',' => [0x00, 0x00, 0x00, 0x00, 0x06, 0x04, 0x08],
        '-' => [0x00, 0x00, 0x00, 0x1F, 0x00, 0x00, 0x00],
        'z' => [0x00, 0x00, 0x1F, 0x02, 0x04, 0x08, 0x1F],
        _ => [0; 7],
    }
}
