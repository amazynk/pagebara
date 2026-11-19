//! Reading a page into objects: the content streams are run with the graphics and text
//! state, and every drawing operator becomes an object (consecutive path painting with
//! one state one object), remembering the bytes it came from and the state it needs.

use std::collections::HashMap;
use std::sync::Arc;

use pagebara_content::font::{Font, ProgramLoader, embedded_programs};
use pagebara_content::lex::Operand;
use pagebara_content::oc::OptionalContent;
use pagebara_content::{ContentParser, Instr, Matrix, Op, Point, Rect};
use pagebara_syntax::{Dict, Document, Name, ObjRef, Object};

use crate::emit;
use crate::geom::{PageFrame, corners, intersect};
use crate::model::{
    FontInfo, FormInfo, ImageInfo, MarkedContent, ObjectData, Paint, PathInfo, ShadingInfo, Span,
    StreamId,
};

/// Forms nested deeper are not read.
const MAX_FORM_DEPTH: usize = 16;
/// Operators read per page, forms included.
const MAX_OPERATORS: u64 = 20_000_000;
/// Paths painted one after another join one object up to this many.
const MAX_GROUP: u32 = 4096;
const MAX_SAVE_DEPTH: usize = 256;

/// A content stream that was read.
#[derive(Clone, Debug)]
pub(crate) struct StreamRec {
    pub data: Arc<Vec<u8>>,
    pub kind: StreamKind,
    /// The resources its names refer to.
    pub res: Dict,
    /// For forms: the stream and span of the `Do` drawing it, and the form's item.
    pub drawn_by: Option<(StreamId, (u32, u32), usize)>,
    /// `q` left open at the end.
    pub open_saves: u32,
    /// The stream ends inside a text object.
    pub open_text: bool,
}

#[derive(Clone, Debug)]
pub(crate) enum StreamKind {
    /// The page's `/Contents`: each stream's range in the joined data.
    Page {
        parts: Vec<(Option<ObjRef>, u32, u32)>,
    },
    Form {
        object: Option<ObjRef>,
    },
}

/// The graphics state an object needs, enough to draw it elsewhere.
#[derive(Clone, Debug)]
pub(crate) struct Snap {
    pub ctm: Matrix,
    /// Clip records in effect, oldest first.
    pub clips: Vec<u32>,
    /// ExtGState names applied since the stream began (raw names).
    pub ext: Vec<Vec<u8>>,
    pub line_width: f32,
    pub cap: u8,
    pub join: u8,
    pub miter: f32,
    /// `d` and `ri` operators as written, when set.
    pub dash: Option<Vec<u8>>,
    /// The dash array is not empty.
    pub dashed: bool,
    pub intent: Option<Vec<u8>>,
    pub fill: Paint,
    pub stroke: Paint,
    /// Operators re-establishing the fill and stroke colours.
    pub fill_ops: Vec<u8>,
    pub stroke_ops: Vec<u8>,
    fill_space_ops: Vec<u8>,
    stroke_space_ops: Vec<u8>,
    pub fill_alpha: f32,
    pub stroke_alpha: f32,
    pub font: Option<FontInfo>,
    /// The `Tf` name as written.
    pub font_raw: Option<Vec<u8>>,
    pub size: f32,
    pub tc: f32,
    pub tw: f32,
    pub th: f32,
    pub tl: f32,
    pub rise: f32,
    pub mode: u8,
}

impl Snap {
    fn new(ctm: Matrix) -> Self {
        Snap {
            ctm,
            clips: Vec::new(),
            ext: Vec::new(),
            line_width: 1.0,
            cap: 0,
            join: 0,
            miter: 10.0,
            dash: None,
            dashed: false,
            intent: None,
            fill: Paint::black(),
            stroke: Paint::black(),
            fill_ops: b"0 g\n".to_vec(),
            stroke_ops: b"0 G\n".to_vec(),
            fill_space_ops: Vec::new(),
            stroke_space_ops: Vec::new(),
            fill_alpha: 1.0,
            stroke_alpha: 1.0,
            font: None,
            font_raw: None,
            size: 0.0,
            tc: 0.0,
            tw: 0.0,
            th: 1.0,
            tl: 0.0,
            rise: 0.0,
            mode: 0,
        }
    }

    /// Operators setting the state other than CTM and clip: line style, ExtGStates,
    /// colours and text state, as `self` has them.
    pub fn state_ops(&self, out: &mut Vec<u8>) {
        for n in &self.ext {
            out.push(b'/');
            out.extend_from_slice(n);
            out.extend_from_slice(b" gs\n");
        }
        emit::nums(out, &[self.line_width.into()]);
        out.extend_from_slice(b"w\n");
        emit::nums(out, &[self.cap.into()]);
        out.extend_from_slice(b"J\n");
        emit::nums(out, &[self.join.into()]);
        out.extend_from_slice(b"j\n");
        emit::nums(out, &[self.miter.into()]);
        out.extend_from_slice(b"M\n");
        if let Some(d) = &self.dash {
            out.extend_from_slice(d);
        }
        if let Some(r) = &self.intent {
            out.extend_from_slice(r);
        }
        out.extend_from_slice(&self.fill_ops);
        out.extend_from_slice(&self.stroke_ops);
    }

    /// Text state operators (valid in a text object).
    pub fn text_ops(&self, out: &mut Vec<u8>) {
        if let Some(f) = &self.font_raw {
            out.push(b'/');
            out.extend_from_slice(f);
            out.push(b' ');
            emit::nums(out, &[self.size.into()]);
            out.extend_from_slice(b"Tf\n");
        }
        emit::nums(
            out,
            &[
                self.tc.into(),
                self.tw.into(),
                (self.th * 100.0).into(),
                self.tl.into(),
                self.rise.into(),
                self.mode.into(),
            ],
        );
        out.extend_from_slice(b"Tc Tw Tz TL Ts Tr\n");
    }
}

/// A clipping path: what it intersects the clip with.
#[derive(Clone, Debug)]
pub(crate) struct ClipRec {
    pub ctm: Matrix,
    /// Path construction operators (in the space of `ctm`) and `W` or `W*`; `None` for
    /// clipping by text, which cannot be repeated elsewhere.
    pub path: Option<Vec<u8>>,
    pub bbox: Rect,
}

/// A marked-content sequence.
#[derive(Clone, Debug)]
pub(crate) struct FrameRec {
    pub stream: StreamId,
    /// The `BMC`/`BDC` operator and the `EMC` closing it.
    pub begin: (u32, u32),
    pub end: Option<(u32, u32)>,
    /// The operator as written, to open the sequence elsewhere.
    pub begin_ops: Vec<u8>,
    pub info: MarkedContent,
    /// The inline property list's bytes, when it has `/ActualText`.
    pub actual_text: Option<(u32, u32)>,
    /// Items drawn inside.
    pub items: Vec<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShowKind {
    Tj,
    TJ,
    /// `'`: next line, then show.
    Quote,
    /// `"`: word and character spacing, next line, then show.
    DoubleQuote,
}

/// Where a run is written and the text state around it.
#[derive(Clone, Debug)]
pub(crate) struct RunSrc {
    pub show: ShowKind,
    /// The string or array operand as written (for `"` also the spacing operands).
    pub operand: Vec<u8>,
    pub prefix: Vec<u8>,
    /// Text line matrix and text matrix before the glyphs, and the text matrix after.
    pub tlm: Matrix,
    pub tm: Matrix,
    pub tm_after: Matrix,
    /// The text object's `BT` start and `ET` end.
    pub bt: (u32, u32),
    /// The show operator.
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Debug)]
pub(crate) enum Source {
    /// Consecutive path painting `start..end`; `clip` is the `W`/`W*` operator of a path
    /// that also clips, so the clip can be kept on its own.
    Paths {
        start: u32,
        end: u32,
        clip: Option<(u32, u32)>,
    },
    /// One operator: `Do`, an inline image, `sh`.
    Op {
        start: u32,
        end: u32,
    },
    Run(RunSrc),
}

#[derive(Clone, Debug)]
pub(crate) enum ItemData {
    Object(ObjectData),
    /// The text of a show operator, as glyphs of the scan's list.
    Run(RunData),
}

#[derive(Clone, Debug)]
pub(crate) struct RunData {
    pub glyphs: Vec<RawGlyph>,
    pub vertical: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct RawGlyph {
    pub code: u32,
    pub bytes: (u32, u32),
    /// Origin in text space relative to the run's text matrix.
    pub x: f32,
    pub y: f32,
    /// Horizontal width (font width × size × scaling).
    pub width: f32,
    pub advance: f32,
    /// The part of the advance that scales with the font size.
    pub scaled: f32,
}

/// Something drawn, in paint order.
#[derive(Clone, Debug)]
pub(crate) struct Item {
    pub data: ItemData,
    pub stream: StreamId,
    pub src: Source,
    pub snap: Arc<Snap>,
    pub bbox: Rect,
    pub quad: [Point; 4],
    /// The form item drawing it.
    pub parent: Option<usize>,
    /// Open frames, outermost first.
    pub frames: Vec<u32>,
    pub hidden: bool,
    /// Bytes of shown strings, for runs.
    pub bytes: Vec<u8>,
}

/// The state between two operators of the page's own stream, for inserting there.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Boundary {
    pub offset: u32,
    pub snap: u32,
    pub in_text: bool,
    /// The innermost open marked-content frame plus one (0 for none).
    pub frame: u32,
}

/// The result of reading a page.
#[derive(Debug, Default)]
pub(crate) struct Scanned {
    pub streams: Vec<StreamRec>,
    pub items: Vec<Item>,
    pub clips: Vec<ClipRec>,
    pub frames: Vec<FrameRec>,
    pub boundaries: Vec<Boundary>,
    pub boundary_snaps: Vec<Arc<Snap>>,
}

/// Per content stream being read.
struct Local {
    stream: StreamId,
    gs: Arc<Snap>,
    stack: Vec<Arc<Snap>>,
    ignored_saves: u32,
    tm: Matrix,
    tlm: Matrix,
    in_text: bool,
    bt_start: u32,
    text_clip: bool,
    text_items: Vec<usize>,
    path_start: Option<u32>,
    path_ops: Vec<u8>,
    subpaths: Vec<Vec<Point>>,
    current: Option<Point>,
    sub_start: Point,
    clip: Option<(bool, u32, u32)>,
    frames: Vec<u32>,
    hidden: u32,
    /// Hidden flags of open frames.
    frame_hides: Vec<bool>,
    group: Option<usize>,
    group_open: bool,
    parent: Option<usize>,
    res: Dict,
}

pub(crate) struct Scanner<'d> {
    doc: &'d Document,
    oc: OptionalContent,
    programs: Arc<ProgramLoader>,
    fonts: HashMap<ObjRef, Arc<Font>>,
    frame: PageFrame,
    out: Scanned,
    forms: Vec<ObjRef>,
    ops: u64,
    boundaries: bool,
    buf: Vec<u8>,
}

impl<'d> Scanner<'d> {
    pub fn new(doc: &'d Document, frame: PageFrame, boundaries: bool) -> Self {
        Scanner {
            doc,
            oc: OptionalContent::new(doc),
            programs: embedded_programs(),
            fonts: HashMap::new(),
            frame,
            out: Scanned::default(),
            forms: Vec::new(),
            ops: 0,
            boundaries,
            buf: Vec::new(),
        }
    }

    /// Reads a page dictionary's content.
    pub fn page(mut self, page: &Dict, res: Dict) -> Scanned {
        let doc = self.doc;
        let contents = doc.get_in(page, b"Contents");
        let mut data = Vec::new();
        let mut parts = Vec::new();
        let mut add = |o: &Object| {
            if let Some(s) = doc.resolve(o).as_stream()
                && let Ok(d) = doc.decode(s)
            {
                if !data.is_empty() {
                    data.push(b'\n');
                }
                let start = data.len() as u32;
                data.extend_from_slice(&d.data);
                parts.push((o.as_objref(), start, data.len() as u32));
            }
        };
        match &*contents {
            Object::Array(a) => a.iter().for_each(&mut add),
            o => add(o),
        }
        self.out.streams.push(StreamRec {
            data: Arc::new(data),
            kind: StreamKind::Page { parts },
            res: res.clone(),
            drawn_by: None,
            open_saves: 0,
            open_text: false,
        });
        self.run(StreamId(0), res, Matrix::IDENTITY, None);
        self.out
    }

    fn run(&mut self, stream: StreamId, res: Dict, ctm: Matrix, parent: Option<usize>) {
        let data = self.out.streams[stream.0 as usize].data.clone();
        // A form inherits the state at its `Do`.
        let base = match parent {
            Some(p) => {
                let mut s = (*self.out.items[p].snap).clone();
                s.ctm = ctm;
                s
            }
            None => Snap::new(ctm),
        };
        let mut l = Local {
            stream,
            gs: Arc::new(base),
            stack: Vec::new(),
            ignored_saves: 0,
            tm: Matrix::IDENTITY,
            tlm: Matrix::IDENTITY,
            in_text: false,
            bt_start: 0,
            text_clip: false,
            text_items: Vec::new(),
            path_start: None,
            path_ops: Vec::new(),
            subpaths: Vec::new(),
            current: None,
            sub_start: Point::default(),
            clip: None,
            frames: Vec::new(),
            hidden: parent.map_or(0, |p| u32::from(self.out.items[p].hidden)),
            frame_hides: Vec::new(),
            group: None,
            group_open: false,
            parent,
            res,
        };
        if let Some(p) = parent
            && let ItemData::Object(ObjectData::Form(f)) = &self.out.items[p].data
            && let Some(b) = f.bbox
        {
            // The form's box clips its content.
            let path = format!("{} {} {} {} re W n\n", b.x0, b.y0, b.x1 - b.x0, b.y1 - b.y0);
            let m = Matrix::IDENTITY;
            let bb = b.transform(&ctm);
            let id = self.add_clip(&l, m, Some(path.into_bytes()), bb);
            Arc::make_mut(&mut l.gs).clips.push(id);
        }
        let mut p = ContentParser::new(&data);
        let page = stream.0 == 0;
        if page && self.boundaries {
            self.boundary(&l, 0);
        }
        loop {
            let start = p.pos() as u32;
            let Some(i) = p.next() else { break };
            self.ops += 1;
            if self.ops > MAX_OPERATORS {
                break;
            }
            let end = i.end as u32;
            self.exec(&mut l, &i, start, end);
            if page && self.boundaries {
                self.boundary(&l, end);
            }
        }
        let rec = &mut self.out.streams[stream.0 as usize];
        rec.open_saves = l.stack.len() as u32;
        rec.open_text = l.in_text;
        // Frames left open close with the stream.
        while let Some(f) = l.frames.pop() {
            l.frame_hides.pop();
            let _ = f;
        }
    }

    fn boundary(&mut self, l: &Local, offset: u32) {
        let o = &mut self.out;
        let same = o
            .boundary_snaps
            .last()
            .is_some_and(|s| Arc::ptr_eq(s, &l.gs));
        if !same {
            o.boundary_snaps.push(l.gs.clone());
        }
        o.boundaries.push(Boundary {
            offset,
            snap: o.boundary_snaps.len() as u32 - 1,
            in_text: l.in_text,
            frame: l.frames.last().map_or(0, |f| f + 1),
        });
    }

    fn add_clip(&mut self, _l: &Local, ctm: Matrix, path: Option<Vec<u8>>, bbox: Rect) -> u32 {
        let bbox = match self.current_clip_bbox_of(&_l.gs.clips) {
            Some(c) => {
                intersect(&c, &bbox).unwrap_or(Rect::new(bbox.x0, bbox.y0, bbox.x0, bbox.y0))
            }
            None => bbox,
        };
        self.out.clips.push(ClipRec { ctm, path, bbox });
        self.out.clips.len() as u32 - 1
    }

    fn current_clip_bbox_of(&self, clips: &[u32]) -> Option<Rect> {
        clips.last().map(|&c| self.out.clips[c as usize].bbox)
    }

    fn exec(&mut self, l: &mut Local, i: &Instr<'_, '_>, start: u32, end: u32) {
        let is_path = matches!(
            i.op,
            Op::MoveTo
                | Op::LineTo
                | Op::CurveTo
                | Op::CurveToV
                | Op::CurveToY
                | Op::ClosePath
                | Op::Rect
                | Op::Clip
                | Op::ClipEvenOdd
                | Op::Stroke
                | Op::CloseStroke
                | Op::Fill
                | Op::FillCompat
                | Op::FillEvenOdd
                | Op::FillStroke
                | Op::FillStrokeEvenOdd
                | Op::CloseFillStroke
                | Op::CloseFillStrokeEvenOdd
                | Op::EndPath
        );
        if !is_path {
            l.group_open = false;
        }
        match i.op {
            Op::Save => {
                if l.stack.len() >= MAX_SAVE_DEPTH {
                    l.ignored_saves += 1;
                } else {
                    l.stack.push(l.gs.clone());
                }
            }
            Op::Restore => {
                if l.ignored_saves > 0 {
                    l.ignored_saves -= 1;
                } else if let Some(g) = l.stack.pop() {
                    l.gs = g;
                }
            }
            Op::Concat => {
                if let Some(m) = i.numbers::<6>() {
                    let m = Matrix::from_array(m).then(&l.gs.ctm);
                    if m.is_finite() {
                        Arc::make_mut(&mut l.gs).ctm = m;
                    }
                }
            }
            Op::LineWidth => {
                if let Some(w) = i.number() {
                    Arc::make_mut(&mut l.gs).line_width = w.abs();
                }
            }
            Op::LineCap => {
                if let Some(v) = i.number() {
                    Arc::make_mut(&mut l.gs).cap = v.clamp(0.0, 2.0) as u8;
                }
            }
            Op::LineJoin => {
                if let Some(v) = i.number() {
                    Arc::make_mut(&mut l.gs).join = v.clamp(0.0, 2.0) as u8;
                }
            }
            Op::MiterLimit => {
                if let Some(v) = i.number() {
                    Arc::make_mut(&mut l.gs).miter = v.max(1.0);
                }
            }
            Op::Dash | Op::RenderingIntent => {
                let mut b = Vec::new();
                emit::instr(&mut b, i);
                let g = Arc::make_mut(&mut l.gs);
                if i.op == Op::Dash {
                    g.dash = Some(b);
                    g.dashed = matches!(
                        i.args.first(),
                        Some(Operand::Array { start, end }) if end > start
                    );
                } else {
                    g.intent = Some(b);
                }
            }
            Op::ExtGState => {
                if let Some(n) = i.args.last().and_then(Operand::name) {
                    self.ext_gstate(l, &n, i.raw_name().unwrap_or_default());
                }
            }
            Op::MoveTo
            | Op::LineTo
            | Op::CurveTo
            | Op::CurveToV
            | Op::CurveToY
            | Op::ClosePath
            | Op::Rect => self.path_op(l, i, start, end),
            Op::Clip => l.clip = Some((false, start, end)),
            Op::ClipEvenOdd => l.clip = Some((true, start, end)),
            Op::Stroke
            | Op::CloseStroke
            | Op::Fill
            | Op::FillCompat
            | Op::FillEvenOdd
            | Op::FillStroke
            | Op::FillStrokeEvenOdd
            | Op::CloseFillStroke
            | Op::CloseFillStrokeEvenOdd
            | Op::EndPath => self.paint(l, i, end),
            Op::BeginText => {
                l.tm = Matrix::IDENTITY;
                l.tlm = Matrix::IDENTITY;
                l.in_text = true;
                l.bt_start = start;
                l.text_clip = false;
                l.text_items.clear();
            }
            Op::EndText => {
                l.in_text = false;
                for &k in &l.text_items {
                    if let Source::Run(r) = &mut self.out.items[k].src {
                        r.bt = (l.bt_start, end);
                    }
                }
                if l.text_clip {
                    let bbox = l
                        .text_items
                        .iter()
                        .map(|&k| self.out.items[k].bbox)
                        .reduce(|a, b| a.union(&b))
                        .unwrap_or_default();
                    let id = self.add_clip(l, Matrix::IDENTITY, None, bbox);
                    Arc::make_mut(&mut l.gs).clips.push(id);
                }
                l.text_items.clear();
            }
            Op::CharSpacing => {
                if let Some(v) = i.number() {
                    Arc::make_mut(&mut l.gs).tc = v;
                }
            }
            Op::WordSpacing => {
                if let Some(v) = i.number() {
                    Arc::make_mut(&mut l.gs).tw = v;
                }
            }
            Op::HorizontalScale => {
                if let Some(v) = i.number() {
                    Arc::make_mut(&mut l.gs).th = v / 100.0;
                }
            }
            Op::Leading => {
                if let Some(v) = i.number() {
                    Arc::make_mut(&mut l.gs).tl = v;
                }
            }
            Op::TextRise => {
                if let Some(v) = i.number() {
                    Arc::make_mut(&mut l.gs).rise = v;
                }
            }
            Op::TextRender => {
                if let Some(v) = i.number() {
                    Arc::make_mut(&mut l.gs).mode = v.clamp(0.0, 7.0) as u8;
                }
            }
            Op::Font => {
                if let [.., name, size] = i.args
                    && let (Some(n), Some(size)) = (name.name(), size.number())
                {
                    let font = self.font(&l.res, &n);
                    let g = Arc::make_mut(&mut l.gs);
                    g.size = size;
                    g.font = font;
                    g.font_raw = name.raw_name().map(<[u8]>::to_vec);
                }
            }
            Op::MoveText => {
                if let Some([x, y]) = i.numbers() {
                    next_line(l, x, y);
                }
            }
            Op::MoveTextLeading => {
                if let Some([x, y]) = i.numbers() {
                    Arc::make_mut(&mut l.gs).tl = -y;
                    next_line(l, x, y);
                }
            }
            Op::TextMatrix => {
                if let Some(m) = i.numbers::<6>() {
                    let m = Matrix::from_array(m);
                    if m.is_finite() {
                        l.tm = m;
                        l.tlm = m;
                    }
                }
            }
            Op::NextLine => {
                let tl = l.gs.tl;
                next_line(l, 0.0, -tl);
            }
            Op::ShowText | Op::ShowTextArray | Op::NextLineShow | Op::NextLineSpacingShow => {
                self.show(l, i, start, end)
            }
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
            | Op::FillCmyk => self.color(l, i),
            Op::Shade => self.shade(l, i, start, end),
            Op::InlineImage => self.inline_image(l, i, start, end),
            Op::XObject => {
                if let Some(n) = i.args.last().and_then(Operand::name) {
                    self.xobject(l, &n, i.raw_name().unwrap_or_default(), start, end);
                }
            }
            Op::BeginMarked | Op::BeginMarkedProps => self.begin_marked(l, i, start, end),
            Op::EndMarked => {
                if let Some(f) = l.frames.pop() {
                    if l.frame_hides.pop() == Some(true) {
                        l.hidden -= 1;
                    }
                    self.out.frames[f as usize].end = Some((start, end));
                }
            }
            _ => {}
        }
    }

    // Graphics state

    fn ext_gstate(&mut self, l: &mut Local, name: &[u8], raw: &[u8]) {
        let doc = self.doc;
        let Some(o) = resource(doc, &l.res, b"ExtGState", name) else {
            return;
        };
        let d = doc.resolve(&o);
        let Some(d) = d.as_dict() else { return };
        let font = match doc.get_in(d, b"Font").as_array() {
            Some([f, size]) => Some((self.load_font(f), f.clone(), size.as_f64())),
            _ => None,
        };
        let g = Arc::make_mut(&mut l.gs);
        g.ext.push(raw.to_vec());
        let num = |k: &[u8]| doc.get_in(d, k).as_f64().map(|v| v as f32);
        if let Some(v) = num(b"LW") {
            g.line_width = v.abs();
        }
        if let Some(v) = num(b"LC") {
            g.cap = v.clamp(0.0, 2.0) as u8;
        }
        if let Some(v) = num(b"LJ") {
            g.join = v.clamp(0.0, 2.0) as u8;
        }
        if let Some(v) = num(b"ML") {
            g.miter = v.max(1.0);
        }
        if let Some(v) = num(b"CA") {
            g.stroke_alpha = v.clamp(0.0, 1.0);
        }
        if let Some(v) = num(b"ca") {
            g.fill_alpha = v.clamp(0.0, 1.0);
        }
        if d.contains(b"D") {
            g.dash = None;
            g.dashed = false;
        }
        if let Some((font, object, size)) = font {
            g.font = font.map(|font| FontInfo {
                name: None,
                object,
                font,
            });
            g.font_raw = None;
            if let Some(s) = size {
                g.size = s as f32;
            }
        }
    }

    fn color(&mut self, l: &mut Local, i: &Instr<'_, '_>) {
        let stroke = matches!(
            i.op,
            Op::StrokeSpace
                | Op::StrokeColor
                | Op::StrokeColorN
                | Op::StrokeGray
                | Op::StrokeRgb
                | Op::StrokeCmyk
        );
        let mut ops = Vec::new();
        emit::instr(&mut ops, i);
        let device = |space: &[u8], n: usize| {
            let comps: Vec<f32> = i
                .args
                .iter()
                .rev()
                .take(n)
                .rev()
                .filter_map(|a| a.number())
                .collect();
            (comps.len() == n).then(|| Paint {
                space: Name::new(space),
                components: comps,
                pattern: None,
            })
        };
        let doc = self.doc;
        let res = l.res.clone();
        let g = Arc::make_mut(&mut l.gs);
        let (paint, paint_ops, space_ops) = if stroke {
            (&mut g.stroke, &mut g.stroke_ops, &mut g.stroke_space_ops)
        } else {
            (&mut g.fill, &mut g.fill_ops, &mut g.fill_space_ops)
        };
        match i.op {
            Op::FillGray
            | Op::StrokeGray
            | Op::FillRgb
            | Op::StrokeRgb
            | Op::FillCmyk
            | Op::StrokeCmyk => {
                let (space, n): (&[u8], usize) = match i.op {
                    Op::FillGray | Op::StrokeGray => (b"DeviceGray", 1),
                    Op::FillRgb | Op::StrokeRgb => (b"DeviceRGB", 3),
                    _ => (b"DeviceCMYK", 4),
                };
                if let Some(p) = device(space, n) {
                    *paint = p;
                    *paint_ops = ops;
                    space_ops.clear();
                    space_ops.extend_from_slice(b"/");
                    space_ops.extend_from_slice(space);
                    space_ops.extend_from_slice(if stroke { b" CS\n" } else { b" cs\n" });
                }
            }
            Op::FillSpace | Op::StrokeSpace => {
                if let Some(n) = i.args.last().and_then(Operand::name) {
                    let init = initial_color(doc, &res, &n);
                    let space = match &*n {
                        b"G" => b"DeviceGray".as_slice(),
                        b"RGB" => b"DeviceRGB",
                        b"CMYK" => b"DeviceCMYK",
                        n => n,
                    };
                    *paint = Paint {
                        space: Name::new(space),
                        components: init,
                        pattern: None,
                    };
                    *space_ops = ops.clone();
                    *paint_ops = ops;
                }
            }
            _ => {
                let comps: Vec<f32> = i.args.iter().filter_map(|a| a.number()).collect();
                let pattern = i.args.last().and_then(Operand::name).map(|n| Name::new(&n));
                paint.components = comps;
                paint.pattern = pattern;
                let mut b = space_ops.clone();
                b.extend_from_slice(&ops);
                *paint_ops = b;
            }
        }
    }

    // Paths

    fn path_op(&mut self, l: &mut Local, i: &Instr<'_, '_>, start: u32, _end: u32) {
        if l.path_start.is_none() {
            l.path_start = Some(start);
            l.path_ops.clear();
            l.subpaths.clear();
        }
        emit::instr(&mut l.path_ops, i);
        let ctm = l.gs.ctm;
        let pt = |x: f32, y: f32| ctm.apply(Point::new(x, y));
        match i.op {
            Op::MoveTo => {
                if let Some([x, y]) = i.numbers() {
                    let p = pt(x, y);
                    l.subpaths.push(vec![p]);
                    l.current = Some(p);
                    l.sub_start = p;
                }
            }
            Op::LineTo => {
                if let Some([x, y]) = i.numbers() {
                    let p = pt(x, y);
                    if l.subpaths.is_empty() {
                        l.subpaths.push(Vec::new());
                    }
                    l.subpaths.last_mut().unwrap().push(p);
                    l.current = Some(p);
                }
            }
            Op::CurveTo | Op::CurveToV | Op::CurveToY => {
                let pts: Vec<f32> = i.args.iter().filter_map(|a| a.number()).collect();
                if pts.len() >= 4 {
                    if l.subpaths.is_empty() {
                        l.subpaths.push(Vec::new());
                    }
                    let s = l.subpaths.last_mut().unwrap();
                    for c in pts.as_chunks::<2>().0 {
                        s.push(pt(c[0], c[1]));
                    }
                    l.current = s.last().copied();
                }
            }
            Op::ClosePath => {
                l.current = Some(l.sub_start);
            }
            Op::Rect => {
                if let Some([x, y, w, h]) = i.numbers() {
                    l.subpaths
                        .push(vec![pt(x, y), pt(x + w, y), pt(x + w, y + h), pt(x, y + h)]);
                    l.current = Some(pt(x, y));
                    l.sub_start = pt(x, y);
                }
            }
            _ => {}
        }
    }

    fn paint(&mut self, l: &mut Local, i: &Instr<'_, '_>, end: u32) {
        let start = l.path_start.take().unwrap_or(end);
        let clip = l.clip.take();
        let subs = std::mem::take(&mut l.subpaths);
        let (fill, stroke) = match i.op {
            Op::Stroke | Op::CloseStroke => (false, true),
            Op::Fill | Op::FillCompat | Op::FillEvenOdd => (true, false),
            Op::EndPath => (false, false),
            _ => (true, true),
        };
        let pts: Vec<Point> = subs.iter().flatten().copied().collect();
        let mut bbox = if pts.is_empty() {
            None
        } else {
            Some(Rect::from_points(&pts))
        };
        if stroke && let Some(b) = &mut bbox {
            let m = &l.gs.ctm;
            let w = l.gs.line_width.max(0.5) / 2.0 * (m.a.hypot(m.b)).max(m.c.hypot(m.d));
            *b = Rect::new(b.x0 - w, b.y0 - w, b.x1 + w, b.y1 + w);
        }
        if let Some((even_odd, clip_start, clip_end)) = clip {
            let mut ops = l.path_ops.clone();
            ops.extend_from_slice(if even_odd { b"W* n\n" } else { b"W n\n" });
            let b = bbox.unwrap_or_default();
            let ctm = l.gs.ctm;
            let id = self.add_clip(l, ctm, Some(ops), b);
            // A painted path that also clips is drawn under the clip it was before.
            if fill || stroke {
                let snap = l.gs.clone();
                let clip = Some((clip_start, clip_end));
                self.path_item(l, start, end, clip, subs, bbox, fill, stroke, snap);
            }
            Arc::make_mut(&mut l.gs).clips.push(id);
            l.group = None;
            l.group_open = false;
            return;
        }
        if !(fill || stroke) {
            l.group_open = false;
            return;
        }
        let snap = l.gs.clone();
        // Joins the previous object when nothing came between and the state is the same.
        if l.group_open
            && let Some(g) = l.group
            && Arc::ptr_eq(&self.out.items[g].snap, &snap)
            && self.out.items[g].frames == l.frames
        {
            let it = &mut self.out.items[g];
            if let (Source::Paths { end: e, .. }, ItemData::Object(ObjectData::Path(p))) =
                (&mut it.src, &mut it.data)
                && p.paints < MAX_GROUP
            {
                *e = end;
                p.paints += 1;
                if fill && p.fill.is_none() {
                    p.fill = Some(snap.fill.clone());
                }
                if stroke && p.stroke.is_none() {
                    p.stroke = Some(snap.stroke.clone());
                }
                p.outline.extend(subs);
                if let Some(b) = bbox {
                    it.bbox = it.bbox.union(&b);
                    it.quad = corners(&it.bbox);
                }
                l.group_open = true;
                return;
            }
        }
        let k = self.path_item(l, start, end, None, subs, bbox, fill, stroke, snap);
        l.group = Some(k);
        l.group_open = true;
    }

    #[allow(clippy::too_many_arguments)]
    fn path_item(
        &mut self,
        l: &Local,
        start: u32,
        end: u32,
        clip: Option<(u32, u32)>,
        subs: Vec<Vec<Point>>,
        bbox: Option<Rect>,
        fill: bool,
        stroke: bool,
        snap: Arc<Snap>,
    ) -> usize {
        let bbox = bbox.unwrap_or_default();
        let data = ObjectData::Path(PathInfo {
            paints: 1,
            fill: fill.then(|| snap.fill.clone()),
            stroke: stroke.then(|| snap.stroke.clone()),
            line_width: snap.line_width,
            dashed: snap.dashed,
            ctm: snap.ctm,
            fill_alpha: snap.fill_alpha,
            stroke_alpha: snap.stroke_alpha,
            clips: clip.is_some(),
            outline: subs,
        });
        self.push_item(
            l,
            ItemData::Object(data),
            Source::Paths { start, end, clip },
            snap,
            bbox,
            corners(&bbox),
        )
    }

    fn push_item(
        &mut self,
        l: &Local,
        data: ItemData,
        src: Source,
        snap: Arc<Snap>,
        bbox: Rect,
        quad: [Point; 4],
    ) -> usize {
        let k = self.out.items.len();
        for &f in &l.frames {
            self.out.frames[f as usize].items.push(k);
        }
        self.out.items.push(Item {
            data,
            stream: l.stream,
            src,
            snap,
            bbox,
            quad,
            parent: l.parent,
            frames: l.frames.clone(),
            hidden: l.hidden > 0,
            bytes: Vec::new(),
        });
        k
    }

    // Text

    fn show(&mut self, l: &mut Local, i: &Instr<'_, '_>, start: u32, end: u32) {
        let (show, items): (ShowKind, &[Operand<'_>]) = match (i.op, i.args) {
            (Op::ShowText, [.., s]) => (ShowKind::Tj, std::slice::from_ref(s)),
            (Op::ShowTextArray, [.., a]) => (ShowKind::TJ, i.array(a)),
            (Op::NextLineShow, [.., s]) => (ShowKind::Quote, std::slice::from_ref(s)),
            (Op::NextLineSpacingShow, [.., _, _, s]) => {
                (ShowKind::DoubleQuote, std::slice::from_ref(s))
            }
            _ => return,
        };
        let mut prefix = Vec::new();
        let mut operand = Vec::new();
        match show {
            ShowKind::Quote => {
                let tl = l.gs.tl;
                next_line(l, 0.0, -tl);
                prefix.extend_from_slice(b"T*\n");
                emit::operand(&mut operand, i, &items[0]);
            }
            ShowKind::DoubleQuote => {
                if let [.., aw, ac, _] = i.args {
                    let g = Arc::make_mut(&mut l.gs);
                    g.tw = aw.number().unwrap_or(g.tw);
                    g.tc = ac.number().unwrap_or(g.tc);
                    emit::operand(&mut prefix, i, aw);
                    prefix.extend_from_slice(b" Tw ");
                    emit::operand(&mut prefix, i, ac);
                    prefix.extend_from_slice(b" Tc T*\n");
                }
                let tl = l.gs.tl;
                next_line(l, 0.0, -tl);
                emit::operand(&mut operand, i, &items[0]);
            }
            ShowKind::Tj => emit::operand(&mut operand, i, &items[0]),
            ShowKind::TJ => emit::operand(&mut operand, i, &i.args[i.args.len() - 1]),
        }
        let g = l.gs.clone();
        let tm = l.tm;
        let tlm = l.tlm;
        let (size, h) = (g.size, g.th);
        let mut glyphs = Vec::new();
        let mut shown = Vec::new();
        let (mut tx, mut ty) = (0.0f32, 0.0f32);
        let font = g.font.as_ref().map(|f| f.font.clone());
        let vertical = font.as_ref().is_some_and(|f| f.vertical);
        for item in items {
            if let Some(n) = item.number() {
                let adj = n / 1000.0 * size;
                if vertical {
                    ty -= adj;
                } else {
                    tx -= adj * h;
                }
                continue;
            }
            self.buf.clear();
            if !item.string_into(&mut self.buf) {
                continue;
            }
            let Some(font) = &font else { continue };
            let mut s = &self.buf[..];
            while !s.is_empty() {
                let (code, len) = font.next_code(s);
                let len = len.clamp(1, s.len());
                let b0 = shown.len() as u32;
                shown.extend_from_slice(&s[..len]);
                s = &s[len..];
                let w0 = font.width(code);
                let spacing = g.tc + if len == 1 && code == 32 { g.tw } else { 0.0 };
                if vertical {
                    let (w1, v) = font.vertical_metrics(code);
                    let adv = w1 * size + spacing;
                    glyphs.push(RawGlyph {
                        code,
                        bytes: (b0, shown.len() as u32),
                        x: -v.x * size * h,
                        y: ty - v.y * size,
                        width: w0 * size * h,
                        advance: adv,
                        scaled: w1 * size,
                    });
                    ty += adv;
                } else {
                    let adv = (w0 * size + spacing) * h;
                    glyphs.push(RawGlyph {
                        code,
                        bytes: (b0, shown.len() as u32),
                        x: tx,
                        y: 0.0,
                        width: w0 * size * h,
                        advance: adv,
                        scaled: w0 * size * h,
                    });
                    tx += adv;
                }
            }
        }
        let next = Matrix::translate(tx, ty).then(&l.tm);
        if next.is_finite() {
            l.tm = next;
        }
        if g.mode >= 4 {
            l.text_clip = true;
        }
        if glyphs.is_empty() {
            return;
        }
        let m = tm.then(&g.ctm);
        let (asc, desc) = font
            .as_ref()
            .map_or((0.8, -0.2), |f| (f.ascent * size, f.descent * size));
        let mut pts = Vec::with_capacity(glyphs.len() * 2);
        for gl in &glyphs {
            let w = if gl.width > 0.0 {
                gl.width
            } else {
                0.25 * size
            };
            let y = gl.y + g.rise;
            pts.push(m.apply(Point::new(gl.x, y + desc)));
            pts.push(m.apply(Point::new(gl.x + w, y + asc)));
            pts.push(m.apply(Point::new(gl.x, y + asc)));
            pts.push(m.apply(Point::new(gl.x + w, y + desc)));
        }
        let bbox = Rect::from_points(&pts);
        let src = Source::Run(RunSrc {
            show,
            operand,
            prefix,
            tlm,
            tm,
            tm_after: l.tm,
            bt: (l.bt_start, end),
            start,
            end,
        });
        let k = self.push_item(
            l,
            ItemData::Run(RunData { glyphs, vertical }),
            src,
            g,
            bbox,
            corners(&bbox),
        );
        self.out.items[k].bytes = shown;
        l.text_items.push(k);
    }

    fn font(&mut self, res: &Dict, name: &[u8]) -> Option<FontInfo> {
        let raw = resource(self.doc, res, b"Font", name)?;
        let font = self.load_font(&raw)?;
        Some(FontInfo {
            name: Some(Name::new(name)),
            object: raw,
            font,
        })
    }

    fn load_font(&mut self, o: &Object) -> Option<Arc<Font>> {
        let doc = self.doc;
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

    // Images, forms, shadings

    fn xobject(&mut self, l: &mut Local, name: &[u8], raw: &[u8], start: u32, end: u32) {
        let doc = self.doc;
        let Some(o) = resource(doc, &l.res, b"XObject", name) else {
            return;
        };
        let obj = doc.resolve(&o);
        let Some(s) = obj.as_stream() else { return };
        let d = &s.dict;
        let oc_hidden = d.get(b"OC").is_some_and(|oc| !self.oc.visible(doc, oc));
        let ctm = l.gs.ctm;
        match d.name(b"Subtype") {
            Some(b"Image") => {
                let info = image_info(doc, d, Some(Name::new(raw)), o.as_objref(), &l.gs);
                let quad = corners(&Rect::new(0.0, 0.0, 1.0, 1.0)).map(|p| ctm.apply(p));
                let bbox = Rect::from_points(&quad);
                let snap = l.gs.clone();
                let k = self.push_item(
                    l,
                    ItemData::Object(ObjectData::Image(info)),
                    Source::Op { start, end },
                    snap,
                    bbox,
                    quad,
                );
                self.out.items[k].hidden |= oc_hidden;
            }
            Some(b"Form") | None => {
                let r = o.as_objref();
                if self.forms.len() >= MAX_FORM_DEPTH || r.is_some_and(|r| self.forms.contains(&r))
                {
                    return;
                }
                let Ok(dec) = doc.decode(s) else { return };
                let fm = d
                    .get(b"Matrix")
                    .and_then(|m| Matrix::from_object(&doc.resolve(m)))
                    .filter(Matrix::is_finite)
                    .unwrap_or_default();
                let fctm = fm.then(&ctm);
                let bbox_f = d
                    .get(b"BBox")
                    .and_then(|b| Rect::from_object(&doc.resolve(b)));
                let quad = match bbox_f {
                    Some(b) => corners(&b).map(|p| fctm.apply(p)),
                    None => corners(&self.frame.media_box),
                };
                let mut bbox = Rect::from_points(&quad);
                if let Some(c) = self.current_clip_bbox_of(&l.gs.clips) {
                    bbox = intersect(&bbox, &c).unwrap_or(bbox);
                }
                let fres = match doc.get_in(d, b"Resources").as_dict() {
                    Some(r) => r.clone(),
                    None => l.res.clone(),
                };
                let sid = StreamId(self.out.streams.len() as u32);
                self.out.streams.push(StreamRec {
                    data: Arc::new(dec.data),
                    kind: StreamKind::Form { object: r },
                    res: fres.clone(),
                    drawn_by: None,
                    open_saves: 0,
                    open_text: false,
                });
                let info = FormInfo {
                    name: Name::new(raw),
                    object: r,
                    matrix: fctm,
                    bbox: bbox_f,
                    stream: sid,
                    children: Vec::new(),
                    uses_on_page: 0,
                };
                let snap = l.gs.clone();
                let k = self.push_item(
                    l,
                    ItemData::Object(ObjectData::Form(info)),
                    Source::Op { start, end },
                    snap,
                    bbox,
                    quad,
                );
                self.out.items[k].hidden |= oc_hidden;
                self.out.streams[sid.0 as usize].drawn_by = Some((l.stream, (start, end), k));
                if let Some(r) = r {
                    self.forms.push(r);
                }
                self.run(sid, fres, fctm, Some(k));
                if r.is_some() {
                    self.forms.pop();
                }
            }
            _ => {}
        }
    }

    fn inline_image(&mut self, l: &mut Local, i: &Instr<'_, '_>, start: u32, end: u32) {
        let s = crate::inline::to_xobject(self.doc, &l.res, i);
        let mut info = image_info(self.doc, &s.dict, None, None, &l.gs);
        info.filter = s.dict.get(b"Filter").and_then(|f| match f {
            Object::Name(n) => Some(n.clone()),
            Object::Array(a) => a.last().and_then(|n| n.as_name()).map(Name::new),
            _ => None,
        });
        let ctm = l.gs.ctm;
        let quad = corners(&Rect::new(0.0, 0.0, 1.0, 1.0)).map(|p| ctm.apply(p));
        let bbox = Rect::from_points(&quad);
        let snap = l.gs.clone();
        self.push_item(
            l,
            ItemData::Object(ObjectData::Image(info)),
            Source::Op { start, end },
            snap,
            bbox,
            quad,
        );
    }

    fn shade(&mut self, l: &mut Local, i: &Instr<'_, '_>, start: u32, end: u32) {
        let Some(n) = i.args.last().and_then(Operand::name) else {
            return;
        };
        let object = resource(self.doc, &l.res, b"Shading", &n);
        let clip = self.current_clip_bbox_of(&l.gs.clips);
        let page = self.frame.media_box;
        let bbox = clip.and_then(|c| intersect(&c, &page)).unwrap_or(page);
        let info = ShadingInfo {
            name: Name::new(i.raw_name().unwrap_or_default()),
            object,
            ctm: l.gs.ctm,
        };
        let snap = l.gs.clone();
        self.push_item(
            l,
            ItemData::Object(ObjectData::Shading(info)),
            Source::Op { start, end },
            snap,
            bbox,
            corners(&bbox),
        );
    }

    // Marked content

    fn begin_marked(&mut self, l: &mut Local, i: &Instr<'_, '_>, start: u32, end: u32) {
        let doc = self.doc;
        let mut ops = Vec::new();
        emit::instr(&mut ops, i);
        let tag = match i.op {
            Op::BeginMarked => i.args.last(),
            _ => i.args.len().checked_sub(2).map(|k| &i.args[k]),
        }
        .and_then(Operand::name)
        .unwrap_or_default();
        let mut props = None;
        let mut raw = None;
        let mut actual_text = None;
        if i.op == Op::BeginMarkedProps
            && let Some(p) = i.args.last()
        {
            match p {
                Operand::Dict(_) => {
                    props = i.dict(p).cloned();
                    if props.as_ref().is_some_and(|d| d.contains(b"ActualText")) {
                        actual_text = Some((start, end));
                    }
                }
                Operand::Name(_) => {
                    let n = p.name().unwrap_or_default();
                    raw = resource(doc, &l.res, b"Properties", &n);
                    props = raw.as_ref().and_then(|o| doc.resolve(o).as_dict().cloned());
                }
                _ => {}
            }
        }
        let oc = (&*tag == b"OC")
            .then(|| raw.clone().or_else(|| props.clone().map(Object::Dict)))
            .flatten();
        let hides = oc.as_ref().is_some_and(|o| !self.oc.visible(doc, o));
        let mcid = props.as_ref().and_then(|p| p.int(b"MCID"));
        self.out.frames.push(FrameRec {
            stream: l.stream,
            begin: (start, end),
            end: None,
            begin_ops: ops,
            info: MarkedContent {
                tag: Name::new(&tag),
                mcid,
                optional_content: oc,
                properties: props,
            },
            actual_text,
            items: Vec::new(),
        });
        l.frames.push(self.out.frames.len() as u32 - 1);
        l.frame_hides.push(hides);
        if hides {
            l.hidden += 1;
        }
    }
}

fn next_line(l: &mut Local, x: f32, y: f32) {
    let m = Matrix::translate(x, y).then(&l.tlm);
    if m.is_finite() {
        l.tlm = m;
        l.tm = m;
    }
}

/// `res[category][name]`, unresolved.
pub(crate) fn resource(doc: &Document, res: &Dict, category: &[u8], name: &[u8]) -> Option<Object> {
    doc.get_in(res, category).as_dict()?.get(name).cloned()
}

/// The initial colour of a colour space named by `cs`.
fn initial_color(doc: &Document, res: &Dict, n: &[u8]) -> Vec<f32> {
    match n {
        b"DeviceGray" | b"G" | b"CalGray" => return vec![0.0],
        b"DeviceRGB" | b"RGB" => return vec![0.0; 3],
        b"DeviceCMYK" | b"CMYK" => return vec![0.0, 0.0, 0.0, 1.0],
        b"Pattern" => return Vec::new(),
        _ => {}
    }
    let Some(o) = resource(doc, res, b"ColorSpace", n) else {
        return vec![0.0];
    };
    let o = doc.resolve(&o);
    let (family, arr) = match &*o {
        Object::Name(f) => (f.as_bytes().to_vec(), Vec::new()),
        Object::Array(a) => (
            a.first()
                .and_then(|f| f.as_name())
                .unwrap_or_default()
                .to_vec(),
            a.clone(),
        ),
        _ => return vec![0.0],
    };
    match &family[..] {
        b"DeviceRGB" | b"CalRGB" | b"Lab" => vec![0.0; 3],
        b"DeviceCMYK" => vec![0.0, 0.0, 0.0, 1.0],
        b"Pattern" => Vec::new(),
        b"Separation" => vec![1.0],
        b"DeviceN" => {
            let n = arr
                .get(1)
                .and_then(|a| doc.resolve(a).as_array().map(<[Object]>::len))
                .unwrap_or(1);
            vec![1.0; n]
        }
        b"ICCBased" => {
            let n = arr
                .get(1)
                .and_then(|s| doc.resolve(s).as_stream().and_then(|s| s.dict.int(b"N")))
                .unwrap_or(3)
                .clamp(1, 4) as usize;
            if n == 4 {
                vec![0.0, 0.0, 0.0, 1.0]
            } else {
                vec![0.0; n]
            }
        }
        _ => vec![0.0],
    }
}

fn image_info(
    doc: &Document,
    d: &Dict,
    name: Option<Name>,
    object: Option<ObjRef>,
    g: &Snap,
) -> ImageInfo {
    let int = |k: &[u8]| {
        d.get(k)
            .and_then(|o| doc.resolve(o).as_int())
            .unwrap_or(0)
            .clamp(0, u32::MAX.into()) as u32
    };
    let is_mask = d
        .get(b"ImageMask")
        .is_some_and(|o| doc.resolve(o).as_bool() == Some(true));
    ImageInfo {
        name,
        object,
        width: int(b"Width"),
        height: int(b"Height"),
        bits_per_component: if is_mask { 1 } else { int(b"BitsPerComponent") },
        color_space: d.get(b"ColorSpace").cloned(),
        is_mask,
        has_soft_mask: d.contains(b"SMask"),
        has_mask: d.contains(b"Mask"),
        filter: match d.get(b"Filter").map(|f| doc.resolve(f)) {
            Some(f) => match &*f {
                Object::Name(n) => Some(n.clone()),
                Object::Array(a) => a.last().and_then(|n| n.as_name()).map(Name::new),
                _ => None,
            },
            None => None,
        },
        matrix: g.ctm,
        fill: g.fill.clone(),
        alpha: g.fill_alpha,
    }
}

impl Item {
    /// The span of the item's operators.
    pub fn span(&self) -> Span {
        let (start, end) = match &self.src {
            Source::Paths { start, end, .. } | Source::Op { start, end } => (*start, *end),
            Source::Run(r) => (r.start, r.end),
        };
        Span {
            stream: self.stream,
            start,
            end,
        }
    }
}
