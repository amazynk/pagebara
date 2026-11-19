//! Page geometry for the UI, and small helpers on points, rectangles and matrices.

use pagebara_content::{Matrix, Point, Rect};

/// Where a page's default user space shows: its boxes and rotation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageFrame {
    pub media_box: Rect,
    /// The visible region (crop box clipped to the media box).
    pub crop_box: Rect,
    /// Clockwise display rotation: 0, 90, 180 or 270.
    pub rotate: u16,
}

impl PageFrame {
    pub fn of(p: &pagebara_pages::Page) -> Self {
        let r = |b: &pagebara_pages::Rect| {
            Rect::new(b.x0 as f32, b.y0 as f32, b.x1 as f32, b.y1 as f32)
        };
        PageFrame {
            media_box: r(&p.media_box),
            crop_box: r(&p.crop_box),
            rotate: p.rotate,
        }
    }

    /// The displayed size in points.
    pub fn size(&self) -> (f32, f32) {
        let (w, h) = (self.crop_box.width(), self.crop_box.height());
        if self.rotate % 180 == 90 {
            (h, w)
        } else {
            (w, h)
        }
    }

    /// Page space to view space: points from the top left of the displayed (cropped,
    /// rotated) page, y down.
    pub fn to_view(&self) -> Matrix {
        let c = &self.crop_box;
        let flip = Matrix::new(1.0, 0.0, 0.0, -1.0, -c.x0, c.y1);
        let (w, h) = (c.width(), c.height());
        let rot = match self.rotate {
            90 => Matrix::new(0.0, 1.0, -1.0, 0.0, h, 0.0),
            180 => Matrix::new(-1.0, 0.0, 0.0, -1.0, w, h),
            270 => Matrix::new(0.0, -1.0, 1.0, 0.0, 0.0, w),
            _ => Matrix::IDENTITY,
        };
        flip.then(&rot)
    }

    /// View space to page space.
    pub fn from_view(&self) -> Matrix {
        self.to_view().invert().unwrap_or_default()
    }

    pub fn view_point(&self, p: Point) -> Point {
        self.to_view().apply(p)
    }

    pub fn page_point(&self, view: Point) -> Point {
        self.from_view().apply(view)
    }

    /// A page-space rectangle in view space.
    pub fn view_rect(&self, r: Rect) -> Rect {
        r.transform(&self.to_view())
    }
}

/// Grab handles of a quadrilateral: corners, then edge midpoints (bottom, right, top,
/// left in the quad's own frame).
pub fn handles(q: &[Point; 4]) -> [Point; 8] {
    let mid = |a: Point, b: Point| Point::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
    [
        q[0],
        q[1],
        q[2],
        q[3],
        mid(q[0], q[1]),
        mid(q[1], q[2]),
        mid(q[2], q[3]),
        mid(q[3], q[0]),
    ]
}

pub fn corners(r: &Rect) -> [Point; 4] {
    [
        Point::new(r.x0, r.y0),
        Point::new(r.x1, r.y0),
        Point::new(r.x1, r.y1),
        Point::new(r.x0, r.y1),
    ]
}

pub fn intersects(a: &Rect, b: &Rect) -> bool {
    a.x0 <= b.x1 && b.x0 <= a.x1 && a.y0 <= b.y1 && b.y0 <= a.y1
}

pub fn intersect(a: &Rect, b: &Rect) -> Option<Rect> {
    let (x0, y0) = (a.x0.max(b.x0), a.y0.max(b.y0));
    let (x1, y1) = (a.x1.min(b.x1), a.y1.min(b.y1));
    (x0 <= x1 && y0 <= y1).then(|| Rect::new(x0, y0, x1, y1))
}

pub fn contains(r: &Rect, p: Point) -> bool {
    r.x0 <= p.x && p.x <= r.x1 && r.y0 <= p.y && p.y <= r.y1
}

/// Whether a point is inside a convex quadrilateral (either winding).
pub fn quad_contains(q: &[Point; 4], p: Point) -> bool {
    let mut sign = 0.0f32;
    for k in 0..4 {
        let (a, b) = (q[k], q[(k + 1) % 4]);
        let c = (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x);
        if c.abs() < 1e-6 {
            continue;
        }
        if sign == 0.0 {
            sign = c.signum();
        } else if c.signum() != sign {
            return false;
        }
    }
    true
}

/// Distance from a point to a rectangle (0 inside).
pub fn rect_distance(r: &Rect, p: Point) -> f32 {
    let dx = (r.x0 - p.x).max(p.x - r.x1).max(0.0);
    let dy = (r.y0 - p.y).max(p.y - r.y1).max(0.0);
    dx.hypot(dy)
}

/// `a × b⁻¹`, `None` for a singular `b`.
pub fn relative(a: &Matrix, b: &Matrix) -> Option<Matrix> {
    Some(a.then(&b.invert()?))
}

/// A page-space transform as seen in a user space with CTM `ctm`: content drawn there
/// with this prepended lands where `t` takes it.
pub fn in_user_space(t: &Matrix, ctm: &Matrix) -> Option<Matrix> {
    Some(ctm.then(t).then(&ctm.invert()?))
}

pub fn approx_eq(a: &Matrix, b: &Matrix) -> bool {
    let d = [
        a.a - b.a,
        a.b - b.b,
        a.c - b.c,
        a.d - b.d,
        a.e - b.e,
        a.f - b.f,
    ];
    d.iter().all(|v| v.abs() < 1e-4)
}

pub fn is_identity(m: &Matrix) -> bool {
    approx_eq(m, &Matrix::IDENTITY)
}

/// Rotation by `degrees` counterclockwise about `center`.
pub fn rotate_about(center: Point, degrees: f32) -> Matrix {
    let (s, c) = degrees.to_radians().sin_cos();
    Matrix::translate(-center.x, -center.y)
        .then(&Matrix::new(c, s, -s, c, 0.0, 0.0))
        .then(&Matrix::translate(center.x, center.y))
}

/// Scaling by `sx, sy` keeping `anchor` in place.
pub fn scale_about(anchor: Point, sx: f32, sy: f32) -> Matrix {
    Matrix::translate(-anchor.x, -anchor.y)
        .then(&Matrix::scale(sx, sy))
        .then(&Matrix::translate(anchor.x, anchor.y))
}

/// The matrix taking rectangle `from` onto `to`.
pub fn rect_to_rect(from: &Rect, to: &Rect) -> Matrix {
    let sx = if from.width() > 0.0 {
        to.width() / from.width()
    } else {
        1.0
    };
    let sy = if from.height() > 0.0 {
        to.height() / from.height()
    } else {
        1.0
    };
    Matrix::new(sx, 0.0, 0.0, sy, to.x0 - from.x0 * sx, to.y0 - from.y0 * sy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_space_round_trip() {
        for rotate in [0, 90, 180, 270] {
            let f = PageFrame {
                media_box: Rect::new(0.0, 0.0, 300.0, 200.0),
                crop_box: Rect::new(10.0, 20.0, 290.0, 180.0),
                rotate,
            };
            let (w, h) = f.size();
            // The crop box's corners land on the view's corners.
            let v = f.view_rect(f.crop_box);
            assert!((v.x0).abs() < 1e-3 && (v.y0).abs() < 1e-3, "{rotate} {v:?}");
            assert!(
                (v.x1 - w).abs() < 1e-3 && (v.y1 - h).abs() < 1e-3,
                "{rotate} {v:?}"
            );
            let p = Point::new(42.0, 77.0);
            let q = f.page_point(f.view_point(p));
            assert!((p.x - q.x).abs() < 1e-3 && (p.y - q.y).abs() < 1e-3);
        }
        // The top left of an unrotated page.
        let f = PageFrame {
            media_box: Rect::new(0.0, 0.0, 300.0, 200.0),
            crop_box: Rect::new(0.0, 0.0, 300.0, 200.0),
            rotate: 0,
        };
        assert_eq!(f.view_point(Point::new(0.0, 200.0)), Point::new(0.0, 0.0));
    }

    #[test]
    fn intersect_disjoint_is_none() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert_eq!(intersect(&a, &Rect::new(20.0, 0.0, 30.0, 10.0)), None);
        assert_eq!(intersect(&a, &Rect::new(0.0, 12.0, 10.0, 30.0)), None);
        assert_eq!(
            intersect(&a, &Rect::new(5.0, 5.0, 30.0, 30.0)),
            Some(Rect::new(5.0, 5.0, 10.0, 10.0))
        );
    }
}
