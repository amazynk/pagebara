//! Find in document from the header's search field (see `pagebara_app_core::find`).

use std::rc::Rc;

use pagebara_app_core::find::Batch;
use slint::ComponentHandle;

use super::{App, State, post_act};
use crate::{AppState, MobileWindow};

pub fn wire(app: &Rc<App>, ui: &MobileWindow) {
    let st = ui.global::<AppState>();
    let a = app.clone();
    st.on_find(move |text| {
        let text = text.to_string();
        a.act(move |app, s, ui| app.find(s, ui, &text))
    });
    let a = app.clone();
    st.on_find_next(move |forward| a.act(move |app, s, ui| app.find_next(s, ui, forward)));
    let a = app.clone();
    st.on_find_close(move || a.act(|app, s, ui| app.find_close(s, ui)));
}

impl App {
    pub fn find(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, text: &str) {
        let Some(d) = &mut s.doc else { return };
        if d.search.query == text {
            return;
        }
        d.search.query = text.to_owned();
        let index = d.session.text.clone();
        let live = d.session.live_ref().map(|l| l.texts()).unwrap_or_default();
        let from = d.view.current_page;
        d.search.start_with(index, live, from, move |batch| {
            post_act(move |app, s, ui| app.search_results(s, ui, batch))
        });
        d.marks_dirty = true;
        ui.global::<AppState>()
            .set_find_status(d.search.status().into());
        self.refresh(s, ui);
    }

    fn search_results(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, batch: Batch) {
        let Some(d) = &mut s.doc else { return };
        let from = d.view.current_page;
        let Some(first) = d.search.add(batch, from) else {
            return;
        };
        d.marks_dirty = true;
        ui.global::<AppState>()
            .set_find_status(d.search.status().into());
        if first {
            self.show_hit(s, ui);
        } else {
            self.refresh(s, ui);
        }
    }

    pub fn find_next(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, forward: bool) {
        let Some(d) = &mut s.doc else { return };
        if d.search.step(forward).is_none() {
            return;
        }
        ui.global::<AppState>()
            .set_find_status(d.search.status().into());
        self.show_hit(s, ui);
    }

    /// Scrolls the current hit into view, a third down the screen.
    fn show_hit(self: &Rc<Self>, s: &mut State, ui: &MobileWindow) {
        let Some(d) = &mut s.doc else { return };
        let Some(hit) = d.search.current_hit().cloned() else {
            return;
        };
        let g = d.session.geoms[hit.page];
        let top = hit
            .boxes
            .iter()
            .map(|b| g.rect_to_view(b.map(f64::from))[1])
            .fold(f64::INFINITY, f64::min);
        let top = if top.is_finite() { top } else { 0.0 };
        let h = d.view.view.viewport.size().1;
        let visible = d
            .view
            .page_to_screen(hit.page, 0.0, top)
            .is_some_and(|(_, y)| y > 40.0 && y < h - 80.0);
        if !visible {
            d.view.glide_to(hit.page, top, h / 3.0);
        }
        d.marks_dirty = true;
        self.animate(s, ui);
    }

    pub fn find_close(self: &Rc<Self>, s: &mut State, ui: &MobileWindow) {
        let Some(d) = &mut s.doc else { return };
        d.search.query.clear();
        d.search.invalidate();
        d.marks_dirty = true;
        let st = ui.global::<AppState>();
        st.set_find_text("".into());
        st.set_find_status("".into());
        self.refresh(s, ui);
    }
}
