//! Glyph outlines in font units.

/// Receives a glyph outline. Every subpath starts with `move_to` and ends with `close`;
/// glyphs are filled with the nonzero winding rule.
pub trait OutlineSink {
    fn move_to(&mut self, x: f32, y: f32);
    fn line_to(&mut self, x: f32, y: f32);
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32);
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32);
    fn close(&mut self);
}

/// A 2×3 affine matrix `[a b c d e f]` as in PDF: `x' = a·x + c·y + e`, `y' = b·x + d·y + f`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Matrix(pub [f64; 6]);

impl Matrix {
    pub const IDENTITY: Matrix = Matrix([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

    pub fn scale(s: f64) -> Matrix {
        Matrix([s, 0.0, 0.0, s, 0.0, 0.0])
    }

    /// `self` followed by `then`.
    pub fn then(&self, then: &Matrix) -> Matrix {
        let [a, b, c, d, e, f] = self.0;
        let [a2, b2, c2, d2, e2, f2] = then.0;
        Matrix([
            a * a2 + b * c2,
            a * b2 + b * d2,
            c * a2 + d * c2,
            c * b2 + d * d2,
            e * a2 + f * c2 + e2,
            e * b2 + f * d2 + f2,
        ])
    }

    pub fn invert(&self) -> Option<Matrix> {
        let [a, b, c, d, e, f] = self.0;
        let det = a * d - b * c;
        if det.abs() < 1e-12 || !det.is_finite() {
            return None;
        }
        let (ia, ib, ic, id) = (d / det, -b / det, -c / det, a / det);
        Some(Matrix([
            ia,
            ib,
            ic,
            id,
            -(e * ia + f * ic),
            -(e * ib + f * id),
        ]))
    }

    pub fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        let [a, b, c, d, e, f] = self.0;
        let (x, y) = (f64::from(x), f64::from(y));
        ((a * x + c * y + e) as f32, (b * x + d * y + f) as f32)
    }

    pub fn is_identity(&self) -> bool {
        *self == Matrix::IDENTITY
    }
}

impl Default for Matrix {
    fn default() -> Self {
        Matrix::IDENTITY
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verb {
    Move,
    Line,
    Quad,
    Cubic,
    Close,
}

/// A recorded outline.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Path {
    pub verbs: Vec<Verb>,
    pub points: Vec<(f32, f32)>,
}

impl Path {
    pub fn new() -> Self {
        Path::default()
    }

    pub fn is_empty(&self) -> bool {
        self.verbs.is_empty()
    }

    pub fn clear(&mut self) {
        self.verbs.clear();
        self.points.clear();
    }

    /// Bounds of all points (control points included): `[x0, y0, x1, y1]`.
    pub fn bounds(&self) -> Option<[f32; 4]> {
        let mut it = self.points.iter();
        let &(x, y) = it.next()?;
        Some(it.fold([x, y, x, y], |[a, b, c, d], &(x, y)| {
            [a.min(x), b.min(y), c.max(x), d.max(y)]
        }))
    }

    /// Replays the path into `sink`.
    pub fn replay(&self, sink: &mut dyn OutlineSink) {
        let mut p = self.points.iter().copied();
        let mut next = || p.next().unwrap_or_default();
        for v in &self.verbs {
            match v {
                Verb::Move => {
                    let (x, y) = next();
                    sink.move_to(x, y)
                }
                Verb::Line => {
                    let (x, y) = next();
                    sink.line_to(x, y)
                }
                Verb::Quad => {
                    let ((x1, y1), (x, y)) = (next(), next());
                    sink.quad_to(x1, y1, x, y)
                }
                Verb::Cubic => {
                    let ((x1, y1), (x2, y2), (x, y)) = (next(), next(), next());
                    sink.curve_to(x1, y1, x2, y2, x, y)
                }
                Verb::Close => sink.close(),
            }
        }
    }

    /// Transforms all points.
    pub fn transform(&mut self, m: &Matrix) {
        for p in &mut self.points {
            *p = m.apply(p.0, p.1);
        }
    }
}

impl OutlineSink for Path {
    fn move_to(&mut self, x: f32, y: f32) {
        self.verbs.push(Verb::Move);
        self.points.push((x, y));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.verbs.push(Verb::Line);
        self.points.push((x, y));
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.verbs.push(Verb::Quad);
        self.points.extend([(x1, y1), (x, y)]);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.verbs.push(Verb::Cubic);
        self.points.extend([(x1, y1), (x2, y2), (x, y)]);
    }
    fn close(&mut self) {
        self.verbs.push(Verb::Close);
    }
}

/// Wraps a sink for the charstring interpreters: applies an optional transform, inserts the
/// implied `move_to` and `close`, and drops non-finite coordinates.
pub(crate) struct Pen<'a> {
    sink: &'a mut dyn OutlineSink,
    matrix: Option<Matrix>,
    open: bool,
    cur: (f32, f32),
    pub empty: bool,
}

impl<'a> Pen<'a> {
    pub fn new(sink: &'a mut dyn OutlineSink, matrix: Option<Matrix>) -> Self {
        Pen {
            sink,
            matrix: matrix.filter(|m| !m.is_identity()),
            open: false,
            cur: (0.0, 0.0),
            empty: true,
        }
    }

    fn map(&self, x: f32, y: f32) -> (f32, f32) {
        let (x, y) = match &self.matrix {
            Some(m) => m.apply(x, y),
            None => (x, y),
        };
        (finite(x), finite(y))
    }

    fn ensure_open(&mut self) {
        if !self.open {
            let (x, y) = self.map(self.cur.0, self.cur.1);
            self.sink.move_to(x, y);
            self.open = true;
        }
    }

    pub fn move_to(&mut self, x: f32, y: f32) {
        self.close();
        self.cur = (x, y);
    }

    pub fn line_to(&mut self, x: f32, y: f32) {
        self.ensure_open();
        self.cur = (x, y);
        let (x, y) = self.map(x, y);
        self.sink.line_to(x, y);
        self.empty = false;
    }

    pub fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.ensure_open();
        self.cur = (x, y);
        let (x1, y1) = self.map(x1, y1);
        let (x2, y2) = self.map(x2, y2);
        let (x, y) = self.map(x, y);
        self.sink.curve_to(x1, y1, x2, y2, x, y);
        self.empty = false;
    }

    pub fn close(&mut self) {
        if self.open {
            self.sink.close();
            self.open = false;
        }
    }

    pub fn finish(&mut self) {
        self.close();
    }
}

fn finite(v: f32) -> f32 {
    if v.is_finite() {
        v.clamp(-1e7, 1e7)
    } else {
        0.0
    }
}

/// An `OutlineSink` that discards everything (for metric-only charstring runs).
pub(crate) struct NullSink;

impl OutlineSink for NullSink {
    fn move_to(&mut self, _: f32, _: f32) {}
    fn line_to(&mut self, _: f32, _: f32) {}
    fn quad_to(&mut self, _: f32, _: f32, _: f32, _: f32) {}
    fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, _: f32, _: f32) {}
    fn close(&mut self) {}
}
