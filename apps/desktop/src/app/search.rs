//! Find in document: pages are searched on a worker thread, nearest first, and hits arrive
//! in batches while the user keeps typing (a newer search cancels the older one).

use std::rc::Rc;

use pagebara_app_core::find::{Batch, FindOptions};
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use super::{App, State, Tab};
use crate::{AppState, AppWindow, FindRow};

pub use pagebara_app_core::find::Search;

/// The search field's "3 of 12", and whether there is a hit to go to.
pub(super) fn show_status(st: &AppState, search: &Search) {
    st.set_find_match_case(search.options.match_case);
    st.set_find_whole_words(search.options.whole_words);
    st.set_find_status(search.status().into());
    st.set_find_current(
        search
            .current
            .and_then(|i| i32::try_from(i).ok())
            .unwrap_or(-1),
    );
    st.set_find_busy(!search.query.trim().is_empty() && !search.done);
    st.set_find_hits(i32::try_from(search.hits.len()).unwrap_or(i32::MAX));
}

/// The list virtualizes its rows. Only publish labels and load previews near its viewport.
pub(super) fn show_rows(st: &AppState, tab: &mut Tab) {
    let total = tab.search.hits.len();
    let first = (st.get_find_first().max(0) as usize).min(total);
    let end = (st.get_find_end().max(0) as usize).clamp(first, total);
    let range = if st.get_find_panel() && !st.get_find_collapsed() {
        first..end
    } else {
        0..0
    };
    tab.search.want_context(range.clone());
    let rows: Vec<_> = range
        .clone()
        .map(|i| {
            let page = tab.search.hits[i].page;
            let number = (page + 1).to_string();
            let label = tab.session.page_label(page);
            let (before, matched, after) = tab.search.context(i).map_or(("", "", ""), |c| {
                (
                    &c.text[..c.matched.start],
                    &c.text[c.matched.clone()],
                    &c.text[c.matched.end..],
                )
            });
            FindRow {
                page: if label.is_empty() || number == label {
                    number
                } else {
                    format!("{number} ({label})")
                }
                .into(),
                before: before.into(),
                matched: matched.into(),
                after: after.into(),
            }
        })
        .collect();
    st.set_find_rows_start(range.start as i32);
    if st.get_find_rows().iter().ne(rows.iter().cloned()) {
        st.set_find_rows(ModelRc::new(VecModel::from(rows)));
    }
}

pub fn wire(app: &Rc<App>, ui: &AppWindow) {
    let st = ui.global::<AppState>();
    let a = app.clone();
    st.on_find(move |text| {
        let text = text.to_string();
        a.act(move |app, s, ui| app.find(s, ui, &text))
    });
    let a = app.clone();
    st.on_set_find_options(move |match_case, whole_words| {
        a.act(move |app, s, ui| {
            let Some(t) = s.tab() else { return };
            let options = FindOptions {
                match_case,
                whole_words,
            };
            if t.search.options != options {
                t.search.options = options;
                t.vim.pending = None;
                app.restart_search(s, ui);
            }
        })
    });
    let a = app.clone();
    st.on_find_next(move |forward| a.act(move |app, s, ui| app.find_next(s, ui, forward)));
    let a = app.clone();
    st.on_find_range_changed(move || {
        a.act(|_, s, ui| {
            if let Some(t) = s.tab() {
                show_rows(&ui.global::<AppState>(), t);
            }
        })
    });
    let a = app.clone();
    st.on_find_select(move |i| {
        a.act(move |app, s, ui| {
            let Some(t) = s.tab() else { return };
            if usize::try_from(i)
                .ok()
                .and_then(|i| t.search.select(i))
                .is_some()
            {
                show_status(&ui.global::<AppState>(), &t.search);
                app.show_hits(s);
                app.show_hit(s, ui, true);
            }
        })
    });
    let a = app.clone();
    st.on_find_close(move || a.act(|app, s, ui| app.find_close(s, ui)));
}

impl App {
    pub(super) fn open_find(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        s.focus_region = 6;
        ui.global::<AppState>().set_find_visible(true);
        self.show_hits(s);
        self.view_op(s, ui, |_| {});
        ui.invoke_focus_find();
    }

    pub fn find(self: &Rc<Self>, s: &mut State, ui: &AppWindow, text: &str) {
        if !text.trim().is_empty() {
            // For vim's `n` after the search closed.
            s.vim.last_query = text.to_owned();
        }
        self.show_hits(s);
        let Some(t) = s.tab() else { return };
        // A vim search waiting for its hits gives way to a different search.
        if t.vim.pending.as_ref().is_some_and(|p| p.query != text) {
            t.vim.pending = None;
        }
        if t.search.query == text && !t.search.hits.is_empty() {
            self.refresh(s, ui);
            return;
        }
        t.search.query = text.to_owned();
        self.restart_search(s, ui);
    }

    /// Runs the current query again from scratch (new text, or the pages changed).
    pub fn restart_search(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let Some(t) = s.tab() else { return };
        let uid = t.uid;
        let index = t.session.text.clone();
        let live = t.session.live_ref().map(|l| l.texts()).unwrap_or_default();
        let from = t.view.current_page;
        t.search.start_with(index, live, from, move |batch| {
            super::post_act(move |app, s, ui| app.search_results(s, ui, uid, batch))
        });
        t.overlay_dirty = true;
        let st = ui.global::<AppState>();
        show_status(&st, &t.search);
        show_rows(&st, t);
        self.refresh(s, ui);
    }

    fn search_results(self: &Rc<Self>, s: &mut State, ui: &AppWindow, uid: u64, batch: Batch) {
        let active = s.tab_ref().is_some_and(|t| t.uid == uid);
        let Some(i) = s.tab_index(uid) else { return };
        let t = &mut s.tabs[i];
        let from = t.view.current_page;
        let context_only = !batch.contexts.is_empty();
        let Some(first) = t.search.add(batch, from) else {
            return;
        };
        if context_only {
            if active {
                show_rows(&ui.global::<AppState>(), t);
            }
            return;
        }
        // A vim search picks its own hit (the first below the top of the view, or the
        // last above it), once the hits found so far tell which.
        let vim = t
            .vim
            .pending
            .as_ref()
            .is_some_and(|p| p.query == t.search.query);
        if vim && active {
            self.vim_search_settle(s, ui);
        } else if first && active {
            self.show_hit(s, ui, false);
        }
        let t = &mut s.tabs[i];
        t.overlay_dirty = true;
        if active {
            let st = ui.global::<AppState>();
            show_status(&st, &t.search);
            show_rows(&st, t);
            self.refresh(s, ui);
        }
    }

    pub fn find_next(self: &Rc<Self>, s: &mut State, ui: &AppWindow, forward: bool) {
        let st = ui.global::<AppState>();
        let query = st.get_find_text();
        if query.trim().is_empty() {
            return self.open_find(s, ui);
        }
        st.set_find_visible(true);
        self.show_hits(s);
        let Some(t) = s.tab() else { return };
        if t.search.query != query.as_str() {
            return self.find(s, ui, &query);
        }
        if t.search.step(forward).is_none() {
            return;
        }
        self.show_hit(s, ui, true);
        let Some(t) = s.tab() else { return };
        show_status(&st, &t.search);
    }

    /// Scrolls to the current hit.
    pub(super) fn show_hit(self: &Rc<Self>, s: &mut State, ui: &AppWindow, remember: bool) {
        let Some(t) = s.tab() else { return };
        let Some(hit) = t.search.current_hit() else {
            return;
        };
        let page = hit.page;
        let g = t.session.geoms[page];
        let [x, y, w, h] = hit
            .boxes
            .first()
            .map(|b| g.rect_to_view(b.map(f64::from)))
            .unwrap_or_default();
        // Consecutive keys can arrive before the last scroll has reached its destination.
        t.view.finish_glide();
        let Some((left, top)) = t.view.view_point(page, x, y) else {
            return;
        };
        let vp = &t.view.view.viewport;
        let (vw, vh) = vp.size();
        let k = vp.k();
        let sideways = left < 0.0 || left + w * k > vw;
        let vertical = top < 60.0 || top + h * k > vh - 60.0 || !vp.visible_pages().contains(&page);
        if sideways || vertical {
            if remember {
                t.push_nav();
            }
            t.view.view.viewport.show_row_of(page);
            if let Some((left, top)) = t.view.view_point(page, x, y) {
                let dx = if sideways { left - 24.0 } else { 0.0 };
                let dy = if vertical { top - vh / 3.0 } else { 0.0 };
                t.view.glide_by(dx, dy);
            }
        }
        t.overlay_dirty = true;
        self.view_op(s, ui, |_| {});
    }

    /// Shows the search's hits again after vim's `:noh` hid them.
    pub fn show_hits(&self, s: &mut State) {
        if let Some(t) = s.tab()
            && t.vim.hits_hidden
        {
            t.vim.hits_hidden = false;
            t.overlay_dirty = true;
        }
    }

    pub fn find_close(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let Some(t) = s.tab() else { return };
        t.vim.hits_hidden = false;
        t.search.query.clear();
        t.search.invalidate();
        t.overlay_dirty = true;
        let st = ui.global::<AppState>();
        st.set_find_text("".into());
        show_status(&st, &t.search);
        show_rows(&st, t);
        ui.invoke_focus_view();
        self.refresh(s, ui);
    }
}
