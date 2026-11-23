//! Reading modes: page layouts and colours, the reading view (text re-flowed), presenting,
//! the user's bookmarks, and what is remembered per document (layout, colours, last page,
//! bookmarks) without changing the file.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use pagebara_app_core::doc_state::{self, DocState, DocStates};
use pagebara_app_core::page_theme::{PageTheme, ThemedRenderer};
use pagebara_app_core::reflow::{self, BlockKind, PageFlow};
use pagebara_app_core::tiles::{Job, Key, PageRenderer, PixelRect};
use pagebara_app_core::view::LayoutMode;
use pagebara_i18n::{tr, trn};
use slint::{
    Color, ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel,
};

use super::{App, State, Tab, post_act};
use crate::{AppState, AppWindow, BookmarkRow, Mode, ReadingState, ReflowPage, ReflowRow};

/// Reading view text sizes, in points.
const REFLOW_SIZES: [f32; 9] = [12.0, 14.0, 15.0, 17.0, 19.0, 22.0, 26.0, 30.0, 36.0];
const REFLOW_DEFAULT: usize = 3;

/// The app's reading state: remembered documents and the presentation running.
#[derive(Debug)]
pub struct Reading {
    pub docs: DocStates,
    pub present: Option<Presenting>,
    reflow_size: usize,
}

impl Reading {
    pub fn load(profile: &crate::profile::Profile) -> Self {
        let path = profile
            .settings_path
            .as_ref()
            .map(|p| p.with_file_name("documents.json"));
        Reading {
            docs: DocStates::load(path),
            present: None,
            reflow_size: REFLOW_DEFAULT,
        }
    }
}

/// A presentation in progress.
#[derive(Debug)]
pub struct Presenting {
    pub uid: u64,
    pub page: usize,
    pub black: bool,
    /// The screen in logical pixels.
    size: (f64, f64),
    images: HashMap<usize, Image>,
    pending: HashSet<usize>,
    /// Bumped when the size changes; older renders are dropped.
    serial: u64,
}

/// Per tab: the document's key and state, and the reading view.
#[derive(Default)]
pub struct TabReading {
    pub key: Option<String>,
    pub state: DocState,
    pub reflow: bool,
    /// The reading view's pages, for a session generation.
    pub reflow_pages: Option<(u64, Rc<VecModel<ReflowPage>>)>,
    /// The page of each entry of `reflow_pages` (pages without text have none).
    reflow_index: Vec<usize>,
    /// The entry of `reflow_pages` being read: the last one whose start is scrolled past
    /// the top of the reading view.
    reflow_at: usize,
    /// The page being read, the current page while the reading view is shown.
    pub reflow_page: usize,
    reflow_busy: bool,
    shown_page: Option<usize>,
}

impl std::fmt::Debug for TabReading {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TabReading")
            .field("key", &self.key)
            .field("state", &self.state)
            .field("reflow", &self.reflow)
            .finish_non_exhaustive()
    }
}

impl TabReading {
    pub fn reflow_text(&self) -> String {
        self.reflow_pages
            .as_ref()
            .map(|(_, m)| {
                m.iter()
                    .flat_map(|p| p.blocks.iter().collect::<Vec<_>>())
                    .filter(|r| r.kind < 4 || (r.kind == 4 && !r.text.is_empty()))
                    .map(|r| r.text.to_string())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    }

    /// How many blocks the reading view has.
    pub fn reflow_blocks(&self) -> usize {
        self.reflow_pages
            .as_ref()
            .map_or(0, |(_, m)| m.iter().map(|p| p.blocks.row_count()).sum())
    }
}

/// `r` recoloured with the tab's page colours.
pub fn themed(t: &Tab, r: Arc<dyn PageRenderer>) -> Arc<dyn PageRenderer> {
    ThemedRenderer::wrap(r, t.reading.state.theme)
}

pub fn wire(app: &Rc<App>, ui: &AppWindow) {
    let rs = ui.global::<ReadingState>();
    macro_rules! on {
        ($cb:ident, |$app:ident, $s:ident, $ui:ident $(, $arg:ident)*| $body:expr) => {{
            let a = app.clone();
            rs.$cb(move |$($arg),*| {
                a.act(move |$app, $s, $ui| {
                    let _ = (&$app, &$ui);
                    $body
                })
            });
        }};
    }
    on!(on_set_layout, |app, s, ui, i| set_layout(
        app,
        s,
        ui,
        LayoutMode::from_index(i.max(0) as usize)
    ));
    on!(on_set_page_theme, |app, s, ui, i| set_theme(
        app,
        s,
        ui,
        PageTheme::from_index(i.max(0) as usize)
    ));
    on!(on_toggle_reflow, |app, s, ui| {
        let on = !s.tab_ref().is_some_and(|t| t.reading.reflow);
        set_reflow(app, s, ui, on);
    });
    on!(on_reflow_zoom, |app, s, ui, steps| reflow_zoom(
        app, s, ui, steps
    ));
    on!(on_reflow_open, |app, s, ui, page| {
        set_reflow(app, s, ui, false);
        app.jump(s, ui, page.max(0) as usize, None);
    });
    on!(on_reflow_reached, |app, s, ui, i, reached| reflow_reached(
        app,
        s,
        ui,
        i.max(0) as usize,
        reached
    ));
    on!(on_present, |app, s, ui| present(app, s, ui));
    on!(on_present_step, |app, s, ui, forward| present_step(
        app,
        s,
        ui,
        if forward { 1 } else { -1 }
    ));
    on!(on_present_resized, |app, s, ui, w, h| {
        if let Some(p) = &mut s.reading.present
            && p.size != (f64::from(w), f64::from(h))
        {
            p.size = (f64::from(w), f64::from(h));
            p.images.clear();
            p.pending.clear();
            p.serial += 1;
            show_present(app, s, ui);
        }
    });
    on!(on_toggle_bookmark, |app, s, ui| toggle_bookmark(app, s, ui));
    on!(on_show_bookmarks, |_app, _s, ui, v| ui
        .global::<ReadingState>()
        .set_bookmarks_visible(v));
    on!(on_bookmark_open, |app, s, ui, i| {
        let page = s
            .tab_ref()
            .and_then(|t| t.reading.state.bookmarks.get(i.max(0) as usize))
            .map(|b| b.page);
        if let Some(p) = page {
            app.jump(s, ui, p, None);
        }
    });
    on!(on_bookmark_rename, |app, s, ui, i, title| edit_bookmarks(
        app,
        s,
        ui,
        |st| {
            if let Some(b) = st.bookmarks.get_mut(i.max(0) as usize) {
                let t = title.trim();
                if !t.is_empty() {
                    b.title = t.to_owned();
                }
            }
        }
    ));
    on!(on_bookmark_delete, |app, s, ui, i| edit_bookmarks(
        app,
        s,
        ui,
        |st| {
            let i = i.max(0) as usize;
            if i < st.bookmarks.len() {
                st.bookmarks.remove(i);
            }
        }
    ));
    on!(on_bookmarks_to_outline, |app, s, ui| bookmarks_to_outline(
        app, s, ui
    ));
}

/// Keys for reading modes, presenting and the views over the pages; `None` when the app's
/// usual shortcuts should handle the key.
pub fn shortcut(
    app: &Rc<App>,
    s: &mut State,
    ui: &AppWindow,
    c: char,
    ctrl: bool,
    shift: bool,
    alt: bool,
) -> Option<bool> {
    use slint::platform::Key;
    let key = |k: Key| c == char::from(k);
    let st = ui.global::<AppState>();
    if s.reading.present.is_some() {
        match () {
            _ if key(Key::Escape) => stop_presenting(app, s, ui),
            _ if key(Key::RightArrow)
                || key(Key::DownArrow)
                || key(Key::PageDown)
                || key(Key::Return)
                || c == ' '
                || c == 'n' =>
            {
                present_step(app, s, ui, 1)
            }
            _ if key(Key::LeftArrow)
                || key(Key::UpArrow)
                || key(Key::PageUp)
                || key(Key::Backspace)
                || c == 'p' =>
            {
                present_step(app, s, ui, -1)
            }
            _ if key(Key::Home) => present_step(app, s, ui, i32::MIN),
            _ if key(Key::End) => present_step(app, s, ui, i32::MAX),
            _ if c.eq_ignore_ascii_case(&'b') || c == '.' => {
                if let Some(p) = &mut s.reading.present {
                    p.black = !p.black;
                    ui.global::<ReadingState>().set_present_black(p.black);
                }
            }
            _ => return Some(false),
        }
        return Some(true);
    }
    let lower = c.to_ascii_lowercase();
    let has_doc = !s.tabs.is_empty() && st.get_mode() == Mode::View;
    if ctrl && has_doc {
        match lower {
            'd' => toggle_bookmark(app, s, ui),
            'b' => {
                let rs = ui.global::<ReadingState>();
                rs.set_bookmarks_visible(!rs.get_bookmarks_visible());
            }
            'r' if shift => {
                let on = !s.tab_ref().is_some_and(|t| t.reading.reflow);
                set_reflow(app, s, ui, on);
            }
            _ => return None,
        }
        return Some(true);
    }
    if key(Key::F5) && has_doc {
        present(app, s, ui);
        return Some(true);
    }
    let reflow = has_doc && s.tab_ref().is_some_and(|t| t.reading.reflow);
    if reflow {
        if key(Key::Escape) {
            set_reflow(app, s, ui, false);
            return Some(true);
        }
        if ctrl && matches!(c, '+' | '=' | '-') {
            reflow_zoom(app, s, ui, if c == '-' { -1 } else { 1 });
            return Some(true);
        }
        // Left and Right turn the pages, as in the page view.
        if !ctrl && !alt && (key(Key::LeftArrow) || key(Key::RightArrow)) {
            let forward = key(Key::RightArrow);
            if let Some(page) = s.tab_ref().and_then(|t| turned(&t.reading, forward)) {
                app.jump(s, ui, page, None);
            }
            return Some(true);
        }
        // Scrolling keys go to the reading view, not the hidden pages.
        if !ctrl
            && [
                Key::UpArrow,
                Key::DownArrow,
                Key::PageUp,
                Key::PageDown,
                Key::Home,
                Key::End,
            ]
            .into_iter()
            .any(key)
            || c == ' '
        {
            return Some(false);
        }
    }
    None
}

/// After a document opened in the active tab: its remembered layout, colours, page.
pub fn opened(app: &Rc<App>, s: &mut State, ui: &AppWindow) {
    let Some(t) = s.tabs.get_mut(s.active) else {
        return;
    };
    let key = t
        .session
        .path
        .as_deref()
        .map(|p| doc_state::identity(p, t.session.doc()));
    let state = key
        .as_deref()
        .and_then(|k| s.reading.docs.get(k))
        .cloned()
        .unwrap_or_default();
    t.reading.key = key;
    let page = state.page.min(t.session.page_count().saturating_sub(1));
    t.reading.state = state;
    apply_view(t);
    if page > 0 {
        t.view.view.viewport.go_to_page(page);
    }
    super::library::opened(app, s, ui);
    show(s, ui);
}

/// The tab's layout and page colours on its (new) view.
pub fn apply_view(t: &mut Tab) {
    let mode = t.reading.state.layout;
    t.view.set_mode(mode);
    if t.reading.state.theme != PageTheme::Normal {
        let inner = t.view.view.renderer().clone();
        t.view.view.set_renderer(themed(t, inner), None);
    }
}

/// Shows the active tab's reading state in the UI.
pub fn show(s: &mut State, ui: &AppWindow) {
    let rs = ui.global::<ReadingState>();
    let Some(t) = s.tabs.get_mut(s.active) else {
        rs.set_reflow(false);
        rs.set_bookmarks(ModelRc::default());
        rs.set_layout(0);
        rs.set_page_theme(0);
        set_colours(ui, PageTheme::Normal);
        return;
    };
    rs.set_layout(t.reading.state.layout.index() as i32);
    rs.set_page_theme(t.reading.state.theme.index() as i32);
    set_colours(ui, t.reading.state.theme);
    rs.set_reflow(t.reading.reflow);
    rs.set_reflow_size(REFLOW_SIZES[s.reading.reflow_size]);
    t.reading.shown_page = None;
    match &t.reading.reflow_pages {
        Some((_, pages)) => rs.set_reflow_pages(ModelRc::from(pages.clone())),
        None => rs.set_reflow_pages(ModelRc::default()),
    }
    rs.set_reflow_busy(t.reading.reflow_busy);
    if t.reading.reflow {
        scroll_reflow(ui, t.reading.reflow_at);
    }
    sync_page(t, ui);
}

/// Scrolls the reading view to the start of its entry `at`.
fn scroll_reflow(ui: &AppWindow, at: usize) {
    let rs = ui.global::<ReadingState>();
    rs.set_reflow_target(at as i32);
    rs.set_reflow_serial(rs.get_reflow_serial().wrapping_add(1));
}

/// The reading view's entry for `page`: the first one at or after it, else the last.
fn reflow_entry(r: &TabReading, page: usize) -> usize {
    r.reflow_index
        .partition_point(|&p| p < page)
        .min(r.reflow_index.len().saturating_sub(1))
}

/// The page after (or before) the one being read that has an entry in the reading view.
fn turned(r: &TabReading, forward: bool) -> Option<usize> {
    if !r.reflow {
        return None;
    }
    let at = r.reflow_at;
    let to = if forward { at + 1 } else { at.checked_sub(1)? };
    r.reflow_index.get(to).copied()
}

/// While the reading view is shown, it follows the page number: it scrolls to `page`, the
/// page asked for (from the tool row, the outline, a link, reading aloud).
pub fn follow(t: &mut Tab, ui: &AppWindow, page: usize) {
    if !t.reading.reflow {
        return;
    }
    t.reading.reflow_page = page;
    if t.reading.reflow_index.is_empty() {
        return;
    }
    let at = reflow_entry(&t.reading, page);
    t.reading.reflow_at = at;
    t.reading.reflow_page = t.reading.reflow_index[at];
    scroll_reflow(ui, at);
}

/// While the reading view is shown, the pages under it stay where they were: before a
/// jump is remembered (for Back), they are moved to the page being read.
pub fn settle(t: &mut Tab) {
    let page = t.reading.reflow_page;
    if t.reading.reflow && t.view.view.viewport.current_page() != page {
        t.view.view.viewport.go_to_page(page);
    }
}

/// While the reading view is shown, the page being read is the current page.
pub fn current_page(t: &mut Tab) {
    if t.reading.reflow {
        t.view.current_page = t.reading.reflow_page;
    }
}

/// The start of the reading view's entry `i` scrolled past the top of the view, or back
/// below it: the page being read changes. Those that have scrolled past are always the
/// first ones, so after a jump the events may come in any order.
fn reflow_reached(app: &Rc<App>, s: &mut State, ui: &AppWindow, i: usize, reached: bool) {
    let Some(t) = s.tabs.get_mut(s.active) else {
        return;
    };
    let r = &mut t.reading;
    if !r.reflow {
        return;
    }
    let at = if reached {
        r.reflow_at.max(i)
    } else {
        r.reflow_at.min(i.saturating_sub(1))
    };
    if at == r.reflow_at {
        return;
    }
    r.reflow_at = at;
    if let Some(&page) = r.reflow_index.get(at) {
        r.reflow_page = page;
        app.refresh(s, ui);
    }
}

fn set_colours(ui: &AppWindow, theme: PageTheme) {
    let rs = ui.global::<ReadingState>();
    let paper = theme.paper();
    let mut ink = [0x18, 0x18, 0x1B, 255];
    theme.apply(&mut ink);
    rs.set_paper(Color::from_rgb_u8(paper[0], paper[1], paper[2]));
    rs.set_ink(Color::from_rgb_u8(ink[0], ink[1], ink[2]));
}

/// Bookmark state for the current page; cheap when the page did not change.
pub fn sync_page(t: &mut Tab, ui: &AppWindow) {
    let page = t.view.current_page;
    if t.reading.shown_page == Some(page) {
        return;
    }
    t.reading.shown_page = Some(page);
    let rs = ui.global::<ReadingState>();
    rs.set_page_bookmarked(t.reading.state.has_bookmark(page));
    let rows: Vec<BookmarkRow> = t
        .reading
        .state
        .bookmarks
        .iter()
        .map(|b| BookmarkRow {
            page: b.page as i32,
            title: b.title.as_str().into(),
            label: t.session.page_label(b.page).into(),
            current: b.page == page,
        })
        .collect();
    rs.set_bookmarks(ModelRc::new(VecModel::from(rows)));
}

/// Stores the tab's state (with its current page) in the remembered documents.
pub fn remember(docs: &mut DocStates, t: &mut Tab) {
    let Some(key) = &t.reading.key else { return };
    t.reading.state.page = t.view.current_page;
    docs.set(key, t.reading.state.clone());
}

/// Remembers every open document and writes the file.
pub fn save_all(s: &mut State) {
    for t in &mut s.tabs {
        remember(&mut s.reading.docs, t);
    }
    if let Err(e) = s.reading.docs.save() {
        log::warn!("could not save document states: {e}");
    }
}

/// A tab closed: remember where it was.
pub fn closed(s: &mut State, t: &mut Tab) {
    remember(&mut s.reading.docs, t);
    if let Err(e) = s.reading.docs.save() {
        log::warn!("could not save document states: {e}");
    }
    if s.reading.present.as_ref().is_some_and(|p| p.uid == t.uid) {
        s.reading.present = None;
    }
}

fn save_active(s: &mut State) {
    if let Some(t) = s.tabs.get_mut(s.active) {
        remember(&mut s.reading.docs, t);
    }
    if let Err(e) = s.reading.docs.save() {
        log::warn!("could not save document states: {e}");
    }
}

pub fn set_layout(app: &Rc<App>, s: &mut State, ui: &AppWindow, mode: LayoutMode) {
    let Some(t) = s.tab() else { return };
    t.view.set_mode(mode);
    t.reading.state.layout = mode;
    t.overlay_dirty = true;
    ui.global::<ReadingState>().set_layout(mode.index() as i32);
    save_active(s);
    app.view_op(s, ui, |_| {});
}

pub fn set_theme(app: &Rc<App>, s: &mut State, ui: &AppWindow, theme: PageTheme) {
    let Some(t) = s.tabs.get(s.active) else {
        return;
    };
    let base = super::renderer_for(s, &t.session);
    let t = &mut s.tabs[s.active];
    t.reading.state.theme = theme;
    // Previews over the page (text being edited) are rendered in its colours too.
    t.overlay_dirty = true;
    let renderer = themed(t, base);
    t.render_gen = t.session.generation();
    t.view.view.set_renderer(renderer, None);
    ui.global::<ReadingState>()
        .set_page_theme(theme.index() as i32);
    set_colours(ui, theme);
    save_active(s);
    app.refresh(s, ui);
}

fn toggle_bookmark(app: &Rc<App>, s: &mut State, ui: &AppWindow) {
    let Some(t) = s.tab() else { return };
    let page = t.view.current_page;
    let title = bookmark_title(t, page);
    let added = t.reading.state.toggle_bookmark(page, title);
    t.reading.shown_page = None;
    sync_page(t, ui);
    let label = t.session.page_label(page);
    save_active(s);
    app.message(
        s,
        &if added {
            tr!("Bookmarked page {}.", label)
        } else {
            tr!("Removed the bookmark on page {}.", label)
        },
    );
}

/// A name for a new bookmark: the outline entry the page is in, else the page.
fn bookmark_title(t: &Tab, page: usize) -> String {
    let label = t.session.page_label(page);
    match t
        .session
        .outline
        .iter()
        .rev()
        .find(|o| o.page.is_some_and(|p| p <= page))
    {
        Some(o) if !o.title.is_empty() => tr!("{} (page {})", o.title, label),
        _ => tr!("Page {}", label),
    }
}

fn edit_bookmarks(app: &Rc<App>, s: &mut State, ui: &AppWindow, f: impl FnOnce(&mut DocState)) {
    let Some(t) = s.tab() else { return };
    f(&mut t.reading.state);
    t.reading.shown_page = None;
    sync_page(t, ui);
    save_active(s);
    app.refresh(s, ui);
}

/// Writes the bookmarks into the document's outline (an undoable edit).
fn bookmarks_to_outline(app: &Rc<App>, s: &mut State, ui: &AppWindow) {
    let Some(t) = s.tab() else { return };
    let marks = t.reading.state.bookmarks.clone();
    if marks.is_empty() {
        return;
    }
    let refs = t.session.tree.refs();
    let result = t.session.edit(&tr!("Add bookmarks to outline"), |d| {
        doc_state::add_to_outline(d, &refs, &marks)
    });
    match result {
        Ok(_) => {
            let rows = super::outline_rows(&t.session);
            t.outline.set_vec(rows);
            app.after_edit(s, ui, false);
            app.message(
                s,
                &trn!(
                    "Added {n} bookmark to the outline. Save to keep it in the file.",
                    "Added {n} bookmarks to the outline. Save to keep them in the file.",
                    marks.len()
                ),
            );
        }
        Err(e) => app.show_message(s, ui, &tr!("Could not change the outline"), &e),
    }
}

pub fn set_reflow(app: &Rc<App>, s: &mut State, ui: &AppWindow, on: bool) {
    let size = REFLOW_SIZES[s.reading.reflow_size];
    let Some(t) = s.tabs.get_mut(s.active) else {
        return;
    };
    if t.reading.reflow == on {
        return;
    }
    t.reading.reflow = on;
    ui.global::<ReadingState>().set_reflow(on);
    if !on {
        // The pages come back at the page that was being read.
        t.reading.reflow = true;
        settle(t);
        t.reading.reflow = false;
        app.view_op(s, ui, |_| {});
        ui.invoke_focus_view();
        return;
    }
    t.reading.reflow_page = t.view.current_page;
    let generation = t.session.generation();
    if t.reading
        .reflow_pages
        .as_ref()
        .is_some_and(|(g, _)| *g == generation)
    {
        t.reading.reflow_at = reflow_entry(&t.reading, t.reading.reflow_page);
        show(s, ui);
        return;
    }
    let pages = Rc::new(VecModel::default());
    t.reading.reflow_pages = Some((generation, pages.clone()));
    t.reading.reflow_index.clear();
    t.reading.reflow_at = 0;
    t.reading.reflow_busy = true;
    let rs = ui.global::<ReadingState>();
    rs.set_reflow_pages(ModelRc::from(pages));
    rs.set_reflow_busy(true);
    rs.set_reflow_size(size);
    let uid = t.uid;
    let text = t.session.text.clone();
    let doc = t.session.doc().clone();
    let refs = t.session.tree.refs();
    let structure = t.session.struct_tree();
    let labels: Vec<String> = (0..t.session.page_count())
        .map(|i| t.session.page_label(i))
        .collect();
    pagebara_thread::spawn("reflow", move || {
        let flows: Vec<(usize, PageFlow)> = refs
            .iter()
            .enumerate()
            .map(|(i, &r)| {
                let flow = PageFlow::of_page(&doc, structure.as_deref(), r, &text.page(i));
                (i, flow)
            })
            .filter(|(_, f)| !f.is_empty())
            .collect();
        // Blocks by page, in order.
        let mut pages: Vec<(usize, Vec<(i32, String)>)> = Vec::with_capacity(flows.len());
        for b in reflow::build(&flows) {
            let kind = match b.kind {
                BlockKind::Paragraph => 0,
                BlockKind::Heading(l) => i32::from(l.clamp(1, 3)),
                BlockKind::Image => 4,
            };
            match pages.last_mut() {
                Some((p, blocks)) if *p == b.page => blocks.push((kind, b.text)),
                _ => pages.push((b.page, vec![(kind, b.text)])),
            }
        }
        post_act(move |_app, s, ui| {
            let Some(i) = s.tab_index(uid) else { return };
            let t = &mut s.tabs[i];
            t.reading.reflow_busy = false;
            if let Some((g, model)) = &t.reading.reflow_pages
                && *g == generation
            {
                t.reading.reflow_index = pages.iter().map(|(p, _)| *p).collect();
                model.set_vec(
                    pages
                        .into_iter()
                        .map(|(p, blocks)| {
                            let blocks: Vec<ReflowRow> = blocks
                                .into_iter()
                                .map(|(kind, text)| ReflowRow {
                                    page: p as i32 + 1,
                                    kind,
                                    text: text.into(),
                                })
                                .collect();
                            ReflowPage {
                                page: p as i32 + 1,
                                label: tr!("Page {}", labels[p]).into(),
                                blocks: ModelRc::new(VecModel::from(blocks)),
                            }
                        })
                        .collect::<Vec<_>>(),
                );
                let at = reflow_entry(&t.reading, t.reading.reflow_page);
                t.reading.reflow_at = at;
                if i == s.active && t.reading.reflow {
                    scroll_reflow(ui, at);
                }
            }
            if i == s.active {
                ui.global::<ReadingState>().set_reflow_busy(false);
            }
        });
    });
}

fn reflow_zoom(_app: &Rc<App>, s: &mut State, ui: &AppWindow, steps: i32) {
    let i = (s.reading.reflow_size as i32 + steps).clamp(0, REFLOW_SIZES.len() as i32 - 1);
    s.reading.reflow_size = i as usize;
    ui.global::<ReadingState>()
        .set_reflow_size(REFLOW_SIZES[i as usize]);
    // The page being read stays in view as the text grows or shrinks.
    if let Some(t) = s.tabs.get(s.active)
        && t.reading.reflow
    {
        scroll_reflow(ui, t.reading.reflow_at);
    }
}

fn present(app: &Rc<App>, s: &mut State, ui: &AppWindow) {
    let Some(t) = s.tab_ref() else { return };
    let (w, h) = s.view_size;
    s.reading.present = Some(Presenting {
        uid: t.uid,
        page: t.view.current_page,
        black: false,
        size: (w, h),
        images: HashMap::new(),
        pending: HashSet::new(),
        serial: 0,
    });
    let rs = ui.global::<ReadingState>();
    rs.set_present_black(false);
    rs.set_present_image(Image::default());
    rs.set_presenting(true);
    ui.window().set_fullscreen(true);
    show_present(app, s, ui);
}

fn stop_presenting(app: &Rc<App>, s: &mut State, ui: &AppWindow) {
    let Some(p) = s.reading.present.take() else {
        return;
    };
    ui.window().set_fullscreen(false);
    let rs = ui.global::<ReadingState>();
    rs.set_presenting(false);
    rs.set_present_image(Image::default());
    if let Some(i) = s.tab_index(p.uid) {
        app.select_tab(s, ui, i);
        if let Some(t) = s.tab() {
            t.view.glide_to_page(p.page);
            follow(t, ui, p.page);
        }
        app.view_op(s, ui, |_| {});
    }
    ui.invoke_focus_view();
}

fn present_step(app: &Rc<App>, s: &mut State, ui: &AppWindow, by: i32) {
    let Some(uid) = s.reading.present.as_ref().map(|p| p.uid) else {
        return;
    };
    let Some(n) = s.tab_index(uid).map(|i| s.tabs[i].session.page_count()) else {
        return;
    };
    let Some(p) = &mut s.reading.present else {
        return;
    };
    let page = (p.page as i64 + i64::from(by)).clamp(0, n as i64 - 1) as usize;
    if page == p.page {
        return;
    }
    p.page = page;
    p.black = false;
    ui.global::<ReadingState>().set_present_black(false);
    show_present(app, s, ui);
}

/// Shows the current slide (rendering it if needed) and renders the next one ahead.
fn show_present(_app: &Rc<App>, s: &mut State, ui: &AppWindow) {
    let Some(uid) = s.reading.present.as_ref().map(|p| p.uid) else {
        return;
    };
    let Some(ti) = s.tab_index(uid) else {
        return;
    };
    let t = &s.tabs[ti];
    let Some(p) = &mut s.reading.present else {
        return;
    };
    let n = t.session.page_count();
    let rs = ui.global::<ReadingState>();
    rs.set_present_label(tr!("Page {} of {}", t.session.page_label(p.page), n).into());
    if let Some(img) = p.images.get(&p.page) {
        rs.set_present_image(img.clone());
    }
    let dpr = f64::from(ui.window().scale_factor());
    let renderer = t.view.view.renderer().clone();
    let layout = t.view.view.viewport.layout();
    for page in [p.page, p.page + 1, p.page.saturating_sub(1)] {
        if page >= n || p.images.contains_key(&page) || !p.pending.insert(page) {
            continue;
        }
        let (w, h) = layout.size(page);
        let scale = (p.size.0 / w).min(p.size.1 / h) * dpr;
        let rect = PixelRect {
            x: 0,
            y: 0,
            w: ((w * scale).round() as u32).max(1),
            h: ((h * scale).round() as u32).max(1),
        };
        let job = Job {
            key: Key::preview(page),
            page,
            page_size: (w, h),
            scale,
            rect,
        };
        let renderer = renderer.clone();
        let serial = p.serial;
        pagebara_thread::spawn("present", move || {
            let mut px = vec![0u8; rect.w as usize * rect.h as usize * 4];
            renderer.render(&job, &AtomicBool::new(false), &mut px);
            post_act(move |_app, s, ui| {
                let Some(p) = &mut s.reading.present else {
                    return;
                };
                if p.serial != serial {
                    return;
                }
                p.pending.remove(&page);
                let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&px, rect.w, rect.h);
                let img = Image::from_rgba8_premultiplied(buf);
                if page == p.page {
                    ui.global::<ReadingState>().set_present_image(img.clone());
                }
                p.images.insert(page, img);
                // Keep only the slides around the current one.
                let at = p.page;
                p.images.retain(|&k, _| k + 2 >= at && k <= at + 2);
            });
        });
    }
}
