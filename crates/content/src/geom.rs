//! Points, rectangles and affine matrices in PDF's row-vector convention.

use pagebara_syntax::Object;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const fn new(x: f32, y: f32) -> Self {
        Point { x, y }
    }
}

/// `[a b c d e f]`: a point `(x, y)` maps to `(a x + c y + e, b x + d y + f)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Matrix {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}

impl Default for Matrix {
    fn default() -> Self {
        Matrix::IDENTITY
    }
}

impl Matrix {
    pub const IDENTITY: Matrix = Matrix::new(1.0, 0.0, 0.0, 1.0, 0.0, 0.0);

    pub const fn new(a: f32, b: f32, c: f32, d: f32, e: f32, f: f32) -> Self {
        Matrix { a, b, c, d, e, f }
    }

    pub fn from_array(m: [f32; 6]) -> Self {
        Matrix::new(m[0], m[1], m[2], m[3], m[4], m[5])
    }

    pub const fn translate(x: f32, y: f32) -> Self {
        Matrix::new(1.0, 0.0, 0.0, 1.0, x, y)
    }

    pub const fn scale(x: f32, y: f32) -> Self {
        Matrix::new(x, 0.0, 0.0, y, 0.0, 0.0)
    }

    /// A matrix from a six-number array object; `None` if malformed.
    pub fn from_object(o: &Object) -> Option<Self> {
        let a = o.as_array()?;
        if a.len() != 6 {
            return None;
        }
        let mut m = [0.0; 6];
        for (v, o) in m.iter_mut().zip(a) {
            *v = o.as_f64()? as f32;
        }
        Some(Matrix::from_array(m))
    }

    /// `self` then `next`: `p × self × next`.
    #[inline]
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

    #[inline]
    pub fn apply(&self, p: Point) -> Point {
        Point::new(
            self.a * p.x + self.c * p.y + self.e,
            self.b * p.x + self.d * p.y + self.f,
        )
    }

    /// Applies only the linear part.
    #[inline]
    pub fn apply_vector(&self, p: Point) -> Point {
        Point::new(self.a * p.x + self.c * p.y, self.b * p.x + self.d * p.y)
    }

    pub fn determinant(&self) -> f32 {
        self.a * self.d - self.b * self.c
    }

    pub fn invert(&self) -> Option<Matrix> {
        let det = self.determinant();
        if det == 0.0 || !det.is_finite() {
            return None;
        }
        let (a, b, c, d) = (self.d / det, -self.b / det, -self.c / det, self.a / det);
        Some(Matrix::new(
            a,
            b,
            c,
            d,
            -(self.e * a + self.f * c),
            -(self.e * b + self.f * d),
        ))
    }

    pub fn is_finite(&self) -> bool {
        [self.a, self.b, self.c, self.d, self.e, self.f]
            .iter()
            .all(|v| v.is_finite())
    }
}

/// An axis-aligned rectangle with `x0 <= x1` and `y0 <= y1`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Rect {
    pub fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        Rect {
            x0: x0.min(x1),
            y0: y0.min(y1),
            x1: x0.max(x1),
            y1: y0.max(y1),
        }
    }

    /// A rectangle from a four-number array object, normalised.
    pub fn from_object(o: &Object) -> Option<Self> {
        let a = o.as_array()?;
        if a.len() != 4 {
            return None;
        }
        let v: Vec<f32> = a
            .iter()
            .map(|o| o.as_f64().map(|v| v as f32))
            .collect::<Option<_>>()?;
        Some(Rect::new(v[0], v[1], v[2], v[3]))
    }

    pub fn from_points(ps: &[Point]) -> Self {
        let mut r = Rect::new(ps[0].x, ps[0].y, ps[0].x, ps[0].y);
        for p in &ps[1..] {
            r.x0 = r.x0.min(p.x);
            r.y0 = r.y0.min(p.y);
            r.x1 = r.x1.max(p.x);
            r.y1 = r.y1.max(p.y);
        }
        r
    }

    pub fn width(&self) -> f32 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> f32 {
        self.y1 - self.y0
    }

    pub fn union(&self, o: &Rect) -> Rect {
        Rect {
            x0: self.x0.min(o.x0),
            y0: self.y0.min(o.y0),
            x1: self.x1.max(o.x1),
            y1: self.y1.max(o.y1),
        }
    }

    pub fn corners(&self) -> [Point; 4] {
        [
            Point::new(self.x0, self.y0),
            Point::new(self.x1, self.y0),
            Point::new(self.x1, self.y1),
            Point::new(self.x0, self.y1),
        ]
    }

    /// The bounding box of the rectangle transformed by `m`.
    pub fn transform(&self, m: &Matrix) -> Rect {
        Rect::from_points(&self.corners().map(|p| m.apply(p)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concat_and_invert() {
        let a = Matrix::new(2.0, 0.0, 0.0, 3.0, 10.0, 20.0);
        let b = Matrix::translate(1.0, 1.0);
        let p = a.then(&b).apply(Point::new(1.0, 1.0));
        assert_eq!(p, Point::new(13.0, 24.0));
        let i = a.invert().unwrap();
        assert_eq!(a.then(&i), Matrix::IDENTITY);
        assert!(Matrix::scale(0.0, 1.0).invert().is_none());
    }
}
