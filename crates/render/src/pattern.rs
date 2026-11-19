//! Patterns (§8.7.3): tiling cells rendered once per pattern and transform, then sampled
//! with wrap-around; shading patterns through the shading shaders.

use std::ops::Range;
use std::sync::Arc;

use pagebara_content::display::{BlendMode, Id};
use pagebara_content::{Matrix, Point, Rect};
use pagebara_syntax::Object;

use crate::MiscKey;
use crate::canvas::Pixmap;
use crate::color::Cs;
use crate::draw::{Ctx, Resolved, Shader, Source};
use crate::function::numbers;
use crate::list::Prepared;
use crate::raster::IRect;
use crate::raster::stroke::expansion;
use crate::text::quantize;

/// Largest cell edge in pixels; bigger cells are rendered at reduced resolution.
const MAX_CELL: f32 = 2048.0;

fn nothing() -> Resolved {
    Resolved {
        src: Source::Nothing,
        alpha: 255,
        blend: BlendMode::Normal,
        smask: None,
    }
}

pub fn source(
    ctx: &mut Ctx<'_>,
    p: &Arc<Prepared>,
    id: Id,
    comps: &[f32],
    cs: &Arc<Cs>,
    base: &Matrix,
) -> Resolved {
    let Some(pr) = p.list.patterns.get(id as usize) else {
        return nothing();
    };
    let r = ctx.r;
    let doc = &r.doc;
    let o = doc.resolve(&pr.object);
    let dict = match &*o {
        Object::Dict(d) => d.clone(),
        Object::Stream(s) => s.dict.clone(),
        _ => return nothing(),
    };
    let m = pr.matrix.then(base);
    match doc.get_in(&dict, b"PatternType").as_int() {
        Some(2) => {
            let Some(sh) = dict.get(b"Shading") else {
                return nothing();
            };
            let Some(def) = crate::shading::definition(r, sh) else {
                return nothing();
            };
            let area = ctx.reach();
            match crate::shading::shader(&def, &m, area, true) {
                Some(s) => {
                    let mut res = Resolved {
                        src: Source::Shader(s),
                        alpha: 255,
                        blend: BlendMode::Normal,
                        smask: None,
                    };
                    if let Some(b) = def.bbox {
                        // Clip the shader to its box by sampling only inside.
                        res.src = Source::Shader(Arc::new(Boxed {
                            inner: match res.src {
                                Source::Shader(s) => s,
                                _ => unreachable!(),
                            },
                            inv: m.invert().unwrap_or_default(),
                            bbox: b,
                        }));
                    }
                    res
                }
                None => nothing(),
            }
        }
        Some(1) => tiling(ctx, &pr.object, &dict, &m, comps, cs),
        _ => nothing(),
    }
}

fn tiling(
    ctx: &mut Ctx<'_>,
    obj: &Object,
    dict: &pagebara_syntax::Dict,
    m: &Matrix,
    comps: &[f32],
    cs: &Arc<Cs>,
) -> Resolved {
    let r = ctx.r;
    let doc = &r.doc;
    let Some(bbox) = numbers(doc, dict, b"BBox")
        .filter(|b| b.len() == 4)
        .map(|b| Rect::new(b[0], b[1], b[2], b[3]))
    else {
        return nothing();
    };
    let num = |k: &[u8]| doc.get_in(dict, k).as_f64().map(|v| v as f32);
    let xs = num(b"XStep")
        .filter(|v| *v != 0.0 && v.is_finite())
        .unwrap_or(bbox.width());
    let ys = num(b"YStep")
        .filter(|v| *v != 0.0 && v.is_finite())
        .unwrap_or(bbox.height());
    let (xs, ys) = (xs.abs(), ys.abs());
    if xs <= 0.0 || ys <= 0.0 || expansion(m) == 0.0 || m.invert().is_none() {
        return nothing();
    }
    let uncolored = doc.get_in(dict, b"PaintType").as_int() == Some(2);
    let forced = if uncolored {
        let base = match &**cs {
            Cs::Pattern { base: Some(b) } => b.clone(),
            _ => return nothing(),
        };
        Some(Resolved::solid(base.to_rgb(comps), 1.0))
    } else {
        None
    };
    let Some(list) = r.pattern_list(obj) else {
        return nothing();
    };
    Resolved {
        src: Source::Tiling(Arc::new(Tiling {
            list,
            bbox,
            xs,
            ys,
            m: *m,
            forced,
            obj: obj.as_objref(),
        })),
        alpha: 255,
        blend: BlendMode::Normal,
        smask: None,
    }
}

/// Cells drawn one by one at most; beyond, a cached cell is sampled.
const MAX_CELLS: usize = 2048;

/// A tiling pattern placed in device space.
pub struct Tiling {
    pub list: Arc<Prepared>,
    pub bbox: Rect,
    pub xs: f32,
    pub ys: f32,
    /// Pattern space → device.
    pub m: Matrix,
    /// The colour of an uncoloured pattern.
    pub forced: Option<Resolved>,
    pub obj: Option<pagebara_syntax::ObjRef>,
}

impl Tiling {
    /// Whether this pattern is already being drawn (it paints with itself).
    pub fn recursive(&self, active: &[Option<pagebara_syntax::ObjRef>]) -> bool {
        self.obj.is_some() && active.contains(&self.obj)
    }

    /// The cell indices whose box meets device `area`, if few enough to draw one by one.
    pub fn cells(&self, area: IRect) -> Option<(Range<i32>, Range<i32>)> {
        let inv = self.m.invert()?;
        let a = Rect::new(
            area.x0 as f32,
            area.y0 as f32,
            area.x1 as f32,
            area.y1 as f32,
        )
        .transform(&inv);
        let b = self.bbox;
        let i0 = ((a.x0 - b.x1) / self.xs).floor();
        let i1 = ((a.x1 - b.x0) / self.xs).ceil();
        let j0 = ((a.y0 - b.y1) / self.ys).floor();
        let j1 = ((a.y1 - b.y0) / self.ys).ceil();
        let n = (i1 - i0 + 1.0) * (j1 - j0 + 1.0);
        if !(n.is_finite() && n <= MAX_CELLS as f32 && n >= 0.0) {
            return None;
        }
        Some((i0 as i32..i1 as i32 + 1, j0 as i32..j1 as i32 + 1))
    }
}

/// A shader sampling the pattern's cell, rendered once per transform and cached.
pub fn cell_shader(ctx: &mut Ctx<'_>, t: &Tiling) -> Arc<dyn Shader> {
    let r = ctx.r;
    let (m, xs, ys) = (t.m, t.xs, t.ys);
    // Cell resolution from the pattern matrix.
    let sx_dev = (m.a * m.a + m.b * m.b).sqrt();
    let sy_dev = (m.c * m.c + m.d * m.d).sqrt();
    let mut cw = (xs * sx_dev).ceil().max(1.0);
    let mut ch = (ys * sy_dev).ceil().max(1.0);
    if cw > MAX_CELL || ch > MAX_CELL {
        let k = MAX_CELL / cw.max(ch);
        cw = (cw * k).ceil().max(1.0);
        ch = (ch * k).ceil().max(1.0);
    }
    let inv = m.invert().unwrap_or_default();
    let (sx, sy) = (cw / xs, ch / ys);
    let color = match &t.forced {
        Some(Resolved {
            src: Source::Solid(c),
            ..
        }) => *c,
        _ => [0; 4],
    };
    let key = t.obj.map(|o| MiscKey::Cell(o, quantize(&m), color));
    let cached = key
        .as_ref()
        .and_then(|k| r.misc.get(k))
        .and_then(|v| v.downcast::<Pixmap>().ok());
    let cell = match cached {
        Some(c) => c,
        None if t.recursive(&ctx.patterns) => Arc::new(Pixmap::new(IRect::new(0, 0, 1, 1))),
        None => {
            let rect = IRect::new(0, 0, cw as i32, ch as i32);
            let mut pix = Pixmap::new(rect);
            ctx.patterns.push(t.obj);
            let to_cell = Matrix::scale(sx, sy);
            for ky in -1..=1 {
                for kx in -1..=1 {
                    let off = Matrix::translate(kx as f32 * xs, ky as f32 * ys).then(&to_cell);
                    // Skip copies that cannot reach the cell.
                    let b = t.bbox.transform(&off);
                    if b.x1 <= 0.0 || b.y1 <= 0.0 || b.x0 >= cw || b.y0 >= ch {
                        continue;
                    }
                    pix = ctx.offscreen(&t.list, off, pix, Some((t.bbox, off)), t.forced.as_ref());
                }
            }
            ctx.patterns.pop();
            let pix = Arc::new(pix);
            if let Some(k) = key {
                r.misc.insert(k, pix.clone(), pix.data.len() + 64);
            }
            pix
        }
    };
    Arc::new(Tile {
        cell,
        inv: inv.then(&Matrix::scale(sx, sy)),
    })
}

/// Samples a periodic cell (bilinear, wrapping).
struct Tile {
    cell: Arc<Pixmap>,
    /// Device → cell pixels.
    inv: Matrix,
}

impl Shader for Tile {
    fn shade(&self, x: i32, y: i32, out: &mut [u8]) {
        let c = &self.cell;
        let (w, h) = (c.rect.width(), c.rect.height());
        let p0 = self.inv.apply(Point::new(x as f32 + 0.5, y as f32 + 0.5));
        let (dx, dy) = (self.inv.a, self.inv.b);
        let axis_aligned = self.inv.b == 0.0 && self.inv.c == 0.0;
        let exact =
            axis_aligned && (self.inv.a - 1.0).abs() < 1e-4 && (self.inv.d - 1.0).abs() < 1e-4;
        for (i, px) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let u = p0.x + dx * i as f32 - 0.5;
            let v = p0.y + dy * i as f32 - 0.5;
            if exact {
                let xi = (u + 0.5).floor() as i32;
                let yi = (v + 0.5).floor() as i32;
                *px = c.get(xi.rem_euclid(w), yi.rem_euclid(h));
                continue;
            }
            let (fu, fv) = (u.floor(), v.floor());
            let (tx, ty) = (u - fu, v - fv);
            let x0 = (fu as i32).rem_euclid(w);
            let y0 = (fv as i32).rem_euclid(h);
            let x1 = (x0 + 1) % w;
            let y1 = (y0 + 1) % h;
            let a = c.get(x0, y0);
            let b = c.get(x1, y0);
            let cc = c.get(x0, y1);
            let d = c.get(x1, y1);
            for k in 0..4 {
                let top = a[k] as f32 + (b[k] as f32 - a[k] as f32) * tx;
                let bot = cc[k] as f32 + (d[k] as f32 - cc[k] as f32) * tx;
                px[k] = (top + (bot - top) * ty + 0.5) as u8;
            }
        }
    }
}

/// A shader limited to a box in its own space.
struct Boxed {
    inner: Arc<dyn Shader>,
    inv: Matrix,
    bbox: Rect,
}

impl Shader for Boxed {
    fn shade(&self, x: i32, y: i32, out: &mut [u8]) {
        self.inner.shade(x, y, out);
        let p0 = self.inv.apply(Point::new(x as f32 + 0.5, y as f32 + 0.5));
        for (i, px) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let p = Point::new(p0.x + self.inv.a * i as f32, p0.y + self.inv.b * i as f32);
            let b = &self.bbox;
            if p.x < b.x0 || p.x > b.x1 || p.y < b.y0 || p.y > b.y1 {
                *px = [0; 4];
            }
        }
    }
}
