//! Keyboard ways to do what is otherwise done by pointing: selecting, moving and resizing
//! annotations, placing a box, note, signature or redaction, and marking the selected text.

use std::rc::Rc;

use pagebara_annot::{Kind, Rect};
use pagebara_app_core::annots;
use pagebara_i18n::tr;
use slint::ComponentHandle;
use slint::platform::Key;

use super::{App, Gesture, Sel, State};
use crate::{AppState, AppWindow, RedactState};

/// Size in points of a box placed with the keyboard.
const BOX: (f64, f64) = (160.0, 60.0);

impl App {
    /// Keys over the pages that stand in for the pointer; true when one was used.
    pub fn page_keys(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        c: char,
        ctrl: bool,
        shift: bool,
    ) -> bool {
        let key = |k: Key| c == char::from(k);
        if !ctrl && (c == ']' || c == '[') {
            self.cycle_annotation(s, ui, c == ']');
            return true;
        }
        let selected = s.tab_ref().and_then(|t| t.selected);
        let arrow = [
            (Key::LeftArrow, (-1.0, 0.0)),
            (Key::RightArrow, (1.0, 0.0)),
            (Key::UpArrow, (0.0, -1.0)),
            (Key::DownArrow, (0.0, 1.0)),
        ]
        .into_iter()
        .find(|(k, _)| key(*k))
        .map(|(_, d)| d);
        if let (Some(sel), Some((dx, dy))) = (selected, arrow) {
            let step = if shift { 10.0 } else { 1.0 };
            self.nudge(s, ui, sel, dx * step, dy * step, ctrl);
            return true;
        }
        if key(Key::Return) && !ctrl {
            return self.place_at_centre(s, ui);
        }
        false
    }

    /// Selects the next or previous annotation on the current page.
    pub(super) fn cycle_annotation(self: &Rc<Self>, s: &mut State, ui: &AppWindow, forward: bool) {
        let Some(t) = s.tab() else { return };
        let page = t.view.current_page;
        let list: Vec<_> = t
            .annots(page)
            .iter()
            .filter(|a| {
                annots::is_visible_markup(a)
                    || super::input::is_signature(a)
                    || super::redaction::is_mark(&a.kind)
            })
            .filter_map(|a| a.id.map(|id| (id, annots::kind_label(a).to_owned())))
            .collect();
        if list.is_empty() {
            return self.message(s, &tr!("There are no annotations on this page."));
        }
        let at = t
            .selected
            .filter(|sel| sel.page == page)
            .and_then(|sel| list.iter().position(|(id, _)| *id == sel.id));
        let n = list.len();
        let i = match (at, forward) {
            (None, true) => 0,
            (None, false) => n - 1,
            (Some(i), true) => (i + 1) % n,
            (Some(i), false) => (i + n - 1) % n,
        };
        t.selected = Some(Sel {
            page,
            id: list[i].0,
        });
        t.text_sel = None;
        self.selection_changed(s, ui);
        let text = tr!(
            "{} selected, {} of {}. Arrows move it, Ctrl+arrows resize it, Delete removes it.",
            list[i].1,
            i + 1,
            n
        );
        self.message(s, &text);
    }

    /// Moves the selected annotation by (dx, dy) points on screen, or resizes it.
    fn nudge(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        sel: Sel,
        dx: f64,
        dy: f64,
        resize: bool,
    ) {
        let Some(t) = s.tab() else { return };
        let Some(a) = t.annot(sel) else { return };
        let g = t.session.geoms[sel.page];
        let moved = if resize {
            let r = a.rect;
            let [x, y, w, h] = g.rect_to_view([r.x0, r.y0, r.x1, r.y1]);
            let w2 = (w + dx).max(6.0);
            let h2 = if matches!(a.kind, Kind::Stamp { .. }) {
                w2 * h / w.max(0.01)
            } else {
                (h + dy).max(6.0)
            };
            let [x0, y0, x1, y1] = g.rect_to_pdf([x, y, w2, h2]);
            annots::resized(&a, Rect::new(x0, y0, x1, y1))
        } else {
            let (x0, y0) = g.to_pdf(0.0, 0.0);
            let (x1, y1) = g.to_pdf(dx, dy);
            annots::translated(&a, x1 - x0, y1 - y0)
        };
        self.update_annotation(
            s,
            ui,
            sel,
            moved,
            &if resize { tr!("Resize") } else { tr!("Move") },
        );
    }

    /// Enter with a tool: marks the selected text, or puts a box, note, text box, signature
    /// or redaction in the middle of the visible part of the current page.
    fn place_at_centre(self: &Rc<Self>, s: &mut State, ui: &AppWindow) -> bool {
        let st = ui.global::<AppState>();
        let tool = st.get_tool();
        let has_text = s.tab_ref().is_some_and(|t| t.text_sel.is_some());
        if has_text && (tool == 1 || tool == 6) {
            self.markup_selection(s, ui, None);
            return true;
        }
        if !matches!(tool, 2..=4 | 6) {
            return false;
        }
        let (vw, vh) = s.view_size;
        let Some(t) = s.tab_ref() else { return false };
        let page = t.view.current_page;
        let Some((u, v)) = t.view.screen_to_page_of(page, vw / 2.0, vh / 2.0) else {
            return false;
        };
        let (pw, ph) = t.session.geoms[page].size();
        let (bw, bh) = (BOX.0.min(pw), BOX.1.min(ph));
        let u = u.clamp(bw / 2.0, (pw - bw / 2.0).max(bw / 2.0));
        let v = v.clamp(bh / 2.0, (ph - bh / 2.0).max(bh / 2.0));
        let from = (u - bw / 2.0, v - bh / 2.0);
        let to = (u + bw / 2.0, v + bh / 2.0);
        s.gesture = match tool {
            2 => Gesture::Shape { page, from, to },
            3 if st.get_text_mode() == 1 => Gesture::Note { page, at: from },
            3 => Gesture::TextBox { page, from, to },
            4 => Gesture::Place {
                page,
                from: (u, v),
                to: (u, v),
            },
            _ => {
                if ui.global::<RedactState>().get_mode() != 1 && !has_text {
                    self.message(
                        s,
                        &tr!("Select text with Ctrl+A or the pointer first, or switch to marking areas."),
                    );
                }
                Gesture::Area { page, from, to }
            }
        };
        if tool == 2 && st.get_shape() == 0 {
            // A pen stroke makes no sense from the keyboard: draw a box instead.
            st.set_shape(1);
        }
        self.finish_gesture(s, ui);
        self.select_last(s, ui, page);
        true
    }

    /// Selects the annotation added last on `page`, so the arrows move it.
    fn select_last(self: &Rc<Self>, s: &mut State, ui: &AppWindow, page: usize) {
        let Some(t) = s.tab() else { return };
        if t.selected.is_some() {
            return;
        }
        let id = t.annots(page).iter().rev().find_map(|a| a.id);
        if let Some(id) = id {
            t.selected = Some(Sel { page, id });
            self.selection_changed(s, ui);
        }
    }
}
