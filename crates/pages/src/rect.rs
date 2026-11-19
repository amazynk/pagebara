use pagebara_syntax::{Document, Object};

/// A rectangle in default user space, normalised so that `x0 <= x1` and `y0 <= y1`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Rect {
    /// US Letter, the customary fallback for a missing or broken `/MediaBox`.
    pub const LETTER: Rect = Rect::new(0.0, 0.0, 612.0, 792.0);

    pub const fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        Rect {
            x0: x0.min(x1),
            y0: y0.min(y1),
            x1: x0.max(x1),
            y1: y0.max(y1),
        }
    }

    pub fn width(&self) -> f64 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> f64 {
        self.y1 - self.y0
    }

    pub fn is_empty(&self) -> bool {
        self.width() <= 0.0 || self.height() <= 0.0
    }

    pub fn intersect(&self, o: &Rect) -> Option<Rect> {
        let r = Rect {
            x0: self.x0.max(o.x0),
            y0: self.y0.max(o.y0),
            x1: self.x1.min(o.x1),
            y1: self.y1.min(o.y1),
        };
        (!r.is_empty()).then_some(r)
    }

    /// Parses a rectangle array, resolving references; `None` unless it has four finite
    /// numbers enclosing a non-empty area.
    pub fn from_object(doc: &Document, o: &Object) -> Option<Rect> {
        let a = doc.resolve(o);
        let a = a.as_array()?;
        if a.len() < 4 {
            return None;
        }
        let mut v = [0.0; 4];
        for (x, o) in v.iter_mut().zip(a) {
            *x = doc.resolve(o).as_f64().filter(|x| x.is_finite())?;
        }
        let r = Rect::new(v[0], v[1], v[2], v[3]);
        (!r.is_empty()).then_some(r)
    }

    pub fn to_object(self) -> Object {
        let n = |x: f64| {
            if x.fract() == 0.0 && x.abs() < 1e15 {
                Object::Int(x as i64)
            } else {
                Object::Real(x)
            }
        };
        Object::Array(vec![n(self.x0), n(self.y0), n(self.x1), n(self.y1)])
    }
}
