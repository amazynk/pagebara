//! Editing page content (text, images, paths) through the session: the objects of a
//! page, kept while the document stays the same, and operations as undoable steps.
//!
//! Text is set with [`TypesetEncoder`] by default: edited text keeps its font, which gains
//! the glyphs it lacks, with fallbacks embedded for the rest. Paragraphs are edited as a
//! whole and reflow in their box ([`EditOp::EditBlock`], [`EditOp::EditBlockSpans`]);
//! [`TextEditSession`] drives such an edit keystroke by keystroke without touching the
//! document until it is committed. Reflowed text is hyphenated in its language (the PDF's
//! `/Lang`), or else in the language given to [`set_default_language`] (the UI language).

use std::ops::Range;
use std::sync::Arc;

pub use pagebara_edit::{
    Align, BlockLayout, BlockReport, Caret, PreviewGlyph, SpanFont, SpanStyle, StyleChange,
    StyledText, TextFrame, set_default_language,
};
use pagebara_edit::{
    BlockText, Color, Editor, Fonts, ImageFit, Matrix, NewImage, ObjectId, PageObjects, Point,
    Rect, Target, TextEncoder, TextStyle, TypesetEncoder, ZOrder,
};
use pagebara_i18n::{tr, trn};

use crate::session::{Changed, Session};

/// An owned image for [`EditOp`]s.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageData {
    Rgba {
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    },
    Png(Vec<u8>),
    Jpeg(Vec<u8>),
}

impl ImageData {
    fn as_new(&self) -> NewImage<'_> {
        match self {
            ImageData::Rgba {
                width,
                height,
                pixels,
            } => NewImage::Rgba {
                width: *width,
                height: *height,
                pixels,
            },
            ImageData::Png(b) => NewImage::Png(b),
            ImageData::Jpeg(b) => NewImage::Jpeg(b),
        }
    }

    /// A PNG or JPEG file's bytes, told apart by their signature.
    pub fn from_file(bytes: Vec<u8>) -> Option<Self> {
        if bytes.starts_with(b"\x89PNG") {
            Some(ImageData::Png(bytes))
        } else if bytes.starts_with(&[0xFF, 0xD8]) {
            Some(ImageData::Jpeg(bytes))
        } else {
            None
        }
    }

    /// The size in pixels, read from the header.
    pub fn size(&self) -> pagebara_edit::Result<(u32, u32)> {
        pagebara_edit::image_size(&self.as_new())
    }
}

/// One edit of a page's content.
#[derive(Clone, Debug)]
pub enum EditOp {
    Delete(Vec<Target>),
    /// A page-space matrix: moving, scaling or rotating.
    Transform(Vec<Target>, Matrix),
    Fill(Vec<Target>, Color),
    Stroke(Vec<Target>, Color),
    Opacity(Vec<Target>, Option<f32>, Option<f32>),
    /// Paths' line width in points, and dash pattern (lengths in points, empty for solid).
    Line {
        targets: Vec<Target>,
        width: Option<f32>,
        dash: Option<Vec<f32>>,
    },
    Reorder(Vec<Target>, ZOrder),
    Duplicate(Vec<Target>, Matrix),
    Crop(Vec<Target>, Rect),
    ReplaceImage(Target, ImageData, ImageFit),
    /// An image drawn into a page-space rectangle, keeping its proportions.
    AddImage(ImageData, Rect),
    TextStyle(Vec<Target>, TextStyle),
    ReplaceText(Target, String),
    ReplaceLine(ObjectId, usize, String),
    AddText {
        text: String,
        origin: Point,
        size: f32,
        color: Color,
        angle: f32,
    },
    /// A text block's text replaced (paragraphs separated by `\n`) and laid out again in
    /// its box; unchanged text keeps its styles, new text takes the style before it.
    EditBlock {
        block: ObjectId,
        text: String,
    },
    /// A text block's text replaced with styled text.
    EditBlockSpans {
        block: ObjectId,
        spans: Vec<StyledText>,
    },
    /// A style change (bold, italic, size, colour, font) of a byte range of a block's
    /// text (as [`pagebara_edit::block_text`] reads it).
    BlockStyle {
        block: ObjectId,
        range: Range<usize>,
        change: StyleChange,
    },
    /// New paragraphs in a page-space rectangle, upright as the page is displayed.
    AddTextBox {
        rect: Rect,
        spans: Vec<StyledText>,
        align: Align,
    },
}

impl EditOp {
    /// The undo label.
    pub fn label(&self) -> String {
        match self {
            EditOp::Delete(t) => trn!("Delete object", "Delete objects", t.len()),
            EditOp::Transform(t, m) => {
                if m.a == 1.0 && m.b == 0.0 && m.c == 0.0 && m.d == 1.0 {
                    trn!("Move object", "Move objects", t.len())
                } else if (m.b != 0.0 || m.c != 0.0) && (m.a * m.d - m.b * m.c - 1.0).abs() < 1e-3 {
                    trn!("Rotate object", "Rotate objects", t.len())
                } else {
                    trn!("Resize object", "Resize objects", t.len())
                }
            }
            EditOp::Fill(..) => tr!("Change fill colour"),
            EditOp::Stroke(..) => tr!("Change line colour"),
            EditOp::Opacity(..) => tr!("Change opacity"),
            EditOp::Line { width: Some(_), .. } => tr!("Change line width"),
            EditOp::Line { .. } => tr!("Change line style"),
            EditOp::Reorder(_, z) => match z {
                ZOrder::Front => tr!("Bring to front"),
                ZOrder::Back => tr!("Send to back"),
                ZOrder::Forward => tr!("Bring forward"),
                ZOrder::Backward => tr!("Send backward"),
            },
            EditOp::Duplicate(t, _) => trn!("Duplicate object", "Duplicate objects", t.len()),
            EditOp::Crop(..) => tr!("Crop image"),
            EditOp::ReplaceImage(..) => tr!("Replace image"),
            EditOp::AddImage(..) => tr!("Add image"),
            EditOp::TextStyle(..) => tr!("Change text style"),
            EditOp::ReplaceText(..)
            | EditOp::ReplaceLine(..)
            | EditOp::EditBlock { .. }
            | EditOp::EditBlockSpans { .. } => tr!("Edit text"),
            EditOp::BlockStyle { .. } => tr!("Change text style"),
            EditOp::AddText { .. } | EditOp::AddTextBox { .. } => tr!("Add text"),
        }
    }

    /// Whether the page's text may change (the text index is then refreshed).
    fn changes_text(&self) -> bool {
        !matches!(
            self,
            EditOp::Fill(..)
                | EditOp::Stroke(..)
                | EditOp::Opacity(..)
                | EditOp::Line { .. }
                | EditOp::Reorder(..)
                | EditOp::Crop(..)
                | EditOp::ReplaceImage(..)
                | EditOp::AddImage(..)
        )
    }

    fn is_block(&self) -> bool {
        matches!(
            self,
            EditOp::EditBlock { .. }
                | EditOp::EditBlockSpans { .. }
                | EditOp::BlockStyle { .. }
                | EditOp::AddTextBox { .. }
        )
    }

    fn apply_block(
        &self,
        e: &mut Editor<'_>,
        fonts: &mut Fonts,
        report: &mut Option<BlockReport>,
    ) -> pagebara_edit::Result<()> {
        match self {
            EditOp::EditBlock { block, text } => {
                *report = Some(e.edit_block_text(*block, text, fonts)?);
                Ok(())
            }
            EditOp::EditBlockSpans { block, spans } => {
                *report = Some(e.edit_block(*block, spans, fonts)?);
                Ok(())
            }
            EditOp::BlockStyle {
                block,
                range,
                change,
            } => {
                let bt = pagebara_edit::block_text(e.objects(), *block)?;
                let spans = pagebara_edit::restyle_range(
                    e.document(),
                    fonts,
                    &bt.spans,
                    range.clone(),
                    change,
                )?;
                *report = Some(e.edit_block(*block, &spans, fonts)?);
                Ok(())
            }
            EditOp::AddTextBox { rect, spans, align } => {
                let mut frame = TextFrame::new_box(e.objects(), *rect);
                frame.align = *align;
                *report = Some(e.add_text_box(&frame, spans, fonts)?);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn apply(&self, e: &mut Editor<'_>, enc: &dyn TextEncoder) -> pagebara_edit::Result<()> {
        match self {
            EditOp::EditBlock { .. }
            | EditOp::EditBlockSpans { .. }
            | EditOp::BlockStyle { .. }
            | EditOp::AddTextBox { .. } => Ok(()),
            EditOp::Delete(t) => e.delete(t),
            EditOp::Transform(t, m) => e.transform(t, *m),
            EditOp::Fill(t, c) => e.set_fill(t, *c),
            EditOp::Stroke(t, c) => e.set_stroke(t, *c),
            EditOp::Opacity(t, f, s) => e.set_opacity(t, *f, *s),
            EditOp::Line {
                targets,
                width,
                dash,
            } => e.set_line(targets, *width, dash.as_deref()),
            EditOp::Reorder(t, z) => e.reorder(t, *z),
            EditOp::Duplicate(t, m) => e.duplicate(t, *m),
            EditOp::Crop(t, r) => e.crop(t, *r),
            EditOp::ReplaceImage(t, img, fit) => e.replace_image(*t, &img.as_new(), *fit),
            EditOp::AddImage(img, rect) => e.add_image_in(&img.as_new(), *rect),
            EditOp::TextStyle(t, s) => e.set_text_style(t, *s),
            EditOp::ReplaceText(t, s) => e.replace_text(*t, s, enc),
            EditOp::ReplaceLine(b, l, s) => e.replace_line(*b, *l, s, enc),
            EditOp::AddText {
                text,
                origin,
                size,
                color,
                angle,
            } => e.add_text(
                &pagebara_edit::NewText {
                    text,
                    origin: *origin,
                    size: *size,
                    color: *color,
                    angle: *angle,
                    leading: None,
                },
                enc,
            ),
        }
    }
}

/// The content editing state of one session: the objects of the page being edited,
/// read again when the document changes, and the fonts text is set in.
#[derive(Debug, Default)]
pub struct PageEditing {
    cache: Option<(u64, usize, Arc<PageObjects>)>,
    encoder: TypesetEncoder,
    report: Option<BlockReport>,
}

impl PageEditing {
    pub fn new() -> Self {
        Self::default()
    }

    /// The objects of `page` in the session's current document.
    pub fn objects(&mut self, s: &Session, page: usize) -> pagebara_edit::Result<Arc<PageObjects>> {
        if let Some((g, p, o)) = &self.cache
            && *g == s.generation()
            && *p == page
        {
            return Ok(o.clone());
        }
        let o = Arc::new(pagebara_edit::parse(s.doc(), page)?);
        self.cache = Some((s.generation(), page, o.clone()));
        Ok(o)
    }

    /// The default encoder (keeping fonts, adding glyphs, embedding fallbacks).
    pub fn encoder(&self) -> &TypesetEncoder {
        &self.encoder
    }

    /// What the last paragraph edit did: its new bounds, lines and how far it grew.
    pub fn last_report(&self) -> Option<&BlockReport> {
        self.report.as_ref()
    }

    /// Applies `op` to `page` as one undoable step, with the default encoder.
    pub fn edit(
        &mut self,
        s: &mut Session,
        page: usize,
        op: &EditOp,
    ) -> pagebara_edit::Result<Changed> {
        self.run(s, page, std::slice::from_ref(op), &op.label(), None)
    }

    /// Applies `op` to `page` as one undoable step labelled for it. Ids are those of
    /// [`PageEditing::objects`] for the current document.
    pub fn apply(
        &mut self,
        s: &mut Session,
        page: usize,
        op: &EditOp,
        enc: &dyn TextEncoder,
    ) -> pagebara_edit::Result<Changed> {
        self.run(s, page, std::slice::from_ref(op), &op.label(), Some(enc))
    }

    /// Applies several operations as one step.
    pub fn apply_all(
        &mut self,
        s: &mut Session,
        page: usize,
        ops: &[EditOp],
        label: &str,
        enc: &dyn TextEncoder,
    ) -> pagebara_edit::Result<Changed> {
        self.run(s, page, ops, label, Some(enc))
    }

    fn run(
        &mut self,
        s: &mut Session,
        page: usize,
        ops: &[EditOp],
        label: &str,
        enc: Option<&dyn TextEncoder>,
    ) -> pagebara_edit::Result<Changed> {
        let encoder = &self.encoder;
        let enc = enc.unwrap_or(encoder);
        let mut report = None;
        let f = |doc: &mut pagebara_syntax::Document| {
            let mut e = Editor::open(doc, page)?;
            for op in ops {
                if op.is_block() {
                    op.apply_block(&mut e, &mut encoder.fonts(), &mut report)?;
                } else {
                    op.apply(&mut e, enc)?;
                }
            }
            e.commit()
        };
        self.cache = None;
        let ((), changed) = if ops.iter().any(EditOp::changes_text) {
            s.edit_text(label, f)?
        } else {
            s.edit(label, f)?
        };
        self.report = report;
        Ok(changed)
    }
}

/// A caret movement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    WordLeft,
    WordRight,
    Up,
    Down,
    LineStart,
    LineEnd,
    Start,
    End,
}

/// The style of a selection, for toolbar state: `None` where it is mixed.
#[derive(Clone, Debug, PartialEq)]
pub struct StyleSummary {
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub size: Option<f32>,
    pub family: Option<String>,
    pub fill: Option<[f32; 3]>,
}

#[derive(Clone, Debug)]
enum EditTarget {
    Block(BlockText),
    NewBox,
}

/// Editing one paragraph (a text block, or a new text box) live: the UI feeds keystrokes,
/// IME composition and style changes, draws [`TextEditSession::preview_glyphs`] (or renders
/// [`TextEditSession::preview_document`]) with the caret and selection over the page, and
/// commits the result as one undoable step. The document is not touched before
/// [`TextEditSession::commit`].
///
/// Positions are byte offsets into [`TextEditSession::text`], always on character
/// boundaries; paragraphs are separated by `\n`. Geometry is in page space.
#[derive(Debug)]
pub struct TextEditSession {
    page: usize,
    target: EditTarget,
    frame: TextFrame,
    original: Vec<StyledText>,
    spans: Vec<StyledText>,
    caret: usize,
    anchor: usize,
    preedit: Option<String>,
    /// The style for typed text after a style change with nothing selected.
    typing: Option<SpanStyle>,
    goal_x: Option<f32>,
    fonts: Fonts,
    layout: Option<BlockLayout>,
}

fn nfc(s: &str) -> String {
    pagebara_edit::nfc_string(s)
}

impl TextEditSession {
    /// Starts editing a text block of `page`, the caret at its end.
    pub fn open(
        s: &Session,
        ed: &mut PageEditing,
        page: usize,
        block: ObjectId,
    ) -> pagebara_edit::Result<TextEditSession> {
        let objs = ed.objects(s, page)?;
        let bt = pagebara_edit::block_text(&objs, block)?;
        let spans = pagebara_edit::merge(&bt.spans);
        let len = spans.iter().map(|x| x.text.len()).sum();
        Ok(TextEditSession {
            page,
            frame: bt.frame.clone(),
            original: spans.clone(),
            spans,
            target: EditTarget::Block(bt),
            caret: len,
            anchor: len,
            preedit: None,
            typing: None,
            goal_x: None,
            fonts: Fonts::system(),
            layout: None,
        })
    }

    /// Starts editing the text block at a page-space point, with the caret there; `None`
    /// when there is no text at the point.
    pub fn open_at(
        s: &Session,
        ed: &mut PageEditing,
        page: usize,
        p: Point,
    ) -> pagebara_edit::Result<Option<TextEditSession>> {
        let objs = ed.objects(s, page)?;
        let hit = pagebara_edit::hit::text_pos_at(&objs, p, 4.0)
            .and_then(|pos| objs.run(pos.run))
            .map(|r| r.block);
        let Some(block) = hit else { return Ok(None) };
        let mut t = TextEditSession::open(s, ed, page, block)?;
        t.click(s, p, false);
        Ok(Some(t))
    }

    /// Starts a new text box in a page-space rectangle, typed in `style`.
    pub fn new_box(
        s: &Session,
        ed: &mut PageEditing,
        page: usize,
        rect: Rect,
        style: SpanStyle,
        align: Align,
    ) -> pagebara_edit::Result<TextEditSession> {
        let objs = ed.objects(s, page)?;
        let mut frame = TextFrame::new_box(&objs, rect);
        frame.align = align;
        let spans = vec![StyledText {
            text: String::new(),
            style,
        }];
        Ok(TextEditSession {
            page,
            target: EditTarget::NewBox,
            frame,
            original: spans.clone(),
            spans,
            caret: 0,
            anchor: 0,
            preedit: None,
            typing: None,
            goal_x: None,
            fonts: Fonts::system(),
            layout: None,
        })
    }

    pub fn page(&self) -> usize {
        self.page
    }

    /// The block being edited (`None` for a new box).
    pub fn block(&self) -> Option<ObjectId> {
        match &self.target {
            EditTarget::Block(b) => Some(b.block),
            EditTarget::NewBox => None,
        }
    }

    /// Where the original text is drawn (to hide while the preview shows): the bounds of
    /// the edited block's text.
    pub fn original_bounds(&self, s: &Session, ed: &mut PageEditing) -> Option<Rect> {
        let EditTarget::Block(b) = &self.target else {
            return None;
        };
        let objs = ed.objects(s, self.page).ok()?;
        b.blocks
            .iter()
            .filter_map(|id| objs.get(*id).map(|o| o.bbox))
            .reduce(|a, b| a.union(&b))
    }

    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }

    pub fn spans(&self) -> &[StyledText] {
        &self.spans
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    pub fn selection(&self) -> Range<usize> {
        self.caret.min(self.anchor)..self.caret.max(self.anchor)
    }

    pub fn is_modified(&self) -> bool {
        pagebara_edit::merge(&self.spans) != self.original
    }

    fn len(&self) -> usize {
        self.spans.iter().map(|s| s.text.len()).sum()
    }

    fn changed(&mut self) {
        self.layout = None;
    }

    fn clamp(&self, o: usize) -> usize {
        let t = self.text();
        let mut o = o.min(t.len());
        while !t.is_char_boundary(o) {
            o -= 1;
        }
        o
    }

    /// Moves the caret to an offset; with `extend` the selection grows to it.
    pub fn set_caret(&mut self, offset: usize, extend: bool) {
        self.caret = self.clamp(offset);
        if !extend {
            self.anchor = self.caret;
        }
        self.goal_x = None;
        self.typing = None;
    }

    pub fn select(&mut self, r: Range<usize>) {
        let (anchor, caret) = (self.clamp(r.start), self.clamp(r.end));
        // The same selection again (the field reporting itself) keeps the style chosen for
        // what is typed next.
        if (anchor, caret) != (self.anchor, self.caret) {
            self.typing = None;
        }
        self.anchor = anchor;
        self.caret = caret;
        self.goal_x = None;
    }

    pub fn select_all(&mut self) {
        self.select(0..self.len());
    }

    /// Puts the caret at a page-space point (a click; with `extend`, shift-click).
    pub fn click(&mut self, s: &Session, p: Point, extend: bool) {
        if let Ok(l) = self.layout(s) {
            let o = l.offset_at(p);
            self.set_caret(o, extend);
        }
    }

    /// Selects the word at a page-space point (a double click).
    pub fn select_word_at(&mut self, s: &Session, p: Point) {
        self.click(s, p, false);
        let t = self.text();
        let a = word_left(&t, self.caret.min(t.len()), true);
        let b = word_right(&t, self.caret, true);
        self.select(a..b);
    }

    /// Replaces the selection with `text` (typed or pasted; `\n` starts a paragraph).
    pub fn insert(&mut self, text: &str) {
        self.preedit = None;
        let text = nfc(&text.replace("\r\n", "\n").replace(['\r', '\u{2029}'], "\n"))
            .replace('\u{AD}', "");
        let r = self.selection();
        let style = self
            .typing
            .take()
            .or_else(|| pagebara_edit::style_at(&self.spans, r.start.saturating_sub(1)).cloned());
        let Some(style) = style else { return };
        let len = self.len();
        let mut out = pagebara_edit::slice(&self.spans, 0..r.start);
        out.push(StyledText {
            text: text.clone(),
            style,
        });
        out.extend(pagebara_edit::slice(&self.spans, r.end..len));
        self.spans = pagebara_edit::merge(&out);
        self.caret = r.start + text.len();
        self.anchor = self.caret;
        self.goal_x = None;
        self.changed();
    }

    fn delete_range(&mut self, r: Range<usize>) {
        if r.is_empty() {
            return;
        }
        let len = self.len();
        let keep = pagebara_edit::style_at(&self.spans, r.start).cloned();
        let mut out = pagebara_edit::slice(&self.spans, 0..r.start);
        out.extend(pagebara_edit::slice(&self.spans, r.end..len));
        if out.is_empty()
            && let Some(st) = keep
        {
            out.push(StyledText {
                text: String::new(),
                style: st,
            });
        }
        self.spans = pagebara_edit::merge(&out);
        self.caret = r.start;
        self.anchor = r.start;
        self.goal_x = None;
        self.changed();
    }

    /// Deletes the selection, or what `m` moves over from the caret (Backspace is
    /// `Motion::Left`, Delete `Motion::Right`, Ctrl+Backspace `Motion::WordLeft`).
    pub fn delete(&mut self, s: &Session, m: Motion) {
        self.preedit = None;
        let r = self.selection();
        if !r.is_empty() {
            self.delete_range(r);
            return;
        }
        let from = self.caret;
        let to = self.target_of(s, m);
        self.delete_range(from.min(to)..from.max(to));
    }

    /// Moves the caret; with `extend` the selection follows.
    pub fn move_caret(&mut self, s: &Session, m: Motion, extend: bool) {
        let r = self.selection();
        let to = if !extend && !r.is_empty() && matches!(m, Motion::Left | Motion::Right) {
            if m == Motion::Left { r.start } else { r.end }
        } else {
            self.target_of(s, m)
        };
        let goal = self.goal_x;
        self.caret = to;
        if !extend {
            self.anchor = to;
        }
        self.typing = None;
        self.goal_x = if matches!(m, Motion::Up | Motion::Down) {
            goal
        } else {
            None
        };
    }

    fn target_of(&mut self, s: &Session, m: Motion) -> usize {
        let t = self.text();
        let c = self.caret.min(t.len());
        match m {
            Motion::Left => t[..c].char_indices().next_back().map_or(0, |(i, _)| i),
            Motion::Right => t[c..].chars().next().map_or(c, |ch| c + ch.len_utf8()),
            Motion::WordLeft => word_left(&t, c, false),
            Motion::WordRight => word_right(&t, c, false),
            Motion::Start => 0,
            Motion::End => t.len(),
            Motion::LineStart | Motion::LineEnd | Motion::Up | Motion::Down => {
                let goal = self.goal_x;
                let Ok(l) = self.layout(s) else { return c };
                let li = l.line_of(c);
                let line = &l.lines[li];
                match m {
                    Motion::LineStart => line.range.start,
                    Motion::LineEnd => {
                        let wrapped = l
                            .lines
                            .get(li + 1)
                            .is_some_and(|n| n.range.start == line.range.end);
                        if wrapped {
                            // Before the trailing space of a wrapped line.
                            let e = line.range.end;
                            t[..e].trim_end().len().max(line.range.start)
                        } else {
                            line.range.end
                        }
                    }
                    _ => {
                        let x = goal.unwrap_or_else(|| l.x_of(c));
                        let target = if m == Motion::Up {
                            li.checked_sub(1)
                        } else {
                            (li + 1 < l.lines.len()).then_some(li + 1)
                        };
                        let o = match target {
                            Some(n) => l.offset_in_line(n, x),
                            None if m == Motion::Up => 0,
                            None => t.len(),
                        };
                        self.goal_x = Some(x);
                        o
                    }
                }
            }
        }
    }

    /// Shows IME composition text at the caret (replacing the selection when committed
    /// with [`TextEditSession::insert`]); empty clears it.
    pub fn set_preedit(&mut self, text: &str) {
        self.preedit = (!text.is_empty()).then(|| nfc(text));
        self.changed();
    }

    /// The byte range of the composition text in the previewed text.
    pub fn preedit_range(&self) -> Option<Range<usize>> {
        let p = self.preedit.as_ref()?;
        let at = self.selection().start;
        Some(at..at + p.len())
    }

    /// Applies a style change to the selection, or to what is typed next when nothing is
    /// selected.
    pub fn apply_style(&mut self, s: &Session, change: &StyleChange) -> pagebara_edit::Result<()> {
        let r = self.selection();
        if r.is_empty() {
            let base = self.typing.clone().or_else(|| {
                pagebara_edit::style_at(&self.spans, r.start.saturating_sub(1)).cloned()
            });
            if let Some(b) = base {
                self.typing = Some(pagebara_edit::restyle(
                    s.doc(),
                    &mut self.fonts,
                    &b,
                    change,
                )?);
            }
            if self.len() == 0 {
                // An empty box: its only style is the typing style.
                if let Some(t) = &self.typing {
                    self.spans = vec![StyledText {
                        text: String::new(),
                        style: t.clone(),
                    }];
                    self.changed();
                }
            }
            return Ok(());
        }
        self.spans =
            pagebara_edit::restyle_range(s.doc(), &mut self.fonts, &self.spans, r, change)?;
        self.changed();
        Ok(())
    }

    /// The style typed text gets at the caret.
    pub fn typing_style(&self) -> Option<SpanStyle> {
        self.typing.clone().or_else(|| {
            pagebara_edit::style_at(&self.spans, self.selection().start.saturating_sub(1)).cloned()
        })
    }

    /// The style of the selection (of the caret when nothing is selected).
    pub fn selection_style(&mut self, s: &Session) -> StyleSummary {
        let r = self.selection();
        let styles: Vec<SpanStyle> = if r.is_empty() {
            self.typing_style().into_iter().collect()
        } else {
            pagebara_edit::slice(&self.spans, r)
                .into_iter()
                .map(|x| x.style)
                .collect()
        };
        let mut sum: Option<StyleSummary> = None;
        for st in styles {
            let fs = self
                .fonts
                .open(s.doc(), &st.font)
                .ok()
                .and_then(|id| self.fonts.session().style(id).cloned());
            let one = StyleSummary {
                bold: Some(fs.as_ref().is_some_and(|f| f.is_bold()) || st.synthetic.bold),
                italic: Some(fs.as_ref().is_some_and(|f| f.italic) || st.synthetic.italic),
                size: Some((st.size * 100.0).round() / 100.0),
                family: fs.map(|f| f.family),
                fill: st.fill.rgb(),
            };
            sum = Some(match sum {
                None => one,
                Some(a) => StyleSummary {
                    bold: a.bold.filter(|v| Some(*v) == one.bold),
                    italic: a.italic.filter(|v| Some(*v) == one.italic),
                    size: a.size.filter(|v| Some(*v) == one.size),
                    family: a.family.filter(|v| Some(v) == one.family.as_ref()),
                    fill: a.fill.filter(|v| Some(*v) == one.fill),
                },
            });
        }
        sum.unwrap_or(StyleSummary {
            bold: None,
            italic: None,
            size: None,
            family: None,
            fill: None,
        })
    }

    /// Font families to offer for [`StyleChange::Family`].
    pub fn font_families(&self, s: &Session) -> Vec<String> {
        pagebara_edit::font_families(s.doc(), &self.fonts)
    }

    /// The spans as shown: with the composition text in place of the selection.
    pub(crate) fn shown(&self) -> Vec<StyledText> {
        let Some(p) = &self.preedit else {
            return self.spans.clone();
        };
        let r = self.selection();
        let len = self.len();
        let Some(style) = self.typing_style() else {
            return self.spans.clone();
        };
        let mut out = pagebara_edit::slice(&self.spans, 0..r.start);
        out.push(StyledText {
            text: p.clone(),
            style,
        });
        out.extend(pagebara_edit::slice(&self.spans, r.end..len));
        pagebara_edit::merge(&out)
    }

    fn layout(&mut self, s: &Session) -> pagebara_edit::Result<&BlockLayout> {
        if self.layout.is_none() {
            let shown = self.shown();
            let l = pagebara_edit::layout_block(s.doc(), &mut self.fonts, &self.frame, &shown)?;
            self.layout = Some(l);
        }
        Ok(self.layout.as_ref().expect("layout"))
    }

    /// The edited text laid out: lines, glyph positions, overflow.
    pub fn preview(&mut self, s: &Session) -> pagebara_edit::Result<&BlockLayout> {
        self.layout(s)
    }

    /// The caret (in the previewed text, after any composition text).
    pub fn caret_shape(&mut self, s: &Session) -> pagebara_edit::Result<Caret> {
        let at = match self.preedit_range() {
            Some(r) => r.end,
            None => self.caret,
        };
        Ok(self.layout(s)?.caret(at))
    }

    /// Quads covering the selection, one per line.
    pub fn selection_quads(&mut self, s: &Session) -> pagebara_edit::Result<Vec<[Point; 4]>> {
        if self.preedit.is_some() {
            return Ok(Vec::new());
        }
        let r = self.selection();
        Ok(self.layout(s)?.range_quads(r))
    }

    /// Quads covering a byte range of the text, one per line (no composition text).
    pub fn range_quads(
        &mut self,
        s: &Session,
        r: Range<usize>,
    ) -> pagebara_edit::Result<Vec<[Point; 4]>> {
        if self.preedit.is_some() {
            return Ok(Vec::new());
        }
        Ok(self.layout(s)?.range_quads(r))
    }

    /// Quads under the composition text (to underline it).
    pub fn preedit_quads(&mut self, s: &Session) -> pagebara_edit::Result<Vec<[Point; 4]>> {
        let Some(r) = self.preedit_range() else {
            return Ok(Vec::new());
        };
        Ok(self.layout(s)?.range_quads(r))
    }

    /// The glyphs of the previewed text to draw over the page (hide
    /// [`TextEditSession::original_bounds`] underneath).
    pub fn preview_glyphs(&mut self, s: &Session) -> pagebara_edit::Result<Vec<PreviewGlyph>> {
        self.layout(s)?;
        let l = self.layout.take().expect("layout");
        let g = l.outlines(&mut self.fonts);
        self.layout = Some(l);
        Ok(g)
    }

    /// The document as it would be after committing, for rendering an exact preview.
    pub fn preview_document(
        &self,
        s: &Session,
    ) -> pagebara_edit::Result<pagebara_syntax::Document> {
        let mut doc = s.doc().clone();
        let mut fonts = Fonts::new(self.fonts.db().cloned());
        let spans = self.shown();
        let mut e = Editor::open(&mut doc, self.page)?;
        match &self.target {
            EditTarget::Block(b) => {
                e.edit_block(b.block, &spans, &mut fonts)?;
            }
            EditTarget::NewBox => {
                if !spans.iter().all(|x| x.text.is_empty()) {
                    e.add_text_box(&self.frame, &spans, &mut fonts)?;
                }
            }
        }
        e.commit()?;
        Ok(doc)
    }

    /// Writes the edit as one undoable step ("Edit text" / "Add text"); `None` when nothing
    /// changed (or a new box stayed empty).
    pub fn commit(
        self,
        s: &mut Session,
        ed: &mut PageEditing,
    ) -> pagebara_edit::Result<Option<(Changed, BlockReport)>> {
        if !self.is_modified() {
            return Ok(None);
        }
        let op = match &self.target {
            EditTarget::Block(b) => {
                // The block must still be what the session opened.
                let objs = ed.objects(s, self.page)?;
                let now = pagebara_edit::block_text(&objs, b.block)?;
                if now.runs != b.runs || now.spans != b.spans {
                    return Err(pagebara_edit::Error::Unsupported(
                        "the text changed meanwhile",
                    ));
                }
                EditOp::EditBlockSpans {
                    block: b.block,
                    spans: self.spans.clone(),
                }
            }
            EditTarget::NewBox => {
                if self.text().trim().is_empty() {
                    return Ok(None);
                }
                let fr = &self.frame;
                let rect = Rect::from_points(&[
                    fr.matrix.apply(Point::new(fr.left, fr.top)),
                    fr.matrix.apply(Point::new(fr.left + fr.width, fr.bottom)),
                ]);
                EditOp::AddTextBox {
                    rect,
                    spans: self.spans.clone(),
                    align: fr.align,
                }
            }
        };
        let changed = ed.edit(s, self.page, &op)?;
        let report = ed
            .last_report()
            .cloned()
            .ok_or(pagebara_edit::Error::Unsupported("no report"))?;
        Ok(Some((changed, report)))
    }
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '\''
}

/// The start of the word before `c` (with `within`, of the word at `c`).
fn word_left(t: &str, c: usize, within: bool) -> usize {
    let mut i = c;
    let chars: Vec<(usize, char)> = t[..c].char_indices().collect();
    let mut k = chars.len();
    if !within {
        while k > 0 && !is_word(chars[k - 1].1) {
            k -= 1;
        }
    }
    while k > 0 && is_word(chars[k - 1].1) {
        k -= 1;
    }
    if k < chars.len() {
        i = chars[k].0;
    } else if !within {
        i = c;
    }
    i
}

/// The end of the word after `c` (with `within`, of the word at `c`).
fn word_right(t: &str, c: usize, within: bool) -> usize {
    let mut it = t[c..].char_indices().peekable();
    if !within {
        while it.peek().is_some_and(|(_, ch)| !is_word(*ch)) {
            it.next();
        }
    }
    while it.peek().is_some_and(|(_, ch)| is_word(*ch)) {
        it.next();
    }
    it.peek().map_or(t.len(), |(i, _)| c + i)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_pdf;
    use pagebara_edit::SimpleEncoder;

    fn session() -> Session {
        let d = pagebara_syntax::Document::open(test_pdf::text(&["First line", "Second line"]))
            .unwrap();
        Session::new(None, d)
    }

    fn contents(s: &Session) -> Vec<u8> {
        let pd = s.doc().get(s.page_ref(0).unwrap()).unwrap();
        pagebara_content::page_contents(s.doc(), pd.as_dict().unwrap())
    }

    #[test]
    fn edits_are_undoable_steps() {
        let mut s = session();
        let mut ed = PageEditing::new();
        let objs = ed.objects(&s, 0).unwrap();
        assert!(Arc::ptr_eq(&objs, &ed.objects(&s, 0).unwrap()));
        let run = objs
            .runs
            .iter()
            .find(|r| r.text == "First line")
            .unwrap()
            .id;
        let before = contents(&s);
        let fp = objs.fingerprint;
        ed.apply(
            &mut s,
            0,
            &EditOp::ReplaceText(Target::Run(run), "Changed line".into()),
            &SimpleEncoder,
        )
        .unwrap();
        assert_eq!(s.undo_label(), Some("Edit text"));
        assert!(s.text.page(0).text.contains("Changed line"));
        let objs = ed.objects(&s, 0).unwrap();
        assert_ne!(objs.fingerprint, fp);
        assert!(objs.runs.iter().any(|r| r.text == "Changed line"));

        let r = objs
            .runs
            .iter()
            .find(|r| r.text == "Second line")
            .unwrap()
            .id;
        ed.apply(
            &mut s,
            0,
            &EditOp::Transform(vec![Target::Run(r)], Matrix::translate(10.0, 0.0)),
            &SimpleEncoder,
        )
        .unwrap();
        assert_eq!(s.undo_label(), Some("Move object"));
        s.undo();
        s.undo();
        assert_eq!(contents(&s), before);
        assert_eq!(ed.objects(&s, 0).unwrap().fingerprint, fp);
        assert!(s.text.page(0).text.contains("First line"));
        s.redo();
        assert!(s.text.page(0).text.contains("Changed line"));
    }

    #[test]
    fn failed_edits_change_nothing() {
        let mut s = session();
        let mut ed = PageEditing::new();
        let objs = ed.objects(&s, 0).unwrap();
        let run = objs.runs[0].id;
        let e = ed.apply(
            &mut s,
            0,
            &EditOp::ReplaceText(Target::Run(run), "漢字".into()),
            &SimpleEncoder,
        );
        assert!(e.is_err());
        assert!(!s.can_undo() && !s.is_dirty());
        let e = ed.apply(
            &mut s,
            0,
            &EditOp::Delete(vec![Target::Object(ObjectId(99))]),
            &SimpleEncoder,
        );
        assert!(matches!(e, Err(pagebara_edit::Error::NoSuchObject(_))));
    }

    fn para_session() -> Session {
        let d = pagebara_syntax::Document::open(test_pdf::text(&[
            "A paragraph of text that is edited",
            "in place, and reflows in its box.",
        ]))
        .unwrap();
        Session::new(None, d)
    }

    fn block_of(ed: &mut PageEditing, s: &Session, t: &str) -> ObjectId {
        let objs = ed.objects(s, 0).unwrap();
        objs.objects
            .iter()
            .find(|o| o.text().is_some_and(|b| b.text.contains(t)))
            .unwrap()
            .id
    }

    #[test]
    fn block_edit_undo_redo_exact() {
        let mut s = para_session();
        let mut ed = PageEditing::new();
        let b = block_of(&mut ed, &s, "paragraph");
        let before = contents(&s);
        ed.edit(
            &mut s,
            0,
            &EditOp::EditBlock {
                block: b,
                text: "A paragraph of text that is edited in place — Grüße, Ωμέγα — and reflows."
                    .into(),
            },
        )
        .unwrap();
        assert_eq!(s.undo_label(), Some("Edit text"));
        let after = contents(&s);
        assert!(s.text.page(0).text.contains("Ωμέγα"));
        assert!(ed.last_report().unwrap().lines >= 2);
        s.undo();
        assert_eq!(contents(&s), before);
        assert!(!s.text.page(0).text.contains("Ωμέγα"));
        s.redo();
        assert_eq!(contents(&s), after);
        // Edit again after undo: the encoder's font cache follows the document.
        s.undo();
        let b = block_of(&mut ed, &s, "paragraph");
        ed.edit(
            &mut s,
            0,
            &EditOp::EditBlock {
                block: b,
                text: "Ωμέγα again.".into(),
            },
        )
        .unwrap();
        let (doc, _) = (s.doc().clone(), ());
        let bytes = doc.save_full().unwrap();
        let d2 = pagebara_syntax::Document::open(bytes).unwrap();
        let s2 = Session::new(None, d2);
        assert!(s2.text.page(0).text.contains("Ωμέγα again."));
    }

    #[test]
    fn live_session_typing_and_commit() {
        let mut s = para_session();
        let mut ed = PageEditing::new();
        let b = block_of(&mut ed, &s, "paragraph");
        let mut t = TextEditSession::open(&s, &mut ed, 0, b).unwrap();
        assert_eq!(
            t.text(),
            "A paragraph of text that is edited in place, and reflows in its box."
        );
        let end = t.caret_shape(&s).unwrap();
        t.move_caret(&s, Motion::WordLeft, false);
        assert_eq!(&t.text()[t.caret()..], "box.");
        t.move_caret(&s, Motion::End, false);
        t.insert(" More words");
        let c = t.caret_shape(&s).unwrap();
        assert!(c.bottom.x > end.bottom.x || c.bottom.y < end.bottom.y - 1.0);
        t.delete(&s, Motion::WordLeft);
        assert!(t.text().ends_with("box. More "));
        t.delete(&s, Motion::Left);
        t.insert(" é");
        // IME composition shows but is not text yet.
        t.set_preedit("漢");
        assert!(!t.text().contains('漢'));
        assert_eq!(t.preedit_quads(&s).unwrap().len(), 1);
        t.insert("漢字");
        assert!(t.text().ends_with("More é漢字"));
        // Up and down move between lines.
        let l = t.preview(&s).unwrap().lines.len();
        t.move_caret(&s, Motion::Start, false);
        t.move_caret(&s, Motion::Down, false);
        if l > 1 {
            assert!(t.caret() > 0);
        }
        t.move_caret(&s, Motion::Up, false);
        assert_eq!(t.caret(), 0);
        // Select a word and make it bold.
        let at = t.text().find("paragraph").unwrap();
        t.select(at..at + "paragraph".len());
        assert_eq!(t.selection_quads(&s).unwrap().len(), 1);
        t.apply_style(&s, &StyleChange::Bold(true)).unwrap();
        assert_eq!(t.selection_style(&s).bold, Some(true));
        t.select(0..1);
        assert_eq!(t.selection_style(&s).bold, Some(false));
        assert!(!t.preview_glyphs(&s).unwrap().is_empty());
        let pd = t.preview_document(&s).unwrap();
        assert!(!pd.save_full().unwrap().is_empty());
        let before = contents(&s);
        let text = t.text();
        let (_, report) = t.commit(&mut s, &mut ed).unwrap().unwrap();
        assert!(report.lines >= 2);
        assert_eq!(s.undo_label(), Some("Edit text"));
        let page = s
            .text
            .page(0)
            .text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(page.contains(&text), "{page} / {text}");
        let objs = ed.objects(&s, 0).unwrap();
        let bold = objs.runs.iter().find(|r| r.text == "paragraph").unwrap();
        let f = s.doc().resolve(&bold.font.as_ref().unwrap().object);
        assert!(
            f.as_dict()
                .unwrap()
                .name(b"BaseFont")
                .unwrap()
                .ends_with(b"Bold")
        );
        s.undo();
        assert_eq!(contents(&s), before);
    }

    #[test]
    fn new_text_box() {
        let mut s = para_session();
        let mut ed = PageEditing::new();
        let style = SpanStyle::new(SpanFont::helvetica(), 14.0);
        let mut t = TextEditSession::new_box(
            &s,
            &mut ed,
            0,
            Rect::new(72.0, 500.0, 300.0, 600.0),
            style,
            Align::Left,
        )
        .unwrap();
        t.apply_style(&s, &StyleChange::Italic(true)).unwrap();
        t.insert("A new box\nwith two paragraphs");
        let c = t.caret_shape(&s).unwrap();
        assert!(c.top.y < 600.0 && c.bottom.y > 500.0, "{c:?}");
        t.commit(&mut s, &mut ed).unwrap().unwrap();
        assert_eq!(s.undo_label(), Some("Add text"));
        let text = s.text.page(0).text.clone();
        assert!(
            text.contains("A new box") && text.contains("with two paragraphs"),
            "{text}"
        );
        // An empty box adds nothing.
        let style = SpanStyle::new(SpanFont::helvetica(), 14.0);
        let t = TextEditSession::new_box(
            &s,
            &mut ed,
            0,
            Rect::new(72.0, 300.0, 300.0, 400.0),
            style,
            Align::Left,
        )
        .unwrap();
        assert!(t.commit(&mut s, &mut ed).unwrap().is_none());
    }

    #[test]
    fn block_style_op_changes_family() {
        let mut s = para_session();
        let mut ed = PageEditing::new();
        let b = block_of(&mut ed, &s, "paragraph");
        ed.edit(
            &mut s,
            0,
            &EditOp::BlockStyle {
                block: b,
                range: 2..11,
                change: StyleChange::Family("Times".into()),
            },
        )
        .unwrap();
        assert_eq!(s.undo_label(), Some("Change text style"));
        let objs = ed.objects(&s, 0).unwrap();
        let r = objs.runs.iter().find(|r| r.text == "paragraph").unwrap();
        let f = s.doc().resolve(&r.font.as_ref().unwrap().object);
        let name =
            String::from_utf8_lossy(f.as_dict().unwrap().name(b"BaseFont").unwrap()).into_owned();
        assert!(
            name.contains("Times")
                || name.to_lowercase().contains("serif")
                || name.contains("Roman"),
            "{name}"
        );
    }
}
