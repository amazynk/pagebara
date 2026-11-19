//! Editing a text block in place (Edit mode). A hidden text field owns the typing, so the
//! keyboard, input methods and the clipboard work as in any field; its text, caret and
//! composition go to the shared [`TextEdit`], which reflows the paragraph in its own fonts.
//! The page shows the paragraph as the renderer will draw it (the page rendered there with
//! the edit), with the caret and selection, until Done writes it as one undo step.

use std::rc::Rc;
use std::sync::Arc;

use pagebara_app_core::edit::{Motion, StyleChange};
use pagebara_app_core::object_edit::{self as oe, TextEdit, TextView};
use pagebara_app_core::page_theme::PageTheme;
use pagebara_app_core::tiles::RenderBackend;
use pagebara_app_core::view::Placement;
use pagebara_app_core::view::gesture;
use pagebara_edit::{PageObjects, Point, Rect};
use pagebara_i18n::tr;
use slint::{Color, ComponentHandle, Image};

use super::edit::{EditGesture, inside, may_edit, reselect_text, view_rect};
use super::marks::{at, fill};
use super::{App, Doc, State};
use crate::{AppState, Mark, MobileWindow};

/// Text smaller than this on the screen (pixels) is zoomed in on for editing.
const READABLE: f64 = 15.0;
/// How far (screen pixels) from a selection handle a touch takes it.
const HANDLE_REACH: f64 = 24.0;

const SELECTION: Color = Color::from_argb_u8(0x40, 0x25, 0x63, 0xEB);
const SPELLING_RED: Color = Color::from_rgb_u8(0xE0, 0x20, 0x1B);

/// The paragraph being edited and what to draw for it.
#[derive(Debug)]
pub struct Editing {
    pub te: TextEdit,
    view: TextView,
    /// Editing zoomed in (and zooms back out when it ends).
    zoomed: bool,
    /// The paragraph's rendering for the page, and the pixels it was made from.
    image: Option<(Arc<Vec<u8>>, Image)>,
}

impl Editing {
    pub fn page(&self) -> usize {
        self.te.page
    }

    /// Lines of the paragraph as laid out now.
    pub fn lines(&self) -> usize {
        self.view.lines.len()
    }

    /// The misspelled word holding byte offset `at`.
    pub fn misspelled_around(&self, at: usize) -> Option<&oe::Misspelled> {
        self.view.misspelled_around(at)
    }

    /// The paragraph's misspelled words.
    pub fn misspelled(&self) -> &[oe::Misspelled] {
        &self.view.misspelled
    }
}

/// Lays the paragraph out again after a change.
fn relayout(d: &mut Doc) {
    if let Some(e) = &mut d.edit.text {
        e.view = e.te.view(&d.session, &mut d.edit.pages);
    }
}

/// A page-space point of the paragraph's page in view points.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub(super) fn view_point(d: &Doc, p: Point) -> Option<Point> {
    Some(frame_of(d)?.frame.view_point(p))
}

/// Lays the paragraph out again (after the spelling dictionary changed).
pub(super) fn relayout_doc(d: &mut Doc) {
    relayout(d);
}

fn frame_of(d: &Doc) -> Option<&PageObjects> {
    d.edit.objs.as_ref().map(|(_, o)| &**o)
}

/// The paragraph as shown and as the page had it, in view space.
fn text_rect(e: &Editing, objs: &PageObjects) -> oe::ViewRect {
    let r = match (e.view.bbox, e.view.cover) {
        (Some(a), Some(b)) => a.union(&b),
        (a, b) => a.or(b).unwrap_or_default(),
    };
    view_rect(objs, r)
}

fn quad_box(objs: &PageObjects, q: &[Point; 4]) -> [f64; 4] {
    view_rect(objs, Rect::from_points(q)).map(f64::from)
}

/// The ends of the selection on the screen: left of the first box, right of the last.
fn handles(d: &Doc) -> Option<[(f64, f64); 2]> {
    let e = d.edit.text.as_ref()?;
    let objs = frame_of(d)?;
    let a = quad_box(objs, e.view.selection.first()?);
    let b = quad_box(objs, e.view.selection.last()?);
    Some([
        d.view.page_to_screen(e.page(), a[0], a[1] + a[3])?,
        d.view.page_to_screen(e.page(), b[0] + b[2], b[1] + b[3])?,
    ])
}

/// A screen point as a page-space point of the page being edited.
fn page_point(d: &Doc, x: f64, y: f64) -> Option<Point> {
    let e = d.edit.text.as_ref()?;
    let (u, v) = d.view.screen_to_page_of(e.page(), x, y)?;
    Some(
        frame_of(d)?
            .frame
            .page_point(Point::new(u as f32, v as f32)),
    )
}

/// Puts the editor's selection into the hidden field.
fn push_selection(d: &Doc, ui: &MobileWindow) {
    let Some(e) = &d.edit.text else { return };
    let (anchor, caret) = e.te.selection();
    let st = ui.global::<AppState>();
    st.set_edit_sel_anchor(anchor as i32);
    st.set_edit_sel_focus(caret as i32);
    st.set_edit_sel_serial(st.get_edit_sel_serial().wrapping_add(1));
}

/// Scrolls the caret onto the screen when typing takes it off.
fn keep_caret_shown(d: &mut Doc) {
    let Some(e) = &d.edit.text else { return };
    let Some(objs) = frame_of(d) else { return };
    let Some(c) = e.view.caret else { return };
    let v = objs.frame.view_point(c.bottom);
    let Some((x, y)) = d
        .view
        .page_to_screen(e.page(), f64::from(v.x), f64::from(v.y))
    else {
        return;
    };
    let (w, h) = d.view.view.viewport.size();
    let margin = 32.0;
    let off = |p: f64, max: f64| {
        if p < margin {
            p - margin
        } else if p > max - margin {
            p - (max - margin)
        } else {
            0.0
        }
    };
    let (dx, dy) = (off(x, w), off(y, h));
    if dx != 0.0 || dy != 0.0 {
        d.view.view.viewport.scroll_by(dx, dy);
    }
}

/// A finger went down on `page` at view point `(u, v)` (screen `(x, y)`) while editing:
/// takes it in the paragraph or on a selection handle.
pub(super) fn down(
    d: &mut Doc,
    page: usize,
    u: f64,
    v: f64,
    x: f64,
    y: f64,
) -> Option<EditGesture> {
    let e = d.edit.text.as_ref()?;
    if e.page() != page {
        return None;
    }
    let k = d.view.last.k.max(0.01);
    // A handle moves its end of the selection; the other end stays.
    if let Some([a, b]) = handles(d) {
        let near = |p: (f64, f64)| (p.0 - x).hypot(p.1 - y) <= HANDLE_REACH;
        let (anchor, caret) = e.te.selection();
        let (lo, hi) = (anchor.min(caret), anchor.max(caret));
        let ends = if near(b) {
            Some((lo, hi))
        } else if near(a) {
            Some((hi, lo))
        } else {
            None
        };
        if let Some((fixed, moving)) = ends {
            let e = d.edit.text.as_mut()?;
            let text = e.te.text();
            e.te.set_field(&text, moving, fixed, "");
            return Some(EditGesture::Caret);
        }
    }
    let objs = frame_of(d)?;
    if !inside(text_rect(e, objs), u, v, gesture::SLOP / k) {
        return None;
    }
    let p = objs.frame.page_point(Point::new(u as f32, v as f32));
    let e = d.edit.text.as_mut()?;
    e.te.click(&d.session, p, false);
    relayout(d);
    Some(EditGesture::Caret)
}

impl App {
    /// The finger placing the caret moved to screen `(x, y)`: the selection grows to it.
    pub(super) fn text_moved(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, x: f64, y: f64) {
        let Some(d) = &mut s.doc else { return };
        let Some(p) = page_point(d, x, y) else { return };
        if let Some(e) = &mut d.edit.text {
            e.te.click(&d.session, p, true);
        }
        relayout(d);
        push_selection(d, ui);
        d.marks_dirty = true;
        self.refresh(s, ui);
    }

    /// The finger lifted; a double tap selects the word there.
    pub(super) fn text_up(
        self: &Rc<Self>,
        s: &mut State,
        ui: &MobileWindow,
        (x, y): (f64, f64),
        tap: bool,
        now: std::time::Instant,
    ) {
        let double = tap && s.input.taps.tap(x, y, now);
        let Some(d) = &mut s.doc else { return };
        if double
            && let Some(p) = page_point(d, x, y)
            && let Some(e) = &mut d.edit.text
        {
            e.te.select_word_at(&d.session, p);
            relayout(d);
        }
        push_selection(d, ui);
        d.marks_dirty = true;
        self.refresh(s, ui);
    }

    /// Starts editing the selected text block.
    pub(crate) fn edit_text_start(self: &Rc<Self>, s: &mut State, ui: &MobileWindow) {
        let Some(d) = &mut s.doc else { return };
        if !may_edit(d) {
            return self.message(
                s,
                &tr!("This document's security settings do not allow editing."),
            );
        }
        let Some((page, block, size, r)) = d.edit.sel.as_ref().and_then(|sel| {
            let objs = frame_of(d)?;
            let ob = sel.single_text(objs)?;
            let size = oe::text_look(objs, ob).map_or(12.0, |l| f64::from(l.size));
            Some((sel.page, ob.id, size, view_rect(objs, ob.bbox)))
        }) else {
            return;
        };
        let te = match TextEdit::open(&d.session, &mut d.edit.pages, page, block) {
            Ok(te) => te,
            Err(e) => return self.message(s, &e),
        };
        // Small text is zoomed in on, about the block, so it can be read while typing.
        let k = d.view.last.k.max(0.01);
        let centre = (f64::from(r[0] + r[2] / 2.0), f64::from(r[1] + r[3] / 2.0));
        let mut zoomed = false;
        if size * k < READABLE
            && let Some((x, y)) = d.view.page_to_screen(page, centre.0, centre.1)
        {
            // As large as readable, the block still as wide as the screen at most.
            let (vw, _) = d.view.view.viewport.size();
            let fit = vw * 0.94 / (f64::from(r[2]) * k).max(1.0);
            let f = (READABLE / (size * k)).min(fit).min(3.0);
            if f > 1.05 {
                d.view.pinch_start();
                d.view.pinch(f, x, y);
                // The block's centre stays at `x`: then it goes to the middle, across.
                d.view.view.viewport.scroll_by(x - vw / 2.0, 0.0);
                zoomed = true;
            }
        }
        let text = te.text();
        d.edit.text = Some(Editing {
            te,
            view: TextView::default(),
            zoomed,
            image: None,
        });
        let checker = self.spell_checker(s, ui);
        let Some(d) = &mut s.doc else { return };
        if let Some(e) = &mut d.edit.text {
            e.te.set_spelling(checker);
        }
        relayout(d);
        let st = ui.global::<AppState>();
        st.set_edit_text(text.as_str().into());
        push_selection(d, ui);
        st.set_edit_panel(0);
        st.set_edit_text_editing(true);
        d.marks_dirty = true;
        self.refresh(s, ui);
    }

    /// Replaces a range of the paragraph with a correction.
    pub(super) fn edit_text_replace(
        self: &Rc<Self>,
        s: &mut State,
        ui: &MobileWindow,
        range: std::ops::Range<usize>,
        with: &str,
    ) {
        let Some(d) = &mut s.doc else { return };
        let Some(e) = &mut d.edit.text else { return };
        e.te.replace(range, with);
        ui.global::<AppState>()
            .set_edit_text(e.te.text().as_str().into());
        push_selection(d, ui);
        relayout(d);
        d.marks_dirty = true;
        self.refresh(s, ui);
    }

    /// The hidden field's text, caret and anchor (byte offsets) and preedit changed.
    pub(crate) fn edit_text_input(
        self: &Rc<Self>,
        s: &mut State,
        ui: &MobileWindow,
        text: &str,
        cursor: usize,
        anchor: usize,
        preedit: &str,
    ) {
        let Some(d) = &mut s.doc else { return };
        let Some(e) = &mut d.edit.text else { return };
        if e.te.set_field(text, cursor, anchor, preedit) {
            // The edit changed what was typed: the field takes the editor's text.
            ui.global::<AppState>()
                .set_edit_text(e.te.text().as_str().into());
            push_selection(d, ui);
        }
        relayout(d);
        keep_caret_shown(d);
        d.marks_dirty = true;
        self.refresh(s, ui);
        self.spell_page_caret(s, ui);
    }

    /// Up, Down, Home or End (0 to 3) by the paragraph's lines; with `extend` the
    /// selection grows.
    pub(crate) fn edit_text_move(
        self: &Rc<Self>,
        s: &mut State,
        ui: &MobileWindow,
        motion: i32,
        extend: bool,
    ) {
        let m = match motion {
            0 => Motion::Up,
            1 => Motion::Down,
            2 => Motion::LineStart,
            _ => Motion::LineEnd,
        };
        let Some(d) = &mut s.doc else { return };
        let Some(e) = &mut d.edit.text else { return };
        e.te.move_caret(&d.session, m, extend);
        relayout(d);
        push_selection(d, ui);
        keep_caret_shown(d);
        d.marks_dirty = true;
        self.refresh(s, ui);
    }

    /// Bold (0) or italic (1) on or off for the selection, or for what is typed next.
    pub(crate) fn edit_text_style(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, which: i32) {
        let Some(d) = &mut s.doc else { return };
        let Some(e) = &mut d.edit.text else { return };
        let st = e.te.style(&d.session);
        let change = if which == 0 {
            StyleChange::Bold(st.bold != Some(true))
        } else {
            StyleChange::Italic(st.italic != Some(true))
        };
        if let Err(msg) = e.te.apply_style(&d.session, &change) {
            return self.message(s, &msg);
        }
        relayout(d);
        d.marks_dirty = true;
        self.refresh(s, ui);
    }

    /// Ends text editing, writing the text as one undo step with `commit`.
    pub(crate) fn edit_text_done(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, commit: bool) {
        let Some(d) = &mut s.doc else { return };
        let Some(e) = d.edit.text.take() else { return };
        ui.global::<AppState>().set_edit_text_editing(false);
        if e.zoomed {
            d.view.fit_width(4.0);
        }
        let block = e.te.block();
        if commit && e.te.changed() {
            if !may_edit(d) {
                self.message(
                    s,
                    &tr!("This document's security settings do not allow editing."),
                );
            } else {
                match e.te.commit(&mut d.session, &mut d.edit.pages) {
                    Ok(Some(_)) => {
                        if let Some(b) = block {
                            reselect_text(d, b);
                        }
                    }
                    Ok(None) => {}
                    Err(msg) => self.message(s, &msg),
                }
            }
        }
        self.after_edit(s, ui);
    }

    /// Selects the text being edited (byte offsets), for scripts.
    pub(crate) fn edit_set_caret(
        self: &Rc<Self>,
        s: &mut State,
        ui: &MobileWindow,
        caret: usize,
        anchor: usize,
    ) {
        let Some(d) = &mut s.doc else { return };
        let Some(e) = &mut d.edit.text else { return };
        let text = e.te.text();
        e.te.set_field(&text, caret, anchor, "");
        relayout(d);
        push_selection(d, ui);
        d.marks_dirty = true;
        self.refresh(s, ui);
        self.spell_page_caret(s, ui);
    }
}

/// What text editing draws over page `page`: the page there rendered with the edit (by the
/// renderer `backend` makes, so it is what Done will give), a frame around it, the
/// selection with its handles, or the caret.
pub fn marks(
    d: &mut Doc,
    page: usize,
    pl: &Placement,
    out: &mut Vec<Mark>,
    backend: &dyn RenderBackend,
    theme: PageTheme,
) {
    let k = d.view.last.k.max(0.01);
    let made = d.edit.text.as_ref().and_then(|e| e.te.raster_scale());
    let scale = super::edit::preview_scale(d, made);
    let Some(e) = d.edit.text.as_mut().filter(|e| e.page() == page) else {
        return;
    };
    let raster =
        e.te.raster(&d.session, &mut d.edit.pages, backend, theme, scale);
    if let Some(r) = raster {
        let image = match &e.image {
            Some((px, img)) if Arc::ptr_eq(px, &r.pixels) => img.clone(),
            _ => {
                let img = super::edit::raster_image(&r);
                e.image = Some((r.pixels.clone(), img.clone()));
                img
            }
        };
        let rect = [r.rect.x0, r.rect.y0, r.rect.width(), r.rect.height()].map(f64::from);
        out.push(Mark {
            image,
            ..at(pl, rect, 5)
        });
    }
    let Some(e) = d.edit.text.as_ref() else {
        return;
    };
    let Some(objs) = frame_of(d) else { return };
    let v = &e.view;
    // In the accent colour, edged to show on any page.
    let accent = |r: [f64; 4], kind: i32| Mark {
        halo: true,
        ..at(pl, r, kind)
    };
    for q in &v.preedit {
        let [x, y, w, h] = quad_box(objs, q);
        out.push(accent([x, y + h, w, (1.5 / k).max(0.5)], 0));
    }
    for m in &v.misspelled {
        for q in &m.quads {
            let p = |i: usize| {
                let v = objs.frame.view_point(q[i]);
                (f64::from(v.x), f64::from(v.y))
            };
            let pts = pagebara_app_core::spell::squiggle(p(0), p(1), p(3), 1.0 / k);
            out.extend(super::marks::polyline(pl, &pts, 1.0 / k, SPELLING_RED));
        }
    }
    let [x, y, w, h] = text_rect(e, objs).map(f64::from);
    let pad = 3.0;
    out.push(accent([x - pad, y - pad, w + 2.0 * pad, h + 2.0 * pad], 3));
    for q in &v.selection {
        out.push(fill(pl, quad_box(objs, q), SELECTION));
    }
    if let (Some(a), Some(b)) = (v.selection.first(), v.selection.last()) {
        let a = quad_box(objs, a);
        let b = quad_box(objs, b);
        out.push(accent([a[0], a[1], 0.01, a[3]], 4));
        out.push(accent([b[0] + b[2], b[1], 0.01, b[3]], 4));
    } else if v.preedit.is_empty()
        && let Some(c) = v.caret
    {
        let b = objs.frame.view_point(c.bottom);
        let t = objs.frame.view_point(c.top);
        let (bx, by, ty) = (f64::from(b.x), f64::from(b.y), f64::from(t.y));
        let w = 2.0 / k;
        out.push(accent([bx - w / 2.0, ty.min(by), w, (by - ty).abs()], 0));
    }
}

/// While editing: where the field goes (for the input method's popups) and the style of
/// the selection for the Bold and Italic toggles.
pub fn show(d: &mut Doc, ui: &MobileWindow) {
    let Some(e) = &mut d.edit.text else { return };
    let st = ui.global::<AppState>();
    let style = e.te.style(&d.session);
    st.set_edit_text_bold(style.bold == Some(true));
    st.set_edit_text_italic(style.italic == Some(true));
    let Some(objs) = d.edit.objs.as_ref().map(|(_, o)| o.clone()) else {
        return;
    };
    let Some(e) = &d.edit.text else { return };
    let r = text_rect(e, &objs);
    if let Some((x, y)) = d
        .view
        .page_to_screen(e.page(), f64::from(r[0]), f64::from(r[1]))
    {
        st.set_edit_x(x as f32);
        st.set_edit_y(y as f32);
    }
}
