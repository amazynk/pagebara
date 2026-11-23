//! Rewriting content streams without what lies under the regions.
//!
//! The rewriter follows the graphics and text state like the interpreter does and writes
//! every operator back out, changed where content meets a region:
//!
//! - glyphs whose box (as text extraction measures it) overlaps a region are removed from
//!   their strings; their advance becomes a `TJ` adjustment, so the remaining glyphs keep
//!   their exact positions;
//! - paths lose the geometry inside regions ([`crate::path`]); clipping paths too;
//! - images and inline images get the pixels under regions overwritten in their samples
//!   (and soft masks and stencil masks), or are dropped when wholly covered;
//! - form XObjects that meet a region are rewritten into a copy for this use only, so other
//!   pages showing the form are unchanged;
//! - shadings are clipped out of regions, mesh shadings lose the parts inside them and
//!   sampled functions the samples only shown there ([`crate::shading`]);
//! - tiling patterns painted where regions are may be replaced ([`crate::tiling`]);
//! - marked content whose contents changed loses `/ActualText`, `/Alt` and `/E`.
//!
//! Hidden optional content (when asked for) is removed the same way, as if a region
//! covered the whole page.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use pagebara_content::font::{Font, ProgramLoader, embedded_programs};
use pagebara_content::lex::Operand;
use pagebara_content::oc::OptionalContent;
use pagebara_content::{ContentParser, Instr, Op};
use pagebara_syntax::{Dict, Document, ObjRef, Object, Stream, write_object};

use crate::emit;
use crate::geom::{Matrix, Point, Rect, Region};
use crate::image::{self, Samples};
use crate::path::{self, Seg, Sub};
use crate::shading;
use crate::tiling::{Area, Shape, Use};

/// Forms nested deeper than this are dropped when they meet a region.
const MAX_FORM_DEPTH: usize = 16;
/// Keys of marked-content properties that repeat or describe the content.
const DESCRIBING: [&[u8]; 3] = [b"ActualText", b"Alt", b"E"];

/// What a rewrite changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub glyphs_removed: usize,
    pub paths_changed: usize,
    pub images_blanked: usize,
    pub images_removed: usize,
    pub pixels_blanked: usize,
    pub forms_copied: usize,
    pub shadings_clipped: usize,
    /// Tiling patterns replaced by copies without what lay under regions.
    pub patterns_rewritten: usize,
    /// Triangles and patches of mesh shadings that lay wholly inside regions.
    pub mesh_parts_removed: usize,
    /// Samples of shading functions zeroed because they were shown only inside regions.
    pub shading_samples_blanked: usize,
}

impl Stats {
    pub(crate) fn add(&mut self, o: &Stats) {
        self.glyphs_removed += o.glyphs_removed;
        self.paths_changed += o.paths_changed;
        self.images_blanked += o.images_blanked;
        self.images_removed += o.images_removed;
        self.pixels_blanked += o.pixels_blanked;
        self.forms_copied += o.forms_copied;
        self.shadings_clipped += o.shadings_clipped;
        self.patterns_rewritten += o.patterns_rewritten;
        self.mesh_parts_removed += o.mesh_parts_removed;
        self.shading_samples_blanked += o.shading_samples_blanked;
    }
}

/// A resource dictionary being edited: names of new resources are added to a copy.
#[derive(Debug, Clone)]
pub(crate) struct Res {
    pub dict: Dict,
    pub changed: bool,
    counter: u32,
}

impl Res {
    pub fn new(doc: &Document, o: Option<&Object>) -> Res {
        let dict = o
            .and_then(|o| doc.resolve(o).as_dict().cloned())
            .unwrap_or_default();
        Res {
            dict,
            changed: false,
            counter: 0,
        }
    }

    pub(crate) fn get(&self, doc: &Document, cat: &[u8], name: &[u8]) -> Option<Object> {
        doc.get_in(&self.dict, cat).as_dict()?.get(name).cloned()
    }

    pub fn add(&mut self, doc: &Document, cat: &[u8], value: Object) -> Vec<u8> {
        let mut sub = doc
            .get_in(&self.dict, cat)
            .as_dict()
            .cloned()
            .unwrap_or_default();
        let name = loop {
            self.counter += 1;
            let n = format!("Rd{}", self.counter);
            if !sub.contains(n.as_bytes()) {
                break n;
            }
        };
        sub.set(name.as_bytes(), value);
        self.dict.set(cat, Object::Dict(sub));
        self.changed = true;
        name.into_bytes()
    }
}

#[derive(Clone)]
struct Gs {
    ctm: Matrix,
    line_width: f64,
    dash: Vec<f64>,
    phase: f64,
    tc: f64,
    tw: f64,
    th: f64,
    tl: f64,
    size: f64,
    rise: f64,
    font: Option<Arc<Font>>,
    /// The font dictionary's object, when it has one.
    font_ref: Option<ObjRef>,
    /// The `Tf` operand naming the font, as written.
    font_name: Option<Vec<u8>>,
    /// Pattern names of the fill and stroke colours.
    fill_pattern: Option<Vec<u8>>,
    stroke_pattern: Option<Vec<u8>>,
    /// Clipping paths in force (device space; `None` for text).
    clip: Arc<Vec<Option<Area>>>,
    /// Text rendering mode.
    render: i64,
}

struct Frame {
    /// Output range of an inline property list with describing keys, and the list.
    props: Option<(usize, usize, Dict)>,
    mcid: Option<i64>,
    touched: bool,
    hides: bool,
}

/// The state of one content stream being rewritten.
struct Run {
    gs: Gs,
    stack: Vec<Gs>,
    tm: Matrix,
    tlm: Matrix,
    in_text: bool,
    subs: Vec<Sub>,
    new_sub: bool,
    path_raw: Vec<u8>,
    clip: Option<bool>,
    out: Vec<u8>,
    patches: Vec<(usize, usize, Vec<u8>)>,
    frames: Vec<Frame>,
    hidden: u32,
    changed: bool,
    struct_parents: Option<i64>,
    /// Shading names drawn as they are, and ones replaced by a rewritten copy.
    shadings_kept: HashSet<Vec<u8>>,
    shadings_replaced: HashSet<Vec<u8>>,
    /// Areas painted with each pattern, in order of first use.
    pattern_uses: Vec<(Vec<u8>, Vec<Use>)>,
    /// Text since `BT` added to the clip.
    text_clip: bool,
}

/// Uses kept per pattern; more are merged into the last one.
const MAX_USES: usize = 64;

impl Run {
    /// Something under a region was removed: the stream and enclosing marked content change.
    fn touch(&mut self) {
        self.changed = true;
        for f in &mut self.frames {
            f.touched = true;
        }
    }

    fn pattern_use(&mut self, name: &[u8], u: Use) {
        let i = match self.pattern_uses.iter().position(|u| u.0 == name) {
            Some(i) => i,
            None => {
                self.pattern_uses.push((name.to_vec(), Vec::new()));
                self.pattern_uses.len() - 1
            }
        };
        let uses = &mut self.pattern_uses[i].1;
        if uses.len() < MAX_USES {
            uses.push(u);
        } else if let Some(l) = uses.last_mut() {
            l.bbox = l.bbox.union(&u.bbox);
            l.shape = Shape::Unknown;
        }
    }

    /// Notes box `b` of `shape` painted with the fill and/or the stroke colour.
    fn painted(&mut self, b: Rect, shape: Shape, fill: bool, stroke: bool) {
        let u = Use {
            bbox: b,
            shape,
            clip: self.gs.clip.clone(),
        };
        if fill && let Some(n) = self.gs.fill_pattern.clone() {
            self.pattern_use(&n, u.clone());
        }
        if stroke && let Some(n) = self.gs.stroke_pattern.clone() {
            self.pattern_use(&n, u);
        }
    }

    fn current(&self) -> Option<Point> {
        let s = self.subs.last()?;
        Some(s.segs.last().map_or(s.start, Seg::end))
    }
}

/// The rewriter for one page, keeping fonts and copied forms and images between streams.
pub(crate) struct Rewriter<'d> {
    pub doc: &'d mut Document,
    pub regions: Vec<Region>,
    /// Share of a glyph's box a region must cover to remove it.
    pub overlap: f64,
    /// Removes content hidden by this optional content configuration.
    pub hidden_oc: Option<OptionalContent>,
    /// Streams are read as wholly hidden (to note the glyphs of a dropped form).
    pub(crate) drop_all: bool,
    fonts: HashMap<ObjRef, Arc<Font>>,
    programs: Arc<ProgramLoader>,
    /// Rewritten XObjects by (object, placement): `None` drops them.
    cache: HashMap<(u32, [i64; 6]), Option<ObjRef>>,
    /// Rewritten shadings by (object, placement): `None` unchanged, `Some(None)` drawing
    /// nothing.
    meshes: HashMap<(u32, [i64; 6]), Option<Option<ObjRef>>>,
    forms: Vec<u32>,
    pub stats: Stats,
    /// Original XObjects whose content under a region survives in them.
    pub tainted: HashSet<u32>,
    /// `(StructParents, MCID)` of marked content that changed.
    pub mcids: Vec<(i64, i64)>,
    /// Codes removed, by font dictionary.
    pub removed: crate::fonts::Removed,
    pub warnings: Vec<String>,
}

fn key(m: &Matrix) -> [i64; 6] {
    [m.a, m.b, m.c, m.d, m.e, m.f].map(|v| (v * 1e4).round() as i64)
}

impl<'d> Rewriter<'d> {
    pub fn new(doc: &'d mut Document, regions: Vec<Region>, overlap: f64) -> Self {
        Rewriter {
            doc,
            regions,
            overlap,
            hidden_oc: None,
            drop_all: false,
            fonts: HashMap::new(),
            programs: embedded_programs(),
            cache: HashMap::new(),
            meshes: HashMap::new(),
            forms: Vec::new(),
            stats: Stats::default(),
            tainted: HashSet::new(),
            mcids: Vec::new(),
            removed: HashMap::new(),
            warnings: Vec::new(),
        }
    }

    /// Rewrites one decoded content stream drawn with `ctm`. Returns the new stream and
    /// whether anything changed (otherwise the original should be kept).
    pub fn rewrite(
        &mut self,
        src: &[u8],
        res: &mut Res,
        ctm: Matrix,
        struct_parents: Option<i64>,
    ) -> (Vec<u8>, bool) {
        let mut run = Run {
            gs: Gs {
                ctm,
                line_width: 1.0,
                dash: Vec::new(),
                phase: 0.0,
                tc: 0.0,
                tw: 0.0,
                th: 1.0,
                tl: 0.0,
                size: 0.0,
                rise: 0.0,
                font: None,
                font_ref: None,
                font_name: None,
                fill_pattern: None,
                stroke_pattern: None,
                clip: Arc::new(Vec::new()),
                render: 0,
            },
            stack: Vec::new(),
            tm: Matrix::IDENTITY,
            tlm: Matrix::IDENTITY,
            in_text: false,
            subs: Vec::new(),
            new_sub: false,
            path_raw: Vec::new(),
            clip: None,
            out: Vec::with_capacity(src.len() + 64),
            patches: Vec::new(),
            frames: Vec::new(),
            hidden: u32::from(self.drop_all),
            changed: false,
            struct_parents,
            shadings_kept: HashSet::new(),
            shadings_replaced: HashSet::new(),
            pattern_uses: Vec::new(),
            text_clip: false,
        };
        let patterns = self.shading_patterns(res, &ctm);
        let mut p = ContentParser::new(src);
        while let Some(i) = p.next() {
            self.exec(&mut run, res, &i);
        }
        // Leave the state balanced for content that follows.
        if !run.path_raw.is_empty() {
            run.path_raw.clear();
        }
        if run.in_text {
            run.out.extend_from_slice(b"ET\n");
        }
        while !run.frames.is_empty() {
            self.end_marked(&mut run);
        }
        for _ in 0..run.stack.len() {
            run.out.extend_from_slice(b"Q\n");
        }
        let mut out = run.out;
        if !run.patches.is_empty() {
            run.patches.sort_by_key(|p| p.0);
            let mut o = Vec::with_capacity(out.len());
            let mut at = 0;
            for (s, e, rep) in run.patches {
                o.extend_from_slice(&out[at..s]);
                o.extend_from_slice(&rep);
                at = e;
            }
            o.extend_from_slice(&out[at..]);
            out = o;
        }
        // Shadings no longer drawn by their old name leave the resources.
        let gone: Vec<&Vec<u8>> = run
            .shadings_replaced
            .difference(&run.shadings_kept)
            .collect();
        if !gone.is_empty()
            && let Some(mut sh) = self.doc.get_in(&res.dict, b"Shading").as_dict().cloned()
        {
            for n in gone {
                sh.remove(n);
            }
            res.dict.set(b"Shading", Object::Dict(sh));
        }
        let tiling = !self.regions.is_empty()
            && !self.drop_all
            && self.tiling_patterns(res, &ctm, std::mem::take(&mut run.pattern_uses));
        (out, run.changed || patterns || tiling)
    }

    /// Replaces shading patterns of `res` whose shadings hold something inside regions
    /// (pattern space maps to the page by the pattern matrix then `ctm`). Returns whether
    /// any did.
    fn shading_patterns(&mut self, res: &mut Res, ctm: &Matrix) -> bool {
        if self.regions.is_empty() {
            return false;
        }
        let Some(mut pats) = self.doc.get_in(&res.dict, b"Pattern").as_dict().cloned() else {
            return false;
        };
        let mut changed = false;
        for (name, v) in pats.clone().iter() {
            let po = self.doc.resolve(v);
            let Some(pd) = po.as_dict() else { continue };
            if self.doc.get_in(pd, b"PatternType").as_int() != Some(2) {
                continue;
            }
            let Some(sh) = pd.get(b"Shading").cloned() else {
                continue;
            };
            let m = pd
                .get(b"Matrix")
                .and_then(|m| Matrix::from_object(self.doc, m))
                .unwrap_or_default()
                .then(ctm);
            let mut pd = pd.clone();
            let Some(c) = shading::redact(self.doc, &sh, &m, &self.regions) else {
                continue;
            };
            self.stats.mesh_parts_removed += c.parts;
            self.stats.shading_samples_blanked += c.samples;
            if let Some(r) = sh.as_objref() {
                self.tainted.insert(r.num);
            }
            pd.set(b"Shading", Object::Ref(self.doc.add(c.shading)));
            let r = self.doc.add(pd);
            pats.set(name.as_bytes(), Object::Ref(r));
            changed = true;
        }
        if changed {
            res.dict.set(b"Pattern", Object::Dict(pats));
            res.changed = true;
        }
        changed
    }

    fn hit_all(&self, run: &Run) -> bool {
        run.hidden > 0
    }

    fn exec(&mut self, run: &mut Run, res: &mut Res, i: &Instr<'_, '_>) {
        let gs = &mut run.gs;
        match i.op {
            Op::Save => {
                run.stack.push(gs.clone());
                emit::instr(&mut run.out, i);
            }
            Op::Restore => {
                if let Some(g) = run.stack.pop() {
                    run.gs = g;
                    emit::instr(&mut run.out, i);
                }
            }
            Op::Concat => {
                if let Some(m) = i.numbers::<6>() {
                    let m = Matrix::from_slice(&m.map(f64::from));
                    if let Some(m) = m.map(|m| m.then(&gs.ctm)).filter(Matrix::is_finite) {
                        gs.ctm = m;
                    }
                }
                emit::instr(&mut run.out, i);
            }
            Op::LineWidth => {
                if let Some(w) = i.number() {
                    gs.line_width = f64::from(w).abs();
                }
                emit::instr(&mut run.out, i);
            }
            Op::Dash => {
                if let [.., arr @ Operand::Array { .. }, phase] = i.args {
                    gs.dash = i
                        .array(arr)
                        .iter()
                        .filter_map(|e| e.number())
                        .map(f64::from)
                        .collect();
                    gs.phase = phase.number().map_or(0.0, f64::from);
                }
                emit::instr(&mut run.out, i);
            }
            Op::ExtGState => {
                if let Some(n) = i.args.last().and_then(Operand::name) {
                    self.ext_gstate(run, res, &n);
                }
                emit::instr(&mut run.out, i);
            }
            Op::MoveTo
            | Op::LineTo
            | Op::CurveTo
            | Op::CurveToV
            | Op::CurveToY
            | Op::ClosePath
            | Op::Rect => {
                self.path_op(run, i);
                emit::instr(&mut run.path_raw, i);
            }
            Op::FillColorN | Op::StrokeColorN => {
                let n = i.args.last().and_then(Operand::name).map(|n| n.to_vec());
                if i.op == Op::FillColorN {
                    gs.fill_pattern = n;
                } else {
                    gs.stroke_pattern = n;
                }
                emit::instr(&mut run.out, i);
            }
            Op::FillSpace | Op::FillColor | Op::FillGray | Op::FillRgb | Op::FillCmyk => {
                gs.fill_pattern = None;
                emit::instr(&mut run.out, i);
            }
            Op::StrokeSpace | Op::StrokeColor | Op::StrokeGray | Op::StrokeRgb | Op::StrokeCmyk => {
                gs.stroke_pattern = None;
                emit::instr(&mut run.out, i);
            }
            Op::Clip => run.clip = Some(false),
            Op::ClipEvenOdd => run.clip = Some(true),
            Op::Stroke
            | Op::CloseStroke
            | Op::Fill
            | Op::FillCompat
            | Op::FillEvenOdd
            | Op::FillStroke
            | Op::FillStrokeEvenOdd
            | Op::CloseFillStroke
            | Op::CloseFillStrokeEvenOdd
            | Op::EndPath => {
                // A clipping path applies from after the painting operator.
                let clip = run.clip.map(|eo| Area::new(&run.subs, eo));
                self.paint(run, i);
                if let Some(a) = clip {
                    Arc::make_mut(&mut run.gs.clip).push(Some(a));
                }
            }
            Op::BeginText => {
                run.tm = Matrix::IDENTITY;
                run.tlm = Matrix::IDENTITY;
                run.in_text = true;
                emit::instr(&mut run.out, i);
            }
            Op::EndText => {
                run.in_text = false;
                if std::mem::take(&mut run.text_clip) {
                    Arc::make_mut(&mut run.gs.clip).push(None);
                }
                emit::instr(&mut run.out, i);
            }
            Op::TextRender => {
                gs.render = i.number().map_or(gs.render, |v| v as i64);
                emit::instr(&mut run.out, i);
            }
            Op::CharSpacing => {
                gs.tc = i.number().map_or(gs.tc, f64::from);
                emit::instr(&mut run.out, i);
            }
            Op::WordSpacing => {
                gs.tw = i.number().map_or(gs.tw, f64::from);
                emit::instr(&mut run.out, i);
            }
            Op::HorizontalScale => {
                gs.th = i.number().map_or(gs.th, |v| f64::from(v) / 100.0);
                emit::instr(&mut run.out, i);
            }
            Op::Leading => {
                gs.tl = i.number().map_or(gs.tl, f64::from);
                emit::instr(&mut run.out, i);
            }
            Op::TextRise => {
                gs.rise = i.number().map_or(gs.rise, f64::from);
                emit::instr(&mut run.out, i);
            }
            Op::Font => {
                if let [.., name, size] = i.args
                    && let (Some(n), Some(size)) = (name.name(), size.number())
                {
                    run.gs.size = f64::from(size);
                    let o = res.get(self.doc, b"Font", &n);
                    run.gs.font_ref = o.as_ref().and_then(Object::as_objref);
                    run.gs.font = o.and_then(|o| self.load_font(&o));
                    run.gs.font_name = name.raw_name().map(<[u8]>::to_vec);
                }
                emit::instr(&mut run.out, i);
            }
            Op::MoveText => {
                if let Some([x, y]) = i.numbers() {
                    next_line(run, f64::from(x), f64::from(y));
                }
                emit::instr(&mut run.out, i);
            }
            Op::MoveTextLeading => {
                if let Some([x, y]) = i.numbers() {
                    run.gs.tl = -f64::from(y);
                    next_line(run, f64::from(x), f64::from(y));
                }
                emit::instr(&mut run.out, i);
            }
            Op::TextMatrix => {
                if let Some(m) = i.numbers::<6>()
                    && let Some(m) = Matrix::from_slice(&m.map(f64::from))
                {
                    run.tm = m;
                    run.tlm = m;
                }
                emit::instr(&mut run.out, i);
            }
            Op::NextLine => {
                let l = gs.tl;
                next_line(run, 0.0, -l);
                emit::instr(&mut run.out, i);
            }
            Op::ShowText => {
                if let Some(s) = i.args.last() {
                    self.show(run, i, std::slice::from_ref(s), b"");
                }
            }
            Op::ShowTextArray => {
                if let Some(a) = i.args.last() {
                    self.show(run, i, i.array(a), b"");
                }
            }
            Op::NextLineShow => {
                let l = gs.tl;
                next_line(run, 0.0, -l);
                if let Some(s) = i.args.last() {
                    self.show(run, i, std::slice::from_ref(s), b"T*\n");
                }
            }
            Op::NextLineSpacingShow => {
                if let [.., aw, ac, s] = i.args {
                    gs.tw = aw.number().map_or(gs.tw, f64::from);
                    gs.tc = ac.number().map_or(gs.tc, f64::from);
                    let l = gs.tl;
                    next_line(run, 0.0, -l);
                    let mut prefix = Vec::new();
                    emit::operand(&mut prefix, i, aw);
                    prefix.extend_from_slice(b" Tw ");
                    emit::operand(&mut prefix, i, ac);
                    prefix.extend_from_slice(b" Tc T*\n");
                    self.show(run, i, std::slice::from_ref(s), &prefix);
                }
            }
            Op::XObject => {
                if let Some(n) = i.args.last().and_then(Operand::name) {
                    self.xobject(run, res, i, &n);
                } else {
                    emit::instr(&mut run.out, i);
                }
            }
            Op::InlineImage => self.inline_image(run, res, i),
            Op::Shade => self.shade(run, res, i),
            Op::BeginMarked => {
                run.frames.push(Frame {
                    props: None,
                    mcid: None,
                    touched: false,
                    hides: false,
                });
                emit::instr(&mut run.out, i);
            }
            Op::BeginMarkedProps => self.begin_marked(run, res, i),
            Op::EndMarked => {
                if !run.frames.is_empty() {
                    self.end_marked(run);
                }
            }
            _ => emit::instr(&mut run.out, i),
        }
    }

    // Graphics state

    fn ext_gstate(&mut self, run: &mut Run, res: &Res, name: &[u8]) {
        let Some(o) = res.get(self.doc, b"ExtGState", name) else {
            return;
        };
        let doc = &*self.doc;
        let d = doc.resolve(&o);
        let Some(d) = d.as_dict() else { return };
        if let Some(w) = doc.get_in(d, b"LW").as_f64() {
            run.gs.line_width = w.abs();
        }
        if let Some(a) = doc.get_in(d, b"D").as_array()
            && let [arr, phase] = a
        {
            run.gs.dash = doc
                .resolve(arr)
                .as_array()
                .unwrap_or_default()
                .iter()
                .filter_map(Object::as_f64)
                .collect();
            run.gs.phase = phase.as_f64().unwrap_or(0.0);
        }
        if let Some(a) = doc.get_in(d, b"Font").as_array()
            && let [f, size] = a
        {
            run.gs.font = self.load_font(f);
            run.gs.font_ref = f.as_objref();
            run.gs.size = size.as_f64().unwrap_or(run.gs.size);
            run.gs.font_name = None;
        }
    }

    fn load_font(&mut self, o: &Object) -> Option<Arc<Font>> {
        let doc = &*self.doc;
        if let Object::Ref(r) = o
            && let Some(f) = self.fonts.get(r)
        {
            return Some(f.clone());
        }
        let d = doc.resolve(o);
        let f = Arc::new(Font::load_with(doc, d.as_dict()?, Some(&*self.programs)));
        if let Object::Ref(r) = o {
            self.fonts.insert(*r, f.clone());
        }
        Some(f)
    }

    // Paths

    fn path_op(&mut self, run: &mut Run, i: &Instr<'_, '_>) {
        let ctm = run.gs.ctm;
        let pt = |x: f32, y: f32| ctm.apply(Point::new(f64::from(x), f64::from(y)));
        let start_if_needed = |run: &mut Run| {
            if run.new_sub || run.subs.is_empty() {
                let p = run.current().unwrap_or_default();
                let p = if run.new_sub {
                    run.subs.last().map_or(p, |s| s.start)
                } else {
                    p
                };
                run.subs.push(Sub::new(p));
                run.new_sub = false;
            }
        };
        match i.op {
            Op::MoveTo => {
                if let Some([x, y]) = i.numbers() {
                    run.subs.push(Sub::new(pt(x, y)));
                    run.new_sub = false;
                }
            }
            Op::LineTo => {
                if let Some([x, y]) = i.numbers() {
                    start_if_needed(run);
                    let a = run.current().unwrap_or_default();
                    run.subs
                        .last_mut()
                        .unwrap()
                        .segs
                        .push(Seg::Line(a, pt(x, y)));
                }
            }
            Op::CurveTo | Op::CurveToV | Op::CurveToY => {
                let pts: Option<[Point; 3]> = match i.op {
                    Op::CurveTo => i
                        .numbers::<6>()
                        .map(|[a, b, c, d, e, f]| [pt(a, b), pt(c, d), pt(e, f)]),
                    Op::CurveToV => i.numbers::<4>().map(|[c, d, e, f]| {
                        let p0 = run.current().unwrap_or(pt(c, d));
                        [p0, pt(c, d), pt(e, f)]
                    }),
                    _ => i
                        .numbers::<4>()
                        .map(|[a, b, e, f]| [pt(a, b), pt(e, f), pt(e, f)]),
                };
                if let Some([b, c, d]) = pts {
                    start_if_needed(run);
                    let a = run.current().unwrap_or_default();
                    run.subs
                        .last_mut()
                        .unwrap()
                        .segs
                        .push(Seg::Cubic(a, b, c, d));
                }
            }
            Op::ClosePath => {
                if let Some(s) = run.subs.last_mut() {
                    s.closed = true;
                    run.new_sub = true;
                }
            }
            Op::Rect => {
                if let Some([x, y, w, h]) = i.numbers() {
                    let c = [pt(x, y), pt(x + w, y), pt(x + w, y + h), pt(x, y + h)];
                    let mut s = Sub::new(c[0]);
                    for k in 0..4 {
                        s.segs.push(Seg::Line(c[k], c[(k + 1) % 4]));
                    }
                    s.closed = true;
                    run.subs.push(s);
                    run.new_sub = true;
                }
            }
            _ => {}
        }
    }

    fn paint(&mut self, run: &mut Run, i: &Instr<'_, '_>) {
        let mut subs = std::mem::take(&mut run.subs);
        let raw = std::mem::take(&mut run.path_raw);
        let clip = run.clip.take();
        run.new_sub = false;
        let (fill, even_odd, stroke, close) = match i.op {
            Op::Stroke => (false, false, true, false),
            Op::CloseStroke => (false, false, true, true),
            Op::Fill | Op::FillCompat => (true, false, false, false),
            Op::FillEvenOdd => (true, true, false, false),
            Op::FillStroke => (true, false, true, false),
            Op::FillStrokeEvenOdd => (true, true, true, false),
            Op::CloseFillStroke => (true, false, true, true),
            Op::CloseFillStrokeEvenOdd => (true, true, true, true),
            _ => (false, false, false, false),
        };
        let clip_op: &[u8] = match clip {
            Some(true) => b"W*\n",
            Some(false) => b"W\n",
            None => b"",
        };
        if self.hit_all(run) {
            // Hidden content: keep only the clip, which later visible content may rely on.
            if clip.is_some() {
                run.out.extend_from_slice(&raw);
                run.out.extend_from_slice(clip_op);
                run.out.extend_from_slice(b"n\n");
            }
            if fill || stroke {
                run.touch();
            }
            return;
        }
        let bbox = subs.iter().map(Sub::bbox).reduce(|a, b| a.union(&b));
        if let Some(b) = bbox {
            if fill {
                run.painted(b, Shape::Fill(Area::new(&subs, even_odd)), true, false);
            }
            if stroke {
                let m = &run.gs.ctm;
                let w =
                    run.gs.line_width.max(1.0) * (m.a.abs() + m.b.abs()).max(m.c.abs() + m.d.abs());
                let grown = Rect::new(b.x0 - w, b.y0 - w, b.x1 + w, b.y1 + w);
                let shape = stroke_shape(&run.gs, &subs, close);
                run.painted(grown, shape, false, true);
            }
        }
        let meets = bbox.is_some_and(|b| {
            self.regions.iter().any(|r| {
                let rb = r.bbox();
                b.x0 <= rb.x1 && rb.x0 <= b.x1 && b.y0 <= rb.y1 && rb.y0 <= b.y1
            })
        });
        if !meets {
            run.out.extend_from_slice(&raw);
            run.out.extend_from_slice(clip_op);
            emit::instr(&mut run.out, i);
            return;
        }
        if close && let Some(s) = subs.last_mut() {
            s.closed = true;
        }
        let Some(to_user) = run.gs.ctm.invert() else {
            // Nothing visible is drawn through a singular matrix; the clip stays.
            if clip.is_some() {
                run.out.extend_from_slice(&raw);
                run.out.extend_from_slice(clip_op);
                run.out.extend_from_slice(b"n\n");
            }
            run.touch();
            return;
        };
        let area = if fill || clip.is_some() {
            let mut a = subs.clone();
            for r in &self.regions {
                a = path::fill_minus(a, r);
            }
            Some(a)
        } else {
            None
        };
        let lines = if stroke {
            let mut l = subs.clone();
            for r in &self.regions {
                l = path::stroke_minus(l, r, &to_user);
            }
            Some(l)
        } else {
            None
        };
        if area.as_ref().is_none_or(|a| *a == subs) && lines.as_ref().is_none_or(|l| *l == subs) {
            run.out.extend_from_slice(&raw);
            run.out.extend_from_slice(clip_op);
            emit::instr(&mut run.out, i);
            return;
        }
        let user = |subs: &[Sub]| -> Vec<Sub> {
            subs.iter()
                .map(|s| Sub {
                    start: to_user.apply(s.start),
                    segs: s.segs.iter().map(|g| g.transform(&to_user)).collect(),
                    closed: s.closed,
                    dash_offset: s.dash_offset,
                })
                .collect()
        };
        let out = &mut run.out;
        if fill
            && let Some(a) = &area
            && !a.is_empty()
        {
            emit::path(out, &user(a));
            out.extend_from_slice(if even_odd { b"f*\n" } else { b"f\n" });
        }
        if let Some(l) = &lines
            && !l.is_empty()
        {
            let l = user(l);
            if run.gs.dash.is_empty() || l.iter().all(|s| s.dash_offset == 0.0) {
                emit::path(out, &l);
                out.extend_from_slice(b"S\n");
            } else {
                for s in &l {
                    dash(out, &run.gs.dash, run.gs.phase + s.dash_offset);
                    emit::path(out, std::slice::from_ref(s));
                    out.extend_from_slice(b"S\n");
                }
                dash(out, &run.gs.dash, run.gs.phase);
            }
        }
        if clip.is_some() {
            match &area {
                Some(a) if !a.is_empty() => emit::path(out, &user(a)),
                _ => out.extend_from_slice(b"0 0 0 0 re\n"),
            }
            out.extend_from_slice(clip_op);
            out.extend_from_slice(b"n\n");
        }
        self.stats.paths_changed += 1;
        run.touch();
    }

    // Text

    /// Shows strings and adjustments, writing them back without the glyphs under regions.
    /// `prefix` replaces the positioning part of `'` and `"`.
    fn show(&mut self, run: &mut Run, i: &Instr<'_, '_>, items: &[Operand<'_>], prefix: &[u8]) {
        let gs = run.gs.clone();
        let m = run.tm.then(&gs.ctm);
        let hit_all = self.hit_all(run);
        let (size, h) = (gs.size, gs.th);
        let Some(font) = gs.font.clone() else {
            self.show_unknown_font(run, i, items, prefix, &m);
            return;
        };
        let vertical = font.vertical;
        let (asc, desc) = (
            f64::from(font.ascent) * size,
            f64::from(font.descent) * size,
        );
        let (mut tx, mut ty) = (0.0f64, 0.0f64);
        let mut elems: Vec<Elem> = Vec::new();
        let mut pending = 0.0f64;
        let mut removed = 0usize;
        let mut shown: Option<Rect> = None;
        let mut buf = Vec::new();
        for item in items {
            if let Some(n) = item.number() {
                let n = f64::from(n);
                let adj = n / 1000.0 * size;
                if vertical {
                    ty -= adj;
                } else {
                    tx -= adj * h;
                }
                pending += n;
                continue;
            }
            buf.clear();
            if !item.string_into(&mut buf) {
                continue;
            }
            let mut s = &buf[..];
            while !s.is_empty() {
                let (code, len) = font.next_code(s);
                let len = len.clamp(1, s.len());
                let bytes = &s[..len];
                s = &s[len..];
                let w0 = f64::from(font.width(code));
                let spacing = gs.tc + if len == 1 && code == 32 { gs.tw } else { 0.0 };
                let width = w0 * size * h;
                let (gx, gy, adv) = if vertical {
                    let (w1, v) = font.vertical_metrics(code);
                    let gx = -f64::from(v.x) * size * h;
                    let gy = ty - f64::from(v.y) * size;
                    let adv = f64::from(w1) * size + spacing;
                    ty += adv;
                    (gx, gy, adv)
                } else {
                    let gx = tx;
                    let adv = w0 * size + spacing;
                    tx += adv * h;
                    (gx, 0.0, adv)
                };
                let w = if width > 0.0 { width } else { 0.25 * size };
                let y = gy + gs.rise;
                let quad = [
                    Point::new(gx, y + desc),
                    Point::new(gx + w, y + desc),
                    Point::new(gx + w, y + asc),
                    Point::new(gx, y + asc),
                ]
                .map(|p| m.apply(p));
                let qb = Rect::from_points(&quad);
                shown = Some(shown.map_or(qb, |s| s.union(&qb)));
                let hit = hit_all || self.regions.iter().any(|r| r.hits(&quad, self.overlap));
                if hit {
                    removed += 1;
                    if let Some(r) = gs.font_ref {
                        self.removed
                            .entry(r)
                            .or_insert_with(|| (font.clone(), Default::default()))
                            .1
                            .insert(code);
                    }
                    if size != 0.0 {
                        pending -= adv * 1000.0 / size;
                    }
                } else {
                    if pending != 0.0 {
                        elems.push(Elem::Num(pending));
                        pending = 0.0;
                    }
                    match elems.last_mut() {
                        Some(Elem::Str(v)) => v.extend_from_slice(bytes),
                        _ => elems.push(Elem::Str(bytes.to_vec())),
                    }
                }
            }
        }
        if let Some(b) = shown
            && !hit_all
        {
            text_painted(run, b);
        }
        let next = Matrix::translate(tx, ty).then(&run.tm);
        if next.is_finite() {
            run.tm = next;
        }
        if removed == 0 {
            emit::instr(&mut run.out, i);
            return;
        }
        self.stats.glyphs_removed += removed;
        run.touch();
        let out = &mut run.out;
        out.extend_from_slice(prefix);
        if size == 0.0 {
            // Glyphs of size 0 cannot be replaced by adjustments: move by the whole
            // advance at size 1 instead (they draw nothing).
            if let Some(name) = &gs.font_name
                && (tx != 0.0 || ty != 0.0)
            {
                let n = if vertical {
                    -ty * 1000.0
                } else if h != 0.0 {
                    -tx / h * 1000.0
                } else {
                    0.0
                };
                out.push(b'/');
                out.extend_from_slice(name);
                out.extend_from_slice(b" 1 Tf [");
                emit::num(out, n);
                out.extend_from_slice(b"] TJ /");
                out.extend_from_slice(name);
                out.extend_from_slice(b" 0 Tf\n");
            }
            return;
        }
        if pending.abs() > 1e-9 {
            elems.push(Elem::Num(pending));
        }
        if elems.is_empty() {
            return;
        }
        out.push(b'[');
        for e in &elems {
            match e {
                Elem::Str(b) => emit::hex_string(out, b),
                Elem::Num(n) => {
                    out.push(b' ');
                    emit::num(out, *n);
                    out.push(b' ');
                }
            }
        }
        out.extend_from_slice(b"] TJ\n");
    }

    /// Text in a font that is missing: its extent is estimated (half an em per byte) and
    /// the whole operator goes when that meets a region.
    fn show_unknown_font(
        &mut self,
        run: &mut Run,
        i: &Instr<'_, '_>,
        items: &[Operand<'_>],
        prefix: &[u8],
        m: &Matrix,
    ) {
        let mut bytes = 0usize;
        let mut buf = Vec::new();
        for item in items {
            buf.clear();
            if item.string_into(&mut buf) {
                bytes += buf.len();
            }
        }
        let size = run.gs.size;
        let quad = Rect::new(
            0.0,
            -0.2 * size,
            0.5 * size * bytes as f64 * run.gs.th,
            0.8 * size,
        )
        .corners()
        .map(|p| m.apply(p));
        let hit = self.hit_all(run) || self.regions.iter().any(|r| r.touches(&quad));
        if bytes > 0 && !self.hit_all(run) {
            text_painted(run, Rect::from_points(&quad));
        }
        if bytes == 0 || !hit {
            emit::instr(&mut run.out, i);
            return;
        }
        self.stats.glyphs_removed += bytes;
        run.touch();
        run.out.extend_from_slice(prefix);
    }

    // XObjects and images

    fn xobject(&mut self, run: &mut Run, res: &mut Res, i: &Instr<'_, '_>, name: &[u8]) {
        let Some(raw) = res.get(self.doc, b"XObject", name) else {
            emit::instr(&mut run.out, i);
            return;
        };
        let obj = self.doc.resolve(&raw).into_owned_arc();
        let Some(s) = obj.as_stream() else {
            emit::instr(&mut run.out, i);
            return;
        };
        let num = raw.as_objref().map_or(0, |r| r.num);
        if self.hit_all(run) || self.oc_hidden(&s.dict) {
            run.touch();
            self.tainted.insert(num);
            if s.dict.name(b"Subtype") != Some(b"Image") {
                self.note_glyphs(s, num, &run.gs.ctm.clone(), res);
            }
            return;
        }
        let ctm = run.gs.ctm;
        match s.dict.name(b"Subtype") {
            Some(b"Image") => {
                let quad = Rect::new(0.0, 0.0, 1.0, 1.0)
                    .corners()
                    .map(|p| ctm.apply(p));
                // A stencil mask paints with the fill colour.
                if self.doc.get_in(&s.dict, b"ImageMask").as_bool() == Some(true) {
                    run.painted(Rect::from_points(&quad), Shape::Unknown, true, false);
                }
                if !self.regions.iter().any(|r| r.touches(&quad)) {
                    emit::instr(&mut run.out, i);
                    return;
                }
                let k = (num, key(&ctm));
                let new = match self.cache.get(&k) {
                    Some(n) => *n,
                    None => {
                        let n = self.redact_image(s, &ctm, &quad, res);
                        let n = match n {
                            ImageResult::Unchanged => {
                                emit::instr(&mut run.out, i);
                                return;
                            }
                            ImageResult::Drop => None,
                            ImageResult::New(stream) => Some(self.doc.add(stream)),
                        };
                        if num != 0 {
                            self.cache.insert(k, n);
                        }
                        n
                    }
                };
                self.tainted.insert(num);
                run.touch();
                if let Some(r) = new {
                    let n = res.add(self.doc, b"XObject", Object::Ref(r));
                    do_op(&mut run.out, &n);
                }
            }
            Some(b"Form") | None => {
                let fm = s
                    .dict
                    .get(b"Matrix")
                    .and_then(|m| Matrix::from_object(self.doc, m))
                    .unwrap_or_default();
                let fctm = fm.then(&ctm);
                let bbox = s
                    .dict
                    .get(b"BBox")
                    .and_then(|b| Rect::from_object(self.doc, b));
                let quad = bbox.map(|b| b.corners().map(|p| fctm.apply(p)));
                let meets = match &quad {
                    Some(q) => self.regions.iter().any(|r| r.touches(q)),
                    None => true,
                };
                // The form paints with the colours it inherits unless it sets its own.
                let painted =
                    quad.map_or(Rect::new(-1e7, -1e7, 1e7, 1e7), |q| Rect::from_points(&q));
                run.painted(painted, Shape::Unknown, true, true);
                if !meets && self.hidden_oc.is_none() {
                    emit::instr(&mut run.out, i);
                    return;
                }
                let k = (num, key(&fctm));
                let new = match self.cache.get(&k) {
                    Some(n) => *n,
                    None => {
                        let n = match self.redact_form(s, num, &fctm, res) {
                            FormResult::Unchanged => {
                                emit::instr(&mut run.out, i);
                                return;
                            }
                            FormResult::Drop => None,
                            FormResult::New(stream) => {
                                self.stats.forms_copied += 1;
                                Some(self.doc.add(stream))
                            }
                        };
                        if num != 0 {
                            self.cache.insert(k, n);
                        }
                        n
                    }
                };
                self.tainted.insert(num);
                run.touch();
                if let Some(r) = new {
                    let n = res.add(self.doc, b"XObject", Object::Ref(r));
                    do_op(&mut run.out, &n);
                }
            }
            _ => emit::instr(&mut run.out, i),
        }
    }

    /// Notes the glyphs a dropped form shows as removed, so fonts can lose them.
    fn note_glyphs(&mut self, s: &Stream, num: u32, ctm: &Matrix, parent: &Res) {
        let stats = self.stats.clone();
        let warnings = self.warnings.len();
        let drop_all = std::mem::replace(&mut self.drop_all, true);
        let _ = self.redact_form(s, num, ctm, parent);
        self.drop_all = drop_all;
        self.stats = stats;
        self.warnings.truncate(warnings);
    }

    fn oc_hidden(&self, d: &Dict) -> bool {
        match (&self.hidden_oc, d.get(b"OC")) {
            (Some(oc), Some(o)) => !oc.visible(self.doc, o),
            _ => false,
        }
    }

    pub(crate) fn redact_form(
        &mut self,
        s: &Stream,
        num: u32,
        ctm: &Matrix,
        parent: &Res,
    ) -> FormResult {
        if self.forms.len() >= MAX_FORM_DEPTH || (num != 0 && self.forms.contains(&num)) {
            self.warnings
                .push("a form nested too deeply or recursively was dropped".into());
            return FormResult::Drop;
        }
        let Ok(data) = self.doc.decode(s) else {
            self.warnings.push("an undecodable form was dropped".into());
            return FormResult::Drop;
        };
        let mut res = match s.dict.get(b"Resources") {
            Some(o) => Res::new(self.doc, Some(o)),
            None => Res {
                dict: parent.dict.clone(),
                changed: false,
                counter: parent.counter + 100,
            },
        };
        let sp = self.doc.get_in(&s.dict, b"StructParents").as_int();
        self.forms.push(num);
        let (content, changed) = self.rewrite(&data.data, &mut res, *ctm, sp);
        self.forms.pop();
        if !changed {
            return FormResult::Unchanged;
        }
        let mut d = Dict::new();
        for (k, v) in s.dict.iter() {
            if !matches!(
                k.as_bytes(),
                b"Filter" | b"DecodeParms" | b"Length" | b"Resources"
            ) {
                d.set(k.as_bytes(), v.clone());
            }
        }
        // A form inheriting its resources keeps inheriting them unless they changed, so
        // patterns the caller replaces reach it.
        let inherits = !s.dict.contains(b"Resources") && !res.changed && res.dict == parent.dict;
        if !inherits {
            d.set(b"Resources", Object::Dict(res.dict));
        }
        d.set(b"Filter", Object::name(b"FlateDecode"));
        FormResult::New(Stream::new(d, pagebara_filters::encode_flate(&content, 6)))
    }

    /// The image with the pixels under regions overwritten, its masks too.
    fn redact_image(
        &mut self,
        s: &Stream,
        ctm: &Matrix,
        quad: &[Point; 4],
        res: &Res,
    ) -> ImageResult {
        if quad
            .iter()
            .all(|&p| self.regions.iter().any(|r| r.contains(p)))
            && self
                .regions
                .iter()
                .any(|r| quad.iter().all(|&p| r.contains(p)))
        {
            self.stats.images_removed += 1;
            return ImageResult::Drop;
        }
        let regions: Vec<&Region> = self.regions.iter().collect();
        let Some(mut img) = image::decode(self.doc, s, Some(&res.dict)) else {
            self.warnings
                .push("an image under a region could not be decoded and was removed".into());
            self.stats.images_removed += 1;
            return ImageResult::Drop;
        };
        let Some(mask) = image::coverage(img.w, img.h, ctm, &regions) else {
            return ImageResult::Unchanged;
        };
        let stencil = self.doc.get_in(&s.dict, b"ImageMask").as_bool() == Some(true);
        let value = if stencil {
            not_painted(self.doc, &s.dict)
        } else {
            0
        };
        self.stats.pixels_blanked += img.blank(&mask, value);
        let (mut dict, data) = img.encode(&s.dict);
        for key in [&b"SMask"[..], b"Mask"] {
            let Some(m) = s.dict.get(key) else { continue };
            let mo = self.doc.resolve(m).into_owned_arc();
            let Some(ms) = mo.as_stream() else { continue };
            let Some(mut mi) = image::decode(self.doc, ms, None) else {
                self.warnings
                    .push("an image mask could not be decoded; the image was removed".into());
                self.stats.images_removed += 1;
                return ImageResult::Drop;
            };
            if let Some(mm) = image::coverage(mi.w, mi.h, ctm, &regions) {
                let v = if key == b"Mask" {
                    not_painted(self.doc, &ms.dict)
                } else {
                    0
                };
                mi.blank(&mm, v);
                let (md, mdata) = mi.encode(&ms.dict);
                if let Some(r) = m.as_objref() {
                    self.tainted.insert(r.num);
                }
                let r = self.doc.add(Stream::new(md, mdata));
                dict.set(key, Object::Ref(r));
            }
        }
        if let Some(alpha) = img.alpha.take() {
            let a = Samples {
                w: img.w,
                h: img.h,
                n: 1,
                bpc: 8,
                data: alpha,
                color_space: Some(Object::name(b"DeviceGray")),
                drop_decode: true,
                alpha: None,
            };
            let mut ad = Dict::new();
            ad.set(b"Type", Object::name(b"XObject"));
            ad.set(b"Subtype", Object::name(b"Image"));
            let (ad, adata) = a.encode(&ad);
            let r = self.doc.add(Stream::new(ad, adata));
            dict.set(b"SMask", Object::Ref(r));
        }
        self.stats.images_blanked += 1;
        ImageResult::New(Stream::new(dict, data))
    }

    fn inline_image(&mut self, run: &mut Run, res: &Res, i: &Instr<'_, '_>) {
        if self.hit_all(run) {
            run.touch();
            return;
        }
        let ctm = run.gs.ctm;
        let quad = Rect::new(0.0, 0.0, 1.0, 1.0)
            .corners()
            .map(|p| ctm.apply(p));
        let stencil = i.args.as_chunks::<2>().0.iter().any(|kv| {
            kv[0]
                .name()
                .is_some_and(|k| matches!(&k[..], b"IM" | b"ImageMask"))
                && matches!(i.object(&kv[1]), Object::Bool(true))
        });
        if stencil {
            run.painted(Rect::from_points(&quad), Shape::Unknown, true, false);
        }
        if !self.regions.iter().any(|r| r.touches(&quad)) {
            emit::instr(&mut run.out, i);
            return;
        }
        let mut dict = Dict::new();
        let mut kept: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        for kv in i.args.as_chunks::<2>().0 {
            let Some(k) = kv[0].name() else { continue };
            let full = expand_key(&k);
            dict.set(full, i.object(&kv[1]));
            if !matches!(
                full,
                b"Width" | b"Height" | b"BitsPerComponent" | b"Filter" | b"DecodeParms" | b"Length"
            ) {
                let mut v = Vec::new();
                emit::operand(&mut v, i, &kv[1]);
                kept.push((full.to_vec(), v));
            }
        }
        let stream = Stream::new(dict, i.data);
        let s = &stream;
        match self.redact_image(s, &ctm, &quad, res) {
            ImageResult::Unchanged => emit::instr(&mut run.out, i),
            ImageResult::Drop => run.touch(),
            ImageResult::New(new) => {
                run.touch();
                let d = &new.dict;
                let data = match &new.data {
                    pagebara_syntax::StreamData::Owned(b) => b.to_vec(),
                    _ => Vec::new(),
                };
                let out = &mut run.out;
                out.extend_from_slice(b"BI");
                // Inline images are never JPX, so the colour space stays as written.
                for (k, v) in &kept {
                    if k == b"Decode" && !d.contains(b"Decode") {
                        continue;
                    }
                    out.extend_from_slice(b" /");
                    out.extend_from_slice(k);
                    out.push(b' ');
                    out.extend_from_slice(v);
                }
                for k in [&b"Width"[..], b"Height", b"BitsPerComponent"] {
                    if let Some(v) = d.get(k) {
                        out.extend_from_slice(b" /");
                        out.extend_from_slice(k);
                        out.push(b' ');
                        write_object(out, v);
                    }
                }
                out.extend_from_slice(b" /Filter [/ASCIIHexDecode /FlateDecode] ID\n");
                for b in &data {
                    out.extend_from_slice(format!("{b:02X}").as_bytes());
                }
                out.extend_from_slice(b">\nEI\n");
            }
        }
    }

    fn shade(&mut self, run: &mut Run, res: &mut Res, i: &Instr<'_, '_>) {
        if self.hit_all(run) {
            run.touch();
            return;
        }
        if self.regions.is_empty() || run.in_text {
            emit::instr(&mut run.out, i);
            return;
        }
        let ctm = run.gs.ctm;
        let Some(to_user) = ctm.invert() else {
            run.touch();
            return;
        };
        // The shading loses what it holds inside regions; what is drawn is clipped.
        let mut name = None;
        if let Some(n) = i.args.last().and_then(Operand::name)
            && let Some(so) = res.get(self.doc, b"Shading", &n)
        {
            let num = so.as_objref().map_or(0, |r| r.num);
            let k = (num, key(&ctm));
            let new = match self.meshes.get(&k).filter(|_| num != 0) {
                Some(v) => *v,
                None => {
                    let v = shading::redact(self.doc, &so, &ctm, &self.regions).map(|c| {
                        self.stats.mesh_parts_removed += c.parts;
                        self.stats.shading_samples_blanked += c.samples;
                        (!c.empty).then(|| self.doc.add(c.shading))
                    });
                    if v.is_some() {
                        self.tainted.insert(num);
                    }
                    if num != 0 {
                        self.meshes.insert(k, v);
                    }
                    v
                }
            };
            match new {
                None => {
                    run.shadings_kept.insert(n.to_vec());
                }
                Some(None) => {
                    run.shadings_replaced.insert(n.to_vec());
                    run.touch();
                    return;
                }
                Some(Some(r)) => {
                    run.shadings_replaced.insert(n.to_vec());
                    name = Some(res.add(self.doc, b"Shading", Object::Ref(r)));
                }
            }
        }
        let out = &mut run.out;
        out.extend_from_slice(b"q\n");
        let big = Rect::new(-1e5, -1e5, 1e5, 1e5);
        for r in &self.regions {
            for poly in [&big.corners()[..], r.points()] {
                for (k, p) in poly.iter().enumerate() {
                    let p = to_user.apply(*p);
                    emit::nums(out, &[p.x, p.y]);
                    out.extend_from_slice(if k == 0 { b"m\n" } else { b"l\n" });
                }
                out.extend_from_slice(b"h\n");
            }
            out.extend_from_slice(b"W* n\n");
        }
        match &name {
            Some(n) => {
                out.push(b'/');
                out.extend_from_slice(n);
                out.extend_from_slice(b" sh\n");
            }
            None => emit::instr(out, i),
        }
        out.extend_from_slice(b"Q\n");
        self.stats.shadings_clipped += 1;
        if name.is_some() {
            run.touch();
        }
        run.changed = true;
    }

    // Marked content

    fn begin_marked(&mut self, run: &mut Run, res: &Res, i: &Instr<'_, '_>) {
        let [.., tag, props] = i.args else {
            emit::instr(&mut run.out, i);
            return;
        };
        let (dict, inline) = match props {
            Operand::Dict(_) => (i.dict(props).cloned(), true),
            Operand::Name(_) => {
                let n = props.name().unwrap_or_default();
                let d = res
                    .get(self.doc, b"Properties", &n)
                    .and_then(|o| self.doc.resolve(&o).as_dict().cloned());
                (d, false)
            }
            _ => (None, false),
        };
        let hides = match (&self.hidden_oc, tag.raw_name(), props) {
            (Some(oc), Some(b"OC"), Operand::Name(_)) => {
                let n = props.name().unwrap_or_default();
                res.get(self.doc, b"Properties", &n)
                    .is_some_and(|o| !oc.visible(self.doc, &o))
            }
            _ => false,
        };
        let mcid = dict.as_ref().and_then(|d| d.int(b"MCID"));
        let out = &mut run.out;
        emit::operand(out, i, tag);
        out.push(b' ');
        let start = out.len();
        emit::operand(out, i, props);
        let end = out.len();
        out.extend_from_slice(b" BDC\n");
        let describing = dict
            .filter(|d| inline && DESCRIBING.iter().any(|k| d.contains(k)))
            .map(|d| (start, end, d));
        if hides {
            run.hidden += 1;
        }
        run.frames.push(Frame {
            props: describing,
            mcid,
            touched: false,
            hides,
        });
    }

    fn end_marked(&mut self, run: &mut Run) {
        let Some(f) = run.frames.pop() else { return };
        if f.hides {
            run.hidden -= 1;
        }
        if f.touched {
            if let Some((s, e, mut d)) = f.props {
                for k in DESCRIBING {
                    d.remove(k);
                }
                let mut rep = Vec::new();
                write_object(&mut rep, &Object::Dict(d));
                run.patches.push((s, e, rep));
            }
            if let (Some(sp), Some(m)) = (run.struct_parents, f.mcid) {
                self.mcids.push((sp, m));
            }
        }
        run.out.extend_from_slice(b"EMC\n");
    }
}

enum Elem {
    Str(Vec<u8>),
    Num(f64),
}

enum ImageResult {
    Unchanged,
    Drop,
    New(Stream),
}

pub(crate) enum FormResult {
    Unchanged,
    Drop,
    New(Stream),
}

/// The sample value of a stencil mask that leaves the page alone.
fn not_painted(doc: &Document, d: &Dict) -> u32 {
    let inverted = doc
        .get_in(d, b"Decode")
        .as_array()
        .and_then(|a| a.first().and_then(Object::as_f64))
        == Some(1.0);
    if inverted { 0 } else { 1 }
}

/// Notes text shown in box `b` as painted the way the rendering mode says. The box grows by
/// a unit: the cells drawn for a tiling pattern end at it, and an edge there right on the
/// glyphs' edge (a flat top at the box's top) would be smoothed twice, lighter than before.
fn text_painted(run: &mut Run, b: Rect) {
    let b = Rect::new(b.x0 - 1.0, b.y0 - 1.0, b.x1 + 1.0, b.y1 + 1.0);
    let mode = run.gs.render;
    let fill = matches!(mode, 0 | 2 | 4 | 6);
    let stroke = matches!(mode, 1 | 2 | 5 | 6);
    if fill || stroke {
        run.painted(b, Shape::Unknown, fill, stroke);
    }
    if (4..=7).contains(&mode) {
        run.text_clip = true;
    }
}

/// The shape a stroke of `subs` covers for sure: a band of the least half width it has
/// along each segment, when it is solid.
fn stroke_shape(gs: &Gs, subs: &[Sub], close: bool) -> Shape {
    let m = &gs.ctm;
    let t = m.a * m.a + m.b * m.b + m.c * m.c + m.d * m.d;
    let det = m.a * m.d - m.b * m.c;
    let least = ((t - (t * t - 4.0 * det * det).max(0.0).sqrt()) / 2.0)
        .max(0.0)
        .sqrt();
    let half = gs.line_width * least / 2.0;
    if !gs.dash.is_empty() || half <= 0.0 {
        return Shape::Unknown;
    }
    let mut lines = Vec::new();
    for (k, s) in subs.iter().enumerate() {
        let mut p = s.polyline();
        if s.closed || (close && k + 1 == subs.len()) {
            p.push(s.start);
        }
        lines.extend(p.windows(2).map(|w| (w[0], w[1])));
    }
    Shape::Stroke(lines, half)
}

fn next_line(run: &mut Run, x: f64, y: f64) {
    let m = Matrix::translate(x, y).then(&run.tlm);
    if m.is_finite() {
        run.tlm = m;
        run.tm = m;
    }
}

fn dash(out: &mut Vec<u8>, dash: &[f64], phase: f64) {
    out.push(b'[');
    emit::nums(out, dash);
    out.extend_from_slice(b"] ");
    emit::num(out, phase);
    out.extend_from_slice(b" d\n");
}

fn do_op(out: &mut Vec<u8>, name: &[u8]) {
    out.push(b'/');
    out.extend_from_slice(name);
    out.extend_from_slice(b" Do\n");
}

fn expand_key(k: &[u8]) -> &[u8] {
    match k {
        b"BPC" => b"BitsPerComponent",
        b"CS" => b"ColorSpace",
        b"D" => b"Decode",
        b"DP" => b"DecodeParms",
        b"F" => b"Filter",
        b"H" => b"Height",
        b"IM" => b"ImageMask",
        b"I" => b"Interpolate",
        b"W" => b"Width",
        b"L" => b"Length",
        _ => k,
    }
}

trait IntoArc {
    fn into_owned_arc(self) -> Arc<Object>;
}

impl IntoArc for pagebara_syntax::Resolved<'_> {
    fn into_owned_arc(self) -> Arc<Object> {
        match self {
            pagebara_syntax::Resolved::Shared(a) => a,
            pagebara_syntax::Resolved::Borrowed(o) => Arc::new(o.clone()),
        }
    }
}
