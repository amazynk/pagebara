//! Text detection post-processing (DB, "Differentiable Binarization"): the model gives a
//! per-pixel text probability for shrunk text regions; regions above a threshold become
//! connected components, each fitted with a minimum-area rotated rectangle and grown back
//! to the full text size.

use crate::image::RotRect;

#[derive(Clone, Copy, Debug)]
pub struct DbParams {
    /// Pixels above this probability are text.
    pub thresh: f32,
    /// Regions whose mean probability is lower are dropped.
    pub box_thresh: f32,
    /// How far shrunk regions are grown back (area × ratio / perimeter).
    pub unclip_ratio: f32,
    /// Regions with a side shorter than this many pixels are dropped.
    pub min_size: f32,
}

impl Default for DbParams {
    fn default() -> Self {
        DbParams {
            thresh: 0.3,
            box_thresh: 0.6,
            unclip_ratio: 1.5,
            min_size: 3.0,
        }
    }
}

/// One run of pixels `x0..x1` on row `y`.
#[derive(Clone, Copy)]
struct Run {
    y: u32,
    x0: u32,
    x1: u32,
    label: u32,
}

fn find(parent: &mut [u32], mut i: u32) -> u32 {
    while parent[i as usize] != i {
        let p = parent[parent[i as usize] as usize];
        parent[i as usize] = p;
        i = p;
    }
    i
}

/// Text regions of a `width` × `height` probability map.
pub fn detections(prob: &[f32], width: usize, height: usize, p: &DbParams) -> Vec<RotRect> {
    // Label 8-connected components over row runs.
    let mut runs: Vec<Run> = Vec::new();
    let mut parent: Vec<u32> = Vec::new();
    let mut prev = 0..0;
    for y in 0..height {
        let row = &prob[y * width..(y + 1) * width];
        let start = runs.len();
        let mut x = 0;
        while x < width {
            if row[x] <= p.thresh {
                x += 1;
                continue;
            }
            let x0 = x;
            while x < width && row[x] > p.thresh {
                x += 1;
            }
            let label = parent.len() as u32;
            parent.push(label);
            for r in &runs[prev.clone()] {
                // Touching, diagonals included.
                if (r.x0 as usize) <= x && x0 <= r.x1 as usize {
                    let (a, b) = (find(&mut parent, label), find(&mut parent, r.label));
                    if a != b {
                        parent[a.max(b) as usize] = a.min(b);
                    }
                }
            }
            runs.push(Run {
                y: y as u32,
                x0: x0 as u32,
                x1: x as u32,
                label,
            });
        }
        prev = start..runs.len();
    }

    // Group runs by root.
    let mut index = vec![u32::MAX; parent.len()];
    let mut groups: Vec<Vec<Run>> = Vec::new();
    for r in &runs {
        let root = find(&mut parent, r.label) as usize;
        if index[root] == u32::MAX {
            index[root] = groups.len() as u32;
            groups.push(Vec::new());
        }
        groups[index[root] as usize].push(*r);
    }

    let mut out = Vec::new();
    for g in groups {
        let mut sum = 0.0f64;
        let mut n = 0usize;
        let mut pts = Vec::with_capacity(g.len() * 4);
        for r in &g {
            let row = &prob[r.y as usize * width..];
            sum += row[r.x0 as usize..r.x1 as usize]
                .iter()
                .map(|&v| f64::from(v))
                .sum::<f64>();
            n += (r.x1 - r.x0) as usize;
            let (y0, y1) = (r.y as f32, r.y as f32 + 1.0);
            pts.extend([
                [r.x0 as f32, y0],
                [r.x0 as f32, y1],
                [r.x1 as f32, y0],
                [r.x1 as f32, y1],
            ]);
        }
        if n < 4 {
            continue;
        }
        let score = (sum / n as f64) as f32;
        if score < p.box_thresh {
            continue;
        }
        let hull = convex_hull(pts);
        let Some(rect) = min_area_rect(&hull) else {
            continue;
        };
        let (w, h) = (rect.width(), rect.height());
        if w.min(h) < p.min_size {
            continue;
        }
        let rect = grow(&rect, w * h * p.unclip_ratio / (2.0 * (w + h)));
        if rect.width().min(rect.height()) < p.min_size + 2.0 {
            continue;
        }
        out.push(rect);
    }
    out
}

fn cross(o: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
}

/// Andrew's monotone chain; counter-clockwise in a y-up frame, no collinear points.
fn convex_hull(mut pts: Vec<[f32; 2]>) -> Vec<[f32; 2]> {
    pts.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    pts.dedup();
    if pts.len() < 3 {
        return pts;
    }
    let mut hull: Vec<[f32; 2]> = Vec::with_capacity(pts.len() * 2);
    for pass in 0..2 {
        let start = hull.len();
        let iter: Box<dyn Iterator<Item = &[f32; 2]>> = if pass == 0 {
            Box::new(pts.iter())
        } else {
            Box::new(pts.iter().rev())
        };
        for &p in iter {
            while hull.len() >= start + 2
                && cross(hull[hull.len() - 2], hull[hull.len() - 1], p) <= 0.0
            {
                hull.pop();
            }
            hull.push(p);
        }
        hull.pop();
    }
    hull
}

/// The minimum-area rectangle around a convex hull (one side lies on a hull edge). Its `u`
/// axis is the side direction nearest to the image's +x axis, so text reads along it.
fn min_area_rect(hull: &[[f32; 2]]) -> Option<RotRect> {
    if hull.len() < 2 {
        return None;
    }
    let mut best: Option<(f32, RotRect)> = None;
    for i in 0..hull.len() {
        let (a, b) = (hull[i], hull[(i + 1) % hull.len()]);
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len = dx.hypot(dy);
        if len < 1e-6 {
            continue;
        }
        let e = [dx / len, dy / len];
        let n = [-e[1], e[0]];
        let (mut s0, mut s1, mut t0, mut t1) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
        for p in hull {
            let s = p[0] * e[0] + p[1] * e[1];
            let t = p[0] * n[0] + p[1] * n[1];
            (s0, s1, t0, t1) = (s0.min(s), s1.max(s), t0.min(t), t1.max(t));
        }
        let area = (s1 - s0) * (t1 - t0);
        if best.as_ref().is_none_or(|b| area < b.0 - 1e-3) {
            best = Some((area, axes(e, n, s0, s1, t0, t1)));
        }
    }
    best.map(|b| b.1)
}

/// The rectangle spanning `s0..s1` along `e` and `t0..t1` along `n`, with `u` the side
/// direction nearest to +x and `v` pointing down.
fn axes(e: [f32; 2], n: [f32; 2], s0: f32, s1: f32, t0: f32, t1: f32) -> RotRect {
    let (ws, wt) = (s1 - s0, t1 - t0);
    let (cs, ct) = ((s0 + s1) / 2.0, (t0 + t1) / 2.0);
    let c = [e[0] * cs + n[0] * ct, e[1] * cs + n[1] * ct];
    let neg = |a: [f32; 2]| [-a[0], -a[1]];
    let (dir, len, other) = [(e, ws, wt), (neg(e), ws, wt), (n, wt, ws), (neg(n), wt, ws)]
        .into_iter()
        .max_by(|a, b| a.0[0].total_cmp(&b.0[0]))
        .unwrap_or((e, ws, wt));
    let mut down = [-dir[1], dir[0]];
    if down[1] < 0.0 {
        down = neg(down);
    }
    let u = [dir[0] * len, dir[1] * len];
    let v = [down[0] * other, down[1] * other];
    RotRect {
        origin: [c[0] - (u[0] + v[0]) / 2.0, c[1] - (u[1] + v[1]) / 2.0],
        u,
        v,
    }
}

/// `r` grown by `d` on every side.
fn grow(r: &RotRect, d: f32) -> RotRect {
    let (w, h) = (r.width().max(1e-6), r.height().max(1e-6));
    let eu = [r.u[0] / w, r.u[1] / w];
    let ev = [r.v[0] / h, r.v[1] / h];
    RotRect {
        origin: [
            r.origin[0] - (eu[0] + ev[0]) * d,
            r.origin[1] - (eu[1] + ev[1]) * d,
        ],
        u: [eu[0] * (w + 2.0 * d), eu[1] * (w + 2.0 * d)],
        v: [ev[0] * (h + 2.0 * d), ev[1] * (h + 2.0 * d)],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn finds_rectangles() {
        let (w, h) = (40, 20);
        let mut prob = vec![0.0f32; w * h];
        for y in 2..6 {
            for x in 3..30 {
                prob[y * w + x] = 0.9;
            }
        }
        for y in 10..18 {
            for x in 5..9 {
                prob[y * w + x] = 0.8;
            }
        }
        prob[19 * w + 39] = 0.9; // noise
        let p = DbParams {
            unclip_ratio: 0.0,
            min_size: 1.0,
            ..DbParams::default()
        };
        let mut d = detections(&prob, w, h, &p);
        d.sort_by(|a, b| a.origin[1].total_cmp(&b.origin[1]));
        assert_eq!(d.len(), 2);
        let r = d[0];
        assert!(close(r.origin[0], 3.0) && close(r.origin[1], 2.0), "{r:?}");
        assert!(close(r.width(), 27.0) && close(r.height(), 4.0));
        assert!(r.u[0] > 0.0 && r.v[1] > 0.0);
        // A tall region still reads left to right.
        let r = d[1];
        assert!(close(r.width(), 4.0) && close(r.height(), 8.0), "{r:?}");
    }

    #[test]
    fn rotated_region() {
        let (w, h) = (60, 60);
        let mut prob = vec![0.0f32; w * h];
        // A band at 30° below the x axis.
        let (s, c) = (30f32.to_radians().sin(), 30f32.to_radians().cos());
        for y in 0..h {
            for x in 0..w {
                let (dx, dy) = (x as f32 - 10.0, y as f32 - 10.0);
                let along = dx * c + dy * s;
                let across = -dx * s + dy * c;
                if (0.0..40.0).contains(&along) && (0.0..6.0).contains(&across) {
                    prob[y * w + x] = 1.0;
                }
            }
        }
        let d = detections(&prob, w, h, &DbParams::default());
        assert_eq!(d.len(), 1);
        let r = d[0];
        let angle = r.u[1].atan2(r.u[0]).to_degrees();
        assert!((angle - 30.0).abs() < 3.0, "{angle}");
        assert!(r.width() > r.height() * 3.0);
    }

    #[test]
    fn grow_keeps_centre() {
        let r = RotRect {
            origin: [10.0, 10.0],
            u: [20.0, 0.0],
            v: [0.0, 4.0],
        };
        let g = grow(&r, 2.0);
        assert_eq!(g.origin, [8.0, 8.0]);
        assert!(close(g.width(), 24.0) && close(g.height(), 8.0));
    }
}
