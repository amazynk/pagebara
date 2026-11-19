//! Editing a paragraph in place, as the apps drive it: a hidden text field owns the typing
//! (keys, clipboard, the input method's composition) and reports its text, caret and
//! selection; [`TextEdit`] turns those reports into a [`TextEditSession`] edit (so styles
//! and reflow come from the engine) and gives back what to draw: the paragraph rendered as
//! the page will show it ([`TextEdit::raster`]), the caret and the selection.
//!
//! Offsets are byte offsets into [`TextEdit::text`], as Slint's `TextInput` counts them.
//! Moving by visual lines (Up, Down, Home, End) goes through the layout
//! ([`TextEdit::move_caret`]); the field is then told the new selection.

use pagebara_edit::{ObjectId, PageObject, PageObjects, Point, Rect, StyledText, TextRun};
use pagebara_i18n::tr;

use super::raster::{Raster, pixel_rect, render_page};
use crate::edit::{
    BlockReport, Motion, PageEditing, SpanFont, SpanStyle, StyleChange, TextEditSession,
};
use crate::page_theme::PageTheme;
use crate::session::Session;
use crate::spell::SpellChecker;
use crate::tiles::RenderBackend;

pub use crate::edit::StyleSummary;

/// A caret: from the descent to the ascent, in page space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CaretBox {
    pub bottom: Point,
    pub top: Point,
}

/// What a drag in the text selects by: characters after a click, words after a double
/// click, lines after a triple click.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DragUnit {
    Character,
    Word,
    Line,
}

/// What to draw for the paragraph being edited (page space).
#[derive(Clone, Debug, Default)]
pub struct TextView {
    /// Where the page draws the original text.
    pub cover: Option<Rect>,
    /// Bounds of the laid-out text.
    pub bbox: Option<Rect>,
    pub caret: Option<CaretBox>,
    pub selection: Vec<[Point; 4]>,
    /// Under the input method's composition text.
    pub preedit: Vec<[Point; 4]>,
    /// How far the text runs below where the original ended (page units).
    pub overflow: f32,
    /// A word is too wide for the box.
    pub overfull: bool,
    /// Characters no font can show.
    pub missing: String,
    /// The text of each laid-out line (with any composition text).
    pub lines: Vec<String>,
    /// Misspelled words, with quads covering each (none while composing).
    pub misspelled: Vec<Misspelled>,
}

/// A misspelled word of the paragraph being edited.
#[derive(Clone, Debug, PartialEq)]
pub struct Misspelled {
    /// Byte range in [`TextEdit::text`].
    pub range: std::ops::Range<usize>,
    pub word: String,
    /// One quad per line the word is on (page space).
    pub quads: Vec<[Point; 4]>,
}

impl TextView {
    /// The misspelled word at a page-space point.
    pub fn misspelled_at(&self, p: Point) -> Option<&Misspelled> {
        self.misspelled.iter().find(|m| {
            m.quads.iter().any(|q| {
                let r = Rect::from_points(q);
                p.x >= r.x0 && p.x <= r.x1 && p.y >= r.y0 && p.y <= r.y1
            })
        })
    }

    /// The misspelled word holding byte offset `at` (the caret), ends included.
    pub fn misspelled_around(&self, at: usize) -> Option<&Misspelled> {
        self.misspelled
            .iter()
            .find(|m| m.range.start <= at && at <= m.range.end)
    }
}

/// What a [`Raster`] was rendered for.
#[derive(Debug, PartialEq)]
struct RasterKey {
    spans: Vec<StyledText>,
    generation: u64,
    scale: u32,
    theme: PageTheme,
}

/// How a text block looks, for the property panel.
#[derive(Clone, Debug, PartialEq)]
pub struct TextLook {
    /// The font's name without a subset tag.
    pub font: String,
    pub size: f32,
    pub color: [f32; 3],
    pub bold: bool,
    pub italic: bool,
}

fn len(a: Point) -> f32 {
    a.x.hypot(a.y)
}

/// A font name without its subset tag (`ABCDEF+`).
fn clean_name(name: &str) -> String {
    match name.split_once('+') {
        Some((tag, rest)) if tag.len() == 6 && tag.chars().all(|c| c.is_ascii_uppercase()) => {
            rest.to_owned()
        }
        _ => name.to_owned(),
    }
}

/// The look of a run: font name, size in page units, colour, and bold or italic as the
/// font's name or flags say.
pub fn run_look(run: &TextRun) -> TextLook {
    let font = run
        .font
        .as_ref()
        .and_then(|f| f.font.base_font.as_ref())
        .map(|n| clean_name(&String::from_utf8_lossy(n.as_bytes())))
        .unwrap_or_else(|| "Helvetica".into());
    let flags = run.font.as_ref().map_or(0, |f| f.font.flags);
    let lower = font.to_lowercase();
    let em = len(run.matrix().apply_vector(Point::new(0.0, run.size)));
    TextLook {
        bold: ["bold", "black", "heavy", "semibold", "demi"]
            .iter()
            .any(|w| lower.contains(w))
            || flags & (1 << 18) != 0,
        italic: lower.contains("italic") || lower.contains("oblique") || flags & 64 != 0,
        font,
        size: em,
        color: run.fill.rgb().unwrap_or([0.0; 3]),
    }
}

/// The look of a text block (its first run).
pub fn text_look(objs: &PageObjects, ob: &PageObject) -> Option<TextLook> {
    let r = ob.text()?.runs().next()?;
    objs.run(r).map(run_look)
}

/// The length in bytes of a block's text as paragraph edits count it, for
/// [`EditOp::BlockStyle`](crate::edit::EditOp::BlockStyle) over all of it.
pub fn block_len(objs: &PageObjects, block: ObjectId) -> Option<usize> {
    let bt = pagebara_edit::block_text(objs, block).ok()?;
    Some(bt.spans.iter().map(|s| s.text.len()).sum())
}

fn err(e: pagebara_edit::Error) -> String {
    match e {
        pagebara_edit::Error::Encode(pagebara_edit::EncodeError::Missing(m)) => {
            tr!("No font here can show “{}”.", m)
        }
        e => tr!("The text could not be changed: {}.", e),
    }
}

/// The paragraph being edited.
#[derive(Debug)]
pub struct TextEdit {
    pub page: usize,
    session: TextEditSession,
    /// The text as the field has it (without composition text).
    field: String,
    raster: Option<(RasterKey, Raster)>,
    spelling: Option<SpellChecker>,
}

impl TextEdit {
    /// Starts editing text block `block` of `page`, the caret at its end.
    pub fn open(
        s: &Session,
        ed: &mut PageEditing,
        page: usize,
        block: ObjectId,
    ) -> Result<TextEdit, String> {
        let session = TextEditSession::open(s, ed, page, block).map_err(err)?;
        Ok(TextEdit {
            page,
            field: session.text(),
            raster: None,
            spelling: None,
            session,
        })
    }

    /// Starts a new text box in a page-space rectangle, in Helvetica at `size` points.
    pub fn new_box(
        s: &Session,
        ed: &mut PageEditing,
        page: usize,
        rect: Rect,
        size: f32,
        color: [f32; 3],
    ) -> Result<TextEdit, String> {
        let mut style = SpanStyle::new(SpanFont::helvetica(), size);
        style.fill = pagebara_edit::Paint {
            space: pagebara_syntax::Name::new(b"DeviceRGB"),
            components: color.to_vec(),
            pattern: None,
        };
        let session = TextEditSession::new_box(s, ed, page, rect, style, crate::edit::Align::Left)
            .map_err(err)?;
        Ok(TextEdit {
            page,
            field: String::new(),
            raster: None,
            spelling: None,
            session,
        })
    }

    /// The block being edited (`None` for a new box).
    pub fn block(&self) -> Option<ObjectId> {
        self.session.block()
    }

    pub fn text(&self) -> String {
        self.session.text()
    }

    pub fn changed(&self) -> bool {
        self.session.is_modified()
    }

    /// The selection as `(anchor, caret)` byte offsets.
    pub fn selection(&self) -> (usize, usize) {
        let r = self.session.selection();
        let c = self.session.caret();
        if c == r.start {
            (r.end, c)
        } else {
            (r.start, c)
        }
    }

    /// The field's text, caret, anchor (bytes) and composition text now. Returns true when
    /// the field should take [`TextEdit::text`] (the edit normalised what was typed).
    pub fn set_field(&mut self, text: &str, caret: usize, anchor: usize, preedit: &str) -> bool {
        if text != self.field {
            let old = &self.field;
            let p = old
                .char_indices()
                .zip(text.chars())
                .find(|((_, a), b)| a != b)
                .map_or(old.len().min(text.len()), |((i, _), _)| i);
            let max_q = (old.len() - p).min(text.len() - p);
            let mut q = 0;
            for (a, b) in old[p..].chars().rev().zip(text[p..].chars().rev()) {
                if a != b || q + a.len_utf8() > max_q {
                    break;
                }
                q += a.len_utf8();
            }
            self.session.select(p..old.len() - q);
            self.session.insert(&text[p..text.len() - q]);
            self.field = text.to_owned();
        }
        let reset = self.session.text() != self.field;
        if reset {
            self.field = self.session.text();
        } else {
            self.session.select(anchor..caret);
        }
        self.session.set_preedit(preedit);
        reset
    }

    /// Puts the caret at a page-space point; with `extend` the selection grows to it.
    pub fn click(&mut self, s: &Session, p: Point, extend: bool) {
        self.session.click(s, p, extend);
    }

    pub fn select_word_at(&mut self, s: &Session, p: Point) {
        self.session.select_word_at(s, p);
    }

    /// Selects the (visual) line at a page-space point.
    pub fn select_line_at(&mut self, s: &Session, p: Point) {
        self.session.click(s, p, false);
        self.session.move_caret(s, Motion::LineStart, false);
        self.session.move_caret(s, Motion::LineEnd, true);
    }

    /// A drag to a page-space point after a press that selected `first` (anchor and caret,
    /// as [`TextEdit::selection`] gave them). After a double or triple click the selection
    /// grows from `first` by whole words or lines and never shrinks into it, so the hand
    /// moving a little between press and release leaves the word selected.
    pub fn drag_to(&mut self, s: &Session, p: Point, unit: DragUnit, first: (usize, usize)) {
        match unit {
            DragUnit::Character => return self.session.click(s, p, true),
            DragUnit::Word => self.session.select_word_at(s, p),
            DragUnit::Line => self.select_line_at(s, p),
        }
        let here = self.session.selection();
        let (start, end) = (first.0.min(first.1), first.0.max(first.1));
        let (anchor, caret) = if here.start < start {
            (end, here.start)
        } else {
            (start, end.max(here.end))
        };
        self.session.select(anchor..caret);
    }

    pub fn select_all(&mut self) {
        self.session.select_all();
    }

    /// Moves the caret by the layout's lines (or anything else `m` says).
    pub fn move_caret(&mut self, s: &Session, m: Motion, extend: bool) {
        self.session.move_caret(s, m, extend);
    }

    /// Bold, italic, size, colour or font for the selection (or what is typed next).
    pub fn apply_style(&mut self, s: &Session, change: &StyleChange) -> Result<(), String> {
        self.session.apply_style(s, change).map_err(err)
    }

    /// The selection's style; `None` where it is mixed.
    pub fn style(&mut self, s: &Session) -> StyleSummary {
        self.session.selection_style(s)
    }

    /// Font families to offer.
    pub fn families(&self, s: &Session) -> Vec<String> {
        self.session.font_families(s)
    }

    /// What to draw now.
    pub fn view(&mut self, s: &Session, ed: &mut PageEditing) -> TextView {
        let cover = self.session.original_bounds(s, ed);
        let mut v = TextView {
            cover,
            ..Default::default()
        };
        if let Ok(l) = self.session.preview(s) {
            v.bbox = Some(l.bbox);
            v.overflow = l.overflow;
            v.overfull = l.overfull;
            v.missing = l.missing.iter().map(|(_, c)| *c).collect();
            v.lines = l
                .lines
                .iter()
                .map(|line| l.text.get(line.range.clone()).unwrap_or("").to_owned())
                .collect();
        }
        v.caret = self.session.caret_shape(s).ok().map(|c| CaretBox {
            bottom: c.bottom,
            top: c.top,
        });
        v.selection = self.session.selection_quads(s).unwrap_or_default();
        v.preedit = self.session.preedit_quads(s).unwrap_or_default();
        if let Some(c) = &self.spelling
            && v.preedit.is_empty()
        {
            let text = self.session.text();
            v.misspelled = c
                .check_text(&text)
                .into_iter()
                .map(|m| Misspelled {
                    quads: self
                        .session
                        .range_quads(s, m.range.clone())
                        .unwrap_or_default(),
                    range: m.range,
                    word: m.word,
                })
                .collect();
        }
        v
    }

    /// Checks the spelling with `checker` (`None`: not at all) from the next [`TextEdit::view`].
    pub fn set_spelling(&mut self, checker: Option<SpellChecker>) {
        self.spelling = checker;
    }

    pub fn spelling(&self) -> Option<&SpellChecker> {
        self.spelling.as_ref()
    }

    /// Replaces byte range `r` of the text with `with` (a correction), the caret after it.
    /// The field must then take [`TextEdit::text`] and [`TextEdit::selection`].
    pub fn replace(&mut self, r: std::ops::Range<usize>, with: &str) {
        let text = self.session.text();
        if r.end > text.len() || !text.is_char_boundary(r.start) || !text.is_char_boundary(r.end) {
            return;
        }
        self.session.set_preedit("");
        self.session.select(r);
        self.session.insert(with);
        self.field = self.session.text();
    }

    /// The paragraph as committing it will show: the page rendered over the old and new
    /// text's bounds with the edit applied, at `scale` device pixels per point, by the
    /// renderer `backend` makes (the one drawing the page) in `theme`'s colours. Kept until
    /// the text, the document, the scale or the theme change.
    pub fn raster(
        &mut self,
        s: &Session,
        ed: &mut PageEditing,
        backend: &dyn RenderBackend,
        theme: PageTheme,
        scale: f32,
    ) -> Option<Raster> {
        let key = RasterKey {
            spans: self.session.shown(),
            generation: s.generation(),
            scale: scale.to_bits(),
            theme,
        };
        if let Some((k, r)) = &self.raster
            && *k == key
        {
            return Some(r.clone());
        }
        let objs = ed.objects(s, self.page).ok()?;
        let laid = self.session.preview(s).ok().map(|l| l.bbox);
        let area = [self.session.original_bounds(s, ed), laid]
            .into_iter()
            .flatten()
            .reduce(|a, b| a.union(&b))?;
        // A little around the text, for synthetic bold and antialiasing.
        let region = pixel_rect(s, self.page, objs.frame.view_rect(area), 2.0, scale)?;
        let doc = self.session.preview_document(s).ok()?;
        let raster = render_page(s, &doc, self.page, region, backend, theme, scale)?;
        self.raster = Some((key, raster.clone()));
        Some(raster)
    }

    /// The scale the last [`Self::raster`] was rendered at.
    pub fn raster_scale(&self) -> Option<f32> {
        self.raster.as_ref().map(|(k, _)| f32::from_bits(k.scale))
    }

    /// Writes the paragraph as one undoable step; `None` when nothing changed.
    pub fn commit(
        self,
        s: &mut Session,
        ed: &mut PageEditing,
    ) -> Result<Option<BlockReport>, String> {
        self.session
            .commit(s, ed)
            .map(|r| r.map(|(_, report)| report))
            .map_err(err)
    }
}

/// The byte offset of character `n` of `s`.
pub fn byte_of(s: &str, n: usize) -> usize {
    s.char_indices().nth(n).map_or(s.len(), |(i, _)| i)
}

/// The character offset of byte `b` of `s`.
pub fn char_of(s: &str, b: usize) -> usize {
    s.char_indices().take_while(|(i, _)| *i < b).count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_pdf;
    use pagebara_syntax::Document;

    fn session() -> Session {
        let d = Document::open(test_pdf::text(&["First line", "Second line"])).unwrap();
        Session::new(None, d)
    }

    fn open(s: &Session, ed: &mut PageEditing) -> TextEdit {
        let objs = ed.objects(s, 0).unwrap();
        let block = objs
            .objects
            .iter()
            .find(|o| o.text().is_some_and(|t| t.text.contains("First")))
            .unwrap()
            .id;
        TextEdit::open(s, ed, 0, block).unwrap()
    }

    #[test]
    fn misspelled_words_and_corrections() {
        let s = session();
        let mut ed = PageEditing::new();
        let mut e = open(&s, &mut ed);
        let spelling = crate::spell::Spelling::new(crate::spell::Config::default());
        e.set_spelling(Some(spelling.checker("en").unwrap()));
        let t = e.text().replacen("First", "Frist", 1);
        e.set_field(&t, 0, 0, "");
        let v = e.view(&s, &mut ed);
        assert_eq!(v.misspelled.len(), 1);
        let m = &v.misspelled[0];
        assert_eq!((m.word.as_str(), m.range.clone()), ("Frist", 0..5));
        assert_eq!(m.quads.len(), 1);
        let r = Rect::from_points(&m.quads[0]);
        assert!(r.width() > 5.0 && r.height() > 2.0);
        let c = Point::new((r.x0 + r.x1) / 2.0, (r.y0 + r.y1) / 2.0);
        assert_eq!(v.misspelled_at(c).map(|m| m.range.clone()), Some(0..5));
        assert!(v.misspelled_around(5).is_some() && v.misspelled_around(7).is_none());
        e.replace(0..5, "First");
        assert!(e.text().starts_with("First line"));
        assert!(e.view(&s, &mut ed).misspelled.is_empty());
        // Composition text hides the marks until it is committed.
        let t = e.text().replacen("First", "Frist", 1);
        e.set_field(&t, 0, 0, "x");
        assert!(e.view(&s, &mut ed).misspelled.is_empty());
    }

    #[test]
    fn field_changes_become_session_edits() {
        let s = session();
        let mut ed = PageEditing::new();
        let mut e = open(&s, &mut ed);
        let t = e.text();
        assert!(t.contains("First line"));
        assert!(!e.changed());
        // Typing "ly" after "First".
        let typed = t.replacen("First", "Firstly", 1);
        assert!(!e.set_field(&typed, 7, 7, ""));
        assert_eq!(e.text(), typed);
        assert!(e.changed());
        assert_eq!(e.selection(), (7, 7));
        // A selection, then deleting it.
        e.set_field(&typed, 0, 7, "");
        assert_eq!(e.selection(), (7, 0));
        let cut = &typed[7..];
        e.set_field(cut, 0, 0, "");
        assert_eq!(e.text(), cut);
        // Composition text is shown, not part of the text.
        e.set_field(cut, 0, 0, "ab");
        assert_eq!(e.text(), cut);
        let mut ed2 = PageEditing::new();
        let v = e.view(&s, &mut ed2);
        assert!(!v.preedit.is_empty());
        assert!(v.cover.is_some() && v.caret.is_some());
    }

    #[test]
    fn styles_and_commit() {
        let mut s = session();
        let mut ed = PageEditing::new();
        let mut e = open(&s, &mut ed);
        let t = e.text();
        e.set_field(&t, 5, 0, "");
        e.apply_style(&s, &StyleChange::Bold(true)).unwrap();
        assert_eq!(e.style(&s).bold, Some(true));
        e.set_field(&t, 8, 0, "");
        assert_eq!(e.style(&s).bold, None);
        let n = t.len();
        e.set_field(&format!("{t} more"), n + 5, n + 5, "");
        let report = e.commit(&mut s, &mut ed).unwrap();
        assert!(report.is_some());
        assert_eq!(s.undo_label(), Some("Edit text"));
        assert!(
            s.text.page(0).text.contains("more"),
            "{}",
            s.text.page(0).text
        );
        // The bold word is written in a bold face (or emulated with a stroke).
        let objs = ed.objects(&s, 0).unwrap();
        let looks: Vec<String> = objs
            .runs
            .iter()
            .map(|r| format!("{} {}", run_look(r).font, r.render_mode))
            .collect();
        assert!(
            looks
                .iter()
                .any(|l| l.contains("Bold") || l.ends_with(" 2")),
            "{looks:?}"
        );
    }

    /// The raster is the page rendered with the edit: the new text's ink, the page's paper
    /// around it, recoloured for a dark page theme; kept until something changes.
    #[test]
    fn raster_renders_the_edit_in_page_colours() {
        let s = session();
        let mut ed = PageEditing::new();
        let mut e = open(&s, &mut ed);
        let t = e.text();
        let typed = t.replacen("First", "First and longer", 1);
        e.set_field(&typed, 16, 16, "");
        let backend = crate::tiles::EngineBackend;
        let r = e
            .raster(&s, &mut ed, &backend, PageTheme::Normal, 2.0)
            .unwrap();
        assert_eq!(r.pixels.len(), (r.width * r.height * 4) as usize);
        assert!((r.rect.width() * 2.0 - r.width as f32).abs() < 1.0);
        let dark = r.pixels.chunks(4).filter(|p| p[0] < 80).count();
        assert!(dark > 50, "no ink");
        // Paper at the top left corner (padding).
        assert_eq!(&r.pixels[..3], &[255, 255, 255]);
        let again = e
            .raster(&s, &mut ed, &backend, PageTheme::Normal, 2.0)
            .unwrap();
        assert!(std::sync::Arc::ptr_eq(&r.pixels, &again.pixels));
        let d = e
            .raster(&s, &mut ed, &backend, PageTheme::Dark, 2.0)
            .unwrap();
        assert_eq!(&d.pixels[..3], &PageTheme::Dark.paper());
        // Bold shows: more ink.
        let ink = |r: &Raster| r.pixels.chunks(4).filter(|p| p[0] < 128).count();
        e.set_field(&typed, 0, 16, "");
        e.apply_style(&s, &StyleChange::Bold(true)).unwrap();
        let b = e
            .raster(&s, &mut ed, &backend, PageTheme::Normal, 2.0)
            .unwrap();
        assert!(ink(&b) > ink(&r) + 50, "{} {}", ink(&b), ink(&r));
        // The longer text reflows onto another line: the region grows with it.
        let orig = e.view(&s, &mut ed).cover.unwrap();
        assert!(
            r.rect.height() > orig.height() + 10.0,
            "{:?} {orig:?}",
            r.rect
        );
    }

    #[test]
    fn offsets_convert_between_bytes_and_chars() {
        let s = "aé€b";
        assert_eq!(byte_of(s, 2), 3);
        assert_eq!(char_of(s, 3), 2);
        assert_eq!(byte_of(s, 10), s.len());
    }

    #[test]
    fn typing_keeps_styles_elsewhere() {
        let s = session();
        let mut ed = PageEditing::new();
        let mut e = open(&s, &mut ed);
        let t = e.text();
        e.set_field(&t, 5, 0, "");
        e.apply_style(&s, &StyleChange::Bold(true)).unwrap();
        let n = t.len();
        e.set_field(&t, n, n, "");
        e.set_field(&format!("{t}x"), n + 1, n + 1, "");
        e.set_field(&format!("{t}x"), 5, 0, "");
        assert_eq!(e.style(&s).bold, Some(true));
    }

    /// A style chosen with only a caret is for what is typed next: the field reporting the
    /// same caret again does not drop it.
    #[test]
    fn style_chosen_at_the_caret_goes_to_what_is_typed() {
        let s = session();
        let mut ed = PageEditing::new();
        let mut e = open(&s, &mut ed);
        let t = e.text();
        let n = t.len();
        e.set_field(&t, n, n, "");
        e.apply_style(&s, &StyleChange::Size(30.0)).unwrap();
        e.set_field(&t, n, n, "");
        assert_eq!(e.style(&s).size, Some(30.0));
        e.set_field(&format!("{t}xy"), n + 2, n + 2, "");
        e.set_field(&format!("{t}xy"), n + 2, n, "");
        assert_eq!(e.style(&s).size, Some(30.0));
        // Moving the caret away drops a style nothing was typed in.
        e.set_field(&format!("{t}xy"), n, n, "");
        e.apply_style(&s, &StyleChange::Size(40.0)).unwrap();
        e.set_field(&format!("{t}xy"), 1, 1, "");
        e.set_field(&format!("{t}xy"), n, n, "");
        assert_ne!(e.style(&s).size, Some(40.0));
    }

    /// A drag after a double click selects by words from the word first selected, and
    /// never less than that word.
    #[test]
    fn drag_after_a_double_click_selects_whole_words() {
        let s = session();
        let mut ed = PageEditing::new();
        let mut e = open(&s, &mut ed);
        let t = e.text();
        let mut point_at = |e: &mut TextEdit, o: usize| {
            e.set_field(&t, o, o, "");
            let c = e.view(&s, &mut ed).caret.unwrap();
            Point::new((c.bottom.x + c.top.x) / 2.0, (c.bottom.y + c.top.y) / 2.0)
        };
        let line = t.find("line").unwrap();
        let in_first = point_at(&mut e, 2);
        let later_in_first = point_at(&mut e, 4);
        let in_line = point_at(&mut e, line + 2);
        e.select_word_at(&s, in_first);
        let first = e.selection();
        assert_eq!(first, (0, 5));
        // The pointer moving inside the word keeps the word.
        e.drag_to(&s, later_in_first, DragUnit::Word, first);
        assert_eq!(e.selection(), (0, 5));
        // On to the next word: both, whole.
        e.drag_to(&s, in_line, DragUnit::Word, first);
        assert_eq!(e.selection(), (0, line + 4));
        // Back before the word first selected: from its end to the start of the other.
        e.select_word_at(&s, in_line);
        let second = e.selection();
        e.drag_to(&s, in_first, DragUnit::Word, second);
        assert_eq!(e.selection(), (line + 4, 0));
        // After a single click the drag goes character by character.
        e.click(&s, in_first, false);
        let caret = e.selection();
        e.drag_to(&s, later_in_first, DragUnit::Character, caret);
        assert_eq!(e.selection(), (2, 4));
    }

    #[test]
    fn new_paragraph_at_the_end() {
        let s = session();
        let mut ed = PageEditing::new();
        let mut e = open(&s, &mut ed);
        let t = e.text();
        let n = t.len();
        let with = format!("{t}\n");
        let reset = e.set_field(&with, n + 1, n + 1, "");
        assert!(!reset, "{:?}", e.text());
        e.set_field(&format!("{t}\nMore"), n + 5, n + 5, "");
        assert_eq!(e.text(), format!("{t}\nMore"));
    }
}
