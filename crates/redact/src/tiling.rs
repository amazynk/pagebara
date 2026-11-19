//! Tiling patterns painted where regions are.
//!
//! Painting with a tiling pattern is clipped out of regions like any other paint, but the
//! pattern's cell stays in the file. When the pattern also shows a whole cell outside the
//! regions, what the cell holds is shown there anyway, as with a form used again
//! elsewhere, and the pattern stays as it is. This is the usual case of small repeating
//! cells, and keeps their output exactly. A cell counts as shown whole only where the
//! painted shape (a fill's path by its fill rule, a solid stroke's width along a segment)
//! and every clipping path in force hold it; text, stencil masks and forms painting with
//! an inherited pattern count as painting their boxes but never as showing a cell whole.
//!
//! Otherwise the cell's position matters (a few large cells, a pattern used as a picture),
//! and the pattern is replaced, for this content stream, by one large cell drawing every
//! cell the painted areas meet explicitly, each as a form placed where it was: cells
//! meeting a region are copies with the content under the regions (and outside the painted
//! areas) removed, cells wholly inside a region are left out. The result draws the same
//! outside the regions, vector content staying vector. A pattern painted only inside
//! regions so becomes an empty one. Patterns with more cells than [`MAX_CELLS`] to draw are
//! left clipped, with a warning.

use std::sync::Arc;

use pagebara_syntax::{Dict, Object, Stream};

use crate::geom::{Matrix, Point, Rect, Region, clip_polygon};
use crate::path::Sub;
use crate::rewrite::{FormResult, Res, Rewriter};

/// Cells drawn explicitly at most for one pattern.
const MAX_CELLS: usize = 1024;
/// Cells looked at, at most, for one shown whole outside the regions.
const MAX_SCAN: usize = 1 << 16;
/// Far enough to stand for infinity in page space.
const FAR: f64 = 1e7;

/// A filled area in device space: closed polygons and the fill rule.
#[derive(Clone, Debug)]
pub(crate) struct Area {
    polys: Vec<Vec<Point>>,
    even_odd: bool,
}

/// Whether segments `a`–`b` and `c`–`d` meet (touching counts).
fn crosses(a: Point, b: Point, c: Point, d: Point) -> bool {
    let side = |p: Point, q: Point, r: Point| q.sub(p).cross(r.sub(p));
    let (d1, d2) = (side(c, d, a), side(c, d, b));
    let (d3, d4) = (side(a, b, c), side(a, b, d));
    let within = |p: Point, q: Point, r: Point| {
        r.x >= p.x.min(q.x) && r.x <= p.x.max(q.x) && r.y >= p.y.min(q.y) && r.y <= p.y.max(q.y)
    };
    if d1 * d2 < 0.0 && d3 * d4 < 0.0 {
        return true;
    }
    (d1 == 0.0 && within(c, d, a))
        || (d2 == 0.0 && within(c, d, b))
        || (d3 == 0.0 && within(a, b, c))
        || (d4 == 0.0 && within(a, b, d))
}

impl Area {
    pub(crate) fn new(subs: &[Sub], even_odd: bool) -> Area {
        Area {
            polys: subs
                .iter()
                .map(Sub::polyline)
                .filter(|p| p.len() > 2)
                .collect(),
            even_odd,
        }
    }

    fn edges(&self) -> impl Iterator<Item = (Point, Point)> + '_ {
        self.polys
            .iter()
            .flat_map(|p| (0..p.len()).map(move |i| (p[i], p[(i + 1) % p.len()])))
    }

    fn inside(&self, p: Point) -> bool {
        let mut w = 0i32;
        for (a, b) in self.edges() {
            if (a.y <= p.y) != (b.y <= p.y) {
                let x = a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x);
                if x > p.x {
                    w += if b.y > a.y { 1 } else { -1 };
                }
            }
        }
        if self.even_odd { w % 2 != 0 } else { w != 0 }
    }

    /// Whether the area holds all of the convex quadrilateral `q`: no edge enters it, so
    /// its centre decides.
    fn covers(&self, q: &[Point; 4]) -> bool {
        let Some(r) = Region::from_points(q) else {
            return false;
        };
        for (a, b) in self.edges() {
            if r.contains(a) || (0..4).any(|k| crosses(a, b, q[k], q[(k + 1) % 4])) {
                return false;
            }
        }
        let c = Point::new(
            q.iter().map(|p| p.x).sum::<f64>() / 4.0,
            q.iter().map(|p| p.y).sum::<f64>() / 4.0,
        );
        self.inside(c)
    }
}

/// What a paint operation covers, as far as known.
#[derive(Clone, Debug)]
pub(crate) enum Shape {
    Fill(Area),
    /// A solid stroke: its segments (device space) and the least half width around them.
    Stroke(Vec<(Point, Point)>, f64),
    Unknown,
}

impl Shape {
    fn covers(&self, q: &[Point; 4]) -> bool {
        match self {
            Shape::Fill(a) => a.covers(q),
            Shape::Stroke(lines, half) => lines.iter().any(|&(a, b)| {
                let d = b.sub(a);
                let len = d.dot(d).sqrt();
                len > 1e-9
                    && q.iter().all(|&p| {
                        let v = p.sub(a);
                        let t = v.dot(d) / len;
                        (0.0..=len).contains(&t) && (v.cross(d) / len).abs() <= *half
                    })
            }),
            Shape::Unknown => false,
        }
    }
}

/// An area painted with a pattern: its device box, its shape and the clipping paths in
/// force (`None` for one whose shape is not known).
#[derive(Clone, Debug)]
pub(crate) struct Use {
    pub bbox: Rect,
    pub shape: Shape,
    pub clip: Arc<Vec<Option<Area>>>,
}

impl Area {
    /// The box of what of the convex quadrilateral `q` the area may paint.
    fn visible(&self, q: &[Point; 4]) -> Option<Rect> {
        let r = Region::from_points(q)?;
        self.polys
            .iter()
            .filter_map(|p| {
                let mut p = p.clone();
                for (n, c) in r.edges() {
                    p = clip_polygon(&p, n, c);
                }
                (!p.is_empty()).then(|| Rect::from_points(&p))
            })
            .reduce(|a, b| a.union(&b))
    }
}

fn meet(a: Rect, b: Rect) -> Option<Rect> {
    let (x0, y0, x1, y1) = (
        a.x0.max(b.x0),
        a.y0.max(b.y0),
        a.x1.min(b.x1),
        a.y1.min(b.y1),
    );
    (x0 <= x1 && y0 <= y1).then(|| Rect::new(x0, y0, x1, y1))
}

impl Use {
    /// The box of what of `q` this use may paint.
    fn visible(&self, q: &[Point; 4]) -> Option<Rect> {
        let mut b = meet(self.bbox, Rect::from_points(q))?;
        if let Shape::Fill(a) = &self.shape {
            b = meet(b, a.visible(q)?)?;
        }
        for c in self.clip.iter().flatten() {
            b = meet(b, c.visible(q)?)?;
        }
        Some(b)
    }

    /// Whether all of `q` is painted.
    fn covers(&self, q: &[Point; 4]) -> bool {
        self.shape.covers(q)
            && self
                .clip
                .iter()
                .all(|c| c.as_ref().is_some_and(|a| a.covers(q)))
    }
}

/// What became of a tiling pattern.
pub(crate) enum Tiling {
    Keep,
    New(Stream),
    TooMany,
}

/// A pattern's cell grid in device space.
struct Grid {
    /// Pattern space → device.
    m: Matrix,
    inv: Matrix,
    bbox: Rect,
    xs: f64,
    ys: f64,
}

impl Grid {
    fn quad(&self, i: i64, j: i64) -> [Point; 4] {
        let (dx, dy) = (i as f64 * self.xs, j as f64 * self.ys);
        Rect::new(
            self.bbox.x0 + dx,
            self.bbox.y0 + dy,
            self.bbox.x1 + dx,
            self.bbox.y1 + dy,
        )
        .corners()
        .map(|p| self.m.apply(p))
    }

    /// Indices of the cells whose box may meet device rectangle `r`.
    fn cells(&self, r: &Rect) -> Option<(Vec<i64>, Vec<i64>)> {
        let p = Rect::from_points(&r.corners().map(|c| self.inv.apply(c)));
        let b = self.bbox;
        let i0 = ((p.x0 - b.x1) / self.xs).floor();
        let i1 = ((p.x1 - b.x0) / self.xs).ceil();
        let j0 = ((p.y0 - b.y1) / self.ys).floor();
        let j1 = ((p.y1 - b.y0) / self.ys).ceil();
        let n = (i1 - i0 + 1.0) * (j1 - j0 + 1.0);
        if !n.is_finite() || n > 1e12 {
            return None;
        }
        Some((
            (i0 as i64..=i1 as i64).collect(),
            (j0 as i64..=j1 as i64).collect(),
        ))
    }
}

fn inside(r: &Rect, q: &[Point]) -> bool {
    q.iter()
        .all(|p| p.x >= r.x0 && p.x <= r.x1 && p.y >= r.y0 && p.y <= r.y1)
}

/// Regions covering everything outside `r`.
fn outside(r: &Rect) -> [Region; 4] {
    [
        Rect::new(-FAR, -FAR, r.x0, FAR),
        Rect::new(r.x1, -FAR, FAR, FAR),
        Rect::new(r.x0, -FAR, r.x1, r.y0),
        Rect::new(r.x0, r.y1, r.x1, FAR),
    ]
    .map(Region::from_rect)
}

impl Rewriter<'_> {
    /// Replaces the tiling patterns of `res` painted (`uses`, by name) where regions are,
    /// when needed (see the module docs). Returns whether any changed.
    pub(crate) fn tiling_patterns(
        &mut self,
        res: &mut Res,
        ctm: &Matrix,
        uses: Vec<(Vec<u8>, Vec<Use>)>,
    ) -> bool {
        let mut changed = false;
        for (name, uses) in uses {
            let meets = uses
                .iter()
                .any(|u| self.regions.iter().any(|r| r.touches(&u.bbox.corners())));
            if !meets {
                continue;
            }
            let Some(o) = res.get(self.doc, b"Pattern", &name) else {
                continue;
            };
            let Some(ps) = self.doc.resolve(&o).as_stream().cloned() else {
                continue;
            };
            if self.doc.get_in(&ps.dict, b"PatternType").as_int() != Some(1) {
                continue;
            }
            let num = o.as_objref().map_or(0, |r| r.num);
            match self.tiling(&ps, num, ctm, &uses, res) {
                Tiling::Keep => {}
                Tiling::TooMany => self.warnings.push(
                    "a tiling pattern with too many cells under a region was only clipped".into(),
                ),
                Tiling::New(s) => {
                    let r = self.doc.add(s);
                    let mut pats = self
                        .doc
                        .get_in(&res.dict, b"Pattern")
                        .as_dict()
                        .cloned()
                        .unwrap_or_default();
                    pats.set(&name, Object::Ref(r));
                    res.dict.set(b"Pattern", Object::Dict(pats));
                    res.changed = true;
                    self.tainted.insert(num);
                    self.stats.patterns_rewritten += 1;
                    changed = true;
                }
            }
        }
        changed
    }

    /// The pattern `ps` (object `num`) painted as `uses` in a stream drawn with `ctm` and
    /// resources `parent`.
    fn tiling(
        &mut self,
        ps: &Stream,
        num: u32,
        ctm: &Matrix,
        uses: &[Use],
        parent: &Res,
    ) -> Tiling {
        let doc = &*self.doc;
        let pm = ps
            .dict
            .get(b"Matrix")
            .and_then(|m| Matrix::from_object(doc, m))
            .unwrap_or_default();
        let m = pm.then(ctm);
        let step = |k: &[u8]| doc.get_in(&ps.dict, k).as_f64().map(f64::abs);
        let (Some(inv), Some(bbox)) = (
            m.invert(),
            ps.dict.get(b"BBox").and_then(|b| Rect::from_object(doc, b)),
        ) else {
            return Tiling::Keep;
        };
        let xs = step(b"XStep").filter(|v| *v > 1e-9).unwrap_or(bbox.width());
        let ys = step(b"YStep")
            .filter(|v| *v > 1e-9)
            .unwrap_or(bbox.height());
        if xs <= 1e-9 || ys <= 1e-9 {
            return Tiling::Keep;
        }
        let grid = Grid {
            m,
            inv,
            bbox,
            xs,
            ys,
        };
        let clean = |q: &[Point; 4]| !self.regions.iter().any(|r| r.touches(q));

        // A whole cell shown outside the regions?
        let mut scanned = 0;
        for u in uses {
            let Some((is, js)) = grid.cells(&u.bbox) else {
                return Tiling::Keep;
            };
            for &i in &is {
                for &j in &js {
                    let q = grid.quad(i, j);
                    if inside(&u.bbox, &q) && clean(&q) && u.covers(&q) {
                        return Tiling::Keep;
                    }
                    scanned += 1;
                    if scanned > MAX_SCAN {
                        return Tiling::TooMany;
                    }
                }
            }
        }

        // The cells the painted areas meet.
        let Some(area) = uses.iter().map(|u| u.bbox).reduce(|a, b| a.union(&b)) else {
            return Tiling::Keep;
        };
        let Some((is, js)) = grid.cells(&area) else {
            return Tiling::TooMany;
        };
        if is.len().saturating_mul(js.len()) > MAX_SCAN {
            return Tiling::TooMany;
        }
        let mut cells = Vec::new();
        for &i in &is {
            for &j in &js {
                let q = grid.quad(i, j);
                let covered = self
                    .regions
                    .iter()
                    .any(|r| q.iter().all(|&p| r.contains(p)));
                let shown = uses
                    .iter()
                    .filter_map(|u| u.visible(&q))
                    .reduce(|a, b| a.union(&b));
                if let Some(shown) = shown
                    && !covered
                    && shown.width() > 1e-9
                    && shown.height() > 1e-9
                {
                    let whole = clean(&q) && uses.iter().any(|u| u.covers(&q));
                    cells.push((i, j, whole, shown));
                }
            }
        }
        if cells.len() > MAX_CELLS {
            return Tiling::TooMany;
        }

        // The cell as a form.
        let Ok(data) = self.doc.decode(ps) else {
            return Tiling::TooMany;
        };
        let mut fd = Dict::new();
        fd.set(b"Type", Object::name(b"XObject"));
        fd.set(b"Subtype", Object::name(b"Form"));
        fd.set(b"BBox", bbox.to_object());
        let cell_res = match ps.dict.get(b"Resources") {
            Some(r) => r.clone(),
            None => Object::Dict(parent.dict.clone()),
        };
        fd.set(b"Resources", cell_res);
        let raw = data.data.to_vec();
        let form = Stream::new(fd, raw.clone());
        let mut whole = None;

        // Cells not shown whole are copies without what lies under regions and what lies
        // outside the box of their painted part.
        let regions = self.regions.clone();
        let mut xobjects = Dict::new();
        let mut content = Vec::new();
        for (k, &(i, j, shown_whole, shown)) in cells.iter().enumerate() {
            let at = Matrix::translate(i as f64 * xs, j as f64 * ys);
            let copy = if shown_whole {
                FormResult::Unchanged
            } else {
                self.regions = regions.clone();
                self.regions.extend(outside(&shown));
                self.redact_form(&form, num, &at.then(&m), parent)
            };
            let r = match copy {
                FormResult::Drop => continue,
                FormResult::Unchanged => *whole.get_or_insert_with(|| {
                    let mut d = form.dict.clone();
                    d.set(b"Filter", Object::name(b"FlateDecode"));
                    let z = pagebara_filters::encode_flate(&raw, 6);
                    self.doc.add(Stream::new(d, z))
                }),
                FormResult::New(s) => self.doc.add(s),
            };
            let n = format!("C{k}");
            xobjects.set(n.as_bytes(), Object::Ref(r));
            content.extend_from_slice(b"q ");
            crate::emit::nums(&mut content, &[1.0, 0.0, 0.0, 1.0, at.e, at.f]);
            content.extend_from_slice(format!(" cm /{n} Do Q\n").as_bytes());
        }
        self.regions = regions;

        // One cell over all the painted areas, repeating far away.
        let cell = Rect::from_points(&area.corners().map(|p| inv.apply(p)));
        let cell = Rect::new(cell.x0 - 1.0, cell.y0 - 1.0, cell.x1 + 1.0, cell.y1 + 1.0);
        let mut d = Dict::new();
        d.set(b"Type", Object::name(b"Pattern"));
        d.set(b"PatternType", Object::Int(1));
        let paint = self
            .doc
            .get_in(&ps.dict, b"PaintType")
            .as_int()
            .unwrap_or(1);
        d.set(b"PaintType", Object::Int(paint));
        d.set(b"TilingType", Object::Int(1));
        d.set(b"BBox", cell.to_object());
        d.set(b"XStep", Object::Real((2.0 * cell.width() + 1.0).round()));
        d.set(b"YStep", Object::Real((2.0 * cell.height() + 1.0).round()));
        if let Some(pm) = ps.dict.get(b"Matrix") {
            d.set(b"Matrix", pm.clone());
        }
        let mut res = Dict::new();
        res.set(b"XObject", Object::Dict(xobjects));
        d.set(b"Resources", Object::Dict(res));
        d.set(b"Filter", Object::name(b"FlateDecode"));
        Tiling::New(Stream::new(d, pagebara_filters::encode_flate(&content, 6)))
    }
}
