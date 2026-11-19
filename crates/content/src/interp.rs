//! The interpreter: runs content streams against a graphics state and records a
//! [`DisplayList`].

use std::collections::HashMap;
use std::sync::Arc;

use pagebara_syntax::{Dict, Document, ObjRef, Object, Stream};

use crate::display::{
    Cmd, ColorSpace, DisplayList, FillRule, Id, LineCap, LineJoin, Marked, NONE, Paint, PathSpan,
    RenderingIntent, StrokeStyle, Verb,
};
use crate::font::{Font, ProgramLoader};

use crate::geom::{Matrix, Point};
use crate::lex::Operand;
use crate::oc::OptionalContent;
use crate::ops::Op;
use crate::parse::{ContentParser, Instr};
use crate::state::GState;

/// Limits that keep hostile content bounded.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Nested `q` beyond this are ignored (with their `Q`).
    pub max_save_depth: usize,
    /// Form XObjects nested deeper than this are skipped.
    pub max_form_depth: usize,
    /// Operators executed per display list, forms included.
    pub max_operators: u64,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            max_save_depth: 256,
            max_form_depth: 24,
            max_operators: 50_000_000,
        }
    }
}

/// Interprets pages, forms and Type 3 glyphs of one document. Keep it for many pages:
/// fonts are loaded once.
pub struct Interpreter<'d> {
    pub(crate) doc: &'d Document,
    pub(crate) fonts: HashMap<ObjRef, Arc<Font>>,
    pub(crate) oc: OptionalContent,
    pub(crate) programs: Option<Arc<ProgramLoader>>,
    pub options: Options,
}

impl std::fmt::Debug for Interpreter<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Interpreter")
            .field("fonts", &self.fonts.len())
            .field("font_programs", &self.programs.is_some())
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

impl<'d> Interpreter<'d> {
    /// An interpreter that reads embedded font programs with
    /// [`embedded_programs`](crate::font::embedded_programs).
    pub fn new(doc: &'d Document) -> Self {
        Interpreter {
            doc,
            fonts: HashMap::new(),
            oc: OptionalContent::new(doc),
            programs: Some(crate::font::embedded_programs()),
            options: Options::default(),
        }
    }

    /// Reads font programs with `loader` as fonts load, for their built-in encodings, glyph
    /// indices, widths and Unicode.
    pub fn with_font_programs(mut self, loader: Arc<ProgramLoader>) -> Self {
        self.programs = Some(loader);
        self
    }

    /// Decides optional content visibility with `oc` instead of the document's default
    /// configuration (a viewer's layer choices).
    pub fn with_optional_content(mut self, oc: OptionalContent) -> Self {
        self.oc = oc;
        self
    }

    /// Uses the font dictionaries alone, without reading font programs.
    pub fn without_font_programs(mut self) -> Self {
        self.programs = None;
        self
    }

    /// A page's `/Contents` in default user space, given its (inherited) resources.
    pub fn page(&mut self, page: &Dict, resources: &Dict) -> DisplayList {
        let data = page_contents(self.doc, page);
        self.content(&data, resources, Matrix::IDENTITY)
    }

    /// Any content stream's decoded bytes, starting from `ctm`.
    pub fn content(&mut self, src: &[u8], resources: &Dict, ctm: Matrix) -> DisplayList {
        let mut b = Builder::new(self, ctm);
        b.run(src, resources, ctm);
        b.finish()
    }

    /// A form XObject on its own (annotation appearances, soft mask groups, tiling pattern
    /// cells): its matrix, bounding box clip and group apply. `resources` is used when the
    /// form has none.
    pub fn form(&mut self, form: &Stream, resources: &Dict, ctm: Matrix) -> DisplayList {
        let mut b = Builder::new(self, ctm);
        b.form(None, form, resources);
        b.finish()
    }

    /// The glyph description of a code of a Type 3 font in glyph space, with its `d0`/`d1`
    /// metrics. `resources` is used when the font has none. `None` if the glyph is missing.
    pub fn type3_glyph(&mut self, font: &Font, code: u32, resources: &Dict) -> Option<DisplayList> {
        let doc = self.doc;
        let name = font.glyph_name(code)?;
        let procs = doc.get_in(&font.dict, b"CharProcs");
        let proc = doc.resolve(procs.as_dict()?.get(name)?);
        let stream = proc.as_stream()?;
        let data = doc.decode(stream).ok()?.data;
        let res = doc.get_in(&font.dict, b"Resources");
        let res = res.as_dict().unwrap_or(resources);
        Some(self.content(&data, res, Matrix::IDENTITY))
    }
}

/// The decoded `/Contents` of a page, streams joined with white space.
pub fn page_contents(doc: &Document, page: &Dict) -> Vec<u8> {
    let contents = doc.get_in(page, b"Contents");
    let mut out = Vec::new();
    let mut add = |o: &Object| {
        if let Some(s) = doc.resolve(o).as_stream()
            && let Ok(d) = doc.decode(s)
        {
            if out.is_empty() {
                out = d.data;
            } else {
                out.push(b'\n');
                out.extend_from_slice(&d.data);
            }
        }
    };
    match &*contents {
        Object::Array(a) => a.iter().for_each(&mut add),
        o => add(o),
    }
    out
}

/// Per content stream: its resources, default space and name caches.
pub(crate) struct Scope<'r> {
    pub res: &'r Dict,
    /// The CTM when the stream began (pattern space base).
    pub base: Matrix,
    pub fonts: Vec<(Box<[u8]>, Id)>,
    pub spaces: Vec<(Box<[u8]>, ColorSpace)>,
}

/// Builds one display list.
pub(crate) struct Builder<'i, 'd> {
    pub it: &'i mut Interpreter<'d>,
    pub doc: &'d Document,
    pub list: DisplayList,
    pub gs: GState,
    stack: Vec<GState>,
    floor: usize,
    ignored_saves: u32,
    path_verbs: u32,
    path_points: u32,
    current: Option<Point>,
    subpath_start: Point,
    pending_clip: Option<FillRule>,
    pub tm: Matrix,
    pub tlm: Matrix,
    pub text_runs_start: Option<u32>,
    hidden: u32,
    /// Open marked content: (hides content, emitted a command).
    marked: Vec<(bool, bool)>,
    pub forms: Vec<ObjRef>,
    /// After `d1`: colour operators are ignored.
    pub uncolored: bool,
    ops: u64,
    pub font_ids: HashMap<ObjRef, Id>,
    pub space_ids: HashMap<ObjRef, ColorSpace>,
    pub buf: Vec<u8>,
}

impl<'i, 'd> Builder<'i, 'd> {
    fn new(it: &'i mut Interpreter<'d>, ctm: Matrix) -> Self {
        let doc = it.doc;
        Builder {
            it,
            doc,
            list: DisplayList::default(),
            gs: GState::new(ctm),
            stack: Vec::new(),
            floor: 0,
            ignored_saves: 0,
            path_verbs: 0,
            path_points: 0,
            current: None,
            subpath_start: Point::default(),
            pending_clip: None,
            tm: Matrix::IDENTITY,
            tlm: Matrix::IDENTITY,
            text_runs_start: None,
            hidden: 0,
            marked: Vec::new(),
            forms: Vec::new(),
            uncolored: false,
            ops: 0,
            font_ids: HashMap::new(),
            space_ids: HashMap::new(),
            buf: Vec::new(),
        }
    }

    fn finish(mut self) -> DisplayList {
        while self.stack.len() > self.floor {
            self.restore();
        }
        if self.gs.clips > 0 {
            self.list.cmds.push(Cmd::PopClip {
                count: self.gs.clips,
            });
        }
        while !self.marked.is_empty() {
            self.end_marked();
        }
        self.list.shrink_to_fit();
        self.list
    }

    /// Runs one content stream; unbalanced `q` and marked content are closed at its end.
    pub fn run(&mut self, src: &[u8], res: &Dict, base: Matrix) {
        let floor = std::mem::replace(&mut self.floor, self.stack.len());
        let marked = self.marked.len();
        let mut sc = Scope {
            res,
            base,
            fonts: Vec::new(),
            spaces: Vec::new(),
        };
        let mut p = ContentParser::new(src);
        let max = self.it.options.max_operators;
        while let Some(i) = p.next() {
            self.ops += 1;
            if self.ops > max {
                break;
            }
            self.exec(&i, &mut sc);
        }
        while self.stack.len() > self.floor {
            self.restore();
        }
        while self.marked.len() > marked {
            self.end_marked();
        }
        self.floor = floor;
    }

    fn exec(&mut self, i: &Instr<'_, '_>, sc: &mut Scope<'_>) {
        match i.op {
            Op::LineWidth => {
                if let Some(w) = i.number() {
                    self.gs.line_width = w.abs();
                    self.gs.style_id = NONE;
                }
            }
            Op::LineCap => {
                if let Some(v) = i.number() {
                    self.gs.cap = line_cap(v);
                    self.gs.style_id = NONE;
                }
            }
            Op::LineJoin => {
                if let Some(v) = i.number() {
                    self.gs.join = line_join(v);
                    self.gs.style_id = NONE;
                }
            }
            Op::MiterLimit => {
                if let Some(v) = i.number() {
                    self.gs.miter_limit = v.max(1.0);
                    self.gs.style_id = NONE;
                }
            }
            Op::Dash => {
                if let [.., arr @ Operand::Array { .. }, phase] = i.args
                    && let Some(phase) = phase.number()
                {
                    let mut dash = [0.0f32; 16];
                    let mut n = 0;
                    for e in i.array(arr).iter().take(16) {
                        if let Some(v) = e.number() {
                            dash[n] = v.abs();
                            n += 1;
                        }
                    }
                    self.set_dash(&dash[..n], phase);
                }
            }
            Op::RenderingIntent => {
                if let Some(n) = i.raw_name() {
                    self.gs.intent = RenderingIntent::from_name(n);
                    self.gs.paints_changed();
                }
            }
            Op::Flatness => {
                if let Some(v) = i.number() {
                    self.gs.flatness = v.clamp(0.0, 100.0);
                }
            }
            Op::ExtGState => {
                if let Some(n) = i.args.last().and_then(Operand::name) {
                    self.ext_gstate(&n, sc);
                }
            }
            Op::Save => self.save(),
            Op::Restore => {
                if self.ignored_saves > 0 {
                    self.ignored_saves -= 1;
                } else if self.stack.len() > self.floor {
                    self.restore();
                }
            }
            Op::Concat => {
                if let Some(m) = i.numbers::<6>() {
                    let m = Matrix::from_array(m).then(&self.gs.ctm);
                    if m.is_finite() {
                        self.gs.ctm = m;
                        self.gs.ctm_id = NONE;
                    }
                }
            }
            Op::MoveTo => {
                if let Some([x, y]) = i.numbers() {
                    self.move_to(Point::new(x, y));
                }
            }
            Op::LineTo => {
                if let Some([x, y]) = i.numbers() {
                    self.line_to(Point::new(x, y));
                }
            }
            Op::CurveTo => {
                if let Some([a, b, c, d, e, f]) = i.numbers() {
                    self.curve_to(Point::new(a, b), Point::new(c, d), Point::new(e, f));
                }
            }
            Op::CurveToV => {
                if let Some([c, d, e, f]) = i.numbers() {
                    let p0 = self.current.unwrap_or(Point::new(c, d));
                    self.curve_to(p0, Point::new(c, d), Point::new(e, f));
                }
            }
            Op::CurveToY => {
                if let Some([a, b, e, f]) = i.numbers() {
                    self.curve_to(Point::new(a, b), Point::new(e, f), Point::new(e, f));
                }
            }
            Op::ClosePath => self.close_path(),
            Op::Rect => {
                if let Some([x, y, w, h]) = i.numbers() {
                    self.move_to(Point::new(x, y));
                    self.line_to(Point::new(x + w, y));
                    self.line_to(Point::new(x + w, y + h));
                    self.line_to(Point::new(x, y + h));
                    self.close_path();
                }
            }
            Op::Stroke => self.paint_path(None, true, false),
            Op::CloseStroke => self.paint_path(None, true, true),
            Op::Fill | Op::FillCompat => self.paint_path(Some(FillRule::NonZero), false, false),
            Op::FillEvenOdd => self.paint_path(Some(FillRule::EvenOdd), false, false),
            Op::FillStroke => self.paint_path(Some(FillRule::NonZero), true, false),
            Op::FillStrokeEvenOdd => self.paint_path(Some(FillRule::EvenOdd), true, false),
            Op::CloseFillStroke => self.paint_path(Some(FillRule::NonZero), true, true),
            Op::CloseFillStrokeEvenOdd => self.paint_path(Some(FillRule::EvenOdd), true, true),
            Op::EndPath => self.paint_path(None, false, false),
            Op::Clip => self.pending_clip = Some(FillRule::NonZero),
            Op::ClipEvenOdd => self.pending_clip = Some(FillRule::EvenOdd),
            Op::BeginText
            | Op::EndText
            | Op::CharSpacing
            | Op::WordSpacing
            | Op::HorizontalScale
            | Op::Leading
            | Op::Font
            | Op::TextRender
            | Op::TextRise
            | Op::MoveText
            | Op::MoveTextLeading
            | Op::TextMatrix
            | Op::NextLine
            | Op::ShowText
            | Op::ShowTextArray
            | Op::NextLineShow
            | Op::NextLineSpacingShow => self.text_op(i, sc),
            Op::GlyphWidth | Op::GlyphWidthBBox => self.glyph_metrics(i),
            Op::StrokeSpace
            | Op::FillSpace
            | Op::StrokeColor
            | Op::StrokeColorN
            | Op::FillColor
            | Op::FillColorN
            | Op::StrokeGray
            | Op::FillGray
            | Op::StrokeRgb
            | Op::FillRgb
            | Op::StrokeCmyk
            | Op::FillCmyk => {
                if !self.uncolored {
                    self.color_op(i, sc);
                }
            }
            Op::Shade => {
                if let Some(n) = i.args.last().and_then(Operand::name) {
                    self.shade(&n, sc);
                }
            }
            Op::InlineImage => self.inline_image(i, sc),
            Op::XObject => {
                if let Some(n) = i.args.last().and_then(Operand::name) {
                    self.xobject(&n, sc);
                }
            }
            Op::BeginMarked => {
                let tag = i.args.last().and_then(Operand::name);
                self.begin_marked(tag.as_deref().unwrap_or_default(), None, None);
            }
            Op::BeginMarkedProps => {
                let [.., tag, props] = i.args else { return };
                let tag = tag.name().unwrap_or_default();
                let (dict, raw) = match props {
                    Operand::Dict(_) => (i.dict(props).cloned(), None),
                    Operand::Name(_) => {
                        let n = props.name().unwrap_or_default();
                        let raw = self.resource(sc.res, b"Properties", &n);
                        let d = raw
                            .as_ref()
                            .and_then(|o| self.doc.resolve(o).as_dict().cloned());
                        (d, raw)
                    }
                    _ => (None, None),
                };
                self.begin_marked(&tag, dict, raw);
            }
            Op::EndMarked => {
                if !self.marked.is_empty() {
                    self.end_marked();
                }
            }
            Op::MarkPoint | Op::MarkPointProps | Op::BeginCompat | Op::EndCompat | Op::Unknown => {}
        }
    }

    // Resources

    /// `res[category][name]`, unresolved.
    pub fn resource(&self, res: &Dict, category: &[u8], name: &[u8]) -> Option<Object> {
        let c = self.doc.get_in(res, category);
        c.as_dict()?.get(name).cloned()
    }

    // Graphics state

    pub fn visible(&self) -> bool {
        self.hidden == 0
    }

    fn save(&mut self) {
        if self.stack.len() - self.floor >= self.it.options.max_save_depth {
            self.ignored_saves += 1;
        } else {
            self.stack.push(self.gs);
        }
    }

    fn restore(&mut self) {
        if let Some(saved) = self.stack.pop() {
            self.pop_clips_to(saved.clips);
            self.gs = saved;
        }
    }

    /// Saves the state around a form; returns the stack height to restore to.
    pub fn enter(&mut self) -> usize {
        self.stack.push(self.gs);
        self.stack.len() - 1
    }

    pub fn leave(&mut self, height: usize) {
        while self.stack.len() > height + 1 {
            self.restore();
        }
        self.restore();
    }

    fn pop_clips_to(&mut self, clips: u32) {
        let n = self.gs.clips.saturating_sub(clips);
        if n > 0 {
            self.list.cmds.push(Cmd::PopClip { count: n });
        }
    }

    pub fn ctm_id(&mut self) -> Id {
        if self.gs.ctm_id == NONE {
            let m = &mut self.list.matrices;
            if m.last() != Some(&self.gs.ctm) {
                m.push(self.gs.ctm);
            }
            self.gs.ctm_id = m.len() as Id - 1;
        }
        self.gs.ctm_id
    }

    pub fn fill_paint(&mut self) -> Id {
        if self.gs.fill_id == NONE {
            self.gs.fill_id = self.push_paint(false);
        }
        self.gs.fill_id
    }

    pub fn stroke_paint(&mut self) -> Id {
        if self.gs.stroke_id == NONE {
            self.gs.stroke_id = self.push_paint(true);
        }
        self.gs.stroke_id
    }

    fn push_paint(&mut self, stroke: bool) -> Id {
        let gs = &self.gs;
        let (c, alpha, overprint) = if stroke {
            (&gs.stroke, gs.stroke_alpha, gs.overprint_stroke)
        } else {
            (&gs.fill, gs.fill_alpha, gs.overprint_fill)
        };
        let list = &mut self.list;
        let start = list.components.len() as u32;
        let p = Paint {
            space: c.space,
            components: (start, start + u32::from(c.n)),
            pattern: c.pattern,
            alpha,
            blend: gs.blend,
            soft_mask: gs.soft_mask,
            overprint,
            overprint_mode: gs.overprint_mode,
            intent: gs.intent,
        };
        // Consecutive equal paints share an entry.
        for (k, q) in list.paints.iter().enumerate().rev().take(2) {
            if (Paint {
                components: q.components,
                ..p
            }) == *q
                && list.paint_components(q) == c.components()
            {
                return k as Id;
            }
        }
        list.components.extend_from_slice(c.components());
        list.paints.push(p);
        list.paints.len() as Id - 1
    }

    pub fn style_id(&mut self) -> Id {
        if self.gs.style_id == NONE {
            let gs = &self.gs;
            let s = StrokeStyle {
                width: gs.line_width,
                cap: gs.cap,
                join: gs.join,
                miter_limit: gs.miter_limit,
                dash: gs.dash,
                dash_phase: gs.dash_phase,
                stroke_adjust: gs.stroke_adjust,
            };
            let st = &mut self.list.strokes;
            if st.last() != Some(&s) {
                st.push(s);
            }
            self.gs.style_id = st.len() as Id - 1;
        }
        self.gs.style_id
    }

    pub fn set_dash(&mut self, dash: &[f32], phase: f32) {
        let dash = if dash.iter().all(|&d| d == 0.0) {
            &[][..]
        } else {
            dash
        };
        let d = &mut self.list.dashes;
        let cur = &d[self.gs.dash.0 as usize..self.gs.dash.1 as usize];
        if cur != dash {
            let start = d.len() as u32;
            d.extend_from_slice(dash);
            self.gs.dash = (start, d.len() as u32);
        }
        self.gs.dash_phase = phase;
        self.gs.style_id = NONE;
    }

    // Paths

    fn move_to(&mut self, p: Point) {
        // A move after a move replaces it.
        if self.list.verbs.last() == Some(&Verb::Move)
            && self.list.verbs.len() > self.path_verbs as usize
        {
            if let Some(last) = self.list.points.last_mut() {
                *last = p;
            }
        } else {
            self.list.verbs.push(Verb::Move);
            self.list.points.push(p);
        }
        self.current = Some(p);
        self.subpath_start = p;
    }

    fn line_to(&mut self, p: Point) {
        if self.current.is_none() {
            return self.move_to(p);
        }
        self.list.verbs.push(Verb::Line);
        self.list.points.push(p);
        self.current = Some(p);
    }

    fn curve_to(&mut self, a: Point, b: Point, c: Point) {
        if self.current.is_none() {
            self.move_to(a);
        }
        self.list.verbs.push(Verb::Cubic);
        self.list.points.extend_from_slice(&[a, b, c]);
        self.current = Some(c);
    }

    fn close_path(&mut self) {
        if self.current.is_some() && self.list.verbs.last() != Some(&Verb::Close) {
            self.list.verbs.push(Verb::Close);
            self.current = Some(self.subpath_start);
        }
    }

    fn paint_path(&mut self, fill: Option<FillRule>, stroke: bool, close: bool) {
        if close {
            self.close_path();
        }
        let clip = self.pending_clip.take();
        let (v0, p0) = (self.path_verbs, self.path_points);
        let (v1, p1) = (self.list.verbs.len() as u32, self.list.points.len() as u32);
        self.current = None;
        let draws = self.visible() && (fill.is_some() || stroke);
        if v1 == v0 || !(draws || clip.is_some()) {
            self.list.verbs.truncate(v0 as usize);
            self.list.points.truncate(p0 as usize);
            return;
        }
        self.list.paths.push(PathSpan {
            verbs: (v0, v1),
            points: (p0, p1),
        });
        let path = self.list.paths.len() as Id - 1;
        self.path_verbs = v1;
        self.path_points = p1;
        let ctm = self.ctm_id();
        if draws {
            if let Some(rule) = fill {
                let paint = self.fill_paint();
                self.list.cmds.push(Cmd::Fill {
                    path,
                    ctm,
                    paint,
                    rule,
                });
            }
            if stroke {
                let paint = self.stroke_paint();
                let style = self.style_id();
                self.list.cmds.push(Cmd::Stroke {
                    path,
                    ctm,
                    paint,
                    style,
                });
            }
        }
        if let Some(rule) = clip {
            self.list.cmds.push(Cmd::Clip { path, ctm, rule });
            self.gs.clips += 1;
        }
    }

    /// Clips to a rectangle. A path under construction (invalid here) is dropped.
    pub fn clip_rect(&mut self, r: crate::geom::Rect) {
        self.list.verbs.truncate(self.path_verbs as usize);
        self.list.points.truncate(self.path_points as usize);
        self.current = None;
        self.move_to(Point::new(r.x0, r.y0));
        self.line_to(Point::new(r.x1, r.y0));
        self.line_to(Point::new(r.x1, r.y1));
        self.line_to(Point::new(r.x0, r.y1));
        self.close_path();
        let pending = self.pending_clip.replace(FillRule::NonZero);
        self.paint_path(None, false, false);
        self.pending_clip = pending;
    }

    // Marked and optional content

    fn begin_marked(&mut self, tag: &[u8], props: Option<Dict>, raw: Option<Object>) {
        if tag == b"OC" {
            let target = raw.or_else(|| props.clone().map(Object::Dict));
            if let Some(o) = target
                && !self.it.oc.visible(self.doc, &o)
            {
                self.hidden += 1;
                self.marked.push((true, false));
                return;
            }
        }
        if !self.visible() {
            self.marked.push((false, false));
            return;
        }
        self.list.marked.push(Marked {
            tag: pagebara_syntax::Name::new(tag),
            properties: props,
        });
        let marked = self.list.marked.len() as Id - 1;
        self.list.cmds.push(Cmd::BeginMarked { marked });
        self.marked.push((false, true));
    }

    fn end_marked(&mut self) {
        if let Some((hid, emitted)) = self.marked.pop() {
            if hid {
                self.hidden -= 1;
            }
            if emitted {
                self.list.cmds.push(Cmd::EndMarked);
            }
        }
    }

    /// Whether an XObject's `/OC` allows drawing it.
    pub fn oc_visible(&self, dict: &Dict) -> bool {
        dict.get(b"OC")
            .is_none_or(|o| self.it.oc.visible(self.doc, o))
    }

    fn glyph_metrics(&mut self, i: &Instr<'_, '_>) {
        if i.op == Op::GlyphWidth {
            if let Some([wx, wy]) = i.numbers() {
                self.list.glyph_metrics = Some(crate::display::GlyphMetrics {
                    width: Point::new(wx, wy),
                    bbox: None,
                });
            }
        } else if let Some([wx, wy, a, b, c, d]) = i.numbers() {
            self.list.glyph_metrics = Some(crate::display::GlyphMetrics {
                width: Point::new(wx, wy),
                bbox: Some(crate::geom::Rect::new(a, b, c, d)),
            });
            self.uncolored = true;
        }
    }
}

pub(crate) fn line_cap(v: f32) -> LineCap {
    match v as i32 {
        1 => LineCap::Round,
        2 => LineCap::Square,
        _ => LineCap::Butt,
    }
}

pub(crate) fn line_join(v: f32) -> LineJoin {
    match v as i32 {
        1 => LineJoin::Round,
        2 => LineJoin::Bevel,
        _ => LineJoin::Miter,
    }
}
