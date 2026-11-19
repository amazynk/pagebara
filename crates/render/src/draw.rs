//! Replays display lists onto a stack of layers through a stack of clips.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pagebara_content::display::{BlendMode, Cmd, ColorSpace, FillRule, Id, NONE, Path, TextMode};
use pagebara_content::font::FontKind;
use pagebara_content::{Matrix, Point, Rect};

use crate::Inner;
use crate::canvas::{Pixmap, composite, composite_solid};
use crate::color::Cs;
use crate::list::{Prepared, group_blends, group_end};
use crate::pattern::Tiling;
use crate::raster::mask::mul8;
use crate::raster::stroke::Stroker;
use crate::raster::{Edges, Flattener, IRect, Mask, Rasterizer, Span};
use crate::text::{GlyphKey, quantize, subpixel};

/// Produces premultiplied RGBA for pixels `x..x + out.len() / 4` of row `y`.
pub trait Shader: Send + Sync {
    fn shade(&self, x: i32, y: i32, out: &mut [u8]);
}

#[derive(Clone)]
pub enum Source {
    /// Premultiplied, opacity included.
    Solid([u8; 4]),
    Shader(Arc<dyn Shader>),
    /// A tiling pattern, drawn cell by cell.
    Tiling(Arc<Tiling>),
    Nothing,
}

/// A paint ready to composite.
#[derive(Clone)]
pub struct Resolved {
    pub src: Source,
    /// Opacity for shaders (solid colours carry it).
    pub alpha: u8,
    pub blend: BlendMode,
    pub smask: Option<Arc<Mask>>,
}

impl Resolved {
    pub fn solid(rgb: [f32; 3], alpha: f32) -> Resolved {
        let a = alpha.clamp(0.0, 1.0);
        let c = [rgb[0] * a, rgb[1] * a, rgb[2] * a, a].map(|v| (v * 255.0 + 0.5) as u8);
        Resolved {
            src: Source::Solid(c),
            alpha: 255,
            blend: BlendMode::Normal,
            smask: None,
        }
    }

    fn paints(&self) -> bool {
        match self.src {
            Source::Nothing => false,
            Source::Solid(c) => c[3] != 0,
            Source::Shader(_) | Source::Tiling(_) => self.alpha != 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Clip {
    pub rect: IRect,
    pub mask: Option<Arc<Mask>>,
}

struct Layer {
    pix: Pixmap,
    alpha: u8,
    blend: BlendMode,
    smask: Option<Arc<Mask>>,
    /// Painting directly into the parent (no layer was needed).
    passthrough: bool,
    /// Non-isolated groups: the backdrop the group started from.
    backdrop: Option<Pixmap>,
    /// Non-isolated groups: the alpha of the group's own contents.
    own: Option<Mask>,
    knockout: bool,
}

impl Layer {
    fn plain(pix: Pixmap) -> Layer {
        Layer {
            pix,
            alpha: 255,
            blend: BlendMode::Normal,
            smask: None,
            passthrough: false,
            backdrop: None,
            own: None,
            knockout: false,
        }
    }
}

pub struct Ctx<'a> {
    pub r: &'a Inner,
    layers: Vec<Layer>,
    clips: Vec<Clip>,
    pub raster: Rasterizer,
    edges: Edges,
    cov: Vec<u8>,
    px: Vec<u8>,
    cancel: Option<&'a AtomicBool>,
    ops: u32,
    pub cancelled: bool,
    smasks: HashMap<(u64, Id, [u32; 6]), Option<Arc<Mask>>>,
    depth: u32,
    /// Tiling patterns being drawn, outermost first: a pattern that paints with itself
    /// draws nothing, and patterns inside patterns sample cached cells.
    pub patterns: Vec<Option<pagebara_syntax::ObjRef>>,
}

const MAX_DEPTH: u32 = 12;

impl<'a> Ctx<'a> {
    pub fn new(r: &'a Inner, target: Pixmap, cancel: Option<&'a AtomicBool>) -> Self {
        let rect = target.rect;
        Ctx {
            r,
            layers: vec![Layer::plain(target)],
            clips: vec![Clip { rect, mask: None }],
            raster: Rasterizer::new(),
            edges: Edges::new(),
            cov: Vec::new(),
            px: Vec::new(),
            cancel,
            ops: 0,
            cancelled: false,
            smasks: HashMap::new(),
            depth: 0,
            patterns: Vec::new(),
        }
    }

    pub fn finish(mut self) -> Pixmap {
        while self.layers.len() > 1 {
            self.end_group();
        }
        self.layers.pop().map(|l| l.pix).unwrap_or_default()
    }

    pub fn clip(&self) -> &Clip {
        self.clips.last().expect("clip stack")
    }

    /// The pixels painting can reach: the clip within the current layer.
    pub fn reach(&self) -> IRect {
        let l = self.target_rect();
        self.clip().rect.intersect(&l)
    }

    fn target_rect(&self) -> IRect {
        self.layers
            .iter()
            .rev()
            .find(|l| !l.passthrough)
            .map_or(IRect::EMPTY, |l| l.pix.rect)
    }

    fn target(&mut self) -> &mut Layer {
        self.layers
            .iter_mut()
            .rev()
            .find(|l| !l.passthrough)
            .expect("base layer")
    }

    fn check_cancel(&mut self) -> bool {
        self.ops += 1;
        if self.ops & 63 == 0
            && let Some(c) = self.cancel
            && c.load(Ordering::Relaxed)
        {
            self.cancelled = true;
        }
        self.cancelled
    }

    /// Runs a list with `base` mapping its space to device pixels. `forced` replaces the
    /// colour of everything painted (uncoloured Type 3 glyphs and tiling patterns).
    pub fn run(&mut self, p: &Arc<Prepared>, base: Matrix, forced: Option<&Resolved>) {
        if self.depth > MAX_DEPTH || !base.is_finite() {
            return;
        }
        self.depth += 1;
        let clip_floor = self.clips.len();
        let layer_floor = self.layers.len();
        let l = &p.list;
        let mut paints: Vec<Option<Resolved>> = vec![None; l.paints.len()];
        let mut i = 0;
        while i < l.cmds.len() {
            if self.check_cancel() {
                break;
            }
            let cmd = l.cmds[i];
            let visible = || {
                let b = p.bounds[i];
                if b.x0 == f32::MIN {
                    return true;
                }
                let d = device_bounds(&b, &base);
                !d.intersect(&self.reach()).is_empty()
            };
            match cmd {
                Cmd::Fill {
                    path,
                    ctm,
                    paint,
                    rule,
                } => {
                    if visible() {
                        let r = self.paint(p, &mut paints, paint, &base, forced);
                        if r.paints() {
                            let m = l.matrix(ctm).then(&base);
                            let mut e = std::mem::take(&mut self.edges);
                            e.clear();
                            Flattener::new(&mut e, m).path(&l.path(path));
                            // `B`, `b` and their even-odd forms paint fill and stroke as one
                            // object: with transparency the stroke knocks out the fill
                            // beneath it (ISO 32000-2, 11.7.4.4) instead of showing it.
                            let knockout = match l.cmds.get(i + 1) {
                                Some(&Cmd::Stroke {
                                    path: sp,
                                    ctm: sc,
                                    paint: spaint,
                                    style,
                                }) if sp == path && sc == ctm => {
                                    let sr = self.paint(p, &mut paints, spaint, &base, forced);
                                    let clear = |r: &Resolved| {
                                        r.alpha != 255
                                            || r.blend != BlendMode::Normal
                                            || r.smask.is_some()
                                            || matches!(r.src, Source::Solid(c) if c[3] != 255)
                                    };
                                    (sr.paints() && (clear(&r) || clear(&sr))).then_some(style)
                                }
                                _ => None,
                            };
                            match knockout {
                                Some(style) => {
                                    let s = &l.strokes[style as usize];
                                    let mut se = Edges::new();
                                    Stroker::stroke(&l.path(path), s, l.dash(s), m, &mut se);
                                    self.push_clip_outside(&se);
                                    self.fill(&e, rule, &r);
                                    self.pop_clip();
                                }
                                None => self.fill(&e, rule, &r),
                            }
                            self.edges = e;
                        }
                    }
                }
                Cmd::Stroke {
                    path,
                    ctm,
                    paint,
                    style,
                } => {
                    if visible() {
                        let r = self.paint(p, &mut paints, paint, &base, forced);
                        if r.paints() {
                            let m = l.matrix(ctm).then(&base);
                            let s = &l.strokes[style as usize];
                            let mut e = std::mem::take(&mut self.edges);
                            e.clear();
                            Stroker::stroke(&l.path(path), s, l.dash(s), m, &mut e);
                            self.fill(&e, FillRule::NonZero, &r);
                            self.edges = e;
                        }
                    }
                }
                Cmd::Clip { path, ctm, rule } => {
                    let m = l.matrix(ctm).then(&base);
                    let mut e = std::mem::take(&mut self.edges);
                    e.clear();
                    Flattener::new(&mut e, m).path(&l.path(path));
                    self.push_clip(&e, rule);
                    self.edges = e;
                }
                Cmd::ClipText { start, end } => {
                    let mut e = Edges::new();
                    for run in start..end {
                        let r = &l.runs[run as usize];
                        if r.mode.clips() {
                            self.text_outlines(p, run, &base, &mut e);
                        }
                    }
                    self.push_clip(&e, FillRule::NonZero);
                }
                Cmd::PopClip { count } => {
                    for _ in 0..count {
                        if self.clips.len() > clip_floor {
                            self.clips.pop();
                        }
                    }
                }
                Cmd::Text { run } => {
                    if visible() {
                        self.text(p, &mut paints, run, &base, forced);
                    }
                }
                Cmd::Image { image, ctm, paint } => {
                    if visible() {
                        let m = l.matrix(ctm).then(&base);
                        let mask = l.images[image as usize].is_mask;
                        let r = if mask || forced.is_none() {
                            self.paint(p, &mut paints, paint, &base, forced)
                        } else {
                            let mut r = self.paint(p, &mut paints, paint, &base, None);
                            r.src = Source::Nothing;
                            r
                        };
                        crate::image::draw(self, p, image, m, &r);
                    }
                }
                Cmd::Shading {
                    shading,
                    ctm,
                    paint,
                } => {
                    if forced.is_none() {
                        let r = self.paint(p, &mut paints, paint, &base, None);
                        let m = l.matrix(ctm).then(&base);
                        crate::shading::draw_sh(self, p, shading, m, &r);
                    }
                }
                Cmd::BeginGroup { group } => {
                    if !visible() {
                        i = group_end(l, i) + 1;
                        continue;
                    }
                    self.begin_group(p, &mut paints, group, i, &base, forced);
                }
                Cmd::EndGroup => {
                    if self.layers.len() > layer_floor {
                        self.end_group();
                    }
                }
                Cmd::BeginMarked { .. } | Cmd::EndMarked => {}
            }
            i += 1;
        }
        while self.layers.len() > layer_floor {
            self.end_group();
        }
        self.clips.truncate(clip_floor);
        self.depth -= 1;
    }

    /// The colour space of a paint, resolved once per list.
    pub fn space(&self, p: &Prepared, cs: ColorSpace) -> Arc<Cs> {
        match cs {
            ColorSpace::DeviceGray => self.r.gray.clone(),
            ColorSpace::DeviceRgb => self.r.rgb.clone(),
            ColorSpace::DeviceCmyk => self.r.cmyk.clone(),
            ColorSpace::Other(i) => {
                let Some(e) = p.list.color_spaces.get(i as usize) else {
                    return self.r.gray.clone();
                };
                let found = p.spaces[i as usize].get_or_init(|| self.r.resolve_space(&e.object));
                match found {
                    Some(c) => c.clone(),
                    None => match e.base {
                        Some(b) if b != cs => self.space(p, b),
                        _ => match e.components {
                            3 => self.r.rgb.clone(),
                            4 => self.r.cmyk.clone(),
                            _ => self.r.gray.clone(),
                        },
                    },
                }
            }
        }
    }

    fn paint(
        &mut self,
        p: &Arc<Prepared>,
        cache: &mut [Option<Resolved>],
        id: Id,
        base: &Matrix,
        forced: Option<&Resolved>,
    ) -> Resolved {
        if let Some(f) = forced {
            return f.clone();
        }
        if let Some(Some(r)) = cache.get(id as usize) {
            return r.clone();
        }
        let r = self.resolve(p, id, base);
        if let Some(slot) = cache.get_mut(id as usize) {
            *slot = Some(r.clone());
        }
        r
    }

    pub fn resolve(&mut self, p: &Arc<Prepared>, id: Id, base: &Matrix) -> Resolved {
        let l = &p.list;
        let Some(paint) = l.paints.get(id as usize) else {
            return Resolved::solid([0.0; 3], 1.0);
        };
        let smask = if paint.soft_mask != NONE {
            self.soft_mask(p, paint.soft_mask, base)
        } else {
            None
        };
        let cs = self.space(p, paint.space);
        let comps = l.paint_components(paint);
        let mut r = if paint.pattern != NONE {
            crate::pattern::source(self, p, paint.pattern, comps, &cs, base)
        } else if matches!(*cs, Cs::Pattern { .. }) {
            // A pattern space with no pattern selected paints nothing.
            Resolved {
                src: Source::Nothing,
                alpha: 255,
                blend: BlendMode::Normal,
                smask: None,
            }
        } else if cs.is_none() {
            Resolved {
                src: Source::Nothing,
                alpha: 255,
                blend: BlendMode::Normal,
                smask: None,
            }
        } else {
            Resolved::solid(cs.to_rgb(comps), 1.0)
        };
        r.blend = paint.blend;
        r.smask = smask;
        r.alpha = (paint.alpha * 255.0 + 0.5) as u8;
        if let Source::Solid(c) = &mut r.src {
            if r.alpha != 255 {
                let a = u16::from(r.alpha);
                *c = c.map(|v| ((u16::from(v) * a + 127) / 255) as u8);
            }
            r.alpha = 255;
        }
        r
    }

    // Painting

    /// Fills edges with a paint through the clip.
    pub fn fill(&mut self, e: &Edges, rule: FillRule, r: &Resolved) {
        let bounds = self.reach();
        if bounds.is_empty() || !r.paints() {
            return;
        }
        if let Source::Tiling(t) = &r.src {
            let t = t.clone();
            return self.fill_tiling(e, rule, r, &t);
        }
        let clip = self.clips.last().cloned().expect("clip");
        let mut raster = std::mem::take(&mut self.raster);
        let mut cov = std::mem::take(&mut self.cov);
        let mut px = std::mem::take(&mut self.px);
        let target = self.target();
        let mut run = Vec::new();
        raster.fill(e, rule, bounds, &mut |y, x, s| match s {
            Span::Cov(c) => span(target, &clip, r, y, x, c, &mut cov, &mut px),
            Span::Run(n, v) => {
                if !fill_run(target, &clip, r, y, x, n as usize, v) {
                    run.clear();
                    run.resize(n as usize, v);
                    span(target, &clip, r, y, x, &run, &mut cov, &mut px);
                }
            }
        });
        self.raster = raster;
        self.cov = cov;
        self.px = px;
    }

    /// Paints a coverage mask positioned at `(dx, dy)`.
    pub fn fill_mask(&mut self, m: &Mask, dx: i32, dy: i32, r: &Resolved) {
        let rect = IRect::new(
            m.rect.x0 + dx,
            m.rect.y0 + dy,
            m.rect.x1 + dx,
            m.rect.y1 + dy,
        );
        let b = self.reach().intersect(&rect);
        if b.is_empty() || !r.paints() {
            return;
        }
        if let Source::Tiling(t) = &r.src {
            let r = self.untile(r, &t.clone());
            return self.fill_mask(m, dx, dy, &r);
        }
        let clip = self.clips.last().cloned().expect("clip");
        let mut cov = std::mem::take(&mut self.cov);
        let mut px = std::mem::take(&mut self.px);
        let target = self.target();
        for y in b.y0..b.y1 {
            let row = m.row(y - dy);
            let s = &row[(b.x0 - rect.x0) as usize..(b.x1 - rect.x0) as usize];
            span(target, &clip, r, y, b.x0, s, &mut cov, &mut px);
        }
        self.cov = cov;
        self.px = px;
    }

    /// Paints `r` over every pixel the clip allows inside `area`.
    pub fn fill_area(&mut self, area: IRect, r: &Resolved) {
        let b = self.reach().intersect(&area);
        if b.is_empty() || !r.paints() {
            return;
        }
        if let Source::Tiling(t) = &r.src {
            let r = self.untile(r, &t.clone());
            return self.fill_area(area, &r);
        }
        let row = vec![255u8; b.width() as usize];
        let clip = self.clips.last().cloned().expect("clip");
        let mut cov = std::mem::take(&mut self.cov);
        let mut px = std::mem::take(&mut self.px);
        let target = self.target();
        for y in b.y0..b.y1 {
            span(target, &clip, r, y, b.x0, &row, &mut cov, &mut px);
        }
        self.cov = cov;
        self.px = px;
    }

    /// A tiling pattern paint as a shader over its cached cell.
    pub fn untile(&mut self, r: &Resolved, t: &Arc<Tiling>) -> Resolved {
        Resolved {
            src: Source::Shader(crate::pattern::cell_shader(self, t)),
            ..r.clone()
        }
    }

    /// Fills with a tiling pattern by drawing every cell that meets the fill through the
    /// fill as a clip; very many cells fall back to sampling a cached cell.
    fn fill_tiling(&mut self, e: &Edges, rule: FillRule, r: &Resolved, t: &Arc<Tiling>) {
        let area = e.pixel_bounds().intersect(&self.reach());
        if area.is_empty() || t.recursive(&self.patterns) {
            return;
        }
        // Cell by cell only at the outermost level: nested patterns multiply the cells.
        let cells = if self.patterns.is_empty() {
            t.cells(area)
        } else {
            None
        };
        let Some((is, js)) = cells else {
            let r = self.untile(r, t);
            return self.fill(e, rule, &r);
        };
        self.patterns.push(t.obj);
        self.push_clip(e, rule);
        let layered = r.alpha != 255 || r.blend != BlendMode::Normal || r.smask.is_some();
        if layered {
            let rect = self.reach();
            self.layers.push(Layer {
                alpha: r.alpha,
                blend: r.blend,
                smask: r.smask.clone(),
                ..Layer::plain(Pixmap::new(rect))
            });
        }
        'outer: for j in js {
            for i in is.clone() {
                let base = Matrix::translate(i as f32 * t.xs, j as f32 * t.ys).then(&t.m);
                self.push_clip_rect(t.bbox, &base);
                if !self.clip().rect.is_empty() {
                    self.run(&t.list, base, t.forced.as_ref());
                }
                self.pop_clip();
                if self.cancelled {
                    break 'outer;
                }
            }
        }
        if layered {
            self.end_group();
        }
        self.pop_clip();
        self.patterns.pop();
    }

    pub fn push_clip(&mut self, e: &Edges, rule: FillRule) {
        let cur = self.clip().clone();
        if e.is_empty() {
            self.clips.push(Clip {
                rect: IRect::EMPTY,
                mask: None,
            });
            return;
        }
        // Axis-aligned rectangles clip to the pixels they touch, without a mask.
        if let Some(b) = e.as_rect()
            && e.lines.iter().all(|l| l.x0 == l.x1)
        {
            let b = [b[0] + 1e-3, b[1] + 1e-3, b[2] - 1e-3, b[3] - 1e-3];
            let r = IRect::covering(b).intersect(&cur.rect);
            self.clips.push(Clip {
                rect: r,
                mask: cur.mask.clone(),
            });
            return;
        }
        let m = Mask::from_fill(&mut self.raster, e, rule, cur.rect);
        let m = match &cur.mask {
            Some(c) => c.intersect(&m),
            None => m,
        };
        let mut rect = m.rect;
        // Shrink to the rows and columns that are not empty.
        if m.data.iter().all(|&v| v == 0) {
            rect = IRect::EMPTY;
        }
        self.clips.push(Clip {
            rect,
            mask: Some(Arc::new(m)),
        });
    }

    /// Intersects the clip with what lies outside `e` (filled with the nonzero rule).
    fn push_clip_outside(&mut self, e: &Edges) {
        let cur = self.clip().clone();
        let inside = Mask::from_fill(&mut self.raster, e, FillRule::NonZero, cur.rect);
        let mut m = Mask::filled(cur.rect, 255);
        let r = inside.rect;
        for y in r.y0..r.y1 {
            let src = inside.row(y);
            let dst = &mut m.row_mut(y)[(r.x0 - cur.rect.x0) as usize..][..src.len()];
            for (d, s) in dst.iter_mut().zip(src) {
                *d = 255 - s;
            }
        }
        let m = match &cur.mask {
            Some(c) => c.intersect(&m),
            None => m,
        };
        self.clips.push(Clip {
            rect: cur.rect,
            mask: Some(Arc::new(m)),
        });
    }

    /// Pushes an explicit clip (used by annotations and pattern cells).
    pub fn push_clip_rect(&mut self, r: Rect, m: &Matrix) {
        let mut e = Edges::new();
        let mut f = Flattener::new(&mut e, *m);
        let c = r.corners();
        f.move_to(c[0]);
        f.line_to(c[1]);
        f.line_to(c[2]);
        f.line_to(c[3]);
        f.close();
        self.push_clip(&e, FillRule::NonZero);
    }

    pub fn pop_clip(&mut self) {
        if self.clips.len() > 1 {
            self.clips.pop();
        }
    }

    // Groups

    fn begin_group(
        &mut self,
        p: &Arc<Prepared>,
        paints: &mut [Option<Resolved>],
        id: Id,
        at: usize,
        base: &Matrix,
        forced: Option<&Resolved>,
    ) {
        let l = &p.list;
        let g = &l.groups[id as usize];
        let r = self.paint(p, paints, g.paint, base, None);
        let alpha = l.paints.get(g.paint as usize).map_or(1.0, |p| p.alpha);
        let needs = forced.is_none()
            && (alpha < 1.0
                || r.blend != BlendMode::Normal
                || r.smask.is_some()
                || g.knockout
                || (g.isolated && group_blends(l, at)));
        let mut rect = self.reach();
        if g.bbox.width() > 0.0 && g.bbox.height() > 0.0 {
            let m = l.matrix(g.ctm).then(base);
            rect = rect.intersect(&device_bounds(&g.bbox, &m));
        }
        if !needs {
            self.layers.push(Layer {
                passthrough: true,
                ..Layer::plain(Pixmap::default())
            });
            return;
        }
        let (pix, backdrop, own) = if g.isolated {
            (Pixmap::new(rect), None, None)
        } else {
            let b = self.target().pix.crop(rect);
            (b.clone(), Some(b), Some(Mask::new(rect)))
        };
        self.layers.push(Layer {
            pix,
            alpha: (alpha * 255.0 + 0.5) as u8,
            blend: r.blend,
            smask: r.smask,
            passthrough: false,
            backdrop,
            own,
            knockout: g.knockout,
        });
    }

    fn end_group(&mut self) {
        let Some(mut layer) = self.layers.pop() else {
            return;
        };
        if layer.passthrough {
            return;
        }
        if let (Some(b0), Some(own)) = (&layer.backdrop, &layer.own) {
            uncomposite(&mut layer.pix, b0, own);
        }
        let rect = layer.pix.rect;
        let src = Resolved {
            src: Source::Shader(Arc::new(PixmapShader(Arc::new(layer.pix)))),
            alpha: layer.alpha,
            blend: layer.blend,
            smask: layer.smask,
        };
        let open = Clip {
            rect: IRect::new(i32::MIN / 2, i32::MIN / 2, i32::MAX / 2, i32::MAX / 2),
            mask: None,
        };
        let mut cov = std::mem::take(&mut self.cov);
        let mut px = std::mem::take(&mut self.px);
        let target = self.target();
        let b = target.pix.rect.intersect(&rect);
        let row = vec![255u8; b.width() as usize];
        for y in b.y0..b.y1 {
            span(target, &open, &src, y, b.x0, &row, &mut cov, &mut px);
        }
        self.cov = cov;
        self.px = px;
    }

    // Soft masks

    fn soft_mask(&mut self, p: &Arc<Prepared>, id: Id, base: &Matrix) -> Option<Arc<Mask>> {
        let key = (p.id, id, base_bits(base));
        if let Some(m) = self.smasks.get(&key) {
            return m.clone();
        }
        let rect = self.target_rect();
        let m = crate::smask::render(self, p, id, base, rect).map(Arc::new);
        self.smasks.insert(key, m.clone());
        m
    }

    /// Renders a list into a fresh pixmap over `rect` (soft masks, pattern cells).
    pub fn offscreen(
        &mut self,
        list: &Arc<Prepared>,
        base: Matrix,
        init: Pixmap,
        clip: Option<(Rect, Matrix)>,
        forced: Option<&Resolved>,
    ) -> Pixmap {
        let mut c = Ctx::new(self.r, init, self.cancel);
        c.depth = self.depth + 1;
        c.patterns.clone_from(&self.patterns);
        if let Some((r, m)) = clip {
            c.push_clip_rect(r, &m);
        }
        c.run(list, base, forced);
        if c.cancelled {
            self.cancelled = true;
        }
        c.finish()
    }

    // Text

    fn text(
        &mut self,
        p: &Arc<Prepared>,
        paints: &mut [Option<Resolved>],
        run_id: Id,
        base: &Matrix,
        forced: Option<&Resolved>,
    ) {
        let l = &p.list;
        let run = &l.runs[run_id as usize];
        let Some(fr) = l.fonts.get(run.font as usize) else {
            return;
        };
        if run.mode == TextMode::Invisible || run.mode == TextMode::Clip {
            return;
        }
        let fill = (run.mode.fills() && run.fill != NONE)
            .then(|| self.paint(p, paints, run.fill, base, forced));
        if fr.font.kind == FontKind::Type3 {
            if let Some(f) = fill {
                self.type3(p, run_id, base, &f);
            }
            return;
        }
        let stroke = (run.mode.strokes() && run.stroke != NONE)
            .then(|| self.paint(p, paints, run.stroke, base, forced));
        let doc = &self.r.doc;
        let key = crate::text::FontKey::of(fr);
        let ctm = l.matrix(run.ctm);
        let ctm_inv = ctm.invert();
        for g in l.run_glyphs(run) {
            if self.check_cancel() {
                return;
            }
            let gm = run.glyph_matrix(g);
            let dm = gm.then(base);
            if let Some(f) = &fill {
                self.glyph(fr, key, g.code, &dm, f);
            }
            if let (Some(s), Some(inv)) = (&stroke, ctm_inv) {
                let Some(outline) = self.r.fonts.outline(doc, fr, g.code) else {
                    continue;
                };
                let style = &l.strokes[run.style as usize];
                let to_user = gm.then(&inv);
                let pts: Vec<Point> = outline.points.iter().map(|q| to_user.apply(*q)).collect();
                let path = Path {
                    verbs: &outline.verbs,
                    points: &pts,
                };
                let mut e = Edges::new();
                Stroker::stroke(&path, style, l.dash(style), ctm.then(base), &mut e);
                self.fill(&e, FillRule::NonZero, s);
            }
        }
    }

    /// Fills a glyph through the glyph cache; large glyphs are filled as paths.
    fn glyph(
        &mut self,
        fr: &pagebara_content::display::FontRef,
        font: crate::text::FontKey,
        code: u32,
        dm: &Matrix,
        r: &Resolved,
    ) {
        let em = dm.determinant().abs().sqrt();
        // Glyphs (with generous room for marks) far outside the reach are skipped.
        let reach = self.reach();
        let pad = em * 2.0 + 2.0;
        if dm.e + pad < reach.x0 as f32
            || dm.e - pad > reach.x1 as f32
            || dm.f + pad < reach.y0 as f32
            || dm.f - pad > reach.y1 as f32
        {
            return;
        }
        if em > 192.0 || !em.is_finite() {
            let Some(outline) = self.r.fonts.outline(&self.r.doc, fr, code) else {
                return;
            };
            let mut e = Edges::new();
            Flattener::new(&mut e, *dm).path(&outline.path());
            self.fill(&e, FillRule::NonZero, r);
            return;
        }
        let horizontal = dm.a.abs() >= dm.b.abs();
        let (ix, qx) = subpixel(dm.e, em, horizontal);
        let (iy, qy) = subpixel(dm.f, em, !horizontal);
        let k = GlyphKey {
            font,
            code,
            m: quantize(dm),
            sub: (qx, qy),
        };
        let mask = match self.r.glyphs.get(&k) {
            Some(m) => m,
            None => {
                let Some(outline) = self.r.fonts.outline(&self.r.doc, fr, code) else {
                    self.r.glyphs.insert(k, None);
                    return;
                };
                let local = Matrix {
                    e: f32::from(qx) / 256.0,
                    f: f32::from(qy) / 256.0,
                    ..*dm
                };
                let mut e = Edges::new();
                Flattener::new(&mut e, local).path(&outline.path());
                let m = Mask::from_fill(
                    &mut self.raster,
                    &e,
                    FillRule::NonZero,
                    IRect::new(-4096, -4096, 4096, 4096),
                );
                let m = (!m.rect.is_empty()).then(|| Arc::new(m));
                self.r.glyphs.insert(k, m.clone());
                m
            }
        };
        if let Some(m) = mask {
            self.fill_mask(&m, ix, iy, r);
        }
    }

    fn text_outlines(&mut self, p: &Arc<Prepared>, run_id: Id, base: &Matrix, e: &mut Edges) {
        let l = &p.list;
        let run = &l.runs[run_id as usize];
        let Some(fr) = l.fonts.get(run.font as usize) else {
            return;
        };
        if fr.font.kind == FontKind::Type3 {
            return;
        }
        for g in l.run_glyphs(run) {
            if let Some(o) = self.r.fonts.outline(&self.r.doc, fr, g.code) {
                let dm = run.glyph_matrix(g).then(base);
                Flattener::new(e, dm).path(&o.path());
            }
        }
    }

    fn type3(&mut self, p: &Arc<Prepared>, run_id: Id, base: &Matrix, fill: &Resolved) {
        let l = &p.list;
        let run = &l.runs[run_id as usize];
        let fr = &l.fonts[run.font as usize];
        let fm = fr.font.font_matrix;
        for g in l.run_glyphs(run) {
            let Some(glyph) = self.r.type3(fr, g.code) else {
                continue;
            };
            let gb = fm.then(&run.glyph_matrix(g)).then(base);
            let uncolored = glyph.list.glyph_metrics.is_some_and(|m| m.bbox.is_some());
            let forced = uncolored.then_some(fill);
            self.run(&glyph, gb, forced);
            if self.cancelled {
                return;
            }
        }
    }
}

/// Composites one span: coverage × clip × soft mask, then the source.
#[allow(clippy::too_many_arguments)]
#[inline]
fn span(
    layer: &mut Layer,
    clip: &Clip,
    r: &Resolved,
    y: i32,
    x: i32,
    c: &[u8],
    cov: &mut Vec<u8>,
    px: &mut Vec<u8>,
) {
    // Keep to the target layer.
    let t = layer.pix.rect;
    if y < t.y0 || y >= t.y1 {
        return;
    }
    let x0 = x.max(t.x0);
    let x1 = (x + c.len() as i32).min(t.x1);
    if x0 >= x1 {
        return;
    }
    let c = &c[(x0 - x) as usize..(x1 - x) as usize];
    let n = c.len();
    if layer.knockout || layer.own.is_some() {
        return span_group(layer, clip, r, y, x0, c, cov, px);
    }
    let simple = clip.mask.is_none() && r.smask.is_none() && r.alpha == 255;
    let cv: &[u8] = if simple {
        c
    } else {
        cov.clear();
        cov.extend_from_slice(c);
        if let Some(m) = &clip.mask {
            m.apply(y, x0, cov);
        }
        if let Some(m) = &r.smask {
            m.apply(y, x0, cov);
        }
        if r.alpha != 255 {
            for v in cov.iter_mut() {
                *v = crate::raster::mask::mul8(*v, r.alpha);
            }
        }
        cov
    };
    let dst = layer.pix.span_mut(y, x0, n);
    match &r.src {
        Source::Solid(col) => composite_solid(dst, *col, cv, r.blend),
        Source::Shader(s) => {
            px.resize(n * 4, 0);
            s.shade(x0, y, &mut px[..n * 4]);
            composite(dst, &px[..n * 4], cv, r.blend);
        }
        Source::Nothing | Source::Tiling(_) => {}
    }
}

/// An opaque solid run over plain pixels: a straight fill. False when not applicable.
fn fill_run(layer: &mut Layer, clip: &Clip, r: &Resolved, y: i32, x: i32, n: usize, v: u8) -> bool {
    let Source::Solid(c) = r.src else {
        return false;
    };
    if v != 255
        || c[3] != 255
        || r.blend != BlendMode::Normal
        || clip.mask.is_some()
        || r.smask.is_some()
        || r.alpha != 255
        || layer.knockout
        || layer.own.is_some()
    {
        return false;
    }
    let t = layer.pix.rect;
    if y < t.y0 || y >= t.y1 {
        return true;
    }
    let x0 = x.max(t.x0);
    let x1 = (x + n as i32).min(t.x1);
    if x0 < x1 {
        for p in layer
            .pix
            .span_mut(y, x0, (x1 - x0) as usize)
            .as_chunks_mut::<4>()
            .0
        {
            *p = c;
        }
    }
    true
}

/// A span into a knockout or non-isolated group: shape and opacity apart, the group's
/// own alpha tracked, knockout against the group's initial backdrop.
#[allow(clippy::too_many_arguments)]
fn span_group(
    layer: &mut Layer,
    clip: &Clip,
    r: &Resolved,
    y: i32,
    x0: i32,
    c: &[u8],
    cov: &mut Vec<u8>,
    px: &mut Vec<u8>,
) {
    let n = c.len();
    cov.clear();
    cov.extend_from_slice(c);
    if let Some(m) = &clip.mask {
        m.apply(y, x0, cov);
    }
    let mut op = vec![r.alpha; n];
    if let Some(m) = &r.smask {
        m.apply(y, x0, &mut op);
    }
    px.resize(n * 4, 0);
    match &r.src {
        Source::Solid(col) => {
            for p in px[..n * 4].as_chunks_mut::<4>().0 {
                *p = *col;
            }
        }
        Source::Shader(s) => s.shade(x0, y, &mut px[..n * 4]),
        Source::Nothing | Source::Tiling(_) => return,
    }
    for i in 0..n {
        let shape = cov[i];
        if shape == 0 {
            continue;
        }
        let x = x0 + i as i32;
        let s: [u8; 4] = std::array::from_fn(|k| mul8(px[i * 4 + k], op[i]));
        let dst = layer.pix.span_mut(y, x, 1);
        if layer.knockout {
            let mut res = layer.backdrop.as_ref().map_or([0; 4], |b| b.get(x, y));
            composite(&mut res, &s, &[255], r.blend);
            let f = u32::from(shape);
            for k in 0..4 {
                let d = u32::from(dst[k]);
                dst[k] = ((d * (255 - f) + u32::from(res[k]) * f + 127) / 255) as u8;
            }
        } else {
            composite(dst, &s, &[shape], r.blend);
        }
        if let Some(own) = &mut layer.own {
            let a = u32::from(mul8(s[3], shape));
            let ox = own.rect.x0;
            let o = &mut own.row_mut(y)[(x - ox) as usize];
            let v = u32::from(*o);
            *o = (v + a - (v * a + 127) / 255).min(255) as u8;
        }
    }
}

/// Recovers a non-isolated group's own colour from the result composited over its
/// backdrop (§11.4.8): `C = Cn + (Cn − C0) × (α0 / αg − α0)`, premultiplied by `αg`.
fn uncomposite(pix: &mut Pixmap, b0: &Pixmap, own: &Mask) {
    let rect = pix.rect;
    let w = rect.width() as usize;
    for y in rect.y0..rect.y1 {
        let back = b0.span(y, rect.x0, w);
        let g_row = own.row(y);
        let row = pix.span_mut(y, rect.x0, w);
        for ((p, b), &g) in row
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(back.as_chunks::<4>().0)
            .zip(g_row)
        {
            if g == 0 {
                *p = [0; 4];
                continue;
            }
            let ga = f32::from(g) / 255.0;
            let an = f32::from(p[3]) / 255.0;
            let a0 = f32::from(b[3]) / 255.0;
            for k in 0..3 {
                let cn = if an > 0.0 {
                    f32::from(p[k]) / 255.0 / an
                } else {
                    0.0
                };
                let c0 = if a0 > 0.0 {
                    f32::from(b[k]) / 255.0 / a0
                } else {
                    0.0
                };
                let c = (cn + (cn - c0) * (a0 / ga - a0)).clamp(0.0, 1.0);
                p[k] = (c * ga * 255.0 + 0.5) as u8;
            }
            p[3] = g;
        }
    }
}

/// Copies a pixmap's pixels (transparent outside it).
pub struct PixmapShader(pub Arc<Pixmap>);

impl Shader for PixmapShader {
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

/// Device pixel bounds of a list-space rectangle, with a margin for hairlines.
pub fn device_bounds(b: &Rect, m: &Matrix) -> IRect {
    let d = b.transform(m);
    IRect::covering([d.x0 - 2.0, d.y0 - 2.0, d.x1 + 2.0, d.y1 + 2.0])
}

pub fn base_bits(m: &Matrix) -> [u32; 6] {
    [m.a, m.b, m.c, m.d, m.e, m.f].map(f32::to_bits)
}
