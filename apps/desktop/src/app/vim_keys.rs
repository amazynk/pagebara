//! Vim keys over the pages (Settings > Vim keys): feeds keys to
//! [`pagebara_app_core::vim::Vim`] and carries out its actions on the active tab.
//!
//! Searches behave as in vim, with the top of the view as the cursor: `/` lands on the
//! first hit at or below it, `?` on the last one above it, wrapping around the document.
//! `n` and `N` go on from the current hit while it is on screen, and from the top of the
//! view once the view has moved away from it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use pagebara_app_core::find::Hit;
use pagebara_app_core::pages::PageGeom;
use pagebara_app_core::session::SaveMode;
use pagebara_app_core::vim::{
    self, Action, Ex, Key, Mode, Outcome, SearchFrom, Tab as VimTab, Visual, Zoom,
};
use pagebara_i18n::tr;
use slint::ComponentHandle;

use super::{AfterUnsaved, App, State, Tab, TextSel};
use crate::{AppState, AppWindow, DialogKind};

/// The app's side of vim keys, across tabs.
#[derive(Debug, Default)]
pub struct VimState {
    pub engine: vim::Vim,
    /// The last search went up (`?`): `n` then goes up and `N` down.
    pub search_back: bool,
    /// The last search (by vim or in the search field), for `n` after the search closed.
    pub last_query: String,
}

/// A vim search waiting for the hits that decide where it lands.
#[derive(Clone, Debug)]
pub struct PendingSearch {
    pub query: String,
    pub from: SearchFrom,
    /// Additional hits to move after the first match, positive for forward.
    pub steps: i64,
}

/// Per document: marks, where the last jump started, the visual selection and a search
/// on its way.
#[derive(Debug, Default)]
pub struct TabVim {
    pub marks: HashMap<char, (usize, f64)>,
    pub last_jump: Option<(usize, f64)>,
    pub visual: Option<Visual>,
    pub pending: Option<PendingSearch>,
    /// `:noh`: the search goes on (`n` continues from its hit) but its hits are not shown.
    pub hits_hidden: bool,
}

/// Shows the mode and the command line over the pages.
pub fn sync(s: &State, ui: &AppWindow) {
    let st = ui.global::<AppState>();
    st.set_vim_keys(s.settings.vim_keys);
    st.set_vim_mode(s.vim.engine.mode_label().into());
    let line = s.vim.engine.status();
    st.set_vim_before_caret(
        s.vim
            .engine
            .prompt_caret()
            .map_or("", |i| &line[..i])
            .into(),
    );
    st.set_vim_line(line.into());
}

impl App {
    pub fn set_vim_keys(self: &Rc<Self>, s: &mut State, ui: &AppWindow, on: bool) {
        s.settings.vim_keys = on;
        self.save_settings(s);
        self.vim_reset(s, ui);
    }

    /// Back to normal mode, the visual selection dropped (tab switch, setting change).
    pub fn vim_reset(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        s.vim.engine.reset();
        if let Some(t) = s.tab() {
            t.vim.pending = None;
            if t.vim.visual.take().is_some() {
                t.text_sel = None;
                self.selection_changed(s, ui);
            }
        }
        sync(s, ui);
    }

    /// A key for vim; `None` when it is not one and the app's own handling applies.
    /// `vim_ctrl` is the Control key itself.
    pub fn vim_key(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        c: char,
        vim_ctrl: bool,
    ) -> Option<bool> {
        if !ui.get_vim_active() {
            return None;
        }
        // The visual selection went away by other means (a click, Escape): normal mode.
        if matches!(s.vim.engine.mode(), Mode::Visual { .. })
            && s.tab_ref().is_none_or(|t| t.text_sel.is_none())
        {
            s.vim.engine.reset();
            if let Some(t) = s.tab() {
                t.vim.visual = None;
            }
        }
        let outcome = s.vim.engine.key(to_key(c, vim_ctrl), vim_ctrl);
        let handled = match outcome {
            Outcome::Pass => None,
            Outcome::Pending => Some(true),
            Outcome::Act(a) => {
                self.vim_act(s, ui, a);
                Some(true)
            }
        };
        sync(s, ui);
        handled
    }

    fn vim_act(self: &Rc<Self>, s: &mut State, ui: &AppWindow, action: Action) {
        use slint::platform::Key as K;
        let count = s.tab_ref().map_or(0, |t| t.session.page_count());
        // What goes by the position (marks, jumps, searches, page turns, visual mode)
        // starts where a smooth scroll in progress is going (`9Gn` typed quickly).
        let scrolls = matches!(
            action,
            Action::Nothing | Action::Scroll { .. } | Action::Screen { .. } | Action::Zoom(_)
        );
        if !scrolls && let Some(t) = s.tab() {
            t.view.finish_glide();
        }
        match action {
            Action::Nothing => {}
            Action::Scroll { dx, dy } => {
                let (k, n) = match (dx, dy) {
                    (0, d) if d > 0 => (K::DownArrow, d),
                    (0, d) => (K::UpArrow, -d),
                    (d, _) if d > 0 => (K::RightArrow, d),
                    (d, _) => (K::LeftArrow, -d),
                };
                let ch = char::from(k);
                self.view_op(s, ui, move |v| {
                    for _ in 0..n {
                        v.key(ch);
                    }
                });
            }
            Action::Screen { half: false, by } => {
                let ch = char::from(if by > 0 { K::PageDown } else { K::PageUp });
                self.view_op(s, ui, move |v| {
                    for _ in 0..by.unsigned_abs() {
                        v.key(ch);
                    }
                });
            }
            Action::Screen { half: true, by } => self.view_op(s, ui, move |v| v.scroll_half(by)),
            Action::Pages(n) => self.view_op(s, ui, move |v| v.turn_pages(i64::from(n))),
            Action::GotoPage(n) => self.vim_goto_number(s, ui, n),
            Action::FirstPage => self.vim_jump(s, ui, 0),
            Action::LastPage => self.vim_jump(s, ui, count.saturating_sub(1)),
            Action::PagePart(part) => self.view_op(s, ui, move |v| v.glide_to_page_part(part)),
            Action::Zoom(z) => {
                use pagebara_app_core::view::zoom;
                self.view_op(s, ui, move |v| match z {
                    Zoom::In => v.zoom_in(),
                    Zoom::Out => v.zoom_out(),
                    Zoom::FitWidth => v.fit_width(zoom::MAX),
                    Zoom::FitPage => v.fit_page(),
                    Zoom::Actual => v.zoom_to(1.0),
                });
            }
            Action::Search { query, forward } => {
                s.vim.search_back = !forward;
                if query.trim().is_empty() {
                    // As in vim, an empty pattern repeats the last search.
                    self.vim_search_next(s, ui, forward, 1);
                } else {
                    self.vim_search(s, ui, query, forward, 1);
                }
            }
            Action::SearchNext { forward, count } => {
                let forward = forward != s.vim.search_back;
                self.vim_search_next(s, ui, forward, count);
            }
            Action::Tab(tab) => {
                let n = s.tabs.len();
                if n == 0 {
                    return;
                }
                let i = match tab {
                    VimTab::Next(k) => (s.active + k) % n,
                    VimTab::Prev(k) => (s.active + n - k % n) % n,
                    VimTab::Go(i) if i < n => i,
                    VimTab::Go(_) => return,
                };
                self.select_tab(s, ui, i);
            }
            Action::SetMark(m) => {
                let Some(t) = s.tab() else { return };
                t.vim.marks.insert(m, t.view.position());
                self.message(s, &tr!("Mark {} set.", m));
            }
            Action::JumpMark { mark, exact } => {
                let Some(&(page, v)) = s.tab_ref().and_then(|t| t.vim.marks.get(&mark)) else {
                    return self.message(s, &tr!("Mark {} is not set.", mark));
                };
                self.vim_go(s, ui, page, exact.then_some(v));
            }
            Action::JumpBack { exact } => {
                let Some((page, v)) = s.tab_ref().and_then(|t| t.vim.last_jump) else {
                    return;
                };
                self.vim_go(s, ui, page, exact.then_some(v));
            }
            Action::JumpList { older, count } => {
                for _ in 0..count {
                    self.nav(s, ui, !older);
                }
            }
            Action::Undo(n) | Action::Redo(n) => {
                let redo = matches!(action, Action::Redo(_));
                for _ in 0..n {
                    self.undo(s, ui, redo);
                }
            }
            Action::Annotation { forward, count } => {
                for _ in 0..count {
                    self.cycle_annotation(s, ui, forward);
                }
            }
            Action::DeleteAnnotation => {
                if s.tab_ref().is_some_and(|t| t.selected.is_some()) {
                    self.selection_action(s, ui, "delete");
                } else {
                    self.message(s, &tr!("No annotation is selected; ]a selects the next."));
                }
            }
            Action::VisualStart { line } => self.visual_start(s, ui, line),
            Action::VisualKind { line } => {
                if let Some(v) = s.tab().and_then(|t| t.vim.visual.as_mut()) {
                    v.line = line;
                }
                self.visual_show(s, ui);
            }
            Action::VisualMove { motion, count } => {
                let Some(t) = s.tab() else { return };
                let Some(v) = t.vim.visual else { return };
                let text = t.session.sel_text(v.page);
                let cursor = vim::motion(&text, v.cursor, motion, count);
                if let Some(v) = t.vim.visual.as_mut() {
                    v.cursor = cursor;
                }
                self.visual_show(s, ui);
            }
            Action::VisualSwap => {
                if let Some(v) = s.tab().and_then(|t| t.vim.visual.as_mut()) {
                    std::mem::swap(&mut v.anchor, &mut v.cursor);
                }
                self.visual_show(s, ui);
            }
            Action::Yank => {
                self.copy_selection(s);
                self.visual_end(s, ui);
            }
            Action::VisualExit => self.visual_end(s, ui),
            Action::Ex(ex) => self.vim_ex(s, ui, ex),
        }
    }

    fn vim_ex(self: &Rc<Self>, s: &mut State, ui: &AppWindow, ex: Ex) {
        let count = s.tab_ref().map_or(0, |t| t.session.page_count());
        match ex {
            Ex::Page(n) => self.vim_goto_number(s, ui, n),
            Ex::LastPage => self.vim_jump(s, ui, count.saturating_sub(1)),
            Ex::Write { path: None, .. } => self.save(s, ui, None),
            Ex::Write {
                path: Some(path),
                force,
            } => self.vim_write_copy(s, ui, expand(&path), force),
            Ex::SaveAs { path, force } => {
                let path = expand(&path);
                if path.exists() && !force && !is_current_file(s, &path) {
                    return self.message(s, &exists_message(&path));
                }
                self.start_save(s, ui, path, SaveMode::Full, None);
            }
            Ex::Quit { force } => {
                let i = s.active;
                self.close_tab(s, ui, i, force);
            }
            Ex::WriteQuit => {
                let Some(uid) = s.tab_ref().map(|t| t.uid) else {
                    return;
                };
                self.save(s, ui, Some(AfterUnsaved::CloseTab(uid)));
            }
            Ex::Edit(None) => self.pick_file(),
            Ex::Edit(Some(path)) => self.open_path(expand(&path)),
            Ex::Zoom(z) => self.view_op(s, ui, move |v| v.zoom_to(z / 100.0)),
            Ex::TabNext => self.vim_act(s, ui, Action::Tab(VimTab::Next(1))),
            Ex::TabPrev => self.vim_act(s, ui, Action::Tab(VimTab::Prev(1))),
            Ex::NoHighlight => {
                let Some(t) = s.tab() else { return };
                t.vim.pending = None;
                t.vim.hits_hidden = true;
                t.overlay_dirty = true;
                ui.global::<AppState>().set_find_visible(false);
                self.view_op(s, ui, |_| {});
            }
            Ex::Help => self.show_dialog(s, ui, DialogKind::VimHelp),
            Ex::Unknown(cmd) => self.message(s, &tr!("Not a command: {}", cmd)),
        }
    }

    /// `:w FILE`: a copy of the document as it is now; the document stays the file being
    /// worked on, and its unsaved changes stay unsaved.
    fn vim_write_copy(self: &Rc<Self>, s: &mut State, ui: &AppWindow, path: PathBuf, force: bool) {
        if is_current_file(s, &path) {
            return self.save(s, ui, None);
        }
        if path.exists() && !force {
            return self.message(s, &exists_message(&path));
        }
        let Some(t) = s.tab_ref() else { return };
        let job = t.session.save_job(path.clone(), SaveMode::Full);
        self.message(s, &tr!("Saving…"));
        pagebara_thread::spawn("io-save-copy", move || {
            let result = job.run();
            super::post_act(move |app, s, _ui| {
                let shown = path.display().to_string();
                match result {
                    Ok(done) => {
                        let size = pagebara_i18n::bytes(done.bytes as u64);
                        app.message(s, &tr!("Wrote a copy to {} · {}", shown, size));
                    }
                    Err(e) => app.message(s, &tr!("Could not write {}: {}", shown, e)),
                }
            });
        });
    }

    /// `12G`, `:12`: the page labelled 12 when there is one (as the page box goes), else
    /// the 12th page; past the end, the last page.
    fn vim_goto_number(self: &Rc<Self>, s: &mut State, ui: &AppWindow, n: usize) {
        let Some(t) = s.tab_ref() else { return };
        let count = t.session.page_count().max(1);
        let page = t
            .page_named(&n.to_string())
            .unwrap_or(n.clamp(1, count) - 1);
        self.vim_jump(s, ui, page);
    }

    /// Remembers the position for `''` before a jump.
    fn vim_mark_jump(&self, s: &mut State) {
        if let Some(t) = s.tab() {
            t.vim.last_jump = Some(t.view.position());
        }
    }

    /// A jump to page `i` that `''` and `Ctrl-o` come back from.
    fn vim_jump(self: &Rc<Self>, s: &mut State, ui: &AppWindow, i: usize) {
        self.vim_mark_jump(s);
        self.jump(s, ui, i, None);
    }

    /// To a page, or a position on it (`v` displayed points from its top).
    fn vim_go(self: &Rc<Self>, s: &mut State, ui: &AppWindow, page: usize, v: Option<f64>) {
        self.vim_mark_jump(s);
        let Some(t) = s.tab() else { return };
        t.push_nav();
        match v {
            Some(v) => t.view.glide_to(page, v, 0.0),
            None => t.view.glide_to_page(page),
        }
        self.view_op(s, ui, |_| {});
    }

    /// `/query` and `?query` (and `n` once the search closed): shows the search with
    /// the query and lands on a hit from the top of the view, `count` hits on.
    fn vim_search(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        query: String,
        forward: bool,
        count: usize,
    ) {
        let view = s.view_size;
        s.vim.last_query = query.clone();
        let st = ui.global::<AppState>();
        st.set_find_text(query.as_str().into());
        st.set_find_visible(true);
        self.show_hits(s);
        let Some(t) = s.tab() else { return };
        let same = t.search.query == query && !t.search.hits.is_empty();
        // As in vim, searching again from the hit the cursor is on goes on to the next.
        if same && hit_on_screen(t, view) {
            return self.vim_step(s, ui, forward, count);
        }
        self.vim_mark_jump(s);
        let Some(t) = s.tab() else { return };
        let (page, v) = t.view.position();
        t.vim.pending = Some(PendingSearch {
            query: query.clone(),
            from: SearchFrom {
                page,
                v,
                forward,
                // A new search starts on the current page (see `restart_search`).
                start: (!same).then_some(t.view.current_page),
            },
            steps: signed_steps(forward, count.saturating_sub(1)),
        });
        if same {
            self.vim_search_settle(s, ui);
        } else {
            self.find(s, ui, &query);
            // A query of nothing searchable (only punctuation) starts no search.
            if !ui.global::<AppState>().get_find_busy()
                && let Some(t) = s.tab()
                && t.search.hits.is_empty()
            {
                t.vim.pending = None;
                self.message(s, &tr!("Not found: {}", query));
            }
        }
    }

    /// `n` and `N` (`forward` already turned round after `?`).
    fn vim_search_next(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        forward: bool,
        count: usize,
    ) {
        let view = s.view_size;
        let typed = ui.global::<AppState>().get_find_text().to_string();
        let query = if typed.trim().is_empty() {
            s.vim.last_query.clone()
        } else {
            typed
        };
        if query.trim().is_empty() {
            return self.message(s, &tr!("No previous search."));
        }
        let Some(t) = s.tab() else { return };
        // A search still on its way goes further once it lands.
        if let Some(p) = t.vim.pending.as_mut().filter(|p| p.query == query) {
            p.steps += signed_steps(forward, count);
            return;
        }
        let live = t.search.query == query && !t.search.hits.is_empty();
        if live && (hit_on_screen(t, view) || !t.search.done) {
            ui.global::<AppState>().set_find_visible(true);
            self.show_hits(s);
            return self.vim_step(s, ui, forward, count);
        }
        // The view moved away from the current hit, or the search was closed: from the
        // top of the view, as vim goes on from the cursor.
        self.vim_search(s, ui, query, forward, count);
    }

    /// Lands a pending search once the hits found so far tell where (called as they come).
    pub(super) fn vim_search_settle(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let Some(t) = s.tab() else { return };
        let Some(p) = t.vim.pending.clone() else {
            return;
        };
        if t.search.query != p.query {
            t.vim.pending = None;
            return;
        }
        let page = p.from.page;
        t.search.box_page(page);
        let Some(&g) = t.session.geoms.get(page) else {
            t.vim.pending = None;
            return;
        };
        let top = |h: &Hit| hit_top(&g, h);
        // Additional steps may wrap past hits that the worker has yet to return.
        if p.steps != 0 && !t.search.done {
            return;
        }
        match vim::search_target(&t.search.hits, t.search.done, p.from, top) {
            Some((i, wrapped)) => {
                t.vim.pending = None;
                let (target, crossed) = offset_hit(i, t.search.hits.len(), p.steps);
                t.search.select(target);
                let forward = if crossed { p.steps > 0 } else { p.from.forward };
                self.vim_show_hit(s, ui, forward, wrapped || crossed);
            }
            None if t.search.done => {
                t.vim.pending = None;
                self.message(s, &tr!("Not found: {}", p.query));
            }
            None => {}
        }
    }

    /// `count` hits on from the current one, wrapping around.
    fn vim_step(self: &Rc<Self>, s: &mut State, ui: &AppWindow, forward: bool, count: usize) {
        let Some(t) = s.tab() else { return };
        if t.search.hits.is_empty() {
            return;
        }
        let (target, wrapped) = offset_hit(
            t.search.current.unwrap_or(0),
            t.search.hits.len(),
            signed_steps(forward, count),
        );
        t.search.select(target);
        self.vim_show_hit(s, ui, forward, wrapped);
    }

    /// Scrolls to the current hit and shows its number, and whether the search wrapped.
    fn vim_show_hit(self: &Rc<Self>, s: &mut State, ui: &AppWindow, forward: bool, wrapped: bool) {
        self.show_hit(s, ui, true);
        if let Some(t) = s.tab_ref() {
            super::search::show_status(&ui.global::<AppState>(), &t.search);
        }
        if wrapped {
            let text = if forward {
                tr!("Search hit the end, continued from the start.")
            } else {
                tr!("Search hit the start, continued from the end.")
            };
            self.message(s, &text);
        }
    }

    /// Starts a visual selection at the topmost line in view.
    fn visual_start(self: &Rc<Self>, s: &mut State, ui: &AppWindow, line: bool) {
        let Some(t) = s.tab() else { return };
        let (page, top) = t.view.position();
        let text = t.session.sel_text(page);
        if text.chars.is_empty() {
            s.vim.engine.reset();
            return self.message(s, &tr!("This page has no text to select."));
        }
        let g = t.session.geoms[page];
        // Top, then left, of a line as shown.
        let place = |l: &pagebara_content::TextLine| {
            let b = l.bbox;
            let r = g.rect_to_view([b.x0, b.y0, b.x1, b.y1].map(f64::from));
            (r[1], r[0])
        };
        let start = text
            .lines
            .iter()
            .filter(|l| place(l).0 >= top - 1.0)
            .min_by(|a, b| {
                place(a)
                    .partial_cmp(&place(b))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .or(text.lines.first())
            .map_or(0, |l| l.chars.0 as usize);
        t.vim.visual = Some(Visual {
            page,
            anchor: start,
            cursor: start,
            line,
        });
        self.visual_show(s, ui);
    }

    /// Shows the visual selection, scrolling its moving end into view.
    fn visual_show(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let view_h = s.view_size.1;
        let Some(t) = s.tab() else { return };
        let Some(v) = t.vim.visual else { return };
        let text = t.session.sel_text(v.page);
        let r = v.range(&text);
        t.selected = None;
        t.text_sel = Some(TextSel {
            page: v.page,
            anchor: r.start,
            caret: r.end,
        });
        if let Some(ch) = text.chars.get(v.cursor) {
            let b = ch.bbox();
            let g = t.session.geoms[v.page];
            let [u, y, _, h] = g.rect_to_view([b.x0, b.y0, b.x1, b.y1].map(f64::from));
            let top = t.view.view_point(v.page, u, y);
            let bottom = t.view.view_point(v.page, u, y + h);
            let on_screen = top
                .zip(bottom)
                .is_some_and(|((_, a), (_, b))| a >= 0.0 && b <= view_h);
            if !on_screen {
                t.view.glide_to(v.page, y, view_h / 3.0);
            }
        }
        self.selection_changed(s, ui);
        self.view_op(s, ui, |_| {});
    }

    fn visual_end(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let Some(t) = s.tab() else { return };
        t.vim.visual = None;
        t.text_sel = None;
        self.selection_changed(s, ui);
    }
}

/// The top of a hit in displayed points; `None` before its boxes are worked out.
fn hit_top(g: &PageGeom, h: &Hit) -> Option<f64> {
    h.boxes
        .iter()
        .map(|b| g.rect_to_view(b.map(f64::from))[1])
        .reduce(f64::min)
}

/// Whether the current hit is in view (`view` its size).
fn hit_on_screen(t: &Tab, (view_w, view_h): (f64, f64)) -> bool {
    let Some(h) = t.search.current_hit() else {
        return false;
    };
    let Some(g) = t.session.geoms.get(h.page) else {
        return false;
    };
    let Some(top) = hit_top(g, h) else {
        return false;
    };
    let left = h
        .boxes
        .iter()
        .map(|b| g.rect_to_view(b.map(f64::from))[0])
        .fold(f64::INFINITY, f64::min);
    t.view
        .view_point(h.page, left, top)
        .is_some_and(|(x, y)| (0.0..view_h).contains(&y) && (0.0..view_w).contains(&x))
}

/// The destination in a nonempty list of hits and whether the move wraps.
fn offset_hit(start: usize, hits: usize, steps: i64) -> (usize, bool) {
    let end = start as i64 + steps;
    let hits = hits as i64;
    (end.rem_euclid(hits) as usize, !(0..hits).contains(&end))
}

fn signed_steps(forward: bool, count: usize) -> i64 {
    let count = count as i64;
    if forward { count } else { -count }
}

fn is_current_file(s: &State, path: &std::path::Path) -> bool {
    let abs = |p: &std::path::Path| std::path::absolute(p).ok();
    s.tab_ref()
        .and_then(|t| t.session.path.as_deref())
        .is_some_and(|p| abs(p) == abs(path))
}

fn exists_message(path: &std::path::Path) -> String {
    tr!("{} exists; add ! to overwrite it.", path.display())
}

/// The parser's view of a key from the window. With Control held some platforms give
/// the ASCII control character for a letter (Ctrl-D as U+0004); Tab, Return and
/// Backspace are such characters too, and stay keys.
fn to_key(c: char, ctrl: bool) -> Key {
    use slint::platform::Key as K;
    let special = [K::Tab, K::Return, K::Backspace, K::Escape].map(char::from);
    if ctrl && ('\u{1}'..='\u{1a}').contains(&c) && !special.contains(&c) && c != '\r' {
        return Key::Char(char::from(b'a' + (c as u8) - 1));
    }
    match c {
        _ if c == char::from(K::Escape) => Key::Escape,
        _ if c == char::from(K::Return) || c == '\r' || c == '\n' => Key::Enter,
        _ if c == char::from(K::Backspace) => Key::Backspace,
        _ if c == char::from(K::LeftArrow) => Key::Left,
        _ if c == char::from(K::RightArrow) => Key::Right,
        _ if c == char::from(K::Home) => Key::Home,
        _ if c == char::from(K::End) => Key::End,
        _ if c == char::from(K::UpArrow) => Key::Up,
        _ if c == char::from(K::DownArrow) => Key::Down,
        _ if c == char::from(K::Delete) => Key::Delete,
        // Function keys and the like are private-use or control characters.
        _ if c.is_control() || ('\u{E000}'..='\u{F8FF}').contains(&c) => Key::Other,
        _ => Key::Char(c),
    }
}

/// `~/` at the start means the home folder.
fn expand(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => std::env::home_dir().map_or_else(|| PathBuf::from(path), |h| h.join(rest)),
        None => PathBuf::from(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::platform::Key as K;

    #[test]
    fn count_from_the_first_hit_in_either_direction() {
        assert_eq!(offset_hit(2, 5, 0), (2, false));
        assert_eq!(offset_hit(2, 5, -2), (0, false));
        assert_eq!(offset_hit(0, 5, -1), (4, true));
        assert_eq!(offset_hit(4, 5, 1), (0, true));
        assert_eq!(offset_hit(3, 5, -12), (1, true));
        assert_eq!(offset_hit(0, 1, 3), (0, true));
    }

    #[test]
    fn control_characters() {
        // Ctrl-D as U+0004 is d; Tab, Return and Backspace stay themselves.
        assert_eq!(to_key('\u{4}', true), Key::Char('d'));
        assert_eq!(to_key(char::from(K::Tab), true), Key::Other);
        assert_eq!(to_key(char::from(K::Return), true), Key::Enter);
        assert_eq!(to_key(char::from(K::Backspace), true), Key::Backspace);
        assert_eq!(to_key(char::from(K::Escape), false), Key::Escape);
        assert_eq!(to_key('\u{4}', false), Key::Other);
        assert_eq!(to_key(char::from(K::DownArrow), false), Key::Down);
        assert_eq!(to_key('G', false), Key::Char('G'));
    }
}
