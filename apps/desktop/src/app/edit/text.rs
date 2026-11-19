//! Editing a paragraph in place. A hidden field in the page view takes the keys, the
//! clipboard and the input method's composition and reports its text, caret and
//! selection; [`TextEdit`] makes those an engine edit that reflows the paragraph in its box,
//! and the overlay draws it as the renderer will show it, with the caret and selection. Up, Down, Home and
//! End follow the laid-out lines; Ctrl+B and Ctrl+I, and the panel, style the selection.
//! Presses in the paragraph place the caret (double: a word, triple: a line), Shift
//! extends. Escape, Ctrl+Enter, Done or a click outside write it as one undoable step.

use std::rc::Rc;

use pagebara_app_core::edit::{Motion, StyleChange};
use pagebara_app_core::object_edit::{DragUnit, Selection, TextEdit, TextView};
use pagebara_edit::{Kind, ObjectId, PageObjects, Point, Rect};
use pagebara_i18n::{decimal, tr, trn};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use super::super::{App, State, Tab};
use super::{Press, TabEdit};
use crate::{AppWindow, EditState, OverlayItem, ViewState};

pub fn wire(app: &Rc<App>, ui: &AppWindow) {
    let vs = ui.global::<ViewState>();
    let a = app.clone();
    vs.on_text_edit_changed(move |text, cursor, anchor, preedit| {
        let (text, preedit) = (text.to_string(), preedit.to_string());
        a.act(move |app, s, ui| {
            app.text_changed(s, ui, &text, cursor as usize, anchor as usize, &preedit)
        })
    });
    let a = app.clone();
    vs.on_text_edit_key(move |text, ctrl, shift| {
        let Some(ui) = a.ui.upgrade() else {
            return false;
        };
        let Ok(mut s) = a.state.try_borrow_mut() else {
            return false;
        };
        a.text_key(&mut s, &ui, &text, ctrl, shift)
    });
}

/// The paragraph's preview, laid out again (after any change to it).
pub fn refresh_view(t: &mut Tab) {
    let Some(te) = t.edit.text.as_mut() else {
        t.edit.text_view = None;
        return;
    };
    t.edit.text_view = Some(te.view(&t.session, &mut t.edit.objects));
}

/// Where the paragraph is drawn: its laid-out text and the original, in page space.
pub fn area(v: &TextView) -> Option<Rect> {
    match (v.bbox, v.cover) {
        (Some(a), Some(b)) => Some(a.union(&b)),
        (a, b) => a.or(b),
    }
}

/// Whether a view point is in the paragraph being edited (padded).
pub fn inside(t: &Tab, at: (f32, f32), k: f32) -> bool {
    let (Some(v), Some(objs)) = (&t.edit.text_view, t.edit.sel_objs.as_ref()) else {
        return false;
    };
    let Some(r) = area(v) else { return false };
    let pad = 8.0 / k;
    let p = objs.frame.page_point(Point::new(at.0, at.1));
    p.x >= r.x0 - pad && p.x <= r.x1 + pad && p.y >= r.y0 - pad && p.y <= r.y1 + pad
}

/// The text block of `new` that the edit made: the one meeting `bbox` most.
pub(super) fn written_block(new: &PageObjects, bbox: &Rect) -> Vec<ObjectId> {
    let overlap =
        |r: &Rect| pagebara_edit::geom::intersect(r, bbox).map(|i| i.width() * i.height());
    new.top_level()
        .filter(|o| o.kind == Kind::Text)
        .filter_map(|o| Some((overlap(&o.bbox)?, o.id)))
        .filter(|(a, _)| *a > 0.0)
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, id)| id)
        .into_iter()
        .collect()
}

impl App {
    /// Starts editing the selected text block, the caret at a view point or at the end.
    pub fn start_text_edit(self: &Rc<Self>, s: &mut State, ui: &AppWindow, at: Option<(f32, f32)>) {
        if !self.may_edit_content(s, ui) {
            return;
        }
        let Some(t) = s.tab() else { return };
        let Some((sel, objs)) = TabEdit::current(t) else {
            return;
        };
        let Some(block) = sel.single_text(&objs) else {
            return self.message(s, &tr!("Select one block of text to edit it."));
        };
        let mut te = match TextEdit::open(&t.session, &mut t.edit.objects, sel.page, block.id) {
            Ok(te) => te,
            Err(e) => return self.message(s, &tr!("This text cannot be edited: {}", e)),
        };
        if let Some(a) = at {
            te.click(
                &t.session,
                objs.frame.page_point(Point::new(a.0, a.1)),
                false,
            );
        }
        self.begin_text_edit(s, ui, te);
    }

    /// Starts a new text box with its top left at a view point.
    pub fn new_text_box(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        page: usize,
        at: (f32, f32),
    ) {
        if !self.may_edit_content(s, ui) {
            return;
        }
        let Some(t) = s.tab() else { return };
        let Some(objs) = TabEdit::objs(t, page) else {
            return;
        };
        let size = 14.0;
        let (pw, _) = objs.frame.size();
        let w = 240.0f32.min((pw - at.0).max(60.0));
        let a = objs.frame.page_point(Point::new(at.0, at.1));
        let b = objs
            .frame
            .page_point(Point::new(at.0 + w, at.1 + size * 1.4));
        let rect = Rect::from_points(&[a, b]);
        let te =
            match TextEdit::new_box(&t.session, &mut t.edit.objects, page, rect, size, [0.0; 3]) {
                Ok(te) => te,
                Err(e) => return self.message(s, &e),
            };
        TabEdit::select(t, page, Vec::new());
        self.begin_text_edit(s, ui, te);
    }

    fn begin_text_edit(self: &Rc<Self>, s: &mut State, ui: &AppWindow, te: TextEdit) {
        let Some(t) = s.tab() else { return };
        let page = te.page;
        if t.edit.sel.page != page {
            TabEdit::select(t, page, Vec::new());
        }
        t.edit.sel_objs = TabEdit::objs(t, page);
        let text = te.text();
        let (anchor, caret) = te.selection();
        t.edit.text = Some(te);
        let checker = self.spell_checker(s, ui);
        let Some(t) = s.tab() else { return };
        if let Some(te) = t.edit.text.as_mut() {
            te.set_spelling(checker);
        }
        refresh_view(t);
        let vs = ui.global::<ViewState>();
        vs.set_text_edit_text(text.as_str().into());
        request(ui, anchor, caret);
        vs.set_text_editing(true);
        ui.global::<EditState>().set_text_editing(true);
        self.text_synced(s, ui);
        self.message(s, &tr!("Editing text. Escape or a click outside finishes."));
    }

    /// After any change: the field's position, the panel's style, the overlay.
    fn text_synced(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        place_field(s, ui);
        show_style(s, ui);
        self.overlay_changed(s, ui);
    }

    /// The field's text, caret, anchor (bytes) and composition text changed.
    fn text_changed(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        text: &str,
        cursor: usize,
        anchor: usize,
        preedit: &str,
    ) {
        let Some(t) = s.tab() else { return };
        let Some(te) = t.edit.text.as_mut() else {
            return;
        };
        if te.set_field(text, cursor, anchor, preedit) {
            let (a, c) = te.selection();
            let now = te.text();
            ui.global::<ViewState>()
                .set_text_edit_text(now.as_str().into());
            request(ui, a, c);
        }
        refresh_view(t);
        self.text_synced(s, ui);
    }

    /// Keys before the field handles them.
    fn text_key(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        text: &str,
        ctrl: bool,
        shift: bool,
    ) -> bool {
        use slint::platform::Key;
        let Some(c) = text.chars().next() else {
            return false;
        };
        let key = |k: Key| c == char::from(k);
        let pristine = s
            .tab_ref()
            .and_then(|t| t.edit.text.as_ref())
            .is_none_or(|te| !te.changed());
        if key(Key::Escape) || (ctrl && key(Key::Return)) || key(Key::Tab) || key(Key::Backtab) {
            self.edit_text_done(s, ui, true);
            return true;
        }
        if (shift && key(Key::F10)) || key(Key::Menu) {
            return self.spell_menu_at_caret(s, ui);
        }
        if ctrl && pristine && matches!(c, 'z' | 'Z' | 'y' | 'Y') {
            // Nothing typed yet: undo and redo act on the document.
            self.edit_text_done(s, ui, false);
            self.undo(s, ui, c.eq_ignore_ascii_case(&'y') || shift);
            return true;
        }
        if ctrl && (c.eq_ignore_ascii_case(&'b') || c.eq_ignore_ascii_case(&'i')) {
            let bold = c.eq_ignore_ascii_case(&'b');
            let Some(t) = s.tab() else { return true };
            let Some(te) = t.edit.text.as_mut() else {
                return true;
            };
            let st = te.style(&t.session);
            let change = if bold {
                StyleChange::Bold(st.bold != Some(true))
            } else {
                StyleChange::Italic(st.italic != Some(true))
            };
            self.text_style(s, ui, change);
            return true;
        }
        // The hidden input has different line breaks from the paragraph on the page.
        let command = cfg!(target_os = "macos") && ctrl;
        let motion = match () {
            _ if command && key(Key::LeftArrow) => Motion::LineStart,
            _ if command && key(Key::RightArrow) => Motion::LineEnd,
            _ if command && key(Key::UpArrow) => Motion::Start,
            _ if command && key(Key::DownArrow) => Motion::End,
            _ if key(Key::UpArrow) => Motion::Up,
            _ if key(Key::DownArrow) => Motion::Down,
            _ if key(Key::Home) && ctrl => Motion::Start,
            _ if key(Key::End) && ctrl => Motion::End,
            _ if key(Key::Home) => Motion::LineStart,
            _ if key(Key::End) => Motion::LineEnd,
            _ => return false,
        };
        let Some(t) = s.tab() else { return true };
        let Some(te) = t.edit.text.as_mut() else {
            return true;
        };
        te.move_caret(&t.session, motion, shift);
        let (a, c) = te.selection();
        request(ui, a, c);
        true
    }

    /// A style change of the selection (or of what is typed next).
    pub fn text_style(self: &Rc<Self>, s: &mut State, ui: &AppWindow, change: StyleChange) {
        let Some(t) = s.tab() else { return };
        let Some(te) = t.edit.text.as_mut() else {
            return;
        };
        if let Err(e) = te.apply_style(&t.session, &change) {
            return self.message(s, &e);
        }
        refresh_view(t);
        self.text_synced(s, ui);
    }

    /// A press in the paragraph being edited.
    pub(super) fn text_press(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        at: (f32, f32),
        clicks: u8,
        shift: bool,
    ) {
        let Some(t) = s.tab() else { return };
        let Some(objs) = t.edit.sel_objs.clone() else {
            return;
        };
        let Some(te) = t.edit.text.as_mut() else {
            return;
        };
        let p = objs.frame.page_point(Point::new(at.0, at.1));
        let unit = match clicks {
            2 => {
                te.select_word_at(&t.session, p);
                DragUnit::Word
            }
            3 => {
                te.select_line_at(&t.session, p);
                DragUnit::Line
            }
            _ => {
                te.click(&t.session, p, shift);
                DragUnit::Character
            }
        };
        let (a, c) = te.selection();
        t.edit.press = Some(Press::TextSelect {
            first: (a, c),
            unit,
        });
        request(ui, a, c);
    }

    pub(super) fn text_drag(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        at: (f32, f32),
        unit: DragUnit,
        first: (usize, usize),
    ) {
        let Some(t) = s.tab() else { return };
        let Some(objs) = t.edit.sel_objs.clone() else {
            return;
        };
        let Some(te) = t.edit.text.as_mut() else {
            return;
        };
        let before = te.selection();
        te.drag_to(
            &t.session,
            objs.frame.page_point(Point::new(at.0, at.1)),
            unit,
            first,
        );
        let (a, c) = te.selection();
        if (a, c) != before {
            request(ui, a, c);
        }
    }

    /// The misspelled word of the paragraph being edited at a view point.
    pub(in super::super) fn text_misspelled_at(
        s: &State,
        x: f64,
        y: f64,
    ) -> Option<std::ops::Range<usize>> {
        let (page, at, _) = Self::edit_hit(s, x, y)?;
        let t = s.tab_ref()?;
        let (te, v, objs) = (
            t.edit.text.as_ref()?,
            t.edit.text_view.as_ref()?,
            t.edit.sel_objs.as_ref()?,
        );
        if te.page != page {
            return None;
        }
        let p = objs.frame.page_point(Point::new(at.0, at.1));
        v.misspelled_at(p).map(|m| m.range.clone())
    }

    /// The misspelled word around the caret, with the view point below its start.
    pub(in super::super) fn text_misspelled_at_caret(
        s: &State,
    ) -> Option<(std::ops::Range<usize>, (f64, f64))> {
        let t = s.tab_ref()?;
        let (te, v, objs) = (
            t.edit.text.as_ref()?,
            t.edit.text_view.as_ref()?,
            t.edit.sel_objs.as_ref()?,
        );
        let m = v.misspelled_around(te.selection().1)?;
        let p = objs.frame.view_point(*m.quads.first()?.first()?);
        let at = t
            .view
            .page_to_screen(te.page, f64::from(p.x), f64::from(p.y))?;
        Some((m.range.clone(), at))
    }

    /// The middle of misspelled word `i` of the paragraph in the page view (scripts).
    pub fn text_misspelled_centre(s: &State, i: usize) -> Option<(f64, f64)> {
        let t = s.tab_ref()?;
        let (te, v, objs) = (
            t.edit.text.as_ref()?,
            t.edit.text_view.as_ref()?,
            t.edit.sel_objs.as_ref()?,
        );
        let q = v.misspelled.get(i)?.quads.first()?;
        let r = Rect::from_points(q);
        let p = objs
            .frame
            .view_point(Point::new((r.x0 + r.x1) / 2.0, (r.y0 + r.y1) / 2.0));
        t.view
            .page_to_screen(te.page, f64::from(p.x), f64::from(p.y))
    }

    /// Replaces a range of the paragraph with a correction (the spelling menu).
    pub(in super::super) fn text_replace(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        range: std::ops::Range<usize>,
        with: &str,
    ) {
        let Some(t) = s.tab() else { return };
        let Some(te) = t.edit.text.as_mut() else {
            return;
        };
        te.replace(range, with);
        let (a, c) = te.selection();
        let now = te.text();
        ui.global::<ViewState>()
            .set_text_edit_text(now.as_str().into());
        request(ui, a, c);
        refresh_view(t);
        self.text_synced(s, ui);
    }

    /// Finishes text editing; with `commit`, writes the paragraph as one step.
    pub fn edit_text_done(self: &Rc<Self>, s: &mut State, ui: &AppWindow, commit: bool) {
        let Some(t) = s.tab() else { return };
        let Some(te) = t.edit.text.take() else { return };
        t.edit.text_view = None;
        let vs = ui.global::<ViewState>();
        vs.set_text_editing(false);
        let es = ui.global::<EditState>();
        es.set_text_editing(false);
        es.set_warning("".into());
        ui.invoke_focus_view();
        if !commit || !te.changed() {
            self.edit_selection_changed(s, ui, false);
            return;
        }
        let Some(t) = s.tab() else { return };
        let page = te.page;
        let new_box = te.block().is_none();
        match te.commit(&mut t.session, &mut t.edit.objects) {
            Ok(report) => {
                let mut msg = if new_box {
                    tr!("Add text.")
                } else {
                    tr!("Edit text.")
                };
                if let Some(r) = &report {
                    if let Some(new) = TabEdit::objs(t, page) {
                        let ids = written_block(&new, &r.bbox);
                        t.edit.sel = Selection::new(page, ids);
                        t.edit.sel_objs = Some(new);
                    }
                    if r.overflow > 0.5 {
                        let pt = decimal(f64::from(r.overflow), 0);
                        let more = match r.overlaps.len() {
                            0 => tr!("The text now runs {} points lower than before.", pt),
                            1 => tr!(
                                "The text now runs {} points lower than before and covers another object.",
                                pt
                            ),
                            n => trn!(
                                "The text now runs {} points lower than before and covers {n} other object.",
                                "The text now runs {} points lower than before and covers {n} other objects.",
                                n,
                                pt
                            ),
                        };
                        msg = format!("{msg} {more}");
                    }
                    if r.overfull {
                        msg = format!("{msg} {}", tr!("A word is wider than the box."));
                    }
                }
                self.after_edit(s, ui, false);
                self.message(s, &msg);
            }
            Err(e) => {
                self.show_message(s, ui, &tr!("Could not change the text"), &e);
                self.overlay_changed(s, ui);
            }
        }
    }
}

/// Asks the field to select `anchor..focus` (byte offsets).
fn request(ui: &AppWindow, anchor: usize, focus: usize) {
    let vs = ui.global::<ViewState>();
    vs.set_text_edit_anchor(anchor as i32);
    vs.set_text_edit_focus(focus as i32);
    vs.set_text_edit_serial(vs.get_text_edit_serial().wrapping_add(1));
}

/// The selection's style in the panel, and what the preview reports.
fn show_style(s: &mut State, ui: &AppWindow) {
    let Some(t) = s.tab() else { return };
    let Some(te) = t.edit.text.as_mut() else {
        return;
    };
    let st = te.style(&t.session);
    let es = ui.global::<EditState>();
    let tri = |v: Option<bool>| v.map_or(-1, i32::from);
    es.set_style_bold(tri(st.bold));
    es.set_style_italic(tri(st.italic));
    es.set_style_size(st.size.unwrap_or(0.0));
    es.set_style_fill(super::actions::swatch(st.fill));
    es.set_style_family(st.family.unwrap_or_default().into());
    if es.get_families().row_count() == 0 {
        let fams: Vec<SharedString> = te
            .families(&t.session)
            .into_iter()
            .map(Into::into)
            .collect();
        es.set_families(ModelRc::new(VecModel::from(fams)));
    }
    let warn = match &t.edit.text_view {
        Some(v) if !v.missing.is_empty() => tr!("No font can show “{}”.", v.missing),
        Some(v) if v.overflow > 0.5 => tr!(
            "The text runs {} points below where it ended; it may cover what is under it.",
            decimal(f64::from(v.overflow), 0)
        ),
        _ => String::new(),
    };
    es.set_warning(warn.into());
}

/// Puts the hidden field at the caret, where the input method shows its candidates.
fn place_field(s: &mut State, ui: &AppWindow) {
    let Some(t) = s.tab() else { return };
    let Some(te) = &t.edit.text else { return };
    let Some(objs) = t.edit.sel_objs.clone() else {
        return;
    };
    let Some(pl) = t.view.placement(te.page) else {
        return;
    };
    let Some(c) = t.edit.text_view.as_ref().and_then(|v| v.caret) else {
        return;
    };
    let a = objs.frame.view_point(c.top);
    let b = objs.frame.view_point(c.bottom);
    let h = (a.y - b.y).abs().max(1.0);
    ui.global::<ViewState>().set_text_edit_at(OverlayItem {
        di: pl.di,
        bx: pl.bx + a.x.min(b.x),
        by: pl.by + a.y.min(b.y),
        w: 1.0,
        h,
        font_size: h * 0.8,
        ..Default::default()
    });
}
