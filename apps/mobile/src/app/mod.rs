//! The app controller: the start screen and one open document, connected to the UI's
//! callbacks. Split by concern: touch input and the tools, the marks drawn over the pages,
//! search, signatures, form filling, editing page content, and saving and sharing.

mod document;
mod edit;
mod tools;

pub(crate) use edit::edit_query;
mod forms;
mod input;
mod language;
mod live;
mod marks;
mod prepare;
mod search;
mod sign;
pub(crate) mod spell;
mod text_edit;

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use pagebara_annot::Annotation;
use pagebara_app_core::find::Search;
use pagebara_app_core::forms::{EngineForms, FormBackend, FormSession};
use pagebara_app_core::page_theme::{PageTheme, ThemedRenderer};
use pagebara_app_core::recent::RecentFiles;
use pagebara_app_core::session::{OpenError, Opened, PendingUnlock, Session};
use pagebara_app_core::settings::Settings;
use pagebara_app_core::tiles::{CompleteBackend, Key, PageRenderer, RenderBackend, RenderSource};
use pagebara_app_core::view::form_factor::FormFactor;
use pagebara_i18n::tr;
use pagebara_syntax::ObjRef;
use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};

use crate::platform::{Event, Platform};
use crate::view::{self, ViewBridge};
use crate::{AppState, MobileWindow, Options, RecentFile, Sheet, Theme, Thumb, Tool, ViewState};

const MESSAGE_FOR: Duration = Duration::from_secs(4);
/// Frames of smooth scrolling, flings and the long-press wait.
const FRAME: Duration = Duration::from_millis(16);

thread_local! {
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

/// Runs `f` with the app on the UI thread (from callbacks posted by other threads).
pub fn with_app(f: impl FnOnce(&Rc<App>)) {
    let app = APP.with(|a| a.borrow().clone());
    if let Some(app) = app {
        f(&app);
    }
}

/// Posts `f` to run with the state on the UI thread.
pub fn post_act(f: impl FnOnce(&Rc<App>, &mut State, &MobileWindow) + Send + 'static) {
    let _ = slint::invoke_from_event_loop(move || with_app(|app| app.act(f)));
}

/// An annotation on a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sel {
    pub page: usize,
    pub id: ObjRef,
}

/// Selected text on one page: character carets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextSel {
    pub page: usize,
    pub anchor: usize,
    pub caret: usize,
}

impl TextSel {
    pub fn range(&self) -> Range<usize> {
        self.anchor.min(self.caret)..self.anchor.max(self.caret)
    }
}

/// The open document and its view.
pub(crate) struct Doc {
    pub session: Session,
    pub view: ViewBridge,
    pub thumbs: Rc<VecModel<Thumb>>,
    thumbs_shown: Range<usize>,
    /// The session generation the page renderer draws.
    render_gen: u64,
    shown_page: Option<usize>,
    pub text_sel: Option<TextSel>,
    pub selected: Option<Sel>,
    pub search: Search,
    form: Option<(u64, Box<dyn FormSession>)>,
    annots: (u64, HashMap<usize, Rc<Vec<Annotation>>>),
    pub marks_dirty: bool,
    marks_pages: Vec<(usize, pagebara_app_core::view::Placement)>,
    /// Strokes just added, drawn until the page is rendered with them.
    pub pending_ink: Vec<marks::Stroke>,
    chips: marks::Chips,
    /// The first screen of tiles is not complete yet.
    first_screen: bool,
    /// Edit mode: the selected page objects, text being edited.
    pub edit: edit::EditState,
    /// Prepare form: the selected field, fields found.
    pub prep: prepare::PrepState,
}

impl Doc {
    /// The form of the current revision, loaded once per revision.
    pub fn form(&mut self, backend: &dyn FormBackend) -> &mut dyn FormSession {
        let generation = self.session.generation();
        if self.form.as_ref().is_none_or(|(g, _)| *g != generation) {
            self.form = Some((generation, backend.load(self.session.doc())));
        }
        &mut *self.form.as_mut().expect("just loaded").1
    }

    pub fn set_form(&mut self, form: Box<dyn FormSession>) {
        self.form = Some((self.session.generation(), form));
    }

    /// The annotations of page `i`, read once per revision.
    pub fn annots(&mut self, i: usize) -> Rc<Vec<Annotation>> {
        if self.annots.0 != self.session.rev() {
            self.annots = (self.session.rev(), HashMap::new());
        }
        if let Some(a) = self.annots.1.get(&i) {
            return a.clone();
        }
        let list = Rc::new(
            self.session
                .page_ref(i)
                .map(|r| pagebara_app_core::annots::on_page(self.session.doc(), r))
                .unwrap_or_default(),
        );
        self.annots.1.insert(i, list.clone());
        list
    }

    pub fn annot(&mut self, sel: Sel) -> Option<Annotation> {
        self.annots(sel.page)
            .iter()
            .find(|a| a.id == Some(sel.id))
            .cloned()
    }

    /// Puts newly rendered previews into the thumbnails shown.
    fn previews_arrived(&mut self, arrived: &[Key]) {
        for k in arrived.iter().filter(|k| k.is_preview()) {
            let p = k.page as usize;
            if self.thumbs_shown.contains(&p) {
                let image = self.view.image(k).unwrap_or_default();
                self.set_thumb(p, |t| t.image = image);
            }
        }
    }

    /// The thumbnails of `pages` are on screen: they get their previews; the ones off
    /// screen let go of theirs.
    pub fn show_thumbs(&mut self, pages: Range<usize>) {
        let n = self.session.page_count();
        let pages = pages.start.min(n)..pages.end.min(n);
        let old = std::mem::replace(&mut self.thumbs_shown, pages.clone());
        self.view.view.want_previews(pages.clone());
        for p in old.clone() {
            if !pages.contains(&p) && p < n {
                self.set_thumb(p, |t| t.image = Default::default());
            }
        }
        for p in pages {
            if !old.contains(&p)
                && let Some(image) = self.view.image(&Key::preview(p))
            {
                self.set_thumb(p, |t| t.image = image);
            }
        }
    }

    fn set_thumb(&self, p: usize, f: impl FnOnce(&mut Thumb)) {
        if let Some(mut t) = self.thumbs.row_data(p) {
            f(&mut t);
            self.thumbs.set_row_data(p, t);
        }
    }
}

#[derive(Default)]
pub(crate) struct Timers {
    /// Steps smooth scrolls and flings.
    anim: Timer,
    /// Wakes the view when a zoom gesture settles.
    settle: Timer,
    message: Timer,
    long_press: Timer,
}

pub(crate) struct State {
    pub recent: RecentFiles,
    pub doc: Option<Doc>,
    pub pending: Option<PendingUnlock>,
    pub view_size: (f64, f64),
    pub form_factor: FormFactor,
    pub backend: Arc<dyn RenderBackend>,
    pub forms: Arc<dyn FormBackend>,
    pub input: input::Input,
    pub sign: sign::SignState,
    pub fields: forms::FieldEdit,
    pub live: live::Offers,
    pub spell: spell::Spell,
    pub timers: Timers,
    pub settings: Settings,
    settings_path: Option<PathBuf>,
    /// Back was pressed once with unsaved changes.
    discard_armed: bool,
    fonts_warm: bool,
    /// The last copy the output tools wrote, and the last accessibility check (errors,
    /// fixable), for scripts.
    pub tools_output: Option<PathBuf>,
    pub tools_access: Option<(usize, usize)>,
}

pub(crate) struct App {
    pub ui: slint::Weak<MobileWindow>,
    pub state: RefCell<State>,
    pub platform: Rc<dyn Platform>,
}

impl App {
    pub fn start(ui: &MobileWindow, platform: Rc<dyn Platform>, opts: Options) -> Rc<App> {
        // Search indexes pages as it goes; indexing in the background would cost battery.
        let cache = opts.profile.clone().unwrap_or_else(|| platform.cache_dir());
        pagebara_app_core::text_index::configure(pagebara_app_core::text_index::Config {
            background: false,
            cache_dir: Some(cache.join("text-index")),
        });
        let data_dir = opts.profile.clone().or_else(|| platform.data_dir());
        if let Some(d) = &data_dir
            && let Err(e) = std::fs::create_dir_all(d)
        {
            log::warn!("cannot make {}: {e}", d.display());
        }
        let live = live::configure(data_dir.as_deref());
        let recent = match &data_dir {
            Some(d) => RecentFiles::load(d.join("recent.txt")),
            None => RecentFiles::default(),
        };
        let settings_path = data_dir.as_ref().map(|d| d.join("settings.json"));
        let settings = settings_path
            .as_deref()
            .map(Settings::load)
            .unwrap_or_default();
        let app = Rc::new(App {
            ui: ui.as_weak(),
            platform: platform.clone(),
            state: RefCell::new(State {
                sign: sign::SignState::open(data_dir.as_deref()),
                live,
                spell: spell::Spell::new(data_dir.clone()),
                recent,
                doc: None,
                pending: None,
                view_size: (390.0, 600.0),
                form_factor: FormFactor::Phone,
                backend: Arc::new(CompleteBackend),
                forms: Arc::new(EngineForms),
                input: input::Input::default(),
                fields: forms::FieldEdit::default(),
                timers: Timers::default(),
                settings,
                settings_path,
                discard_armed: false,
                fonts_warm: false,
                tools_output: None,
                tools_access: None,
            }),
        });
        APP.with(|a| *a.borrow_mut() = Some(app.clone()));
        let st = ui.global::<AppState>();
        st.set_app_name(crate::APP_NAME.into());
        st.set_can_share(platform.can_share());
        st.set_home_hint(platform.home_hint().into());
        ui.global::<Theme>()
            .set_dark(opts.dark.or_else(|| platform.dark()).unwrap_or(false));
        app.wire(ui);
        language::wire(&app, ui);
        app.act(|app, s, ui| {
            language::apply(s, ui);
            show_page_theme(s, ui);
            app.show_home(s, ui);
            sign::show_library(s, ui);
            spell::show(s, ui);
        });
        let sink_app = Rc::downgrade(&app);
        platform.connect(Rc::new(move |event| {
            if let Some(app) = sink_app.upgrade() {
                app.act(move |app, s, ui| app.platform_event(s, ui, event));
            }
        }));
        for f in opts.files {
            app.open_path(f);
        }
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        if let Some(script) = &opts.script {
            crate::script::start(&app, script);
        }
        app
    }

    /// Runs `f` with the state and the window, unless the window is gone.
    pub fn act(self: &Rc<Self>, f: impl FnOnce(&Rc<App>, &mut State, &MobileWindow) + 'static) {
        let Some(ui) = self.ui.upgrade() else { return };
        match self.state.try_borrow_mut() {
            Ok(mut s) => f(self, &mut s, &ui),
            // Called again from inside a callback: run it right after.
            Err(_) => {
                let app = self.clone();
                slint::Timer::single_shot(Duration::ZERO, move || app.act(f));
            }
        }
    }

    fn wire(self: &Rc<Self>, ui: &MobileWindow) {
        let st = ui.global::<AppState>();
        let a = self.clone();
        st.on_open(move || a.platform.pick_pdf());
        let a = self.clone();
        st.on_open_recent(move |i| {
            let a2 = a.clone();
            a.act(move |_, s, _| {
                if let Some(p) = home_files(s, &*a2.platform).get(i as usize).cloned() {
                    a2.open_path(p);
                }
            })
        });
        let a = self.clone();
        st.on_back(move || {
            a.act(|app, s, ui| {
                app.back(s, ui);
            })
        });
        let a = self.clone();
        st.on_back_key(move || {
            let Some(ui) = a.ui.upgrade() else {
                return false;
            };
            match a.state.try_borrow_mut() {
                Ok(mut s) => a.back(&mut s, &ui),
                Err(_) => true,
            }
        });
        let a = self.clone();
        st.on_go_to_page(move |i| {
            a.act(move |app, s, ui| {
                if let Some(d) = &mut s.doc {
                    d.view.glide_to_page(i.max(0) as usize);
                }
                app.animate(s, ui);
            })
        });
        let a = self.clone();
        st.on_thumbs_shown(move |first, last| {
            a.act(move |app, s, ui| {
                if let Some(d) = &mut s.doc {
                    d.show_thumbs(first.max(0) as usize..last.max(0) as usize);
                }
                app.refresh(s, ui);
            })
        });
        let a = self.clone();
        st.on_tool_changed(move || a.act(|app, s, ui| app.tool_changed(s, ui)));
        let a = self.clone();
        st.on_sheet_changed(move || {
            a.act(|app, s, ui| {
                let sheet = ui.global::<AppState>().get_sheet();
                if sheet == Sheet::Pages
                    && let Some(d) = &mut s.doc
                {
                    // Show the thumbnails around the current page.
                    let cur = d.view.current_page;
                    d.show_thumbs(cur.saturating_sub(4)..cur + 8);
                }
                app.refresh(s, ui);
            })
        });
        let vs = ui.global::<ViewState>();
        let a = self.clone();
        vs.on_resized(move |w, h| {
            a.act(move |app, s, ui| app.resized(s, ui, f64::from(w), f64::from(h)))
        });
        input::wire(self, ui);
        search::wire(self, ui);
        sign::wire(self, ui);
        forms::wire(self, ui);
        edit::wire(self, ui);
        document::wire(self, ui);
        tools::wire(self, ui);
        prepare::wire(self, ui);
        spell::wire(self, ui);
    }

    fn resized(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, w: f64, h: f64) {
        s.view_size = (w, h);
        let size = ui.window().size().to_logical(ui.window().scale_factor());
        let (ww, wh) = (f64::from(size.width), f64::from(size.height));
        let ff = FormFactor::of(ww, wh);
        s.form_factor = ff;
        let st = ui.global::<AppState>();
        st.set_tablet(ff == FormFactor::Tablet);
        st.set_side_width(ff.side_width(ww) as f32);
        st.set_thumb_columns(FormFactor::Phone.thumb_columns(ww) as i32);
        if let Some(d) = &mut s.doc {
            d.view.resized(w, h);
        }
        self.refresh(s, ui);
    }

    /// What Back does: closes what is open on top, then the document. False on the start
    /// screen (the system then leaves the app).
    pub fn back(self: &Rc<Self>, s: &mut State, ui: &MobileWindow) -> bool {
        let st = ui.global::<AppState>();
        if st.get_field_editing() || st.get_sheet() == Sheet::Choices {
            self.field_done(s, ui, false);
            return true;
        }
        match st.get_sheet() {
            Sheet::None => {}
            Sheet::DrawPad => {
                st.set_sheet(Sheet::Signatures);
                return true;
            }
            _ => {
                st.set_sheet(Sheet::None);
                return true;
            }
        }
        if st.get_searching() {
            st.set_searching(false);
            self.find_close(s, ui);
            return true;
        }
        if s.doc.as_ref().is_some_and(|d| d.edit.editing_text()) {
            self.edit_text_done(s, ui, true);
            return true;
        }
        if let Some(d) = &mut s.doc
            && (d.prep.sel.is_some() || !d.prep.found.is_empty())
        {
            d.prep.sel = None;
            d.prep.found.clear();
            d.marks_dirty = true;
            prepare::show(s, ui);
            self.refresh(s, ui);
            return true;
        }
        if let Some(d) = &mut s.doc
            && d.edit.sel.is_some()
        {
            d.edit.clear();
            d.marks_dirty = true;
            self.refresh(s, ui);
            return true;
        }
        if let Some(d) = &mut s.doc
            && (d.text_sel.is_some() || d.selected.is_some())
        {
            d.text_sel = None;
            d.selected = None;
            d.marks_dirty = true;
            self.refresh(s, ui);
            return true;
        }
        let Some(d) = &s.doc else {
            return self.platform.leave();
        };
        if d.session.is_dirty() && !s.discard_armed {
            s.discard_armed = true;
            self.message(
                s,
                &tr!(
                    "There are unsaved changes. Save them from More (⋯), or go back again to close without saving."
                ),
            );
            return true;
        }
        self.close_doc(s, ui);
        true
    }

    fn close_doc(self: &Rc<Self>, s: &mut State, ui: &MobileWindow) {
        s.doc = None;
        s.discard_armed = false;
        s.input = input::Input::default();
        ViewBridge::detach(ui);
        let st = ui.global::<AppState>();
        st.set_searching(false);
        st.set_find_text("".into());
        st.set_sheet(Sheet::None);
        st.set_tool(Tool::Read);
        st.set_has_selection(false);
        st.set_has_selected_annot(false);
        st.set_edit_selected(false);
        st.set_edit_text_editing(false);
        st.set_prep_selected(false);
        st.set_prep_found(0);
        ui.global::<ViewState>().set_marks(ModelRc::default());
        self.show_home(s, ui);
    }

    /// The files on the start screen: recent ones, then the platform's.
    pub(crate) fn show_home(&self, s: &mut State, ui: &MobileWindow) {
        let rows: Vec<RecentFile> = home_files(s, &*self.platform)
            .iter()
            .map(|p| RecentFile {
                name: p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
                    .into(),
                folder: p
                    .parent()
                    .map(|d| d.display().to_string())
                    .unwrap_or_default()
                    .into(),
            })
            .collect();
        let st = ui.global::<AppState>();
        st.set_recent(ModelRc::new(VecModel::from(rows)));
        st.set_reading(s.doc.is_some());
    }

    pub(crate) fn platform_event(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, event: Event) {
        match event {
            Event::Open(path) => {
                if s.doc.as_ref().is_some_and(|d| d.session.is_dirty()) {
                    return self.message(s, &tr!("Save or close this document first."));
                }
                self.open_path(path);
            }
            Event::Image(path) if s.doc.as_ref().is_some_and(|d| d.edit.replacing) => {
                self.edit_replace_image(s, ui, &path)
            }
            Event::Image(path) => self.import_signature(s, ui, path),
            Event::Data(path) => self.import_form_data(s, ui, &path),
            Event::Message(text) => self.message(s, &text),
        }
    }

    /// Reads `path` on a worker thread and shows it.
    pub fn open_path(self: &Rc<Self>, path: PathBuf) {
        // Busy at once (callers may hold the state): the result clears it.
        if let Some(ui) = self.ui.upgrade() {
            ui.global::<AppState>().set_busy(true);
        }
        pagebara_thread::spawn("io-open", move || {
            let result = pagebara_app_core::session::open_file(&path);
            post_act(move |app, s, ui| app.opened(s, ui, path, result));
        });
    }

    fn opened(
        self: &Rc<Self>,
        s: &mut State,
        ui: &MobileWindow,
        path: PathBuf,
        result: Result<Opened, OpenError>,
    ) {
        ui.global::<AppState>().set_busy(false);
        self.clear_message(s, ui);
        match result {
            Ok(Opened::Ready(session)) => {
                s.recent.add(&path);
                self.show(s, ui, *session);
            }
            Ok(Opened::Locked(pending)) => {
                s.pending = Some(pending);
                self.ask_password(s, ui, false);
            }
            Err(e) => {
                log::warn!("opening {}: {e}", path.display());
                s.recent.remove(&path);
                self.show_home(s, ui);
                self.message(s, &e.to_string());
            }
        }
    }

    fn show(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, session: Session) {
        let notify = || post_act(|app, s, ui| app.refresh(s, ui));
        let renderer = renderer_for(s, &session);
        // The session's sizes: asking the renderer would look up every page again.
        let sizes: Vec<(f64, f64)> = session.geoms.iter().map(|g| g.size()).collect();
        let mut view = ViewBridge::with_tuning(ui, &sizes, renderer, view::tuning(), notify);
        view.set_r2l(session.reads_right_to_left());
        view.resized(s.view_size.0, s.view_size.1);
        view.fit_width(4.0);
        s.doc = Some(Doc {
            render_gen: session.generation(),
            thumbs: Rc::new(VecModel::from(thumbs(&session))),
            thumbs_shown: 0..0,
            session,
            view,
            shown_page: None,
            text_sel: None,
            selected: None,
            search: Search::default(),
            form: None,
            annots: (u64::MAX, HashMap::new()),
            marks_dirty: true,
            marks_pages: Vec::new(),
            pending_ink: Vec::new(),
            chips: marks::Chips::default(),
            first_screen: true,
            edit: edit::EditState::default(),
            prep: prepare::PrepState::default(),
        });
        s.discard_armed = false;
        let st = ui.global::<AppState>();
        st.set_tool(Tool::Read);
        st.set_sheet(Sheet::None);
        st.set_searching(false);
        if let Some(d) = &s.doc {
            st.set_thumbs(ModelRc::from(d.thumbs.clone()));
            st.set_page_count(d.session.page_count() as i32);
            d.view.attach(ui);
        }
        self.show_home(s, ui);
        self.doc_changed(s, ui);
        self.refresh(s, ui);
    }

    /// Title, undo state and the like after the document changed.
    pub fn doc_changed(self: &Rc<Self>, s: &mut State, ui: &MobileWindow) {
        let st = ui.global::<AppState>();
        let Some(d) = &s.doc else { return };
        let ses = &d.session;
        st.set_title(ses.title().into());
        st.set_dirty(ses.is_dirty());
        st.set_can_undo(ses.can_undo());
        st.set_can_redo(ses.can_redo());
        if !ses.is_dirty() {
            s.discard_armed = false;
        }
    }

    /// After an edit, undo or redo.
    pub fn after_edit(self: &Rc<Self>, s: &mut State, ui: &MobileWindow) {
        if let Some(d) = &mut s.doc {
            d.marks_dirty = true;
            edit::sync(d);
            if let Some(sel) = d.selected
                && d.annot(sel).is_none()
            {
                d.selected = None;
            }
        }
        prepare::show(s, ui);
        self.doc_changed(s, ui);
        self.refresh(s, ui);
    }

    /// Updates the page view, thumbnails, page number and marks.
    pub fn refresh(self: &Rc<Self>, s: &mut State, ui: &MobileWindow) {
        let Some(d) = &s.doc else { return };
        if d.render_gen != d.session.generation() {
            // Same pages, new content (annotations, form fields): draw them again.
            let renderer = renderer_for(s, &d.session);
            if let Some(d) = &mut s.doc {
                d.render_gen = d.session.generation();
                d.view.view.set_renderer(renderer, None);
            }
        }
        let Some(d) = &mut s.doc else { return };
        let arrived = d.view.update_keys(ui, Instant::now());
        d.previews_arrived(&arrived);
        if !d.pending_ink.is_empty() {
            let before = d.pending_ink.len();
            d.pending_ink.retain(|st| {
                !arrived
                    .iter()
                    .any(|k| !k.is_preview() && k.page as usize == st.page)
            });
            d.marks_dirty |= d.pending_ink.len() != before;
        }
        let page = d.view.current_page;
        if d.shown_page != Some(page) {
            if let Some(old) = d.shown_page {
                d.set_thumb(old, |t| t.current = false);
            }
            d.set_thumb(page, |t| t.current = true);
            d.shown_page = Some(page);
            let st = ui.global::<AppState>();
            st.set_current_page(page as i32);
            st.set_page_label(
                tr!(
                    "Page {} of {}",
                    d.session.page_label(page),
                    d.session.page_count()
                )
                .into(),
            );
        }
        if d.first_screen && !d.view.last.tiles.is_empty() && d.view.last.missing == 0 {
            d.first_screen = false;
            // Scanning the system fonts competes with the first pages, so it starts after
            // them (only documents without embedded fonts need it).
            if !s.fonts_warm {
                s.fonts_warm = true;
                pagebara_thread::spawn("font-scan", pagebara_render::DefaultGlyphs::warm_up);
            }
        }
        let Some(d) = &mut s.doc else { return };
        if edit::previews_stale(d) {
            d.marks_dirty = true;
        }
        if d.marks_dirty || d.marks_pages != d.view.last.pages {
            marks::rebuild(s, ui);
        }
        let Some(d) = &s.doc else { return };
        marks::place_chips(d, ui);
        if let Some(at) = d.view.last.wake_at {
            let app = self.clone();
            s.timers.settle.start(
                TimerMode::SingleShot,
                at.saturating_duration_since(Instant::now()),
                move || app.act(|app, s, ui| app.refresh(s, ui)),
            );
        }
    }

    /// Refreshes and keeps stepping smooth scrolls and flings until they end.
    pub fn animate(self: &Rc<Self>, s: &mut State, ui: &MobileWindow) {
        self.refresh(s, ui);
        let animating = s.doc.as_ref().is_some_and(|d| d.view.animating());
        if animating && !s.timers.anim.running() {
            let app = self.clone();
            s.timers.anim.start(TimerMode::Repeated, FRAME, move || {
                app.act(|app, s, ui| match &mut s.doc {
                    Some(d) if d.view.animating() => {
                        d.view.step(Instant::now());
                        app.refresh(s, ui);
                    }
                    _ => s.timers.anim.stop(),
                })
            });
        }
    }

    pub fn message(self: &Rc<Self>, s: &mut State, text: &str) {
        let Some(ui) = self.ui.upgrade() else { return };
        ui.global::<AppState>().set_message(text.into());
        let app = self.clone();
        s.timers
            .message
            .start(TimerMode::SingleShot, MESSAGE_FOR, move || {
                app.act(|app, s, ui| app.clear_message(s, ui))
            });
    }

    fn clear_message(&self, s: &mut State, ui: &MobileWindow) {
        s.timers.message.stop();
        ui.global::<AppState>().set_message(SharedString::new());
    }

    pub(crate) fn tool_changed(self: &Rc<Self>, s: &mut State, ui: &MobileWindow) {
        // Text being edited is kept.
        self.edit_text_done(s, ui, true);
        let st = ui.global::<AppState>();
        let tool = st.get_tool();
        if tool == Tool::Sign {
            if !st.get_tablet() {
                st.set_sheet(Sheet::Signatures);
            }
        } else if st.get_sheet() == Sheet::Signatures {
            st.set_sheet(Sheet::None);
        }
        if let Some(d) = &mut s.doc {
            d.text_sel = None;
            d.selected = None;
            d.edit.clear();
            d.edit.replacing = false;
            d.prep.sel = None;
            d.prep.found.clear();
            d.marks_dirty = true;
        }
        st.set_edit_panel(0);
        st.set_prep_type(-1);
        prepare::show(s, ui);
        match tool {
            Tool::Highlight => self.message(s, &tr!("Drag over text to highlight it.")),
            Tool::Draw => self.message(
                s,
                &tr!("Draw with a finger or a pen. Two fingers scroll and zoom."),
            ),
            Tool::Forms => self.forms_tool(s, ui),
            Tool::Prepare => self.message(
                s,
                &tr!("Prepare form: choose a field type, then tap the page. Tap a field to change it."),
            ),
            Tool::Edit => self.message(
                s,
                &tr!("Tap text, a picture or a shape to change it. Double-tap text to edit it."),
            ),
            _ => {}
        }
        self.refresh(s, ui);
    }
}

/// A page renderer for the session's current document, form fields shown as they will be
/// in other readers, in the page colours chosen.
fn renderer_for(s: &State, session: &Session) -> Arc<dyn PageRenderer> {
    let r = s.backend.renderer(RenderSource {
        doc: pagebara_app_core::forms::for_display(session.doc()),
        refs: session.tree.refs(),
    });
    ThemedRenderer::wrap(r, s.settings.page_theme)
}

/// The paper behind pages not drawn yet, in the page colours.
fn show_page_theme(s: &State, ui: &MobileWindow) {
    let theme = s.settings.page_theme;
    let [r, g, b] = theme.paper();
    ui.global::<ViewState>()
        .set_paper(slint::Color::from_rgb_u8(r, g, b));
    ui.global::<AppState>().set_page_theme(theme.index() as i32);
}

impl App {
    /// Shows the pages in `theme`'s colours (edit previews too), remembered for next time.
    pub(crate) fn set_page_theme(
        self: &Rc<Self>,
        s: &mut State,
        ui: &MobileWindow,
        theme: PageTheme,
    ) {
        s.settings.page_theme = theme;
        if let Some(path) = &s.settings_path
            && let Err(e) = s.settings.save(path)
        {
            log::warn!("could not save the settings: {e}");
        }
        show_page_theme(s, ui);
        let renderer = s.doc.as_ref().map(|d| renderer_for(s, &d.session));
        if let (Some(renderer), Some(d)) = (renderer, &mut s.doc) {
            d.render_gen = d.session.generation();
            d.view.view.set_renderer(renderer, None);
            d.marks_dirty = true;
        }
        self.refresh(s, ui);
    }
}

fn thumbs(s: &Session) -> Vec<Thumb> {
    (0..s.page_count())
        .map(|i| {
            let (w, h) = s.geoms[i].size();
            Thumb {
                index: i as i32,
                aspect: (h / w.max(1.0)) as f32,
                label: s.page_label(i).into(),
                ..Thumb::default()
            }
        })
        .collect()
}

fn home_files(s: &State, platform: &dyn Platform) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = s.recent.paths().to_vec();
    for p in platform.documents() {
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}
