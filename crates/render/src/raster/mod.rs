//! Scan conversion: paths flattened to device-space lines, then exact area coverage per
//! pixel by signed-area accumulation, one band of rows at a time.

mod accumulate;
pub mod mask;
pub mod stroke;

pub use accumulate::{Rasterizer, Span};
pub use mask::Mask;

use pagebara_content::display::{Path, Verb};
use pagebara_content::{Matrix, Point};

/// Flattening tolerance in device pixels.
pub const TOLERANCE: f32 = 0.2;

/// An integer pixel rectangle, `x0..x1` × `y0..y1`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct IRect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl IRect {
    pub const EMPTY: IRect = IRect {
        x0: 0,
        y0: 0,
        x1: 0,
        y1: 0,
    };

    pub const fn new(x0: i32, y0: i32, x1: i32, y1: i32) -> Self {
        IRect { x0, y0, x1, y1 }
    }

    pub fn width(&self) -> i32 {
        (self.x1 - self.x0).max(0)
    }

    pub fn height(&self) -> i32 {
        (self.y1 - self.y0).max(0)
    }

    pub fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }

    pub fn intersect(&self, o: &IRect) -> IRect {
        let r = IRect {
            x0: self.x0.max(o.x0),
            y0: self.y0.max(o.y0),
            x1: self.x1.min(o.x1),
            y1: self.y1.min(o.y1),
        };
        if r.is_empty() { IRect::EMPTY } else { r }
    }

    pub fn union(&self, o: &IRect) -> IRect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        IRect {
            x0: self.x0.min(o.x0),
            y0: self.y0.min(o.y0),
            x1: self.x1.max(o.x1),
            y1: self.y1.max(o.y1),
        }
    }

    /// The pixels touched by a float rectangle `[x0, y0, x1, y1]`, saturating.
    pub fn covering(b: [f32; 4]) -> IRect {
        let c = |v: f32| v.clamp(-(1 << 24) as f32, (1 << 24) as f32);
        IRect {
            x0: c(b[0].floor()) as i32,
            y0: c(b[1].floor()) as i32,
            x1: c(b[2].ceil()) as i32,
            y1: c(b[3].ceil()) as i32,
        }
    }

    pub fn area(&self) -> usize {
        self.width() as usize * self.height() as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Line {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

/// A flattened path in device pixels: closed polygons as unordered lines.
#[derive(Clone, Debug, Default)]
pub struct Edges {
    pub lines: Vec<Line>,
    /// `[x0, y0, x1, y1]` of all points; empty when `x0 > x1`.
    pub bounds: [f32; 4],
}

impl Edges {
    pub fn new() -> Self {
        Edges {
            lines: Vec::new(),
            bounds: [f32::MAX, f32::MAX, f32::MIN, f32::MIN],
        }
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.bounds = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    #[inline]
    pub fn push(&mut self, a: Point, b: Point) {
        if a.y == b.y {
            // Horizontal lines add no coverage but still bound the path.
            self.extend(a);
            self.extend(b);
            return;
        }
        self.extend(a);
        self.extend(b);
        self.lines.push(Line {
            x0: a.x,
            y0: a.y,
            x1: b.x,
            y1: b.y,
        });
    }

    #[inline]
    fn extend(&mut self, p: Point) {
        let b = &mut self.bounds;
        b[0] = b[0].min(p.x);
        b[1] = b[1].min(p.y);
        b[2] = b[2].max(p.x);
        b[3] = b[3].max(p.y);
    }

    pub fn pixel_bounds(&self) -> IRect {
        if self.bounds[0] > self.bounds[2] || !self.bounds.iter().all(|v| v.is_finite()) {
            return IRect::EMPTY;
        }
        IRect::covering(self.bounds)
    }

    /// Whether the edges are exactly one axis-aligned rectangle; returns it.
    pub fn as_rect(&self) -> Option<[f32; 4]> {
        if self.lines.len() != 2 {
            return None;
        }
        let (a, b) = (self.lines[0], self.lines[1]);
        (a.x0 == a.x1 && b.x0 == b.x1 && a.x0 != b.x0).then_some(self.bounds)
    }
}

/// Builds [`Edges`] from path segments, transforming and flattening as it goes.
#[derive(Debug)]
pub struct Flattener<'a> {
    pub out: &'a mut Edges,
    m: Matrix,
    start: Point,
    cur: Point,
    open: bool,
    tol: f32,
}

impl<'a> Flattener<'a> {
    pub fn new(out: &'a mut Edges, m: Matrix) -> Self {
        Flattener {
            out,
            m,
            start: Point::default(),
            cur: Point::default(),
            open: false,
            tol: TOLERANCE,
        }
    }

    pub fn move_to(&mut self, p: Point) {
        self.close();
        let p = self.m.apply(p);
        self.start = p;
        self.cur = p;
        self.open = true;
    }

    pub fn line_to(&mut self, p: Point) {
        let p = self.m.apply(p);
        self.out.push(self.cur, p);
        self.cur = p;
    }

    pub fn quad_to(&mut self, c: Point, p: Point) {
        let (c, p) = (self.m.apply(c), self.m.apply(p));
        let p0 = self.cur;
        let dd = len(p0.x - 2.0 * c.x + p.x, p0.y - 2.0 * c.y + p.y);
        let n = ((dd / (4.0 * self.tol)).sqrt().ceil() as usize).clamp(1, 1000);
        let mut prev = p0;
        for i in 1..n {
            let t = i as f32 / n as f32;
            let mt = 1.0 - t;
            let q = Point::new(
                mt * mt * p0.x + 2.0 * mt * t * c.x + t * t * p.x,
                mt * mt * p0.y + 2.0 * mt * t * c.y + t * t * p.y,
            );
            self.out.push(prev, q);
            prev = q;
        }
        self.out.push(prev, p);
        self.cur = p;
    }

    pub fn cubic_to(&mut self, a: Point, b: Point, p: Point) {
        let (a, b, p) = (self.m.apply(a), self.m.apply(b), self.m.apply(p));
        let p0 = self.cur;
        flatten_cubic(p0, a, b, p, self.tol, &mut |q, r| self.out.push(q, r));
        self.cur = p;
    }

    pub fn close(&mut self) {
        if self.open {
            if self.cur != self.start {
                self.out.push(self.cur, self.start);
            }
            self.cur = self.start;
            self.open = false;
        }
    }

    /// Adds a display list path; every subpath is closed for filling.
    pub fn path(&mut self, path: &Path<'_>) {
        let mut pts = path.points.iter().copied();
        let mut next = || pts.next().unwrap_or_default();
        for v in path.verbs {
            match v {
                Verb::Move => self.move_to(next()),
                Verb::Line => self.line_to(next()),
                Verb::Cubic => {
                    let (a, b, c) = (next(), next(), next());
                    self.cubic_to(a, b, c)
                }
                Verb::Close => self.close(),
            }
        }
        self.close();
    }
}

#[inline]
fn len(x: f32, y: f32) -> f32 {
    (x * x + y * y).sqrt()
}

/// Segments of a cubic within `tol` (Wang's bound), sent to `emit` as line pairs.
pub fn flatten_cubic(
    p0: Point,
    a: Point,
    b: Point,
    p: Point,
    tol: f32,
    emit: &mut impl FnMut(Point, Point),
) {
    let d1 = len(p0.x - 2.0 * a.x + b.x, p0.y - 2.0 * a.y + b.y);
    let d2 = len(a.x - 2.0 * b.x + p.x, a.y - 2.0 * b.y + p.y);
    let dd = d1.max(d2);
    let n = (0.75 * dd / tol).sqrt().ceil();
    let n = if n.is_finite() {
        (n as usize).clamp(1, 2000)
    } else {
        1
    };
    if n == 1 {
        emit(p0, p);
        return;
    }
    let mut prev = p0;
    let inv = 1.0 / n as f32;
    for i in 1..n {
        let t = i as f32 * inv;
        let mt = 1.0 - t;
        let (c0, c1, c2, c3) = (mt * mt * mt, 3.0 * mt * mt * t, 3.0 * mt * t * t, t * t * t);
        let q = Point::new(
            c0 * p0.x + c1 * a.x + c2 * b.x + c3 * p.x,
            c0 * p0.y + c1 * a.y + c2 * b.y + c3 * p.y,
        );
        emit(prev, q);
        prev = q;
    }
    emit(prev, p);
}
