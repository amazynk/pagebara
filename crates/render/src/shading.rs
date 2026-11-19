//! Shadings (§8.7.4.5): function-based, axial and radial evaluated per pixel through a
//! colour table; triangle meshes and patch meshes rasterised with Gouraud interpolation.

use std::sync::Arc;

use pagebara_content::display::Id;
use pagebara_content::{Matrix, Point, Rect};
use pagebara_syntax::{Dict, Object};

use crate::canvas::Pixmap;
use crate::color::Cs;
use crate::draw::{Ctx, Resolved, Shader, Source};
use crate::function::{Function, numbers};
use crate::list::Prepared;
use crate::raster::IRect;
use crate::{Inner, MiscKey};

const LUT: usize = 1024;

#[derive(Debug, Clone, Copy)]
struct Vertex {
    p: Point,
    /// sRGB, or `[t, 0, 0]` with a function.
    c: [f32; 3],
}

#[derive(Debug)]
enum Kind {
    Function {
        domain: [f32; 4],
        /// Shading space → function domain.
        inv: Matrix,
    },
    Axial {
        c: [f32; 4],
        extend: [bool; 2],
    },
    Radial {
        c: [f32; 6],
        extend: [bool; 2],
    },
    Mesh(Vec<[Vertex; 3]>),
    /// Tensor patches: points `p[i * 4 + j]` (u = i, v = j) and corner colours at
    /// (0,0), (0,1), (1,1), (1,0).
    Patches(Vec<([Point; 16], [[f32; 3]; 4])>),
}

#[derive(Debug)]
pub struct ShadingDef {
    kind: Kind,
    cs: Arc<Cs>,
    func: Option<Function>,
    /// Function input range covered by `lut`.
    t: [f32; 2],
    lut: Vec<[u8; 3]>,
    pub bbox: Option<Rect>,
    background: Option<[u8; 3]>,
}

impl ShadingDef {
    fn color(&self, comps: &[f32]) -> [f32; 3] {
        match &self.func {
            Some(f) => {
                let mut out = [0f32; 32];
                f.eval(comps, &mut out);
                self.cs.to_rgb(&out[..self.cs.components()])
            }
            None => self.cs.to_rgb(comps),
        }
    }

    #[inline]
    fn lut_at(&self, t: f32) -> [u8; 3] {
        let (t0, t1) = (self.t[0], self.t[1]);
        let f = if t1 != t0 { (t - t0) / (t1 - t0) } else { 0.0 };
        let i = (f.clamp(0.0, 1.0) * (LUT - 1) as f32 + 0.5) as usize;
        self.lut[i]
    }

    /// A vertex colour (or `t`) to sRGB bytes.
    fn vertex_rgb(&self, c: [f32; 3]) -> [u8; 3] {
        if self.func.is_some() {
            self.lut_at(c[0])
        } else {
            c.map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
        }
    }
}

/// The parsed shading of a shading object, cached by reference.
pub fn definition(r: &Inner, o: &Object) -> Option<Arc<ShadingDef>> {
    if let Some(rf) = o.as_objref() {
        let key = MiscKey::Shading(rf);
        if let Some(v) = r.misc.get(&key) {
            return v
                .downcast::<Option<Arc<ShadingDef>>>()
                .ok()
                .and_then(|v| (*v).clone());
        }
        let d = parse(r, o).map(Arc::new);
        let size = d.as_ref().map_or(64, |d| match &d.kind {
            Kind::Mesh(m) => m.len() * 64,
            Kind::Patches(p) => p.len() * 200,
            _ => 4096,
        });
        r.misc.insert(key, Arc::new(d.clone()), size);
        return d;
    }
    parse(r, o).map(Arc::new)
}

fn parse(r: &Inner, o: &Object) -> Option<ShadingDef> {
    let doc = &r.doc;
    let obj = doc.resolve(o);
    let (dict, stream): (&Dict, _) = match &*obj {
        Object::Dict(d) => (d, None),
        Object::Stream(s) => (&s.dict, Some(s)),
        _ => return None,
    };
    let ty = doc.get_in(dict, b"ShadingType").as_int()?;
    let cs = r.resolve_space(dict.get(b"ColorSpace")?)?;
    if matches!(*cs, Cs::Pattern { .. } | Cs::Indexed { .. }) && ty < 4 {
        // Indexed is only valid for meshes without functions; tolerate it.
        if matches!(*cs, Cs::Pattern { .. }) {
            return None;
        }
    }
    let func = dict.get(b"Function").and_then(|f| Function::parse(doc, f));
    let bbox = doc
        .get_in(dict, b"BBox")
        .as_array()
        .and_then(|_| Rect::from_object(&resolve_nums(r, dict, b"BBox")?));
    let background = numbers(doc, dict, b"Background")
        .filter(|b| b.len() == cs.components())
        .map(|b| cs.to_rgb(&b).map(|v| (v * 255.0 + 0.5) as u8));
    let extend = |d: &Dict| -> [bool; 2] {
        let e = doc.get_in(d, b"Extend");
        let a = e.as_array().unwrap_or_default();
        let b = |i: usize| {
            a.get(i)
                .and_then(|o| doc.resolve(o).as_bool())
                .unwrap_or(false)
        };
        [b(0), b(1)]
    };
    let domain = numbers(doc, dict, b"Domain");
    let mut t = [0.0, 1.0];
    let kind = match ty {
        1 => {
            let d = domain
                .filter(|d| d.len() == 4)
                .map_or([0.0, 1.0, 0.0, 1.0], |d| [d[0], d[1], d[2], d[3]]);
            let m = Matrix::from_object(&doc.get_in(dict, b"Matrix")).unwrap_or_default();
            Kind::Function {
                domain: d,
                inv: m.invert()?,
            }
        }
        2 | 3 => {
            if let Some(d) = domain.filter(|d| d.len() == 2) {
                t = [d[0], d[1]];
            }
            let c = numbers(doc, dict, b"Coords")?;
            if ty == 2 {
                if c.len() != 4 {
                    return None;
                }
                Kind::Axial {
                    c: [c[0], c[1], c[2], c[3]],
                    extend: extend(dict),
                }
            } else {
                if c.len() != 6 {
                    return None;
                }
                Kind::Radial {
                    c: [c[0], c[1], c[2], c[3], c[4], c[5]],
                    extend: extend(dict),
                }
            }
        }
        4..=7 => {
            let s = stream?;
            let data = doc.decode(s).ok()?.data;
            let int = |k: &[u8]| doc.get_in(dict, k).as_int().unwrap_or(0) as u32;
            let decode = numbers(doc, dict, b"Decode")?;
            let n = if func.is_some() { 1 } else { cs.components() };
            if decode.len() < 4 + 2 * n {
                return None;
            }
            if func.is_some() {
                t = [decode[4], decode[5]];
            }
            let rd = MeshReader {
                bits: Bits::new(&data),
                bpc: int(b"BitsPerCoordinate"),
                bpcomp: int(b"BitsPerComponent"),
                decode: &decode,
                n,
            };
            let conv = |c: &[f32]| -> [f32; 3] {
                if func.is_some() {
                    [c[0], 0.0, 0.0]
                } else {
                    cs.to_rgb(c)
                }
            };
            match ty {
                4 => Kind::Mesh(free_form(rd, int(b"BitsPerFlag"), &conv)),
                5 => Kind::Mesh(lattice(rd, int(b"VerticesPerRow"), &conv)),
                _ => Kind::Patches(patches(rd, int(b"BitsPerFlag"), ty == 7, &conv)),
            }
        }
        _ => return None,
    };
    let mut def = ShadingDef {
        kind,
        cs,
        func,
        t,
        lut: Vec::new(),
        bbox,
        background,
    };
    if def.func.is_some() && !matches!(def.kind, Kind::Function { .. }) {
        def.lut = (0..LUT)
            .map(|i| {
                let x = t[0] + (t[1] - t[0]) * i as f32 / (LUT - 1) as f32;
                def.color(&[x])
                    .map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
            })
            .collect();
    } else if matches!(def.kind, Kind::Axial { .. } | Kind::Radial { .. }) {
        return None;
    }
    Some(def)
}

fn resolve_nums(r: &Inner, d: &Dict, k: &[u8]) -> Option<Object> {
    let v = numbers(&r.doc, d, k)?;
    Some(Object::Array(
        v.into_iter().map(|x| Object::Real(f64::from(x))).collect(),
    ))
}

/// `sh`: paints the shading over the clip (and its `/BBox`).
pub fn draw_sh(ctx: &mut Ctx<'_>, p: &Arc<Prepared>, id: Id, m: Matrix, r: &Resolved) {
    let Some(o) = p.list.shadings.get(id as usize) else {
        return;
    };
    let Some(def) = definition(ctx.r, o) else {
        return;
    };
    let mut area = ctx.reach();
    if let Some(b) = def.bbox {
        area = area.intersect(&crate::draw::device_bounds(&b, &m));
    }
    if area.is_empty() {
        return;
    }
    let Some(s) = shader(&def, &m, area, false) else {
        return;
    };
    let res = Resolved {
        src: Source::Shader(s),
        alpha: r.alpha,
        blend: r.blend,
        smask: r.smask.clone(),
    };
    if let Some(b) = def.bbox {
        // The bounding box clips exactly.
        ctx.push_clip_rect(b, &m);
        ctx.fill_area(area, &res);
        ctx.pop_clip();
    } else {
        ctx.fill_area(area, &res);
    }
}

/// A shader for `def` placed by `m` (shading space → device), valid over `area`.
pub fn shader(
    def: &Arc<ShadingDef>,
    m: &Matrix,
    area: IRect,
    background: bool,
) -> Option<Arc<dyn Shader>> {
    let inv = m.invert()?;
    let bg = if background { def.background } else { None };
    Some(match &def.kind {
        Kind::Axial { .. } | Kind::Radial { .. } | Kind::Function { .. } => Arc::new(Analytic {
            def: def.clone(),
            inv,
            bg,
        }),
        Kind::Mesh(_) | Kind::Patches(_) => {
            let area = area.intersect(&IRect::new(-(1 << 15), -(1 << 15), 1 << 15, 1 << 15));
            if area.area() > 64 << 20 {
                return None;
            }
            let mut pix = match bg {
                Some(c) => Pixmap::filled(area, [c[0], c[1], c[2], 255]),
                None => Pixmap::new(area),
            };
            match &def.kind {
                Kind::Mesh(tris) => {
                    for t in tris {
                        triangle(
                            &mut pix,
                            def,
                            t.map(|v| Vertex {
                                p: m.apply(v.p),
                                ..v
                            }),
                        );
                    }
                }
                Kind::Patches(ps) => {
                    for (pts, cols) in ps {
                        patch(&mut pix, def, &pts.map(|q| m.apply(q)), cols);
                    }
                }
                _ => {}
            }
            Arc::new(PixShader(pix))
        }
    })
}

struct Analytic {
    def: Arc<ShadingDef>,
    inv: Matrix,
    bg: Option<[u8; 3]>,
}

impl Shader for Analytic {
    fn shade(&self, x: i32, y: i32, out: &mut [u8]) {
        let p0 = self.inv.apply(Point::new(x as f32 + 0.5, y as f32 + 0.5));
        let (dx, dy) = (self.inv.a, self.inv.b);
        let def = &*self.def;
        let none = match self.bg {
            Some(c) => [c[0], c[1], c[2], 255],
            None => [0; 4],
        };
        for (i, px) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let p = Point::new(p0.x + dx * i as f32, p0.y + dy * i as f32);
            *px = match &def.kind {
                Kind::Axial { c, extend } => {
                    let (vx, vy) = (c[2] - c[0], c[3] - c[1]);
                    let l2 = vx * vx + vy * vy;
                    // An axis of no length paints nothing (as in PDFium and MuPDF).
                    let s = if l2 > 0.0 {
                        Some(((p.x - c[0]) * vx + (p.y - c[1]) * vy) / l2)
                    } else {
                        None
                    };
                    match s.and_then(|s| param(s, *extend)) {
                        Some(s) => {
                            let t = def.t[0] + s * (def.t[1] - def.t[0]);
                            let c = def.lut_at(t);
                            [c[0], c[1], c[2], 255]
                        }
                        None => none,
                    }
                }
                Kind::Radial { c, extend } => match radial(c, p, *extend) {
                    Some(s) => {
                        let t = def.t[0] + s * (def.t[1] - def.t[0]);
                        let c = def.lut_at(t);
                        [c[0], c[1], c[2], 255]
                    }
                    None => none,
                },
                Kind::Function { domain, inv } => {
                    let q = inv.apply(p);
                    if q.x < domain[0] || q.x > domain[1] || q.y < domain[2] || q.y > domain[3] {
                        none
                    } else {
                        let c = def
                            .color(&[q.x, q.y])
                            .map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
                        [c[0], c[1], c[2], 255]
                    }
                }
                _ => none,
            };
        }
    }
}

fn param(s: f32, extend: [bool; 2]) -> Option<f32> {
    if !s.is_finite() {
        return None;
    }
    if s < 0.0 {
        extend[0].then_some(0.0)
    } else if s > 1.0 {
        extend[1].then_some(1.0)
    } else {
        Some(s)
    }
}

/// The largest `s` whose circle passes through `p` with a non-negative radius.
fn radial(c: &[f32; 6], p: Point, extend: [bool; 2]) -> Option<f32> {
    let (cdx, cdy, dr) = (c[3] - c[0], c[4] - c[1], c[5] - c[2]);
    let (pdx, pdy) = (p.x - c[0], p.y - c[1]);
    let a = cdx * cdx + cdy * cdy - dr * dr;
    let b = pdx * cdx + pdy * cdy + c[2] * dr;
    let cc = pdx * pdx + pdy * pdy - c[2] * c[2];
    let ok = |s: f32| {
        c[2] + s * dr >= 0.0
            && ((0.0..=1.0).contains(&s) || (s < 0.0 && extend[0]) || (s > 1.0 && extend[1]))
    };
    let pick = |s: f32| Some(s.clamp(0.0, 1.0));
    if a.abs() < 1e-6 * (cdx.abs() + cdy.abs() + dr.abs()).max(1e-6).powi(2) {
        if b == 0.0 {
            return None;
        }
        let s = cc / (2.0 * b);
        return if ok(s) { pick(s) } else { None };
    }
    let disc = b * b - a * cc;
    if disc < 0.0 {
        return None;
    }
    let sq = disc.sqrt();
    let (s1, s2) = ((b + sq) / a, (b - sq) / a);
    let (hi, lo) = if s1 > s2 { (s1, s2) } else { (s2, s1) };
    if ok(hi) {
        pick(hi)
    } else if ok(lo) {
        pick(lo)
    } else {
        None
    }
}

struct PixShader(Pixmap);

impl Shader for PixShader {
    fn shade(&self, x: i32, y: i32, out: &mut [u8]) {
        let r = self.0.rect;
        let n = out.len() / 4;
        if y < r.y0 || y >= r.y1 {
            out.fill(0);
            return;
        }
        let lo = (r.x0 - x).clamp(0, n as i32) as usize;
        let hi = (r.x1 - x).clamp(0, n as i32) as usize;
        out[..lo * 4].fill(0);
        out[hi * 4..].fill(0);
        if lo < hi {
            out[lo * 4..hi * 4].copy_from_slice(self.0.span(y, x + lo as i32, hi - lo));
        }
    }
}

/// Gouraud-shaded triangle (pixel centres inside), opaque.
fn triangle(pix: &mut Pixmap, def: &ShadingDef, v: [Vertex; 3]) {
    let r = pix.rect;
    let minx = v.iter().map(|v| v.p.x).fold(f32::MAX, f32::min);
    let maxx = v.iter().map(|v| v.p.x).fold(f32::MIN, f32::max);
    let miny = v.iter().map(|v| v.p.y).fold(f32::MAX, f32::min);
    let maxy = v.iter().map(|v| v.p.y).fold(f32::MIN, f32::max);
    if !(minx.is_finite() && maxx.is_finite() && miny.is_finite() && maxy.is_finite()) {
        return;
    }
    let b = IRect::new(
        (minx - 0.5).floor().max(r.x0 as f32) as i32,
        (miny - 0.5).floor().max(r.y0 as f32) as i32,
        ((maxx + 0.5).ceil().min(r.x1 as f32)) as i32,
        ((maxy + 0.5).ceil().min(r.y1 as f32)) as i32,
    );
    if b.is_empty() {
        return;
    }
    let (a, bb, c) = (v[0].p, v[1].p, v[2].p);
    let area = (bb.x - a.x) * (c.y - a.y) - (bb.y - a.y) * (c.x - a.x);
    if area.abs() < 1e-9 {
        return;
    }
    let inv = 1.0 / area;
    // Slightly generous (a hundredth of a pixel from each edge) so shared edges leave no
    // cracks, the same for triangles of any size.
    let len = |p: Point, q: Point| ((q.x - p.x).powi(2) + (q.y - p.y).powi(2)).sqrt();
    let eps = [len(bb, c), len(c, a), len(a, bb)].map(|l| -0.01 * l * inv.abs());
    for y in b.y0..b.y1 {
        let py = y as f32 + 0.5;
        for x in b.x0..b.x1 {
            let px = x as f32 + 0.5;
            let w0 = ((bb.x - px) * (c.y - py) - (bb.y - py) * (c.x - px)) * inv;
            let w1 = ((c.x - px) * (a.y - py) - (c.y - py) * (a.x - px)) * inv;
            let w2 = 1.0 - w0 - w1;
            if w0 < eps[0] || w1 < eps[1] || w2 < eps[2] {
                continue;
            }
            let col = [0, 1, 2].map(|k| w0 * v[0].c[k] + w1 * v[1].c[k] + w2 * v[2].c[k]);
            let rgb = def.vertex_rgb(col);
            let s = pix.span_mut(y, x, 1);
            s.copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
    }
}

fn bern(t: f32) -> [f32; 4] {
    let mt = 1.0 - t;
    [mt * mt * mt, 3.0 * mt * mt * t, 3.0 * mt * t * t, t * t * t]
}

fn patch(pix: &mut Pixmap, def: &ShadingDef, p: &[Point; 16], c: &[[f32; 3]; 4]) {
    // Subdivide by device size.
    let span = |a: Point, b: Point| ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt();
    let mut len = 0f32;
    for i in 0..4 {
        for j in 0..3 {
            len = len.max(span(p[i * 4 + j], p[i * 4 + j + 1]) * 3.0);
            len = len.max(span(p[j * 4 + i], p[(j + 1) * 4 + i]) * 3.0);
        }
    }
    let n = ((len / 3.0).ceil() as usize).clamp(2, 64);
    let mut grid = Vec::with_capacity((n + 1) * (n + 1));
    for a in 0..=n {
        let u = a as f32 / n as f32;
        let bu = bern(u);
        for b in 0..=n {
            let v = b as f32 / n as f32;
            let bv = bern(v);
            let mut q = Point::default();
            for i in 0..4 {
                for j in 0..4 {
                    let w = bu[i] * bv[j];
                    q.x += w * p[i * 4 + j].x;
                    q.y += w * p[i * 4 + j].y;
                }
            }
            // Corners: c[0] (0,0), c[1] (0,1), c[2] (1,1), c[3] (1,0).
            let col = [0, 1, 2].map(|k| {
                (1.0 - u) * ((1.0 - v) * c[0][k] + v * c[1][k])
                    + u * ((1.0 - v) * c[3][k] + v * c[2][k])
            });
            grid.push(Vertex { p: q, c: col });
        }
    }
    let at = |a: usize, b: usize| grid[a * (n + 1) + b];
    for a in 0..n {
        for b in 0..n {
            triangle(pix, def, [at(a, b), at(a + 1, b), at(a, b + 1)]);
            triangle(pix, def, [at(a + 1, b + 1), at(a + 1, b), at(a, b + 1)]);
        }
    }
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Bits { data, pos: 0 }
    }

    fn read(&mut self, n: u32) -> Option<u32> {
        if n == 0 || n > 32 || self.pos + n as usize > self.data.len() * 8 {
            return None;
        }
        let mut v = 0u64;
        for _ in 0..n {
            let byte = self.data[self.pos / 8];
            v = v << 1 | u64::from(byte >> (7 - self.pos % 8) & 1);
            self.pos += 1;
        }
        Some(v as u32)
    }

    fn align(&mut self) {
        self.pos = self.pos.next_multiple_of(8);
    }
}

struct MeshReader<'a> {
    bits: Bits<'a>,
    bpc: u32,
    bpcomp: u32,
    decode: &'a [f32],
    n: usize,
}

impl MeshReader<'_> {
    fn point(&mut self) -> Option<Point> {
        let max = ((1u64 << self.bpc) - 1) as f32;
        let d = self.decode;
        let x = self.bits.read(self.bpc)? as f32 / max;
        let y = self.bits.read(self.bpc)? as f32 / max;
        Some(Point::new(
            d[0] + x * (d[1] - d[0]),
            d[2] + y * (d[3] - d[2]),
        ))
    }

    fn color(&mut self) -> Option<Vec<f32>> {
        let max = ((1u64 << self.bpcomp.min(32)) - 1) as f32;
        let d = self.decode;
        (0..self.n)
            .map(|i| {
                let v = self.bits.read(self.bpcomp)? as f32 / max;
                Some(d[4 + 2 * i] + v * (d[5 + 2 * i] - d[4 + 2 * i]))
            })
            .collect()
    }
}

fn free_form(
    mut rd: MeshReader<'_>,
    bpf: u32,
    conv: &dyn Fn(&[f32]) -> [f32; 3],
) -> Vec<[Vertex; 3]> {
    let mut out = Vec::new();
    let read = |rd: &mut MeshReader<'_>| -> Option<(u32, Vertex)> {
        let f = rd.bits.read(bpf)?;
        let p = rd.point()?;
        let c = rd.color()?;
        rd.bits.align();
        Some((f, Vertex { p, c: conv(&c) }))
    };
    let (mut a, mut b, mut c): (Option<Vertex>, Option<Vertex>, Option<Vertex>) =
        (None, None, None);
    while let Some((f, v)) = read(&mut rd) {
        match f {
            0 => {
                let Some((_, v2)) = read(&mut rd) else { break };
                let Some((_, v3)) = read(&mut rd) else { break };
                (a, b, c) = (Some(v), Some(v2), Some(v3));
            }
            1 => {
                (a, b, c) = (b, c, Some(v));
            }
            _ => {
                (b, c) = (c, Some(v));
            }
        }
        if let (Some(a), Some(b), Some(c)) = (a, b, c) {
            out.push([a, b, c]);
        }
        if out.len() > 4_000_000 {
            break;
        }
    }
    out
}

fn lattice(
    mut rd: MeshReader<'_>,
    per_row: u32,
    conv: &dyn Fn(&[f32]) -> [f32; 3],
) -> Vec<[Vertex; 3]> {
    let per_row = per_row as usize;
    if per_row < 2 {
        return Vec::new();
    }
    let mut rows: Vec<Vec<Vertex>> = Vec::new();
    'outer: loop {
        let mut row = Vec::with_capacity(per_row);
        for _ in 0..per_row {
            let (Some(p), Some(c)) = (rd.point(), rd.color()) else {
                break 'outer;
            };
            rd.bits.align();
            row.push(Vertex { p, c: conv(&c) });
        }
        rows.push(row);
        if rows.len() * per_row > 4_000_000 {
            break;
        }
    }
    let mut out = Vec::new();
    for w in rows.windows(2) {
        for j in 0..per_row - 1 {
            out.push([w[0][j], w[1][j], w[0][j + 1]]);
            out.push([w[1][j + 1], w[1][j], w[0][j + 1]]);
        }
    }
    out
}

/// Stream order of the twelve boundary points as `(i, j)` of `p[i * 4 + j]`.
const BOUNDARY: [usize; 12] = [0, 1, 2, 3, 7, 11, 15, 14, 13, 12, 8, 4];
const INNER: [usize; 4] = [5, 6, 10, 9];

type Patch = ([Point; 16], [[f32; 3]; 4]);

fn patches(
    mut rd: MeshReader<'_>,
    bpf: u32,
    tensor: bool,
    conv: &dyn Fn(&[f32]) -> [f32; 3],
) -> Vec<Patch> {
    let mut out: Vec<Patch> = Vec::new();
    let mut prev: Option<([Point; 12], [[f32; 3]; 4])> = None;
    while let Some(f) = rd.bits.read(bpf) {
        let mut b = [Point::default(); 12];
        let mut c = [[0f32; 3]; 4];
        let start = match (f, &prev) {
            (0, _) => 0,
            (1..=3, Some((pb, pc))) => {
                let (pts, cols) = match f {
                    1 => ([pb[3], pb[4], pb[5], pb[6]], [pc[1], pc[2]]),
                    2 => ([pb[6], pb[7], pb[8], pb[9]], [pc[2], pc[3]]),
                    _ => ([pb[9], pb[10], pb[11], pb[0]], [pc[3], pc[0]]),
                };
                b[..4].copy_from_slice(&pts);
                c[0] = cols[0];
                c[1] = cols[1];
                4
            }
            _ => break,
        };
        let mut ok = true;
        for q in &mut b[start..] {
            match rd.point() {
                Some(p) => *q = p,
                None => ok = false,
            }
        }
        let mut inner = [Point::default(); 4];
        if tensor {
            for q in &mut inner {
                match rd.point() {
                    Some(p) => *q = p,
                    None => ok = false,
                }
            }
        }
        let first_color = if start == 0 { 0 } else { 2 };
        for cc in &mut c[first_color..] {
            match rd.color() {
                Some(v) => *cc = conv(&v),
                None => ok = false,
            }
        }
        if !ok {
            break;
        }
        let mut p = [Point::default(); 16];
        for (k, &idx) in BOUNDARY.iter().enumerate() {
            p[idx] = b[k];
        }
        if tensor {
            for (k, &idx) in INNER.iter().enumerate() {
                p[idx] = inner[k];
            }
        } else {
            coons_inner(&mut p);
        }
        out.push((p, c));
        prev = Some((b, c));
        if out.len() > 1_000_000 {
            break;
        }
    }
    out
}

fn coons_inner(p: &mut [Point; 16]) {
    let g = |i: usize, j: usize| p[i * 4 + j];
    let f = |w: [(f32, Point); 7]| {
        let mut q = Point::default();
        for (k, pt) in w {
            q.x += k * pt.x;
            q.y += k * pt.y;
        }
        Point::new(q.x / 9.0, q.y / 9.0)
    };
    let p11 = f([
        (-4.0, g(0, 0)),
        (6.0, g(0, 1)),
        (6.0, g(1, 0)),
        (-2.0, g(0, 3)),
        (-2.0, g(3, 0)),
        (3.0, g(3, 1)),
        (3.0, g(1, 3)),
    ]);
    let p11 = Point::new(p11.x - g(3, 3).x / 9.0, p11.y - g(3, 3).y / 9.0);
    let p12 = f([
        (-4.0, g(0, 3)),
        (6.0, g(0, 2)),
        (6.0, g(1, 3)),
        (-2.0, g(0, 0)),
        (-2.0, g(3, 3)),
        (3.0, g(3, 2)),
        (3.0, g(1, 0)),
    ]);
    let p12 = Point::new(p12.x - g(3, 0).x / 9.0, p12.y - g(3, 0).y / 9.0);
    let p22 = f([
        (-4.0, g(3, 3)),
        (6.0, g(3, 2)),
        (6.0, g(2, 3)),
        (-2.0, g(3, 0)),
        (-2.0, g(0, 3)),
        (3.0, g(2, 0)),
        (3.0, g(0, 2)),
    ]);
    let p22 = Point::new(p22.x - g(0, 0).x / 9.0, p22.y - g(0, 0).y / 9.0);
    let p21 = f([
        (-4.0, g(3, 0)),
        (6.0, g(3, 1)),
        (6.0, g(2, 0)),
        (-2.0, g(3, 3)),
        (-2.0, g(0, 0)),
        (3.0, g(2, 3)),
        (3.0, g(0, 1)),
    ]);
    let p21 = Point::new(p21.x - g(0, 3).x / 9.0, p21.y - g(0, 3).y / 9.0);
    p[5] = p11;
    p[6] = p12;
    p[10] = p22;
    p[9] = p21;
}
