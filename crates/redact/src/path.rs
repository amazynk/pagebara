//! Cutting regions out of path geometry.
//!
//! Fills (and clipping paths) keep their exact area outside a region: each subpath is
//! clipped against the convex pieces the region's complement splits into (outside edge
//! `i`, inside edges `1..i`) with a Sutherland–Hodgman pass that splits Bézier curves at
//! the clip lines instead of flattening them. Clipping a closed curve to a half-plane keeps
//! the winding number of every point in it, so both fill rules give the original fill
//! minus the region. Strokes are cut at the region's boundary and the pieces inside it are
//! dropped; dash phases of the remaining pieces are carried over.

use crate::geom::{Matrix, Point, Rect, Region};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Seg {
    Line(Point, Point),
    Cubic(Point, Point, Point, Point),
}

impl Seg {
    pub fn start(&self) -> Point {
        match *self {
            Seg::Line(a, _) | Seg::Cubic(a, ..) => a,
        }
    }

    pub fn end(&self) -> Point {
        match *self {
            Seg::Line(_, b) | Seg::Cubic(.., b) => b,
        }
    }

    fn points(&self) -> Vec<Point> {
        match *self {
            Seg::Line(a, b) => vec![a, b],
            Seg::Cubic(a, b, c, d) => vec![a, b, c, d],
        }
    }

    fn eval(&self, t: f64) -> Point {
        match *self {
            Seg::Line(a, b) => a.lerp(b, t),
            Seg::Cubic(a, b, c, d) => {
                let (ab, bc, cd) = (a.lerp(b, t), b.lerp(c, t), c.lerp(d, t));
                let (abc, bcd) = (ab.lerp(bc, t), bc.lerp(cd, t));
                abc.lerp(bcd, t)
            }
        }
    }

    /// The part between parameters `t0 < t1`.
    fn part(&self, t0: f64, t1: f64) -> Seg {
        match *self {
            Seg::Line(..) => Seg::Line(self.eval(t0), self.eval(t1)),
            Seg::Cubic(..) => {
                let left = if t1 < 1.0 { self.split(t1).0 } else { *self };
                if t0 <= 0.0 {
                    left
                } else {
                    left.split(t0 / t1).1
                }
            }
        }
    }

    fn split(&self, t: f64) -> (Seg, Seg) {
        match *self {
            Seg::Line(a, b) => {
                let m = a.lerp(b, t);
                (Seg::Line(a, m), Seg::Line(m, b))
            }
            Seg::Cubic(a, b, c, d) => {
                let (ab, bc, cd) = (a.lerp(b, t), b.lerp(c, t), c.lerp(d, t));
                let (abc, bcd) = (ab.lerp(bc, t), bc.lerp(cd, t));
                let m = abc.lerp(bcd, t);
                (Seg::Cubic(a, ab, abc, m), Seg::Cubic(m, bcd, cd, d))
            }
        }
    }

    pub fn transform(&self, m: &Matrix) -> Seg {
        match *self {
            Seg::Line(a, b) => Seg::Line(m.apply(a), m.apply(b)),
            Seg::Cubic(a, b, c, d) => Seg::Cubic(m.apply(a), m.apply(b), m.apply(c), m.apply(d)),
        }
    }

    fn length(&self) -> f64 {
        match *self {
            Seg::Line(a, b) => a.sub(b).dot(a.sub(b)).sqrt(),
            Seg::Cubic(..) => {
                let mut prev = self.start();
                let mut len = 0.0;
                for k in 1..=32 {
                    let p = self.eval(f64::from(k) / 32.0);
                    len += p.sub(prev).dot(p.sub(prev)).sqrt();
                    prev = p;
                }
                len
            }
        }
    }

    /// Parameters in (0, 1) where the segment crosses the line `n·p = c`.
    fn roots(&self, n: Point, c: f64) -> Vec<f64> {
        let q: Vec<f64> = self.points().iter().map(|p| n.dot(*p) - c).collect();
        match q.len() {
            2 => {
                let (a, b) = (q[0], q[1]);
                if (a < 0.0 && b > 0.0) || (a > 0.0 && b < 0.0) {
                    vec![a / (a - b)]
                } else {
                    Vec::new()
                }
            }
            _ => cubic_roots(q[0], q[1], q[2], q[3]),
        }
    }
}

/// Roots in (0, 1) of a cubic Bézier polynomial with control values `q0..q3`.
fn cubic_roots(q0: f64, q1: f64, q2: f64, q3: f64) -> Vec<f64> {
    let f = |t: f64| {
        let s = 1.0 - t;
        s * s * s * q0 + 3.0 * s * s * t * q1 + 3.0 * s * t * t * q2 + t * t * t * q3
    };
    // Derivative 3(at² + bt + c) splits [0, 1] into monotonic intervals.
    let a = -q0 + 3.0 * q1 - 3.0 * q2 + q3;
    let b = 2.0 * (q0 - 2.0 * q1 + q2);
    let c = q1 - q0;
    let mut cuts = vec![0.0, 1.0];
    if a.abs() > 1e-12 {
        let disc = b * b - 4.0 * a * c;
        if disc >= 0.0 {
            let s = disc.sqrt();
            cuts.push((-b + s) / (2.0 * a));
            cuts.push((-b - s) / (2.0 * a));
        }
    } else if b.abs() > 1e-12 {
        cuts.push(-c / b);
    }
    cuts.retain(|t| (0.0..=1.0).contains(t));
    cuts.sort_by(f64::total_cmp);
    let mut out = Vec::new();
    for w in cuts.windows(2) {
        let (mut lo, mut hi) = (w[0], w[1]);
        let (flo, fhi) = (f(lo), f(hi));
        if !((flo < 0.0 && fhi > 0.0) || (flo > 0.0 && fhi < 0.0)) {
            continue;
        }
        let rising = flo < 0.0;
        for _ in 0..64 {
            let mid = (lo + hi) / 2.0;
            if (f(mid) < 0.0) == rising {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let t = (lo + hi) / 2.0;
        if t > 1e-9 && t < 1.0 - 1e-9 {
            out.push(t);
        }
    }
    out
}

/// A subpath in page space. `dash_offset` is how far into the original subpath (in user
/// space) a piece left by cutting a stroke starts.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Sub {
    pub start: Point,
    pub segs: Vec<Seg>,
    pub closed: bool,
    pub dash_offset: f64,
}

impl Sub {
    pub fn new(start: Point) -> Sub {
        Sub {
            start,
            segs: Vec::new(),
            closed: false,
            dash_offset: 0.0,
        }
    }

    fn control_points(&self) -> Vec<Point> {
        let mut v = vec![self.start];
        for s in &self.segs {
            v.extend(s.points());
        }
        v
    }

    pub fn bbox(&self) -> Rect {
        Rect::from_points(&self.control_points())
    }

    /// The subpath as a polyline, curves flattened.
    pub fn polyline(&self) -> Vec<Point> {
        let mut out = vec![self.start];
        for s in &self.segs {
            match s {
                Seg::Line(_, b) => out.push(*b),
                Seg::Cubic(..) => out.extend((1..=16).map(|k| s.eval(f64::from(k) / 16.0))),
            }
        }
        out
    }

    /// The segments with the closing line, for fills and closed strokes.
    fn cycle(&self) -> Vec<Seg> {
        let mut segs = self.segs.clone();
        let end = segs.last().map_or(self.start, Seg::end);
        if end != self.start {
            segs.push(Seg::Line(end, self.start));
        }
        segs
    }

    fn from_segs(segs: Vec<Seg>, closed: bool, dash_offset: f64) -> Sub {
        Sub {
            start: segs[0].start(),
            segs,
            closed,
            dash_offset,
        }
    }
}

fn overlaps(r: &Rect, region: &Region) -> bool {
    let b = region.bbox();
    r.x0 <= b.x1 && b.x0 <= r.x1 && r.y0 <= b.y1 && b.y0 <= r.y1
}

/// The fill area of `subs` outside `region`.
pub(crate) fn fill_minus(subs: Vec<Sub>, region: &Region) -> Vec<Sub> {
    let edges: Vec<(Point, f64)> = region.edges().collect();
    let mut out = Vec::with_capacity(subs.len());
    for sub in subs {
        if !overlaps(&sub.bbox(), region) {
            out.push(sub);
            continue;
        }
        // Curves lie within their control points' hull.
        if sub.control_points().iter().all(|&p| region.contains(p)) {
            continue;
        }
        if sub.segs.is_empty() || !intersects(&sub, region, &edges) {
            out.push(sub);
            continue;
        }
        for i in 0..edges.len() {
            let (n, c) = edges[i];
            let Some(mut piece) = clip_closed(&sub, Point::new(-n.x, -n.y), -c) else {
                continue;
            };
            let mut empty = false;
            for &(n, c) in &edges[..i] {
                match clip_closed(&piece, n, c) {
                    Some(p) => piece = p,
                    None => {
                        empty = true;
                        break;
                    }
                }
            }
            if !empty {
                out.push(piece);
            }
        }
    }
    out
}

/// Whether the fill of `sub` and `region` overlap: the outline enters the region, or the
/// region lies inside the outline.
fn intersects(sub: &Sub, region: &Region, edges: &[(Point, f64)]) -> bool {
    let cycle = sub.cycle();
    for seg in &cycle {
        if region.contains(seg.start()) {
            return true;
        }
        for &(n, c) in edges {
            if seg
                .roots(n, c)
                .into_iter()
                .any(|t| region.contains(seg.eval(t)))
            {
                return true;
            }
        }
    }
    // No crossing: the region is wholly inside or outside. Count crossings of a ray from
    // one of its corners with the flattened outline.
    let q = region.points()[0];
    let mut crossings = 0;
    for seg in &cycle {
        let mut prev = seg.start();
        let steps = if matches!(seg, Seg::Line(..)) { 1 } else { 32 };
        for k in 1..=steps {
            let p = seg.eval(f64::from(k) / f64::from(steps));
            if (prev.y > q.y) != (p.y > q.y) {
                let x = prev.x + (q.y - prev.y) / (p.y - prev.y) * (p.x - prev.x);
                if x > q.x {
                    crossings += 1;
                }
            }
            prev = p;
        }
    }
    crossings % 2 == 1
}

/// The closed subpath clipped to `n·p <= c`.
fn clip_closed(sub: &Sub, n: Point, c: f64) -> Option<Sub> {
    let mut pieces: Vec<(Seg, bool)> = Vec::new();
    for seg in sub.cycle() {
        let mut ts = seg.roots(n, c);
        ts.sort_by(f64::total_cmp);
        let mut prev = 0.0;
        for t in ts.into_iter().chain([1.0]) {
            if t - prev < 1e-12 {
                continue;
            }
            let p = seg.part(prev, t);
            let mid = seg.eval((prev + t) / 2.0);
            pieces.push((p, n.dot(mid) <= c));
            prev = t;
        }
    }
    if pieces.iter().all(|p| p.1) {
        return Some(sub.clone());
    }
    if !pieces.iter().any(|p| p.1) {
        return None;
    }
    let len = pieces.len();
    let first = (0..len).find(|&i| pieces[i].1 && !pieces[(i + len - 1) % len].1)?;
    let mut segs: Vec<Seg> = Vec::new();
    for k in 0..len {
        let (p, inside) = pieces[(first + k) % len];
        if !inside {
            continue;
        }
        if let Some(last) = segs.last()
            && last.end() != p.start()
        {
            // Along the clip line.
            segs.push(Seg::Line(last.end(), p.start()));
        }
        segs.push(p);
    }
    let (s, e) = (segs[0].start(), segs[segs.len() - 1].end());
    if s != e {
        segs.push(Seg::Line(e, s));
    }
    Some(Sub::from_segs(segs, true, 0.0))
}

/// The stroked geometry of `subs` outside `region`: subpaths it crosses become open pieces.
/// `to_user` maps page to user space, where dash lengths are measured.
pub(crate) fn stroke_minus(subs: Vec<Sub>, region: &Region, to_user: &Matrix) -> Vec<Sub> {
    let edges: Vec<(Point, f64)> = region.edges().collect();
    let strictly_inside = |p: Point| {
        edges
            .iter()
            .all(|&(n, c)| n.dot(p) < c - 1e-9 * n.dot(n).sqrt())
    };
    let mut out = Vec::with_capacity(subs.len());
    for sub in subs {
        if !overlaps(&sub.bbox(), region) {
            out.push(sub);
            continue;
        }
        if sub.segs.is_empty() {
            if !region.contains(sub.start) {
                out.push(sub);
            }
            continue;
        }
        let segs = if sub.closed {
            sub.cycle()
        } else {
            sub.segs.clone()
        };
        // (piece, kept, user-space length)
        let mut pieces: Vec<(Seg, bool, f64)> = Vec::new();
        for seg in &segs {
            let mut ts: Vec<f64> = edges.iter().flat_map(|&(n, c)| seg.roots(n, c)).collect();
            ts.sort_by(f64::total_cmp);
            let mut prev = 0.0;
            for t in ts.into_iter().chain([1.0]) {
                if t - prev < 1e-12 && t < 1.0 {
                    continue;
                }
                let p = seg.part(prev, t);
                let mid = seg.eval((prev + t) / 2.0);
                let degenerate = p.start() == p.end() && matches!(p, Seg::Line(..));
                let kept = if degenerate {
                    !region.contains(mid)
                } else {
                    !strictly_inside(mid)
                };
                pieces.push((p, kept, p.transform(to_user).length()));
                prev = t;
            }
        }
        if pieces.iter().all(|p| p.1) {
            out.push(sub);
            continue;
        }
        let mut chains: Vec<(Vec<Seg>, f64)> = Vec::new();
        let mut offset = sub.dash_offset;
        let mut open = false;
        for (p, kept, len) in &pieces {
            if *kept {
                if !open {
                    chains.push((Vec::new(), offset));
                    open = true;
                }
                chains.last_mut().unwrap().0.push(*p);
            } else {
                open = false;
            }
            offset += len;
        }
        // A closed subpath's last piece continues into its first.
        if sub.closed && chains.len() > 1 && pieces[0].1 && pieces[pieces.len() - 1].1 {
            let (first, _) = chains.remove(0);
            chains.last_mut().unwrap().0.extend(first);
        }
        out.extend(
            chains
                .into_iter()
                .map(|(segs, off)| Sub::from_segs(segs, false, off)),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{area, signed_area};

    fn rect_sub(r: Rect) -> Sub {
        let c = r.corners();
        let mut s = Sub::new(c[0]);
        for i in 0..4 {
            s.segs.push(Seg::Line(c[i], c[(i + 1) % 4]));
        }
        s.closed = true;
        s
    }

    fn polygon(s: &Sub) -> Vec<Point> {
        s.segs.iter().map(Seg::start).collect()
    }

    #[test]
    fn fill_minus_rect_keeps_area() {
        let sub = rect_sub(Rect::new(0.0, 0.0, 100.0, 100.0));
        let region = Region::from_rect(Rect::new(20.0, 20.0, 40.0, 60.0));
        let out = fill_minus(vec![sub], &region);
        let total: f64 = out.iter().map(|s| signed_area(&polygon(s))).sum();
        assert!((total - (10000.0 - 800.0)).abs() < 1e-6, "{total}");
        // A region over an edge.
        let sub = rect_sub(Rect::new(0.0, 0.0, 100.0, 100.0));
        let region = Region::from_rect(Rect::new(-10.0, 40.0, 10.0, 60.0));
        let out = fill_minus(vec![sub], &region);
        let total: f64 = out.iter().map(|s| area(&polygon(s))).sum();
        assert!((total - 9800.0).abs() < 1e-6);
    }

    #[test]
    fn curves_are_split_not_flattened() {
        let mut s = Sub::new(Point::new(0.0, 0.0));
        s.segs.push(Seg::Cubic(
            Point::new(0.0, 0.0),
            Point::new(0.0, 50.0),
            Point::new(100.0, 50.0),
            Point::new(100.0, 0.0),
        ));
        s.closed = true;
        let region = Region::from_rect(Rect::new(40.0, -10.0, 60.0, 100.0));
        let out = fill_minus(vec![s], &region);
        assert!(!out.is_empty());
        for sub in &out {
            for seg in &sub.segs {
                if let Seg::Cubic(..) = seg {
                    let m = seg.eval(0.5);
                    assert!(m.x <= 40.0 + 1e-6 || m.x >= 60.0 - 1e-6);
                }
            }
            assert!(sub.segs.iter().any(|s| matches!(s, Seg::Cubic(..))));
        }
    }

    #[test]
    fn stroke_cut_with_dash_offsets() {
        let mut s = Sub::new(Point::new(0.0, 0.0));
        s.segs
            .push(Seg::Line(Point::new(0.0, 0.0), Point::new(100.0, 0.0)));
        let region = Region::from_rect(Rect::new(30.0, -5.0, 50.0, 5.0));
        let out = stroke_minus(vec![s], &region, &Matrix::IDENTITY);
        assert_eq!(out.len(), 2);
        assert_eq!(
            out[0].segs,
            [Seg::Line(Point::new(0.0, 0.0), Point::new(30.0, 0.0))]
        );
        assert!((out[1].dash_offset - 50.0).abs() < 1e-9);
        assert_eq!(out[1].start, Point::new(50.0, 0.0));
    }

    #[test]
    fn closed_stroke_wraps_around() {
        let sub = rect_sub(Rect::new(0.0, 0.0, 100.0, 100.0));
        let region = Region::from_rect(Rect::new(40.0, -5.0, 60.0, 5.0));
        let out = stroke_minus(vec![sub], &region, &Matrix::IDENTITY);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].start, Point::new(60.0, 0.0));
        assert_eq!(out[0].segs.last().unwrap().end(), Point::new(40.0, 0.0));
    }
}
