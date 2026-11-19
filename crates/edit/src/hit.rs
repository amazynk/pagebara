//! Hit testing for the UI: objects under a point or in a rectangle, text positions and
//! carets. Points and rectangles are in page space (see [`PageFrame`](crate::PageFrame)
//! for view space).

use pagebara_content::{Point, Rect};

use crate::geom::{contains, quad_contains, rect_distance};
use crate::model::{Kind, ObjectData, ObjectId, PageObject, PageObjects, RunId};

/// How hit testing looks for objects.
#[derive(Clone, Copy, Debug)]
pub struct HitOptions {
    /// Distance in page units that still hits thin paths and text.
    pub tolerance: f32,
    /// Objects inside forms rather than the forms.
    pub into_forms: bool,
    /// Objects in hidden optional content.
    pub hidden: bool,
}

impl Default for HitOptions {
    fn default() -> Self {
        HitOptions {
            tolerance: 2.0,
            into_forms: false,
            hidden: false,
        }
    }
}

fn candidates<'a>(
    objs: &'a PageObjects,
    o: &'a HitOptions,
) -> impl DoubleEndedIterator<Item = &'a PageObject> {
    objs.objects.iter().filter(move |ob| {
        (o.hidden || !ob.hidden)
            && if o.into_forms {
                ob.kind != Kind::Form
            } else {
                ob.parent.is_none()
            }
    })
}

/// Whether `p` is on the object.
pub fn hits(objs: &PageObjects, ob: &PageObject, p: Point, tol: f32) -> bool {
    if rect_distance(&ob.bbox, p) > tol {
        return false;
    }
    if let Some(c) = &ob.clip
        && rect_distance(c, p) > tol
    {
        return false;
    }
    match &ob.data {
        ObjectData::Text(t) => t.runs().any(|r| {
            objs.run(r).is_some_and(|r| {
                rect_distance(&r.bbox, p) <= tol
                    && r.glyphs.iter().any(|g| {
                        quad_contains(&g.quad, p)
                            || rect_distance(&Rect::from_points(&g.quad), p) <= tol
                    })
            })
        }),
        ObjectData::Image(_) | ObjectData::Form(_) => {
            quad_contains(&ob.quad, p) || rect_distance(&ob.bbox, p) <= tol && tol > 0.0
        }
        ObjectData::Path(path) => {
            let stroke_tol = tol + path.line_width.max(1.0) / 2.0;
            path.outline.iter().any(|sub| {
                (path.fill.is_some() && polygon_contains(sub, p))
                    || segments_near(sub, p, stroke_tol)
            })
        }
        ObjectData::Shading(_) => contains(&ob.bbox, p),
    }
}

/// The topmost object at a point.
pub fn object_at(objs: &PageObjects, p: Point, o: &HitOptions) -> Option<ObjectId> {
    candidates(objs, o)
        .rev()
        .find(|ob| hits(objs, ob, p, o.tolerance))
        .map(|ob| ob.id)
}

/// Every object under a point, topmost first.
pub fn objects_at(objs: &PageObjects, p: Point, o: &HitOptions) -> Vec<ObjectId> {
    candidates(objs, o)
        .rev()
        .filter(|ob| hits(objs, ob, p, o.tolerance))
        .map(|ob| ob.id)
        .collect()
}

/// Objects meeting a rectangle (or wholly inside it, with `inside`), in paint order.
pub fn objects_in(objs: &PageObjects, r: Rect, inside: bool, o: &HitOptions) -> Vec<ObjectId> {
    candidates(objs, o)
        .filter(|ob| {
            let b = &ob.bbox;
            if inside {
                b.x0 >= r.x0 && b.y0 >= r.y0 && b.x1 <= r.x1 && b.y1 <= r.y1
            } else {
                crate::geom::intersects(b, &r)
            }
        })
        .map(|ob| ob.id)
        .collect()
}

/// Grab handles of an object: corners then edge midpoints of its frame.
pub fn handles(ob: &PageObject) -> [Point; 8] {
    crate::geom::handles(&ob.quad)
}

/// A place in text: before glyph `glyph` of a run (`glyph` = glyph count for after the
/// last one).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextPos {
    pub run: RunId,
    pub glyph: usize,
}

/// The text position nearest to a point within `max` page units of text.
pub fn text_pos_at(objs: &PageObjects, p: Point, max: f32) -> Option<TextPos> {
    let mut best: Option<(f32, TextPos)> = None;
    for r in &objs.runs {
        if r.hidden || rect_distance(&r.bbox, p) > max {
            continue;
        }
        for (i, g) in r.glyphs.iter().enumerate() {
            let d = if quad_contains(&g.quad, p) {
                0.0
            } else {
                rect_distance(&Rect::from_points(&g.quad), p)
            };
            if d > max || best.is_some_and(|b| b.0 <= d) {
                continue;
            }
            // Before or after the glyph: which half along the advance.
            let a = mid(g.quad[0], g.quad[3]);
            let b = mid(g.quad[1], g.quad[2]);
            let (dx, dy) = (b.x - a.x, b.y - a.y);
            let len2 = dx * dx + dy * dy;
            let t = if len2 > 0.0 {
                ((p.x - a.x) * dx + (p.y - a.y) * dy) / len2
            } else {
                0.0
            };
            let glyph = if t > 0.5 { i + 1 } else { i };
            best = Some((d, TextPos { run: r.id, glyph }));
        }
    }
    best.map(|b| b.1)
}

/// A caret: the line from the descent to the ascent at a text position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Caret {
    pub bottom: Point,
    pub top: Point,
}

impl Caret {
    /// The caret as a rectangle `width` page units wide.
    pub fn rect(&self, width: f32) -> Rect {
        let w = width / 2.0;
        Rect::from_points(&[
            Point::new(self.bottom.x - w, self.bottom.y - w),
            Point::new(self.top.x + w, self.top.y + w),
            Point::new(self.bottom.x + w, self.bottom.y + w),
            Point::new(self.top.x - w, self.top.y - w),
        ])
    }
}

pub fn caret(objs: &PageObjects, pos: TextPos) -> Option<Caret> {
    let r = objs.run(pos.run)?;
    if let Some(g) = r.glyphs.get(pos.glyph) {
        return Some(Caret {
            bottom: g.quad[0],
            top: g.quad[3],
        });
    }
    let g = r.glyphs.last()?;
    Some(Caret {
        bottom: g.quad[1],
        top: g.quad[2],
    })
}

fn mid(a: Point, b: Point) -> Point {
    Point::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0)
}

/// Even-odd point in polygon.
fn polygon_contains(poly: &[Point], p: Point) -> bool {
    if poly.len() < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[j]);
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
        j = i;
    }
    inside
}

fn segments_near(poly: &[Point], p: Point, tol: f32) -> bool {
    if poly.len() == 1 {
        return (poly[0].x - p.x).hypot(poly[0].y - p.y) <= tol;
    }
    poly.windows(2)
        .any(|w| segment_distance(w[0], w[1], p) <= tol)
        || (poly.len() > 2 && segment_distance(poly[poly.len() - 1], poly[0], p) <= tol)
}

fn segment_distance(a: Point, b: Point, p: Point) -> f32 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((p.x - a.x) * dx + (p.y - a.y) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (a.x + t * dx - p.x).hypot(a.y + t * dy - p.y)
}
