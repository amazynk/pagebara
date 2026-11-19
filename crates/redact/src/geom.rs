//! Geometry in double precision: points, affine matrices and convex regions.

use pagebara_syntax::{Document, Object};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const fn new(x: f64, y: f64) -> Self {
        Point { x, y }
    }

    pub(crate) fn sub(self, o: Point) -> Point {
        Point::new(self.x - o.x, self.y - o.y)
    }

    pub(crate) fn dot(self, o: Point) -> f64 {
        self.x * o.x + self.y * o.y
    }

    pub(crate) fn cross(self, o: Point) -> f64 {
        self.x * o.y - self.y * o.x
    }

    pub(crate) fn lerp(self, o: Point, t: f64) -> Point {
        Point::new(self.x + (o.x - self.x) * t, self.y + (o.y - self.y) * t)
    }
}

/// An affine matrix `[a b c d e f]` with PDF's row-vector convention.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Matrix {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Default for Matrix {
    fn default() -> Self {
        Matrix::IDENTITY
    }
}

impl Matrix {
    pub const IDENTITY: Matrix = Matrix::new(1.0, 0.0, 0.0, 1.0, 0.0, 0.0);

    pub const fn new(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Self {
        Matrix { a, b, c, d, e, f }
    }

    pub const fn translate(x: f64, y: f64) -> Self {
        Matrix::new(1.0, 0.0, 0.0, 1.0, x, y)
    }

    pub(crate) fn from_slice(v: &[f64]) -> Option<Self> {
        let m = Matrix::new(*v.first()?, v[1], *v.get(2)?, v[3], *v.get(4)?, *v.get(5)?);
        m.is_finite().then_some(m)
    }

    pub(crate) fn from_object(doc: &Document, o: &Object) -> Option<Self> {
        let a = doc.resolve(o);
        let v: Vec<f64> = a
            .as_array()?
            .iter()
            .filter_map(|o| doc.resolve(o).as_f64())
            .collect();
        if v.len() != 6 {
            return None;
        }
        Matrix::from_slice(&v)
    }

    /// `self` then `n`.
    pub fn then(&self, n: &Matrix) -> Matrix {
        Matrix {
            a: self.a * n.a + self.b * n.c,
            b: self.a * n.b + self.b * n.d,
            c: self.c * n.a + self.d * n.c,
            d: self.c * n.b + self.d * n.d,
            e: self.e * n.a + self.f * n.c + n.e,
            f: self.e * n.b + self.f * n.d + n.f,
        }
    }

    pub fn apply(&self, p: Point) -> Point {
        Point::new(
            self.a * p.x + self.c * p.y + self.e,
            self.b * p.x + self.d * p.y + self.f,
        )
    }

    pub fn invert(&self) -> Option<Matrix> {
        let det = self.a * self.d - self.b * self.c;
        if det.abs() < 1e-12 || !det.is_finite() {
            return None;
        }
        let (a, b, c, d) = (self.d / det, -self.b / det, -self.c / det, self.a / det);
        let m = Matrix::new(
            a,
            b,
            c,
            d,
            -(self.e * a + self.f * c),
            -(self.e * b + self.f * d),
        );
        m.is_finite().then_some(m)
    }

    pub fn is_finite(&self) -> bool {
        [self.a, self.b, self.c, self.d, self.e, self.f]
            .iter()
            .all(|v| v.is_finite())
    }
}

/// An axis-aligned rectangle.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Rect {
    /// Normalised so that `x0 <= x1` and `y0 <= y1`.
    pub fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        Rect {
            x0: x0.min(x1),
            y0: y0.min(y1),
            x1: x0.max(x1),
            y1: y0.max(y1),
        }
    }

    pub fn from_points(ps: &[Point]) -> Rect {
        let mut r = Rect {
            x0: f64::INFINITY,
            y0: f64::INFINITY,
            x1: f64::NEG_INFINITY,
            y1: f64::NEG_INFINITY,
        };
        for p in ps {
            r.x0 = r.x0.min(p.x);
            r.y0 = r.y0.min(p.y);
            r.x1 = r.x1.max(p.x);
            r.y1 = r.y1.max(p.y);
        }
        r
    }

    pub(crate) fn from_object(doc: &Document, o: &Object) -> Option<Rect> {
        let a = doc.resolve(o);
        let v: Vec<f64> = a
            .as_array()?
            .iter()
            .filter_map(|o| doc.resolve(o).as_f64())
            .collect();
        let r = Rect::new(*v.first()?, *v.get(1)?, *v.get(2)?, *v.get(3)?);
        [r.x0, r.y0, r.x1, r.y1]
            .iter()
            .all(|v| v.is_finite())
            .then_some(r)
    }

    pub fn width(&self) -> f64 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> f64 {
        self.y1 - self.y0
    }

    pub fn corners(&self) -> [Point; 4] {
        [
            Point::new(self.x0, self.y0),
            Point::new(self.x1, self.y0),
            Point::new(self.x1, self.y1),
            Point::new(self.x0, self.y1),
        ]
    }

    pub fn union(&self, o: &Rect) -> Rect {
        Rect {
            x0: self.x0.min(o.x0),
            y0: self.y0.min(o.y0),
            x1: self.x1.max(o.x1),
            y1: self.y1.max(o.y1),
        }
    }

    pub(crate) fn overlaps(&self, o: &Rect) -> bool {
        self.x0 < o.x1 && o.x0 < self.x1 && self.y0 < o.y1 && o.y0 < self.y1
    }

    pub(crate) fn to_object(self) -> Object {
        Object::Array(
            [self.x0, self.y0, self.x1, self.y1]
                .iter()
                .map(|&v| Object::Real(round(v)))
                .collect(),
        )
    }
}

/// `v` rounded to 4 decimals, for numbers written into objects.
pub(crate) fn round(v: f64) -> f64 {
    (v * 1e4).round() / 1e4
}

/// A convex area of a page in default user space, counter-clockwise.
#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    points: Vec<Point>,
    bbox: Rect,
}

impl Region {
    pub fn from_rect(r: Rect) -> Region {
        Region::from_points(&r.corners()).expect("a rectangle has corners")
    }

    /// The convex hull of `points` (a QuadPoints quadrilateral in any vertex order);
    /// `None` without area.
    pub fn from_points(points: &[Point]) -> Option<Region> {
        let hull = convex_hull(points);
        if hull.len() < 3 || area(&hull) < 1e-9 {
            return None;
        }
        Some(Region {
            bbox: Rect::from_points(&hull),
            points: hull,
        })
    }

    pub fn points(&self) -> &[Point] {
        &self.points
    }

    pub fn bbox(&self) -> Rect {
        self.bbox
    }

    pub fn area(&self) -> f64 {
        area(&self.points)
    }

    /// Edge half-planes `(n, c)`: inside where `n·p <= c`, `n` pointing out.
    pub(crate) fn edges(&self) -> impl Iterator<Item = (Point, f64)> + '_ {
        let n = self.points.len();
        (0..n).map(move |i| {
            let a = self.points[i];
            let b = self.points[(i + 1) % n];
            let out = Point::new(b.y - a.y, a.x - b.x);
            (out, out.dot(a))
        })
    }

    pub(crate) fn contains(&self, p: Point) -> bool {
        self.edges()
            .all(|(n, c)| n.dot(p) <= c + 1e-9 * n.dot(n).sqrt())
    }

    /// Area shared with the convex polygon `poly` (any orientation).
    pub(crate) fn overlap_area(&self, poly: &[Point]) -> f64 {
        if !self.bbox.overlaps(&Rect::from_points(poly)) {
            return 0.0;
        }
        let mut cur: Vec<Point> = poly.to_vec();
        for (n, c) in self.edges() {
            cur = clip_polygon(&cur, n, c);
            if cur.is_empty() {
                return 0.0;
            }
        }
        area(&cur)
    }

    /// Whether `poly` (convex) overlaps by more than `fraction` of its own area; polygons
    /// without area count when any of their points lies inside.
    pub(crate) fn hits(&self, poly: &[Point], fraction: f64) -> bool {
        let own = area(poly);
        if own < 1e-9 {
            return poly.iter().any(|&p| self.contains(p));
        }
        self.overlap_area(poly) > own * fraction.max(1e-6)
    }

    /// Whether the convex polygon `poly` intersects with positive area or touches.
    pub(crate) fn touches(&self, poly: &[Point]) -> bool {
        if poly.is_empty() {
            return false;
        }
        let bb = Rect::from_points(poly);
        if bb.x1 < self.bbox.x0
            || bb.x0 > self.bbox.x1
            || bb.y1 < self.bbox.y0
            || bb.y0 > self.bbox.y1
        {
            return false;
        }
        if area(poly) < 1e-9 {
            // Degenerate: sample along its edges.
            return poly
                .windows(2)
                .chain([[poly[poly.len() - 1], poly[0]].as_slice()])
                .any(|w| (0..=32).any(|k| self.contains(w[0].lerp(w[1], f64::from(k) / 32.0))));
        }
        self.overlap_area(poly) > 0.0 || poly.iter().any(|&p| self.contains(p))
    }

    pub(crate) fn transform(&self, m: &Matrix) -> Option<Region> {
        let pts: Vec<Point> = self.points.iter().map(|&p| m.apply(p)).collect();
        Region::from_points(&pts)
    }
}

/// Signed area (positive counter-clockwise).
pub(crate) fn signed_area(p: &[Point]) -> f64 {
    let n = p.len();
    (0..n).map(|i| p[i].cross(p[(i + 1) % n])).sum::<f64>() / 2.0
}

pub(crate) fn area(p: &[Point]) -> f64 {
    signed_area(p).abs()
}

/// Sutherland–Hodgman step: the part of a polygon where `n·p <= c`.
pub(crate) fn clip_polygon(poly: &[Point], n: Point, c: f64) -> Vec<Point> {
    let mut out = Vec::with_capacity(poly.len() + 2);
    let len = poly.len();
    for i in 0..len {
        let a = poly[i];
        let b = poly[(i + 1) % len];
        let (da, db) = (n.dot(a) - c, n.dot(b) - c);
        if da <= 0.0 {
            out.push(a);
        }
        if (da < 0.0 && db > 0.0) || (da > 0.0 && db < 0.0) {
            out.push(a.lerp(b, da / (da - db)));
        }
    }
    out
}

fn convex_hull(points: &[Point]) -> Vec<Point> {
    let mut p: Vec<Point> = points
        .iter()
        .copied()
        .filter(|p| p.x.is_finite() && p.y.is_finite())
        .collect();
    p.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    p.dedup();
    if p.len() < 3 {
        return p;
    }
    let mut hull: Vec<Point> = Vec::with_capacity(p.len() * 2);
    for pass in 0..2 {
        let start = hull.len();
        let iter: Box<dyn Iterator<Item = &Point>> = if pass == 0 {
            Box::new(p.iter())
        } else {
            Box::new(p.iter().rev())
        };
        for &q in iter {
            while hull.len() >= start + 2 {
                let (a, b) = (hull[hull.len() - 2], hull[hull.len() - 1]);
                if b.sub(a).cross(q.sub(a)) <= 0.0 {
                    hull.pop();
                } else {
                    break;
                }
            }
            hull.push(q);
        }
        hull.pop();
    }
    hull
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hull_and_overlap() {
        // QuadPoints in Acrobat's order: upper left, upper right, lower left, lower right.
        let r = Region::from_points(&[
            Point::new(0.0, 10.0),
            Point::new(10.0, 10.0),
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
        ])
        .unwrap();
        assert!((signed_area(r.points()) - 100.0).abs() < 1e-9);
        let sq = Rect::new(5.0, 5.0, 15.0, 15.0).corners();
        assert!((r.overlap_area(&sq) - 25.0).abs() < 1e-9);
        assert!(r.hits(&sq, 0.01));
        let touching = Rect::new(10.0, 0.0, 20.0, 10.0).corners();
        assert!(!r.hits(&touching, 0.0));
        assert!(r.touches(&touching));
        assert!(r.contains(Point::new(10.0, 5.0)));
    }

    #[test]
    fn matrices() {
        let m = Matrix::new(0.0, 1.0, -1.0, 0.0, 5.0, 7.0);
        let p = m.apply(Point::new(2.0, 3.0));
        let back = m.invert().unwrap().apply(p);
        assert!((back.x - 2.0).abs() < 1e-12 && (back.y - 3.0).abs() < 1e-12);
    }
}
