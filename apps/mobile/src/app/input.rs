//! Touch on the pages. One finger scrolls (and flings) unless the tool uses it: Highlight
//! selects text it starts on, Draw draws, Sign places and moves the signature, Forms fills
//! in the field tapped, Edit selects and moves page objects (see `edit`). A long press
//! selects a word to highlight; a double tap zooms. Two fingers always scroll and zoom, or
//! resize the selected signature or page object.

use std::rc::Rc;
use std::time::{Duration, Instant};

use pagebara_annot::{Annotation, Kind, MarkupKind, Point, Quad, Rect};
use pagebara_app_core::annots;
use pagebara_app_core::text_index;
use pagebara_app_core::view::gesture::{self, Press, PressState, Taps, Velocity};
use pagebara_i18n::tr;
use slint::{Color, ComponentHandle, TimerMode};

use super::edit::{EditGesture, PinchEdit};
use super::marks::{Stroke, is_signature, view_box};
use super::prepare::PrepDrag;
use super::{App, Sel, State, TextSel};
use crate::{AppState, MobileWindow, Tool, ViewState};

/// Pen width of drawings, in points.
const PEN: f64 = 2.0;
/// Displayed widths a signature can be resized to, in points.
const SIG_MIN: f64 = 24.0;
const SIG_MAX: f64 = 520.0;

#[derive(Clone, Debug, Default)]
enum Gesture {
    #[default]
    None,
    /// Down, not yet a drag, tap or long press.
    Pending,
    Pan,
    /// Selecting text; with `highlight`, it is highlighted when the finger lifts.
    Select {
        page: usize,
        highlight: bool,
    },
    Ink(Stroke),
    /// Moving the selected signature: its box (displayed points) at the start and now.
    Move {
        sel: Sel,
        from: [f64; 4],
        now: [f64; 4],
    },
    Edit(EditGesture),
    Prep(PrepDrag),
}

#[derive(Debug, Default)]
pub struct Input {
    press: Option<Press>,
    gesture: Gesture,
    velocity: Velocity,
    pub(super) taps: Taps,
    /// Resizing the selected signature with two fingers: its box at the start and now.
    pinch_box: Option<(Sel, [f64; 4], [f64; 4])>,
    pinch_center: Option<(f64, f64)>,
    /// Two fingers came down in Edit mode with something selected: whether they act on it
    /// is decided at their first move.
    pinch_edit_pending: bool,
    pinch_edit: Option<PinchEdit>,
    /// The two fingers' turn since they came down, in degrees.
    pinch_rotation: f64,
    /// A long press waits for the picture under it to be recognised: page and point.
    pub live_word: Option<(usize, f64, f64)>,
    /// The text last copied (for scripts).
    pub copied: Option<String>,
}

impl Input {
    /// The stroke being drawn.
    pub fn live_stroke(&self) -> Option<Stroke> {
        match &self.gesture {
            Gesture::Ink(s) => Some(s.clone()),
            _ => None,
        }
    }

    /// The selected signature's box while it is moved or resized.
    pub fn moving_box(&self) -> Option<(Sel, [f64; 4])> {
        match (&self.gesture, &self.pinch_box) {
            (_, Some((sel, _, now))) => Some((*sel, *now)),
            (Gesture::Move { sel, now, .. }, _) => Some((*sel, *now)),
            _ => None,
        }
    }

    /// The selected field while it is moved or resized.
    pub fn prep_drag(&self) -> Option<&PrepDrag> {
        match &self.gesture {
            Gesture::Prep(g) => Some(g),
            _ => None,
        }
    }

    /// A finger is selecting text.
    pub fn selecting(&self) -> bool {
        matches!(self.gesture, Gesture::Select { .. })
    }
}

pub fn wire(app: &Rc<App>, ui: &MobileWindow) {
    let vs = ui.global::<ViewState>();
    let a = app.clone();
    vs.on_pointer(move |kind, x, y| {
        a.act(move |app, s, ui| app.pointer(s, ui, kind, f64::from(x), f64::from(y)))
    });
    let a = app.clone();
    vs.on_pinch_start(move || a.act(|app, s, ui| app.pinch_start(s, ui)));
    let a = app.clone();
    vs.on_pinch(move |scale, x, y| {
        a.act(move |app, s, ui| app.pinch(s, ui, f64::from(scale), f64::from(x), f64::from(y)))
    });
    let a = app.clone();
    vs.on_pinch_end(move || a.act(|app, s, ui| app.pinch_end(s, ui)));
    let a = app.clone();
    vs.on_pinch_rotation(move |deg| a.act(move |_, s, _| s.input.pinch_rotation = f64::from(deg)));
    let st = ui.global::<AppState>();
    let a = app.clone();
    st.on_highlight_selection(move || a.act(|app, s, ui| app.highlight_selection(s, ui)));
    let a = app.clone();
    st.on_clear_selection(move || {
        a.act(|app, s, ui| {
            if let Some(d) = &mut s.doc {
                d.text_sel = None;
                d.marks_dirty = true;
            }
            app.refresh(s, ui);
        })
    });
    let a = app.clone();
    st.on_delete_selected(move || a.act(|app, s, ui| app.delete_selected(s, ui)));
    let a = app.clone();
    st.on_copy_selection(move || a.act(|app, s, ui| app.copy_selection(s, ui)));
    let a = app.clone();
    st.on_offer_accept(move || a.act(|app, s, ui| app.offer_accept(s, ui)));
    let a = app.clone();
    st.on_offer_dismiss(move || a.act(|app, s, ui| app.offer_dismiss(s, ui)));
}

/// How far (screen pixels) from a selection handle a touch takes it.
const HANDLE_REACH: f64 = 28.0;

/// The selection's handles: where its start and end are on the screen (at the foot of
/// the first and last line box).
pub fn handles(d: &mut super::Doc) -> Option<[(f64, f64); 2]> {
    let sel = d.text_sel.filter(|t| t.anchor != t.caret)?;
    let text = d.session.sel_text(sel.page);
    let boxes = text_index::line_boxes(&text, sel.range());
    let g = d.session.geoms[sel.page];
    let first = view_box(&g, boxes.first()?.map(f64::from));
    let last = view_box(&g, boxes.last()?.map(f64::from));
    let a = d
        .view
        .page_to_screen(sel.page, first[0], first[1] + first[3])?;
    let b = d
        .view
        .page_to_screen(sel.page, last[0] + last[2], last[1] + last[3])?;
    Some([a, b])
}

fn colour(ui: &MobileWindow) -> Color {
    let st = ui.global::<AppState>();
    let i = st.get_colour().max(0) as usize;
    slint::Model::row_data(&st.get_colours(), i).unwrap_or(Color::from_rgb_u8(0xFA, 0xCC, 0x15))
}

fn pdf_colour(c: Color) -> pagebara_annot::Color {
    pagebara_annot::Color::Rgb(
        f32::from(c.red()) / 255.0,
        f32::from(c.green()) / 255.0,
        f32::from(c.blue()) / 255.0,
    )
}

impl App {
    fn pointer(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, kind: i32, x: f64, y: f64) {
        if s.doc.is_none() || s.input.pinch_center.is_some() {
            return;
        }
        let now = Instant::now();
        match kind {
            0 => self.down(s, ui, x, y, now),
            1 => self.moved(s, ui, x, y, now),
            2 => self.up(s, ui, x, y, now),
            _ => {
                s.input.press = None;
                s.input.gesture = Gesture::None;
                s.timers.long_press.stop();
                self.marks_changed(s, ui);
            }
        }
    }

    fn marks_changed(self: &Rc<Self>, s: &mut State, ui: &MobileWindow) {
        if let Some(d) = &mut s.doc {
            d.marks_dirty = true;
        }
        self.refresh(s, ui);
    }

    fn down(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, x: f64, y: f64, now: Instant) {
        let tool = ui.global::<AppState>().get_tool();
        s.input.press = Some(Press::down(x, y, now));
        s.input.velocity.clear();
        s.input.velocity.push(x, y, now);
        let Some(d) = &mut s.doc else { return };
        if d.view.animating() {
            // A touch stops a fling where it is.
            d.view.drag_start();
        }
        let hit = d.view.screen_to_page(x, y);
        s.input.gesture = Gesture::Pending;
        s.input.live_word = None;
        if tool == Tool::Edit {
            if let Some((page, u, v)) = hit
                && let Some(g) = Self::edit_down(s, page, u, v, x, y)
            {
                s.input.gesture = Gesture::Edit(g);
            }
            return self.marks_changed(s, ui);
        }
        if tool == Tool::Prepare {
            if let Some((page, u, v)) = hit
                && let Some(g) = Self::prep_down(s, page, u, v)
            {
                s.input.gesture = Gesture::Prep(g);
            }
            return self.marks_changed(s, ui);
        }
        let Some(d) = &mut s.doc else { return };
        // A handle of the selection moves that end; the other stays.
        if let Some(ends) = handles(d)
            && let Some(sel) = d.text_sel
            && let Some(k) = (0..2)
                .filter(|&k| (ends[k].0 - x).hypot(ends[k].1 - y) <= HANDLE_REACH)
                .min_by(|&a, &b| {
                    let d = |k: usize| (ends[k].0 - x).hypot(ends[k].1 - y);
                    d(a).total_cmp(&d(b))
                })
        {
            let r = sel.range();
            let (anchor, caret) = if k == 0 {
                (r.end, r.start)
            } else {
                (r.start, r.end)
            };
            d.text_sel = Some(TextSel {
                page: sel.page,
                anchor,
                caret,
            });
            s.input.gesture = Gesture::Select {
                page: sel.page,
                highlight: false,
            };
            if let Some(p) = &mut s.input.press {
                p.state = PressState::LongPress;
            }
            return self.marks_changed(s, ui);
        }
        match (tool, hit) {
            (Tool::Draw, Some((page, u, v))) => {
                s.input.gesture = Gesture::Ink(Stroke {
                    page,
                    points: vec![(u, v)],
                    color: colour(ui),
                    width: PEN,
                });
            }
            (Tool::Highlight, Some((page, u, v))) => {
                if self.start_select(s, page, u, v, false, true) {
                    s.input.gesture = Gesture::Select {
                        page,
                        highlight: true,
                    };
                }
            }
            (Tool::Sign, Some((page, u, v))) => {
                if let Some(sel) = d.selected.filter(|s| s.page == page)
                    && let Some(a) = d.annot(sel)
                {
                    let g = d.session.geoms[page];
                    let r = view_box(&g, [a.rect.x0, a.rect.y0, a.rect.x1, a.rect.y1]);
                    let slop = gesture::SLOP / d.view.last.k.max(0.01);
                    if u >= r[0] - slop
                        && v >= r[1] - slop
                        && u <= r[0] + r[2] + slop
                        && v <= r[1] + r[3] + slop
                    {
                        s.input.gesture = Gesture::Move {
                            sel,
                            from: r,
                            now: r,
                        };
                    }
                }
            }
            _ => {}
        }
        if matches!(s.input.gesture, Gesture::Pending) && tool == Tool::Read {
            let app = self.clone();
            s.timers
                .long_press
                .start(TimerMode::SingleShot, gesture::LONG_PRESS, move || {
                    app.act(|app, s, ui| app.long_press(s, ui))
                });
        }
        self.marks_changed(s, ui);
    }

    /// Selects the word under a finger held still.
    fn long_press(self: &Rc<Self>, s: &mut State, ui: &MobileWindow) {
        let Some(p) = &mut s.input.press else { return };
        // The timer decides (it may fire a little before `Press::poll` would).
        if p.state != PressState::Pending || !matches!(s.input.gesture, Gesture::Pending) {
            return;
        }
        p.state = PressState::LongPress;
        let (x, y) = p.start;
        let Some((page, u, v)) = s.doc.as_ref().and_then(|d| d.view.screen_to_page(x, y)) else {
            return;
        };
        if self.start_select(s, page, u, v, true, false) {
            s.input.gesture = Gesture::Select {
                page,
                highlight: false,
            };
        } else if let Some(d) = &s.doc {
            // No text there: perhaps a picture of text.
            let g = d.session.geoms[page];
            let (px, py) = g.to_pdf(u, v);
            if self.live_long_press(s, page, px, py) {
                self.message(s, &tr!("Reading the text in the picture…"));
            }
        }
        self.marks_changed(s, ui);
    }

    /// Starts a text selection at a point of a page (the word there with `word`); false
    /// when there is no text there.
    fn start_select(
        self: &Rc<Self>,
        s: &mut State,
        page: usize,
        u: f64,
        v: f64,
        word: bool,
        near_text: bool,
    ) -> bool {
        let Some(d) = &mut s.doc else { return false };
        let g = d.session.geoms[page];
        let (px, py) = g.to_pdf(u, v);
        let text = d.session.sel_text(page);
        if !text_index::over_text(&text, px as f32, py as f32) && (near_text || word) {
            return false;
        }
        let Some(caret) = text_index::caret_at(&text, px as f32, py as f32) else {
            return false;
        };
        let range = if word {
            text_index::word_at(&text, caret.min(text.chars.len().saturating_sub(1)))
        } else {
            None
        };
        d.text_sel = Some(match range {
            Some(r) => TextSel {
                page,
                anchor: r.start,
                caret: r.end,
            },
            None => TextSel {
                page,
                anchor: caret,
                caret,
            },
        });
        d.selected = None;
        true
    }

    fn moved(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, x: f64, y: f64, now: Instant) {
        s.input.velocity.push(x, y, now);
        let Some(p) = &mut s.input.press else { return };
        let state = p.moved(x, y, now);
        let (sx, sy) = p.start;
        let near = (x - sx).hypot(y - sy) <= gesture::SLOP;
        let Some(d) = &mut s.doc else { return };
        match &mut s.input.gesture {
            Gesture::Pending if state == PressState::Drag => {
                s.timers.long_press.stop();
                s.input.gesture = Gesture::Pan;
                d.view.drag_start();
                d.view.drag(x - sx, y - sy);
                self.refresh(s, ui);
            }
            Gesture::Pan => {
                d.view.drag(x - sx, y - sy);
                self.refresh(s, ui);
            }
            // A still finger keeps the word a long press selected.
            Gesture::Select { page, .. } if !(near && state == PressState::LongPress) => {
                let page = *page;
                let Some((u, v)) = d.view.screen_to_page_of(page, x, y) else {
                    return;
                };
                let g = d.session.geoms[page];
                let (px, py) = g.to_pdf(u, v);
                let text = d.session.sel_text(page);
                if let Some(caret) = text_index::caret_at(&text, px as f32, py as f32)
                    && let Some(sel) = &mut d.text_sel
                {
                    sel.caret = caret;
                }
                self.marks_changed(s, ui);
            }
            Gesture::Ink(stroke) => {
                if let Some(pt) = d.view.screen_to_page_of(stroke.page, x, y) {
                    let last = stroke.points.last().copied();
                    let k = d.view.last.k.max(0.01);
                    if last.is_none_or(|l| (l.0 - pt.0).hypot(l.1 - pt.1) * k >= 1.5) {
                        stroke.points.push(pt);
                    }
                }
                self.marks_changed(s, ui);
            }
            Gesture::Move { from, now, .. } => {
                let k = d.view.last.k.max(0.01);
                let (dx, dy) = ((x - sx) / k, (y - sy) / k);
                *now = [from[0] + dx, from[1] + dy, from[2], from[3]];
                self.marks_changed(s, ui);
            }
            Gesture::Edit(g) => {
                let g = g.clone();
                self.edit_moved(s, ui, &g, x, y, state);
            }
            Gesture::Prep(g) if state == PressState::Drag => {
                let k = d.view.last.k.max(0.01);
                Self::prep_drag(g, (x - sx) / k, (y - sy) / k);
                self.marks_changed(s, ui);
            }
            _ => {}
        }
    }

    fn up(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, x: f64, y: f64, now: Instant) {
        s.timers.long_press.stop();
        let gesture = std::mem::take(&mut s.input.gesture);
        let tap = s.input.press.take().is_some_and(|mut p| p.is_tap(now));
        match gesture {
            Gesture::Pan => {
                let v = s.input.velocity.get(now);
                if let Some(d) = &mut s.doc {
                    d.view.fling(v);
                }
                self.animate(s, ui);
            }
            Gesture::Select { highlight, .. } => {
                if highlight {
                    self.highlight_selection(s, ui);
                } else {
                    self.marks_changed(s, ui);
                }
            }
            Gesture::Ink(stroke) => self.add_ink(s, ui, stroke, tap),
            Gesture::Move { sel, from, now } => {
                if from != now {
                    self.move_signature(s, ui, sel, now, &tr!("Move signature"));
                } else {
                    self.marks_changed(s, ui);
                }
            }
            Gesture::Edit(g) => self.edit_up(s, ui, g, (x, y), tap, now),
            Gesture::Prep(_) if tap => self.tap(s, ui, x, y, now),
            Gesture::Prep(g) => self.prep_drop(s, ui, g),
            Gesture::Pending if tap => self.tap(s, ui, x, y, now),
            _ => self.marks_changed(s, ui),
        }
    }

    fn tap(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, x: f64, y: f64, now: Instant) {
        let tool = ui.global::<AppState>().get_tool();
        if tool == Tool::Edit {
            return self.edit_tap(s, ui, x, y, now);
        }
        if s.input.taps.tap(x, y, now) && matches!(tool, Tool::Read | Tool::Highlight) {
            return self.double_tap(s, ui, x, y);
        }
        let Some(d) = &mut s.doc else { return };
        let hadsel = d.text_sel.take().is_some();
        let hit = d.view.screen_to_page(x, y);
        match (tool, hit) {
            (Tool::Sign, Some((page, u, v))) => {
                // A tap on a signature selects it, elsewhere places one.
                let g = d.session.geoms[page];
                let (px, py) = g.to_pdf(u, v);
                let list = d.annots(page);
                let tol = gesture::SLOP / d.view.last.k.max(0.01);
                let on = list
                    .iter()
                    .rev()
                    .filter(|a| is_signature(a))
                    .find(|a| annots::hits(a, Point::new(px, py), tol))
                    .and_then(|a| a.id);
                match on {
                    Some(id) => d.selected = Some(Sel { page, id }),
                    None => return self.place_signature(s, ui, page, u, v),
                }
            }
            (Tool::Forms, Some((page, u, v))) => {
                let g = d.session.geoms[page];
                let (px, py) = g.to_pdf(u, v);
                return self.field_at(s, ui, page, Point::new(px, py));
            }
            (Tool::Prepare, Some((page, u, v))) => return self.prep_tap(s, ui, page, u, v),
            _ => {
                if !hadsel {
                    d.selected = None;
                }
            }
        }
        self.marks_changed(s, ui);
    }

    /// Zooms in on the point, or back to the page width.
    fn double_tap(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, x: f64, y: f64) {
        let Some(d) = &mut s.doc else { return };
        if d.view.is_fit_width() {
            d.view.pinch_start();
            d.view.pinch(2.5, x, y);
        } else {
            d.view.fit_width(4.0);
        }
        self.refresh(s, ui);
    }

    fn add_ink(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, stroke: Stroke, dot: bool) {
        let Some(d) = &mut s.doc else { return };
        if dot && stroke.points.len() < 2 {
            // A tap with the pen is not a drawing.
            return self.marks_changed(s, ui);
        }
        if !may_annotate(d) {
            return self.message(
                s,
                &tr!("This document's security settings do not allow drawing."),
            );
        }
        let g = d.session.geoms[stroke.page];
        let pts: Vec<Point> = stroke
            .points
            .iter()
            .map(|&(u, v)| {
                let (x, y) = g.to_pdf(u, v);
                Point::new(x, y)
            })
            .collect();
        let pts = annots::smooth(&annots::smooth(&pts));
        let mut a = Annotation::new(Kind::Ink { strokes: vec![pts] }, Rect::default());
        a.color = Some(pdf_colour(stroke.color));
        a.border.width = stroke.width;
        let page = stroke.page;
        d.pending_ink.push(stroke);
        self.add_annotation(s, ui, page, a, &tr!("Add drawing"));
    }

    /// Turns the text selection into a highlight.
    pub fn highlight_selection(self: &Rc<Self>, s: &mut State, ui: &MobileWindow) {
        let c = colour(ui);
        let Some(d) = &mut s.doc else { return };
        let Some(sel) = d.text_sel.take() else { return };
        if sel.anchor == sel.caret {
            return self.marks_changed(s, ui);
        }
        if !may_annotate(d) {
            return self.message(
                s,
                &tr!("This document's security settings do not allow highlighting."),
            );
        }
        let text = d.session.sel_text(sel.page);
        let quads: Vec<Quad> = text_index::line_boxes(&text, sel.range())
            .into_iter()
            .map(|b| {
                Quad::from_rect(Rect::new(
                    f64::from(b[0]),
                    f64::from(b[1]),
                    f64::from(b[2]),
                    f64::from(b[3]),
                ))
            })
            .collect();
        if quads.is_empty() {
            return self.marks_changed(s, ui);
        }
        let words = text_index::text_of(&text, sel.range());
        let mut a = Annotation::new(
            Kind::Markup {
                kind: MarkupKind::Highlight,
                quads,
            },
            Rect::default(),
        );
        a.color = Some(pdf_colour(c));
        a.subject = Some(words.chars().take(200).collect());
        self.add_annotation(s, ui, sel.page, a, &tr!("Add highlight"));
    }

    /// Adds `a` to page `page` as one undoable step; returns its reference.
    pub fn add_annotation(
        self: &Rc<Self>,
        s: &mut State,
        ui: &MobileWindow,
        page: usize,
        a: Annotation,
        label: &str,
    ) -> Option<pagebara_syntax::ObjRef> {
        let d = s.doc.as_mut()?;
        let r = d.session.page_ref(page)?;
        let res = d.session.edit(label, |doc| {
            pagebara_annot::add_with(doc, r, &a, &pagebara_annot::Helvetica)
        });
        match res {
            Ok((w, _)) => {
                self.after_edit(s, ui);
                Some(w.id)
            }
            Err(e) => {
                if let Some(d) = &mut s.doc {
                    d.pending_ink.clear();
                }
                self.message(s, &tr!("That did not work: {}.", e));
                self.after_edit(s, ui);
                None
            }
        }
    }

    pub fn delete_selected(self: &Rc<Self>, s: &mut State, ui: &MobileWindow) {
        let Some(d) = &mut s.doc else { return };
        let Some(sel) = d.selected.take() else { return };
        let Some(page) = d.session.page_ref(sel.page) else {
            return;
        };
        if let Err(e) = d.session.edit(&tr!("Delete"), |doc| {
            pagebara_annot::remove(doc, page, sel.id)
        }) {
            self.message(s, &tr!("That did not work: {}.", e));
        }
        self.after_edit(s, ui);
    }

    pub(crate) fn pinch_start(self: &Rc<Self>, s: &mut State, ui: &MobileWindow) {
        s.timers.long_press.stop();
        // The first finger's gesture gives way to the two-finger one.
        s.input.gesture = Gesture::None;
        s.input.press = None;
        let tool = ui.global::<AppState>().get_tool();
        if s.doc.is_none() {
            return;
        }
        s.input.pinch_center = None;
        s.input.pinch_box = None;
        s.input.pinch_edit = None;
        s.input.pinch_rotation = 0.0;
        s.input.pinch_edit_pending = Self::edit_pinch_start(s, tool);
        let Some(d) = &mut s.doc else { return };
        if s.input.pinch_edit_pending {
            // Decided at the first move.
        } else if tool == Tool::Sign
            && let Some(sel) = d.selected
            && let Some(a) = d.annot(sel)
        {
            let g = d.session.geoms[sel.page];
            let r = view_box(&g, [a.rect.x0, a.rect.y0, a.rect.x1, a.rect.y1]);
            s.input.pinch_box = Some((sel, r, r));
        } else {
            d.view.pinch_start();
        }
        s.input.pinch_center = Some((f64::NAN, f64::NAN));
        self.marks_changed(s, ui);
    }

    pub(crate) fn pinch(
        self: &Rc<Self>,
        s: &mut State,
        ui: &MobileWindow,
        scale: f64,
        x: f64,
        y: f64,
    ) {
        if std::mem::take(&mut s.input.pinch_edit_pending) {
            s.input.pinch_edit = Self::edit_pinch_begin(s, x, y);
            if s.input.pinch_edit.is_none()
                && let Some(d) = &mut s.doc
            {
                d.view.pinch_start();
            }
        }
        if let Some(p) = s.input.pinch_edit {
            let rotation = s.input.pinch_rotation;
            return self.edit_pinch(s, ui, p, scale, rotation);
        }
        let Some(d) = &mut s.doc else { return };
        if let Some((_, from, now)) = &mut s.input.pinch_box {
            *now = gesture::scale_box(*from, scale, SIG_MIN, SIG_MAX);
            return self.marks_changed(s, ui);
        }
        d.view.pinch(scale, x, y);
        // Moving both fingers scrolls too.
        if let Some((cx, cy)) = s.input.pinch_center
            && cx.is_finite()
        {
            d.view.view.viewport.scroll_by(cx - x, cy - y);
        }
        s.input.pinch_center = Some((x, y));
        self.refresh(s, ui);
    }

    pub(crate) fn pinch_end(self: &Rc<Self>, s: &mut State, ui: &MobileWindow) {
        s.input.pinch_center = None;
        s.input.pinch_edit_pending = false;
        if s.input.pinch_edit.take().is_some() {
            return self.edit_pinch_end(s, ui);
        }
        if let Some((sel, from, now)) = s.input.pinch_box.take()
            && from != now
        {
            return self.move_signature(s, ui, sel, now, &tr!("Resize signature"));
        }
        // Tiles of the final zoom follow once the gesture settles.
        let app = self.clone();
        slint::Timer::single_shot(Duration::from_millis(200), move || {
            app.act(|app, s, ui| app.refresh(s, ui))
        });
        self.marks_changed(s, ui);
    }

    /// Puts the selected signature at `r` (displayed points).
    fn move_signature(
        self: &Rc<Self>,
        s: &mut State,
        ui: &MobileWindow,
        sel: Sel,
        r: [f64; 4],
        label: &str,
    ) {
        let Some(d) = &mut s.doc else { return };
        let Some(a) = d.annot(sel) else { return };
        let g = d.session.geoms[sel.page];
        let [x0, y0, x1, y1] = g.rect_to_pdf(r);
        let moved = annots::resized(&a, Rect::new(x0, y0, x1, y1));
        if let Err(e) = d.session.edit(label, |doc| {
            pagebara_annot::update_with(doc, sel.id, &moved, &pagebara_annot::Helvetica)
        }) {
            self.message(s, &tr!("That did not work: {}.", e));
        }
        if let Some(d) = &mut s.doc {
            d.selected = Some(sel);
        }
        self.after_edit(s, ui);
    }
}

/// Whether the document allows adding annotations.
pub fn may_annotate(d: &super::Doc) -> bool {
    let p = d.session.doc().permissions();
    p.annotate || p.modify
}
