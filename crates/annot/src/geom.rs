//! Points, rectangles, quadrilaterals and colours in PDF user space.

use pagebara_syntax::Object;

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const fn new(x: f64, y: f64) -> Self {
        Point { x, y }
    }

    pub fn dist(self, o: Point) -> f64 {
        (self.x - o.x).hypot(self.y - o.y)
    }
}

/// A rectangle, normalised so that `x0 <= x1` and `y0 <= y1`.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Rect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Rect {
    pub fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        Rect {
            x0: x0.min(x1),
            y0: y0.min(y1),
            x1: x0.max(x1),
            y1: y0.max(y1),
        }
    }

    /// A rectangle from its lower-left corner and size.
    pub fn from_size(x: f64, y: f64, w: f64, h: f64) -> Self {
        Rect::new(x, y, x + w, y + h)
    }

    pub fn width(&self) -> f64 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> f64 {
        self.y1 - self.y0
    }

    pub fn inset(&self, d: f64) -> Rect {
        let dx = d.min(self.width() / 2.0);
        let dy = d.min(self.height() / 2.0);
        Rect {
            x0: self.x0 + dx,
            y0: self.y0 + dy,
            x1: self.x1 - dx,
            y1: self.y1 - dy,
        }
    }

    pub fn contains(&self, p: Point) -> bool {
        (self.x0..=self.x1).contains(&p.x) && (self.y0..=self.y1).contains(&p.y)
    }

    /// The bounding box of `points`, or `None` if there are none.
    pub fn bounding(points: impl IntoIterator<Item = Point>) -> Option<Rect> {
        points.into_iter().fold(None, |r, p| {
            Some(match r {
                None => Rect::new(p.x, p.y, p.x, p.y),
                Some(r) => Rect {
                    x0: r.x0.min(p.x),
                    y0: r.y0.min(p.y),
                    x1: r.x1.max(p.x),
                    y1: r.y1.max(p.y),
                },
            })
        })
    }

    pub fn to_object(self) -> Object {
        nums([self.x0, self.y0, self.x1, self.y1])
    }

    pub fn from_object(o: &Object) -> Option<Rect> {
        let a = numbers(o)?;
        match a[..] {
            [x0, y0, x1, y1] => Some(Rect::new(x0, y0, x1, y1)),
            _ => None,
        }
    }
}

/// A quadrilateral of `/QuadPoints`, in the order readers expect: upper left, upper right,
/// lower left, lower right of the text run.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quad(pub [Point; 4]);

impl Quad {
    /// The quad covering an axis-aligned text box.
    pub fn from_rect(r: Rect) -> Self {
        Quad([
            Point::new(r.x0, r.y1),
            Point::new(r.x1, r.y1),
            Point::new(r.x0, r.y0),
            Point::new(r.x1, r.y0),
        ])
    }

    pub fn upper_left(&self) -> Point {
        self.0[0]
    }
    pub fn upper_right(&self) -> Point {
        self.0[1]
    }
    pub fn lower_left(&self) -> Point {
        self.0[2]
    }
    pub fn lower_right(&self) -> Point {
        self.0[3]
    }

    /// Distance between the top and bottom edges.
    pub fn height(&self) -> f64 {
        self.upper_left().dist(self.lower_left())
    }
}

/// A colour in one of the device spaces an annotation's `/C` and `/IC` arrays can name.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Color {
    Gray(f32),
    Rgb(f32, f32, f32),
    Cmyk(f32, f32, f32, f32),
}

impl Color {
    pub const BLACK: Color = Color::Gray(0.0);
    pub const YELLOW: Color = Color::Rgb(1.0, 0.92, 0.23);
    pub const RED: Color = Color::Rgb(0.9, 0.1, 0.1);
    pub const BLUE: Color = Color::Rgb(0.08, 0.2, 0.6);

    pub fn components(&self) -> Vec<f32> {
        match *self {
            Color::Gray(g) => vec![g],
            Color::Rgb(r, g, b) => vec![r, g, b],
            Color::Cmyk(c, m, y, k) => vec![c, m, y, k],
        }
    }

    /// A naive conversion to RGB, for styles that only take RGB.
    pub fn to_rgb(self) -> [f32; 3] {
        match self {
            Color::Gray(g) => [g; 3],
            Color::Rgb(r, g, b) => [r, g, b],
            Color::Cmyk(c, m, y, k) => [c, m, y].map(|v| (1.0 - v) * (1.0 - k)),
        }
    }

    pub fn to_object(self) -> Object {
        nums(self.components().into_iter().map(f64::from))
    }

    /// `None` for an empty array (transparent) or an unusable value.
    pub fn from_object(o: &Object) -> Option<Color> {
        let a = numbers(o)?;
        let c = |i: usize| a[i].clamp(0.0, 1.0) as f32;
        match a.len() {
            1 => Some(Color::Gray(c(0))),
            3 => Some(Color::Rgb(c(0), c(1), c(2))),
            4 => Some(Color::Cmyk(c(0), c(1), c(2), c(3))),
            _ => None,
        }
    }
}

pub(crate) fn nums(v: impl IntoIterator<Item = f64>) -> Object {
    Object::Array(v.into_iter().map(num).collect())
}

/// A number as an integer when it is one, else rounded to what PDF readers resolve.
pub(crate) fn num(v: f64) -> Object {
    let r = (v * 10_000.0).round() / 10_000.0;
    if r.fract() == 0.0 && r.abs() < 1e15 {
        Object::Int(r as i64)
    } else {
        Object::Real(r)
    }
}

pub(crate) fn numbers(o: &Object) -> Option<Vec<f64>> {
    o.as_array()?.iter().map(Object::as_f64).collect()
}

pub(crate) fn points(o: &Object) -> Option<Vec<Point>> {
    let a = numbers(o)?;
    Some(
        a.as_chunks::<2>()
            .0
            .iter()
            .map(|c| Point::new(c[0], c[1]))
            .collect(),
    )
}

pub(crate) fn points_object(p: &[Point]) -> Object {
    nums(p.iter().flat_map(|p| [p.x, p.y]))
}
