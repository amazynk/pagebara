//! Smooth pen strokes: near-duplicate points dropped, then a Catmull-Rom spline through the
//! rest as cubic Béziers.

use crate::content::Content;
use crate::geom::Point;

/// Points closer than this to the previous kept one are dropped.
const MIN_STEP: f64 = 0.75;

pub(crate) fn simplify(pts: &[Point]) -> Vec<Point> {
    let mut out: Vec<Point> = Vec::with_capacity(pts.len());
    for &p in pts {
        if out.last().is_none_or(|&q| q.dist(p) >= MIN_STEP) {
            out.push(p);
        }
    }
    // Keep the real end of the stroke.
    if let (Some(&last), Some(&kept)) = (pts.last(), out.last())
        && kept != last
    {
        out.push(last);
    }
    out
}

/// Appends the smoothed path of a stroke; a single point becomes a dot (with round caps).
pub(crate) fn path(c: &mut Content, stroke: &[Point]) {
    let p = simplify(stroke);
    match p.len() {
        0 => {}
        1 => {
            c.move_to(p[0]).line_to(p[0]);
        }
        n => {
            c.move_to(p[0]);
            for i in 0..n - 1 {
                let p0 = p[i.saturating_sub(1)];
                let (p1, p2) = (p[i], p[i + 1]);
                let p3 = p[(i + 2).min(n - 1)];
                let c1 = Point::new(p1.x + (p2.x - p0.x) / 6.0, p1.y + (p2.y - p0.y) / 6.0);
                let c2 = Point::new(p2.x - (p3.x - p1.x) / 6.0, p2.y - (p3.y - p1.y) / 6.0);
                c.curve_to(c1, c2, p2);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_jitter_keeps_ends() {
        let pts: Vec<Point> = (0..100).map(|i| Point::new(i as f64 * 0.1, 0.0)).collect();
        let s = simplify(&pts);
        assert!(s.len() < 20);
        assert_eq!(s.first(), pts.first());
        assert_eq!(s.last(), pts.last());
    }
}
