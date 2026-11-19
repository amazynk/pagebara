//! Underscores the text detector leaves out. Its boxes stop where letters do, so the low
//! strokes of `__init__` or `snake_case_` at either end of a word fall outside them, and a
//! line breaks apart there (`if`, `name`, `==`). A box grows over such a stroke when the
//! picture shows one next to it (dark along the foot of the line, nothing above), and boxes
//! of one line that then come within a space of each other become one, so the recogniser
//! reads the line whole, underscores included.

use crate::image::{RgbImage, RotRect};

/// Boxes of `img` with the underscores next to them taken in (dark text on light).
pub(crate) fn take_in_underscores(img: &RgbImage, boxes: Vec<RotRect>) -> Vec<RotRect> {
    let mut flat: Vec<(Flat, bool)> = Vec::new();
    let mut rest = Vec::new();
    for b in &boxes {
        match Flat::of(b) {
            Some(f) => flat.push((f, false)),
            None => rest.push(*b),
        }
    }
    let level = luma(img);
    let found: Vec<Flat> = flat.iter().map(|f| f.0).collect();
    let ink = threshold(&level, img.width, &found);
    let Some(ink) = ink else { return boxes };
    let dark = |x: usize, y: usize| level[y * img.width + x] < ink;
    let (w, h) = (img.width as f32, img.height as f32);
    // Each box's line: from the top of the highest box on it to the foot of the lowest
    // (words of small letters have short boxes).
    let rows: Vec<(f32, f32)> = flat
        .iter()
        .map(|(f, _)| {
            flat.iter()
                .map(|(o, _)| o)
                .filter(|o| o.y1.min(f.y1) - o.y0.max(f.y0) > 0.5 * (o.y1 - o.y0).min(f.y1 - f.y0))
                .fold((f.y0, f.y1), |r, o| (r.0.min(o.y0), r.1.max(o.y1)))
        })
        .collect();
    for ((f, grown), &(row_top, row_foot)) in flat.iter_mut().zip(&rows) {
        let lh = row_foot - row_top;
        let top = (row_top + 0.15 * lh).max(0.0) as usize;
        let mid = (row_foot - 0.3 * lh).clamp(0.0, h - 1.0) as usize;
        let foot = (row_foot + 0.3 * lh).clamp(0.0, h - 1.0) as usize;
        let thin = (0.15 * lh).max(2.0) as usize;
        // Columns from the box's edge outwards with a thin stroke at the foot of the line
        // (its lowest dark pixel), nothing above and nothing at the bottom of the band.
        let stroke = |x: usize| -> Option<Option<usize>> {
            if (top..mid).any(|y| dark(x, y)) || dark(x, foot) {
                return None;
            }
            let ink = (mid..foot).filter(|&y| dark(x, y)).count();
            if ink > thin {
                return None;
            }
            Some((mid..foot).rev().find(|&y| dark(x, y)))
        };
        // Where a stroke from `from` ends and how low it reaches, if there is one.
        let reach = |from: f32, step: isize| -> Option<(f32, usize)> {
            let mut x = from.round() as isize;
            let (mut run, mut gap, mut end, mut low) = (0usize, 0usize, from, 0);
            let max_gap = (0.15 * lh).ceil() as usize + 1;
            while x >= 0 && (x as f32) < w && run as f32 <= 6.0 * lh {
                match stroke(x as usize) {
                    Some(Some(y)) => {
                        run += 1 + gap;
                        gap = 0;
                        end = x as f32 + if step > 0 { 1.0 } else { 0.0 };
                        low = low.max(y);
                    }
                    Some(None) if gap < max_gap => gap += 1,
                    _ => break,
                }
                x += step;
            }
            // Longer than any run of underscores: a rule.
            let long = run as f32 > 6.0 * lh;
            (run as f32 >= 0.3 * lh && !long).then_some((end, low))
        };
        // A rule or an underline runs on under the letters too: no underscores.
        let under_letters = |y: usize| {
            let xs = (f.x0.max(0.0) as usize)..(f.x1.min(w) as usize);
            let n = xs.len().max(1);
            let rows = y.saturating_sub(1)..=(y + 1).min(img.height - 1);
            let ruled = xs.filter(|&x| rows.clone().any(|y| dark(x, y))).count();
            ruled * 10 > n * 6
        };
        let reach = |from: f32, step: isize| reach(from, step).filter(|&(_, y)| !under_letters(y));
        let (left, right) = (reach(f.x0 - 1.0, -1), reach(f.x1, 1));
        let pad = 0.12 * lh;
        let mut low = None;
        if let Some((x0, y)) = left {
            f.x0 = (x0 - pad).max(0.0);
            low = low.max(Some(y));
        }
        if let Some((x1, y)) = right {
            f.x1 = (x1 + pad).min(w);
            low = low.max(Some(y));
        }
        if let Some(y) = low {
            // The crop takes in the stroke with a little paper below it.
            f.y1 = f.y1.max(y as f32 + 1.0 + pad).min(h);
            *grown = true;
        }
    }
    // Boxes of one line within a space of each other, one of them grown: one box.
    flat.sort_by(|a, b| a.0.x0.total_cmp(&b.0.x0));
    let mut out: Vec<(Flat, bool)> = Vec::new();
    for (f, grown) in flat {
        let near = out.iter_mut().rev().find(|(o, og)| {
            let lh = (o.y1 - o.y0).min(f.y1 - f.y0);
            let overlap = o.y1.min(f.y1) - o.y0.max(f.y0);
            (grown || *og) && overlap > 0.5 * lh && f.x0 - o.x1 <= 0.7 * lh && f.x1 > o.x1
        });
        match near {
            Some((o, og)) => {
                *o = Flat {
                    x0: o.x0.min(f.x0),
                    y0: o.y0.min(f.y0),
                    x1: o.x1.max(f.x1),
                    y1: o.y1.max(f.y1),
                };
                *og = true;
            }
            None => out.push((f, grown)),
        }
    }
    rest.extend(out.into_iter().map(|(f, _)| f.rect()));
    rest
}

/// An upright box.
#[derive(Clone, Copy, Debug)]
struct Flat {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

impl Flat {
    /// A box along the x axis, if it nearly is one.
    fn of(r: &RotRect) -> Option<Flat> {
        let slack = (0.03 * r.u[0]).max(0.15 * r.v[1]);
        let level = r.u[1].abs() <= slack && r.v[0].abs() <= slack;
        if !level || r.width() < r.height() {
            return None;
        }
        let c = r.corners();
        let xs = c.map(|p| p[0]);
        let ys = c.map(|p| p[1]);
        Some(Flat {
            x0: xs.iter().copied().fold(f32::INFINITY, f32::min),
            y0: ys.iter().copied().fold(f32::INFINITY, f32::min),
            x1: xs.iter().copied().fold(f32::NEG_INFINITY, f32::max),
            y1: ys.iter().copied().fold(f32::NEG_INFINITY, f32::max),
        })
    }

    fn rect(self) -> RotRect {
        RotRect {
            origin: [self.x0, self.y0],
            u: [self.x1 - self.x0, 0.0],
            v: [0.0, self.y1 - self.y0],
        }
    }
}

fn luma(img: &RgbImage) -> Vec<u8> {
    img.data
        .as_chunks::<3>()
        .0
        .iter()
        .map(|p| ((u32::from(p[0]) * 3 + u32::from(p[1]) * 6 + u32::from(p[2])) / 10) as u8)
        .collect()
}

/// Halfway between paper and ink in the boxes, when they differ enough to tell.
fn threshold(level: &[u8], width: usize, boxes: &[Flat]) -> Option<u8> {
    let mut hist = [0usize; 256];
    let height = level.len() / width.max(1);
    let area: f32 = boxes.iter().map(|b| (b.x1 - b.x0) * (b.y1 - b.y0)).sum();
    let step = ((area / 100_000.0).sqrt() as usize).max(1);
    let mut n = 0;
    for b in boxes {
        let ys = (b.y0.max(0.0) as usize)..(b.y1.max(0.0) as usize).min(height);
        let xs = (b.x0.max(0.0) as usize)..(b.x1.max(0.0) as usize).min(width);
        for y in ys.step_by(step) {
            for x in xs.clone().step_by(step) {
                hist[usize::from(level[y * width + x])] += 1;
                n += 1;
            }
        }
    }
    let at = |share: f32| {
        let want = (n as f32 * share) as usize;
        let mut sum = 0;
        hist.iter()
            .position(|&c| {
                sum += c;
                sum > want
            })
            .unwrap_or(255) as u8
    };
    let (ink, paper) = (at(0.03), at(0.7));
    (paper > ink.saturating_add(60)).then(|| ink / 2 + paper / 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(x: f32, y: f32, w: f32, h: f32) -> RotRect {
        RotRect {
            origin: [x, y],
            u: [w, 0.0],
            v: [0.0, h],
        }
    }

    /// White, with dark rectangles.
    fn picture(w: usize, h: usize, dark: &[(usize, usize, usize, usize)]) -> RgbImage {
        let mut img = RgbImage {
            width: w,
            height: h,
            data: vec![250; w * h * 3],
        };
        for &(x0, y0, x1, y1) in dark {
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = (y * w + x) * 3;
                    img.data[i..i + 3].copy_from_slice(&[20, 20, 20]);
                }
            }
        }
        img
    }

    #[test]
    fn strokes_at_the_foot_join_boxes() {
        // "if __name__ ==": letters 10..30 high, underscores at 31..33.
        let img = picture(
            300,
            50,
            &[
                (10, 12, 30, 30),   // if
                (40, 31, 60, 33),   // __
                (60, 12, 100, 30),  // name
                (100, 31, 120, 33), // __
                (130, 15, 150, 25), // ==
                (200, 12, 240, 30), // far away
            ],
        );
        let boxes = vec![
            flat(8.0, 10.0, 24.0, 22.0),
            flat(58.0, 10.0, 44.0, 22.0),
            flat(128.0, 10.0, 24.0, 22.0),
            flat(198.0, 10.0, 44.0, 22.0),
        ];
        let mut out = take_in_underscores(&img, boxes);
        out.sort_by(|a, b| a.origin[0].total_cmp(&b.origin[0]));
        assert_eq!(out.len(), 2, "{out:?}");
        assert!(out[0].origin[0] <= 8.0 && out[0].at(1.0, 0.0)[0] >= 152.0);
        assert_eq!(out[1], flat(198.0, 10.0, 44.0, 22.0));
    }

    #[test]
    fn rules_and_underlines_are_not_underscores() {
        // A table header over a rule, and a word underlined a little past its ends.
        let img = picture(
            400,
            100,
            &[
                (10, 12, 50, 30),
                (70, 12, 110, 30),
                (5, 34, 290, 35),
                (300, 62, 340, 80),
                (296, 84, 344, 85),
            ],
        );
        let boxes = vec![
            flat(8.0, 10.0, 44.0, 22.0),
            flat(68.0, 10.0, 44.0, 22.0),
            flat(298.0, 60.0, 44.0, 22.0),
        ];
        assert_eq!(take_in_underscores(&img, boxes.clone()), boxes);
    }

    #[test]
    fn words_apart_stay_apart() {
        // Two words a space apart, no strokes: as they were.
        let img = picture(200, 50, &[(10, 12, 50, 30), (60, 12, 100, 30)]);
        let boxes = vec![flat(8.0, 10.0, 44.0, 22.0), flat(58.0, 10.0, 44.0, 22.0)];
        assert_eq!(take_in_underscores(&img, boxes.clone()), boxes);
    }
}
