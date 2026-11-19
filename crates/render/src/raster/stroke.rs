//! Stroking (§8.5.3.2): the path is flattened and offset in user space, so the pen follows
//! the CTM (ellipses under non-uniform scaling), then the outline is filled nonzero in
//! device space.

use pagebara_content::display::{LineCap, LineJoin, Path, StrokeStyle, Verb};
use pagebara_content::{Matrix, Point};

use super::{Edges, TOLERANCE, flatten_cubic};

/// Largest device-space scale of `m`.
pub fn expansion(m: &Matrix) -> f32 {
    let a = (m.a * m.a + m.b * m.b).sqrt();
    let b = (m.c * m.c + m.d * m.d).sqrt();
    a.max(b)
}

/// One subpath's polyline in user space.
#[derive(Debug, Default)]
struct Poly {
    pts: Vec<Point>,
    closed: bool,
}

#[derive(Debug)]
pub struct Stroker<'a> {
    style: &'a StrokeStyle,
    dash: &'a [f32],
    m: Matrix,
    hw: f32,
    out: &'a mut Edges,
    // Current output contour (user space).
    contour: Vec<Point>,
}

impl<'a> Stroker<'a> {
    /// Strokes `path` with `style` under `m` (user → device) into `out`.
    pub fn stroke(path: &Path<'_>, style: &StrokeStyle, dash: &[f32], m: Matrix, out: &mut Edges) {
        let ex = expansion(&m);
        if ex <= 0.0 || !ex.is_finite() || m.determinant() == 0.0 {
            return;
        }
        let mut w = style.width;
        if w * ex < 0.1 {
            w = 1.0 / ex;
        }
        let tol = TOLERANCE / ex;
        let dash = if dash.iter().sum::<f32>() * ex < 0.05 {
            &[][..]
        } else {
            dash
        };
        let mut s = Stroker {
            style,
            dash,
            m,
            hw: w * 0.5,
            out,
            contour: Vec::new(),
        };
        let mut polys = Vec::new();
        flatten_user(path, tol, &mut polys);
        for p in &polys {
            if s.dash.is_empty() {
                s.poly(&p.pts, p.closed);
            } else {
                s.dashed(p);
            }
        }
    }

    fn dashed(&mut self, p: &Poly) {
        let dash = self.dash;
        let total: f32 = dash.iter().sum();
        let n = dash.len();
        // Odd-length arrays repeat with alternating on/off.
        let period = if n % 2 == 1 { 2.0 * total } else { total };
        let mut phase = self
            .style
            .dash_phase
            .rem_euclid(period.max(f32::MIN_POSITIVE));
        let mut k = 0usize;
        let len_at = |k: usize| dash[k % n];
        while phase >= len_at(k) && len_at(k) >= 0.0 {
            phase -= len_at(k);
            k += 1;
            if k > 64 * n {
                break;
            }
        }
        let mut on = k % 2 == 0;
        let mut left = len_at(k) - phase;
        let mut cur: Vec<Point> = Vec::new();
        let mut pts = p.pts.clone();
        if p.closed && pts.len() > 1 && pts.first() != pts.last() {
            pts.push(pts[0]);
        }
        if pts.len() == 1 {
            if on {
                self.poly(&pts, false);
            }
            return;
        }
        if on {
            cur.push(pts[0]);
        }
        let mut steps = 0u32;
        for seg in pts.windows(2) {
            let (a, b) = (seg[0], seg[1]);
            let l = dist(a, b);
            let mut t0 = 0.0f32;
            while l - t0 > left {
                steps += 1;
                if steps > 1_000_000 {
                    return;
                }
                t0 += left;
                let q = lerp(a, b, t0 / l);
                if on {
                    cur.push(q);
                    let dir = sub(b, a);
                    self.dash_piece(&cur, dir);
                    cur.clear();
                } else {
                    cur.push(q);
                }
                on = !on;
                k += 1;
                left = len_at(k);
            }
            left -= l - t0;
            if on {
                cur.push(b);
            }
        }
        if on && !cur.is_empty() {
            let dir = sub(pts[pts.len() - 1], pts[pts.len() - 2]);
            self.dash_piece(&cur, dir);
        }
    }

    fn dash_piece(&mut self, pts: &[Point], dir: Point) {
        let mut v: Vec<Point> = Vec::with_capacity(pts.len());
        for &p in pts {
            if v.last() != Some(&p) {
                v.push(p);
            }
        }
        if v.len() == 1 {
            self.dot(v[0], dir);
        } else {
            self.poly(&v, false);
        }
    }

    /// Strokes one polyline: one contour when open, two when closed.
    fn poly(&mut self, raw: &[Point], closed: bool) {
        let mut pts: Vec<Point> = Vec::with_capacity(raw.len());
        for &p in raw {
            if pts
                .last()
                .is_none_or(|q| dist(*q, p) > 1e-6 * self.hw.max(1e-3))
            {
                pts.push(p);
            }
        }
        if closed && pts.len() > 2 && dist(pts[0], pts[pts.len() - 1]) <= 1e-6 * self.hw.max(1e-3) {
            pts.pop();
        }
        match pts.len() {
            0 => {}
            // A degenerate subpath only paints with round caps.
            1 if self.style.cap == LineCap::Round => self.dot(pts[0], Point::new(1.0, 0.0)),
            1 => {}
            _ if closed && pts.len() > 2 => {
                self.side(&pts, true);
                self.flush();
                pts.reverse();
                self.side(&pts, true);
                self.flush();
            }
            _ => {
                self.side(&pts, false);
                let n = pts.len();
                self.cap(pts[n - 1], unit(sub(pts[n - 1], pts[n - 2])));
                pts.reverse();
                self.side(&pts, false);
                self.cap(pts[n - 1], unit(sub(pts[n - 1], pts[n - 2])));
                self.flush();
            }
        }
    }

    /// A zero-length subpath: a dot for round caps, a square for square caps.
    fn dot(&mut self, p: Point, dir: Point) {
        let d = unit(dir);
        let d = if d == Point::default() {
            Point::new(1.0, 0.0)
        } else {
            d
        };
        match self.style.cap {
            LineCap::Butt => {}
            LineCap::Round => {
                let hw = self.hw;
                let n = self.arc_steps(std::f32::consts::TAU);
                for i in 0..n {
                    let a = i as f32 / n as f32 * std::f32::consts::TAU;
                    self.contour
                        .push(Point::new(p.x + hw * a.cos(), p.y + hw * a.sin()));
                }
                self.flush();
            }
            LineCap::Square => {
                let (dx, dy) = (d.x * self.hw, d.y * self.hw);
                let (nx, ny) = (-dy, dx);
                for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                    self.contour
                        .push(Point::new(p.x + sx * dx + sy * nx, p.y + sx * dy + sy * ny));
                }
                self.flush();
            }
        }
    }

    /// The left offset of a polyline, with joins; wraps around when `closed`.
    fn side(&mut self, pts: &[Point], closed: bool) {
        let n = pts.len();
        let hw = self.hw;
        let seg = |i: usize| {
            let d = unit(sub(pts[(i + 1) % n], pts[i]));
            (d, Point::new(-d.y * hw, d.x * hw))
        };
        let segs = if closed { n } else { n - 1 };
        let (d0, n0) = seg(0);
        if !closed {
            self.contour.push(add(pts[0], n0));
        } else {
            let (dp, np) = seg(n - 1);
            self.join(pts[0], dp, np, d0, n0);
        }
        let mut prev = (d0, n0);
        for i in 0..segs {
            let (d, nn) = if i == 0 { (d0, n0) } else { seg(i) };
            if i > 0 {
                self.join(pts[i], prev.0, prev.1, d, nn);
            }
            self.contour.push(add(pts[(i + 1) % n], nn));
            prev = (d, nn);
        }
    }

    fn join(&mut self, p: Point, d0: Point, n0: Point, d1: Point, n1: Point) {
        let cross = d0.x * d1.y - d0.y * d1.x;
        let dot = d0.x * d1.x + d0.y * d1.y;
        let a = add(p, n0);
        let b = add(p, n1);
        if cross.abs() < 1e-6 && dot > 0.0 {
            self.contour.push(a);
            return;
        }
        if cross > 0.0 {
            // The left side is on the inside of the turn: route through the pivot.
            self.contour.push(a);
            self.contour.push(p);
            self.contour.push(b);
            return;
        }
        match self.style.join {
            LineJoin::Bevel => {
                self.contour.push(a);
                self.contour.push(b);
            }
            LineJoin::Miter => {
                // 1 / sin(θ/2) with θ the angle between the segments.
                let cos_theta = -dot;
                let sin_half = ((1.0 - cos_theta) * 0.5).max(0.0).sqrt();
                self.contour.push(a);
                if sin_half > 1e-6 && 1.0 / sin_half <= self.style.miter_limit {
                    let m = unit(add(n0, n1));
                    let cos_half = (m.x * n0.x + m.y * n0.y) / self.hw;
                    if cos_half > 1e-6 {
                        let l = self.hw / cos_half;
                        self.contour.push(Point::new(p.x + m.x * l, p.y + m.y * l));
                    }
                }
                self.contour.push(b);
            }
            LineJoin::Round => {
                let a0 = n0.y.atan2(n0.x);
                let mut a1 = n1.y.atan2(n1.x);
                // Clockwise from n0 to n1 (the outside of a right turn).
                while a1 > a0 {
                    a1 -= std::f32::consts::TAU;
                }
                self.arc(p, a0, a1);
            }
        }
    }

    fn cap(&mut self, p: Point, d: Point) {
        let hw = self.hw;
        let n = Point::new(-d.y * hw, d.x * hw);
        match self.style.cap {
            LineCap::Butt => {}
            LineCap::Square => {
                self.contour
                    .push(Point::new(p.x + n.x + d.x * hw, p.y + n.y + d.y * hw));
                self.contour
                    .push(Point::new(p.x - n.x + d.x * hw, p.y - n.y + d.y * hw));
            }
            LineCap::Round => {
                let a0 = n.y.atan2(n.x);
                self.arc(p, a0, a0 - std::f32::consts::PI);
            }
        }
    }

    fn arc_steps(&self, sweep: f32) -> usize {
        let r = self.hw * expansion(&self.m);
        let t = (1.0 - TOLERANCE / r.max(TOLERANCE)).clamp(-1.0, 1.0);
        let step = 2.0 * t.acos();
        if step <= 1e-4 {
            return 64;
        }
        ((sweep.abs() / step).ceil() as usize).clamp(1, 512)
    }

    fn arc(&mut self, c: Point, a0: f32, a1: f32) {
        let n = self.arc_steps(a1 - a0);
        for i in 0..=n {
            let a = a0 + (a1 - a0) * i as f32 / n as f32;
            self.contour
                .push(Point::new(c.x + self.hw * a.cos(), c.y + self.hw * a.sin()));
        }
    }

    fn flush(&mut self) {
        let c = &self.contour;
        if c.len() > 1 {
            let m = self.m;
            let first = m.apply(c[0]);
            let mut prev = first;
            for p in &c[1..] {
                let q = m.apply(*p);
                self.out.push(prev, q);
                prev = q;
            }
            self.out.push(prev, first);
        }
        self.contour.clear();
    }
}

fn flatten_user(path: &Path<'_>, tol: f32, out: &mut Vec<Poly>) {
    let mut pts = path.points.iter().copied();
    let mut next = || pts.next().unwrap_or_default();
    let mut cur = Poly::default();
    let mut has_segment = false;
    let finish = |cur: &mut Poly, has: &mut bool, out: &mut Vec<Poly>| {
        if *has && !cur.pts.is_empty() {
            out.push(std::mem::take(cur));
        } else {
            cur.pts.clear();
            cur.closed = false;
        }
        *has = false;
    };
    for v in path.verbs {
        match v {
            Verb::Move => {
                finish(&mut cur, &mut has_segment, out);
                cur.pts.push(next());
            }
            Verb::Line => {
                let p = next();
                if cur.pts.is_empty() {
                    cur.pts.push(p);
                }
                cur.pts.push(p);
                has_segment = true;
            }
            Verb::Cubic => {
                let (a, b, c) = (next(), next(), next());
                let p0 = *cur.pts.last().unwrap_or(&a);
                if cur.pts.is_empty() {
                    cur.pts.push(p0);
                }
                flatten_cubic(p0, a, b, c, tol, &mut |_, q| cur.pts.push(q));
                has_segment = true;
            }
            Verb::Close => {
                if !cur.pts.is_empty() {
                    cur.closed = true;
                    has_segment = true;
                    let start = cur.pts[0];
                    finish(&mut cur, &mut has_segment, out);
                    // A segment after `h` starts at the closed subpath's start.
                    cur.pts.push(start);
                }
            }
        }
    }
    finish(&mut cur, &mut has_segment, out);
}

#[inline]
fn sub(a: Point, b: Point) -> Point {
    Point::new(a.x - b.x, a.y - b.y)
}

#[inline]
fn add(a: Point, b: Point) -> Point {
    Point::new(a.x + b.x, a.y + b.y)
}

#[inline]
fn dist(a: Point, b: Point) -> f32 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
}

#[inline]
fn lerp(a: Point, b: Point, t: f32) -> Point {
    Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

#[inline]
fn unit(p: Point) -> Point {
    let l = (p.x * p.x + p.y * p.y).sqrt();
    if l > 0.0 && l.is_finite() {
        Point::new(p.x / l, p.y / l)
    } else {
        Point::default()
    }
}
