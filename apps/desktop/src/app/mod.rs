//! The app controller: owns the open documents (one tab each) and connects the UI's
//! callbacks to them. Split by concern: documents and saving, pointer input and tools,
//! the overlay drawn over pages, organizing pages, signatures, search, and dialogs.

pub mod access;
pub mod compare;
mod dialogs;
mod document;
pub mod edit;
pub use edit::script::edit_handle;
mod forms;
mod input;
pub use input::Pointer;
pub mod conformance;
mod keyboard;
mod language;
mod library;
mod live;
#[cfg(target_os = "macos")]
pub mod macos_menu;
pub mod ocr;
pub mod optimize;
mod organize;
mod overlay;
mod page_text;
mod palette;
mod panel;
pub mod pick;
mod preopen;
pub mod prepare;
mod print;
pub mod read_aloud;
mod reading;
pub mod redaction;
mod search;
mod sign;
pub mod signing;
pub mod spell;
pub mod stamps;
mod system;
mod update;
pub(crate) mod view_script;
mod vim_keys;

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use pagebara_annot::Annotation;
use pagebara_app_core::annots::CommentEntry;
use pagebara_app_core::forms::{EngineForms, FormBackend, FormSession};
use pagebara_app_core::recent::RecentFiles;
use pagebara_app_core::session::{self, OpenError, Opened, PendingUnlock, Session};
use pagebara_app_core::settings::{Settings, ThemeChoice};
use pagebara_app_core::tiles::{EngineBackend, Key, PageRenderer, RenderBackend, RenderSource};
use pagebara_app_core::view::zoom;
use pagebara_i18n::{tr, trn};
use pagebara_syntax::ObjRef;
use slint::{ComponentHandle, Image, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};

use crate::args::Args;
use crate::bench::Bench;
use crate::page_view::ViewBridge;
use crate::perf::FrameStats;
use crate::profile::Profile;
use crate::{
    AppState, AppWindow, DialogKind, KeyboardOwner, Mode, OutlineRow, RecentFile, SidebarTab,
    TabInfo, Thumb, links, locale, synthetic,
};

pub use input::Gesture;
pub use preopen::PreOpen;
pub use search::Search;

const MESSAGE_FOR: Duration = Duration::from_secs(5);
/// Background work for a new document waits at most this long for its first screen.
const HOLD_BACKGROUND: Duration = Duration::from_millis(1500);

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

/// Posts `f` to the UI thread.
pub fn post(f: impl FnOnce(&Rc<App>) + Send + 'static) {
    let _ = slint::invoke_from_event_loop(move || with_app(f));
}

/// Posts `f` to run with the state on the UI thread.
pub fn post_act(f: impl FnOnce(&Rc<App>, &mut State, &AppWindow) + Send + 'static) {
    post(move |app| app.act(f));
}

/// The selected annotation.
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
    pub fn range(&self) -> std::ops::Range<usize> {
        self.anchor.min(self.caret)..self.anchor.max(self.caret)
    }
}

/// Back and forward through jumps (links, outline, search, page numbers).
#[derive(Debug, Default)]
pub struct Nav {
    back: Vec<(usize, f64)>,
    forward: Vec<(usize, f64)>,
}

/// The organize view's selection and drag.
#[derive(Debug, Default)]
pub struct Grid {
    pub selected: BTreeSet<usize>,
    pub anchor: Option<usize>,
    pub cols: usize,
    pub width: f64,
    pub visible: std::ops::Range<usize>,
    pub drop: Option<usize>,
}

/// The controls to restore when returning to a document.
#[derive(Default)]
struct TabWorkspace {
    organize: bool,
    group: i32,
    tool: i32,
    markup: i32,
    shape: i32,
    text_mode: i32,
}

impl TabWorkspace {
    fn capture(ui: &AppWindow) -> Self {
        let st = ui.global::<AppState>();
        Self {
            organize: st.get_mode() == Mode::Organize,
            group: ui.global::<crate::Chrome>().get_group(),
            tool: st.get_tool(),
            markup: st.get_markup_kind(),
            shape: st.get_shape(),
            text_mode: st.get_text_mode(),
        }
    }
}

/// One open document and its view.
pub struct Tab {
    /// Identifies the tab across reordering, for work finishing later (saves).
    pub uid: u64,
    workspace: TabWorkspace,
    pub session: Session,
    pub view: ViewBridge,
    pub thumbs: Rc<VecModel<Thumb>>,
    /// The pages whose thumbnails are on screen in the sidebar.
    thumbs_shown: std::ops::Range<usize>,
    pub outline: Rc<VecModel<OutlineRow>>,
    /// The outline row last highlighted as the current one.
    outline_current: Option<usize>,
    pub shown_page: Option<usize>,
    pub shown_zoom: f64,
    pub layout_rev: u64,
    annots: HashMap<usize, Rc<Vec<Annotation>>>,
    annots_rev: u64,
    pub selected: Option<Sel>,
    pub text_sel: Option<TextSel>,
    pub search: Search,
    pub nav: Nav,
    pub grid: Grid,
    pub images: HashMap<ObjRef, Image>,
    pub overlay_dirty: bool,
    pub overlay_pages: Vec<(usize, pagebara_app_core::view::Placement)>,
    pub comments: Vec<CommentEntry>,
    pub comments_rev: Option<u64>,
    /// The session generation the page renderer draws.
    pub render_gen: u64,
    /// The form of the current document, by session generation.
    form: Option<(u64, Box<dyn FormSession>)>,
    pub form_ui: forms::FormUi,
    pub reading: reading::TabReading,
    /// The Edit tool's selection and text editing.
    pub edit: edit::TabEdit,
    pub prep: prepare::TabPrep,
    /// Vim keys: marks and the visual selection.
    pub vim: vim_keys::TabVim,
    /// When opening started, until the first tile of the page is shown (logged).
    first_pixels: Option<Instant>,
    /// Background work (comments, signature checks, the scanned-page check, text for
    /// screen readers) waits for the first screen, so the first tiles get the cores.
    pub held: bool,
}

impl Tab {
    /// The form of the current document, loaded once per revision.
    pub fn form(&mut self, backend: &dyn FormBackend) -> &mut dyn FormSession {
        let generation = self.session.generation();
        if self.form.as_ref().is_none_or(|(g, _)| *g != generation) {
            self.form = Some((generation, backend.load(self.session.doc())));
        }
        &mut *self.form.as_mut().expect("just loaded").1
    }

    /// Keeps `form` as the form of the current document (after an edit made with it).
    pub fn set_form(&mut self, form: Box<dyn FormSession>) {
        self.form = Some((self.session.generation(), form));
    }

    /// The annotations of page `i`, read once per revision.
    pub fn annots(&mut self, i: usize) -> Rc<Vec<Annotation>> {
        if self.annots_rev != self.session.rev() {
            self.annots.clear();
            self.images.clear();
            self.annots_rev = self.session.rev();
        }
        if let Some(a) = self.annots.get(&i) {
            return a.clone();
        }
        let list = Rc::new(
            self.session
                .page_ref(i)
                .map(|r| pagebara_app_core::annots::on_page(self.session.doc(), r))
                .unwrap_or_default(),
        );
        self.annots.insert(i, list.clone());
        list
    }

    /// Reads annotations again (the document was replaced by the saved file).
    pub fn forget_annots(&mut self) {
        self.annots_rev = u64::MAX;
        self.comments_rev = None;
        self.overlay_dirty = true;
    }

    pub fn annot(&mut self, sel: Sel) -> Option<Annotation> {
        self.annots(sel.page)
            .iter()
            .find(|a| a.id == Some(sel.id))
            .cloned()
    }

    /// Remembers where the view is before a jump.
    pub fn push_nav(&mut self) {
        let here = self.view.position();
        if self.nav.back.last() != Some(&here) {
            self.nav.back.push(here);
            if self.nav.back.len() > 100 {
                self.nav.back.remove(0);
            }
        }
        self.nav.forward.clear();
    }
}

/// What to do once the unsaved-changes question is answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AfterUnsaved {
    CloseTab(u64),
    Quit,
}

pub struct State {
    pub profile: Profile,
    pub settings: Settings,
    pub recent: RecentFiles,
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub pending: Option<PendingUnlock>,
    pub view_size: (f64, f64),
    /// The pages whose thumbnails the sidebar shows; a tab shown later starts with these.
    pub thumbs_on_screen: std::ops::Range<usize>,
    pub stats: FrameStats,
    pub bench: Option<Bench>,
    pub startup: Args,
    pub settle: Timer,
    /// Steps smooth scrolls where the renderer gives no frame callbacks.
    pub glide: Timer,
    pub message: Timer,
    pub backend: Arc<dyn RenderBackend>,
    pub forms: Arc<dyn FormBackend>,
    pub gesture: Gesture,
    pub input: input::InputState,
    pub sign: sign::SignState,
    pub signing: signing::Signing,
    pub redaction: redaction::Redaction,
    pub ocr: ocr::Ocr,
    pub optimize: optimize::Optimize,
    pub conformance: conformance::Conformance,
    pub dialog: dialogs::DialogState,
    pub after_unsaved: Option<AfterUnsaved>,
    pub quitting: bool,
    pub next_uid: u64,
    /// When the last file open started.
    pub opening: Option<Instant>,
    pub reading: reading::Reading,
    pub library: library::Library,
    pub print: print::Print,
    pub pro: stamps::Pro,
    pub compare: compare::CompareUi,
    /// The desktop's accessibility preferences, once read.
    pub os_prefs: pagebara_app_core::os_prefs::OsPrefs,
    /// A theme given on the command line, not saved.
    pub theme_override: Option<ThemeChoice>,
    /// The area F6 last moved to (see `access::cycle_region`).
    pub focus_region: i32,
    pub read_aloud: read_aloud::ReadAloud,
    pub spell: spell::Spell,
    pub vim: vim_keys::VimState,
    /// The colour each tool last used (by tool number): Highlight starts yellow, Draw red
    /// and Text black.
    pub tool_colours: [i32; 9],
    /// The page text given to screen readers (the model shown).
    pub page_text: Rc<VecModel<crate::PageTextItem>>,
    /// The reading order of pages on screen is being worked out for `page_text`.
    pub page_text_busy: bool,
    /// Start pages and searches for files being opened (`a.pdf#page=3`, `--search`).
    pub open_at: Vec<pagebara_os::OpenFile>,
    /// Holds this window's place as the one later launches hand their files to.
    pub instance: Option<pagebara_os::instance::Registration>,
    pub update: update::UpdateState,
}

impl State {
    pub fn tab(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active)
    }

    pub fn tab_ref(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    pub fn tab_index(&self, uid: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.uid == uid)
    }
}

pub struct App {
    pub ui: slint::Weak<AppWindow>,
    pub state: RefCell<State>,
}

impl App {
    pub fn start(
        ui: &AppWindow,
        args: Args,
        files: PreOpen,
        open_at: Vec<pagebara_os::OpenFile>,
    ) -> Rc<App> {
        let profile = Profile::new(args.profile.clone());
        live::configure(profile.models_dir.clone());
        let settings = profile.settings();
        let spell = spell::Spell::new(profile.dir.clone());
        let app = Rc::new(App {
            ui: ui.as_weak(),
            state: RefCell::new(State {
                recent: profile.recent(),
                reading: reading::Reading::load(&profile),
                library: library::Library::new(&profile),
                print: print::Print::default(),
                pro: stamps::Pro::default(),
                compare: compare::CompareUi::default(),
                settings,
                profile,
                tabs: Vec::new(),
                active: 0,
                pending: None,
                view_size: (800.0, 600.0),
                thumbs_on_screen: 0..0,
                stats: FrameStats::default(),
                bench: None,
                startup: Args::default(),
                settle: Timer::default(),
                glide: Timer::default(),
                message: Timer::default(),
                backend: Arc::new(EngineBackend),
                forms: Arc::new(EngineForms),
                gesture: Gesture::None,
                input: input::InputState::default(),
                sign: sign::SignState::default(),
                signing: signing::Signing::default(),
                redaction: redaction::Redaction::default(),
                ocr: ocr::Ocr::default(),
                optimize: optimize::Optimize::default(),
                conformance: conformance::Conformance::default(),
                dialog: dialogs::DialogState::default(),
                after_unsaved: None,
                quitting: false,
                next_uid: 1,
                opening: None,
                os_prefs: Default::default(),
                theme_override: None,
                focus_region: 3,
                read_aloud: read_aloud::ReadAloud::new(args.script.is_some()),
                spell,
                vim: Default::default(),
                tool_colours: [0, 0, 5, 6, 0, 0, 0, 0, 0],
                page_text: Rc::default(),
                page_text_busy: false,
                open_at,
                instance: None,
                update: update::UpdateState::default(),
            }),
        });
        APP.with(|a| *a.borrow_mut() = Some(app.clone()));
        app.wire(ui);
        let st = ui.global::<AppState>();
        st.set_discord_available(!links::DISCORD_INVITE_URL.is_empty());
        st.set_keyboard_language(locale::input_language().into());
        st.set_app_name(pagebara_identity::NAME.into());
        st.set_version(env!("CARGO_PKG_VERSION").into());
        {
            let mut s = app.state.borrow_mut();
            app.apply_settings(&mut s, ui, args.dark);
            spell::show(&s, ui);
            st.set_setup_card_visible(!s.settings.system.setup_card_done && !args.developer_run());
            show_recent(&s, ui);
            sign::load_library(&app, &mut s, ui);
            update::start(&mut s, ui);
        }
        app.rendering_hooks(ui);
        app.window_hooks(ui, args.size);
        if let Some(path) = &args.screenshot {
            app.screenshot_after(path.clone(), args.after.unwrap_or(Duration::from_secs(2)));
        }
        let synthetic = args.synthetic;
        let script = args.script.clone();
        app.state.borrow_mut().startup = args;
        if let Some(n) = synthetic {
            let result =
                session::open_bytes(Some(format!("{n} pages.pdf").into()), synthetic::pdf(n));
            app.opened(None, result);
        }
        files.finish(&app);
        if script.is_none() && synthetic.is_none() {
            app.restore_session();
        }
        if let Some(script) = script {
            crate::script::start(&app, &script);
        }
        app
    }

    /// Runs `f` with the state and window; if the state is busy (a callback re-entered),
    /// runs it right after.
    pub fn act(self: &Rc<Self>, f: impl FnOnce(&Rc<App>, &mut State, &AppWindow) + 'static) {
        let Some(ui) = self.ui.upgrade() else { return };
        match self.state.try_borrow_mut() {
            Ok(mut s) => f(self, &mut s, &ui),
            Err(_) => {
                let app = self.clone();
                Timer::single_shot(Duration::ZERO, move || app.act(f));
            }
        }
    }

    fn apply_settings(self: &Rc<Self>, s: &mut State, ui: &AppWindow, dark: Option<bool>) {
        let st = ui.global::<AppState>();
        st.set_sidebar_visible(s.settings.sidebar_visible);
        st.set_sidebar_tab(match s.settings.sidebar_tab {
            1 => SidebarTab::Outline,
            2 => SidebarTab::Comments,
            _ => SidebarTab::Pages,
        });
        st.set_author(s.settings.author.as_str().into());
        vim_keys::sync(s, ui);
        st.set_flatten_signatures(s.settings.flatten_signatures);
        s.theme_override = dark.map(|d| {
            if d {
                ThemeChoice::Dark
            } else {
                ThemeChoice::Light
            }
        });
        language::apply(s, ui);
        access::apply(s, ui);
    }

    pub fn save_settings(&self, s: &mut State) {
        let Some(ui) = self.ui.upgrade() else { return };
        let st = ui.global::<AppState>();
        s.settings.sidebar_visible = st.get_sidebar_visible();
        s.settings.sidebar_tab = match st.get_sidebar_tab() {
            SidebarTab::Pages => 0,
            SidebarTab::Outline => 1,
            SidebarTab::Comments => 2,
        };
        s.settings.author = st.get_author().to_string();
        s.settings.flatten_signatures = st.get_flatten_signatures();
        for t in &s.tabs {
            remember_zoom(&mut s.settings, t);
        }
        if s.settings.system.restore_session {
            s.settings.system.session = s
                .tabs
                .iter()
                .filter_map(|t| {
                    Some(pagebara_app_core::settings::SessionDoc {
                        path: std::path::absolute(t.session.path.as_ref()?).ok()?,
                        page: t.view.current_page,
                    })
                })
                .collect();
        }
        reading::save_all(s);
        if let Some(path) = &s.profile.settings_path
            && let Err(e) = s.settings.save(path)
        {
            log::warn!("could not save settings: {e}");
        }
    }

    fn wire(self: &Rc<Self>, ui: &AppWindow) {
        ui.global::<crate::Keys>()
            .set_macos(cfg!(target_os = "macos"));
        let st = ui.global::<AppState>();
        let vs = ui.global::<crate::ViewState>();
        macro_rules! on {
            ($global:ident . $cb:ident, |$app:ident, $s:ident, $ui:ident $(, $arg:ident)*| $body:expr) => {{
                let a = self.clone();
                $global.$cb(move |$($arg),*| {
                    a.act(move |$app, $s, $ui| {
                        let _ = (&$app, &$ui);
                        $body
                    })
                });
            }};
        }
        on!(st.on_open_file, |app, _s, _ui| app.pick_file());
        on!(st.on_open_recent, |app, s, _ui, i| {
            if let Some(p) = s.recent.paths().get(i as usize).cloned() {
                app.open_path(p);
            }
        });
        on!(st.on_save, |app, s, ui| app.save(s, ui, None));
        on!(st.on_save_as, |app, s, ui, compact| app
            .save_as(s, ui, compact));
        on!(st.on_undo, |app, s, ui| app.undo(s, ui, false));
        on!(st.on_redo, |app, s, ui| app.undo(s, ui, true));
        on!(st.on_select_tab, |app, s, ui, i| app
            .select_tab(s, ui, i as usize));
        on!(st.on_step_tab, |app, s, ui, back, reorder| app
            .step_tab(s, ui, back, reorder));
        on!(st.on_close_tab, |app, s, ui, i| {
            let i = if i < 0 { s.active } else { i as usize };
            app.close_tab(s, ui, i, false);
        });
        on!(st.on_zoom_in, |app, s, ui| app
            .view_op(s, ui, |v| v.zoom_in()));
        on!(st.on_zoom_out, |app, s, ui| app
            .view_op(s, ui, |v| v.zoom_out()));
        on!(st.on_set_zoom, |app, s, ui, p| app.view_op(
            s,
            ui,
            move |v| v.zoom_to(f64::from(p) / 100.0)
        ));
        on!(st.on_fit_width, |app, s, ui| app
            .view_op(s, ui, |v| v.fit_width(zoom::MAX)));
        on!(st.on_fit_page, |app, s, ui| app
            .view_op(s, ui, |v| v.fit_page()));
        on!(st.on_go_to_page, |app, s, ui, i| app.jump(
            s,
            ui,
            i.max(0) as usize,
            None
        ));
        on!(st.on_page_entered, |app, s, ui, text| app
            .page_entered(s, ui, &text));
        on!(st.on_back, |app, s, ui| app.nav(s, ui, false));
        on!(st.on_forward, |app, s, ui| app.nav(s, ui, true));
        on!(st.on_thumbs_scrolled, |app, s, ui, first, last| {
            if let Some(t) = s.tab() {
                // One thumbnail to a row.
                let from = first.max(0) as usize;
                let to = last.max(0) as usize + 1;
                t.show_thumbs(from..to);
                s.thumbs_on_screen = from..to;
                app.refresh(s, ui);
            }
        });
        on!(st.on_unlock, |app, s, ui, pw| app.unlock(
            s,
            ui,
            pw.to_string()
        ));
        on!(st.on_cancel_unlock, |app, s, ui| {
            s.pending = None;
            ui.global::<AppState>().set_password_visible(false);
            app.show_active(s, ui);
        });
        on!(st.on_tool_changed, |app, s, ui, _tool| app
            .tool_changed(s, ui));
        on!(st.on_option_changed, |app, s, ui, what| app
            .option_changed(s, ui, &what));
        on!(st.on_open_website, |_app, _s, _ui| links::open(
            links::WEBSITE_URL
        ));
        on!(st.on_open_discord, |_app, _s, _ui| links::open(
            links::DISCORD_INVITE_URL
        ));
        on!(st.on_about, |app, s, ui| app.show_about(s, ui));
        on!(st.on_set_theme, |app, s, ui, choice| {
            s.settings.theme = access::choice_of(choice);
            s.theme_override = None;
            access::apply(s, ui);
            app.save_settings(s);
        });
        access::wire(self, ui);
        language::wire(self, ui);
        read_aloud::wire(self, ui);
        system::wire(self, ui);
        page_text::wire(self, ui);
        organize::wire(self, ui);
        search::wire(self, ui);
        panel::wire(self, ui);
        palette::wire(ui);
        forms::wire(self, ui);
        sign::wire(self, ui);
        dialogs::wire(self, ui);
        signing::wire(self, ui);
        redaction::wire(self, ui);
        ocr::wire(self, ui);
        optimize::wire(self, ui);
        conformance::wire(self, ui);
        stamps::wire(self, ui);
        compare::wire(self, ui);
        reading::wire(self, ui);
        library::wire(self, ui);
        print::wire(self, ui);
        edit::wire(self, ui);
        spell::wire(self, ui);
        let a = self.clone();
        ui.global::<crate::PrepState>()
            .on_action(move |what, value| {
                let (what, value) = (what.to_string(), value.to_string());
                a.act(move |app, s, ui| app.prep_action(s, ui, &what, &value))
            });

        let a = self.clone();
        st.on_shortcut(move |text, ctrl, shift, alt, meta| {
            a.shortcut(&text, ctrl, shift, alt, meta)
        });
        on!(st.on_cancel_vim_input, |app, s, ui| app.vim_reset(s, ui));
        on!(st.on_set_vim_keys, |app, s, ui, on| {
            app.set_vim_keys(s, ui, on)
        });

        on!(vs.on_resized, |app, s, ui, w, h| {
            s.view_size = (f64::from(w), f64::from(h));
            if let Some(t) = s.tab() {
                t.view.resized(f64::from(w), f64::from(h));
                app.refresh(s, ui);
            }
        });
        on!(vs.on_wheel, |app, s, ui, dx, dy, x, y, ctrl, shift| app
            .view_op(s, ui, move |v| v.wheel(
                f64::from(dx),
                f64::from(dy),
                f64::from(x),
                f64::from(y),
                ctrl,
                shift
            )));
        on!(vs.on_pointer, |app,
                            s,
                            ui,
                            kind,
                            x,
                            y,
                            button,
                            ctrl,
                            shift,
                            alt| app.pointer(
            s,
            ui,
            input::Pointer {
                kind,
                x: f64::from(x),
                y: f64::from(y),
                button,
                ctrl,
                shift,
                alt,
            }
        ));
        on!(vs.on_context_action, |app, s, ui, what| app.context_action(
            s,
            ui,
            what.as_str()
        ));
        on!(vs.on_editor_done, |app, s, ui, commit| app
            .editor_done(s, ui, commit));
        on!(vs.on_pinch_start, |app, s, ui| app
            .view_op(s, ui, |v| v.pinch_start()));
        on!(vs.on_pinch, |app, s, ui, scale, x, y| app.view_op(
            s,
            ui,
            move |v| v.pinch(f64::from(scale), f64::from(x), f64::from(y))
        ));
        on!(vs.on_pinch_end, |app, s, ui| app.refresh(s, ui));
        on!(vs.on_scrollbar, |app, s, ui, vertical, start| app.view_op(
            s,
            ui,
            move |v| v.scrollbar(vertical, f64::from(start))
        ));
    }

    fn shortcut(
        self: &Rc<Self>,
        text: &str,
        ctrl: bool,
        shift: bool,
        alt: bool,
        meta: bool,
    ) -> bool {
        let Some(c) = text.chars().next() else {
            return false;
        };
        let Some(ui) = self.ui.upgrade() else {
            return false;
        };
        let Ok(mut s) = self.state.try_borrow_mut() else {
            return false;
        };
        let s = &mut *s;
        use slint::platform::Key;
        let key = |k: Key| c == char::from(k);
        let st = ui.global::<AppState>();
        match ui.get_keyboard_owner() {
            KeyboardOwner::Document => {}
            KeyboardOwner::Print => return print::shortcut(self, s, &ui, c, ctrl).unwrap_or(false),
            KeyboardOwner::Library => {
                return library::shortcut(self, s, &ui, c, ctrl).unwrap_or(false);
            }
            KeyboardOwner::Presentation => {
                return reading::shortcut(self, s, &ui, c, ctrl, shift, alt).unwrap_or(false);
            }
            KeyboardOwner::Dialog => return false,
        }
        if key(Key::F1) || (ctrl && c == '/') {
            st.set_dialog(DialogKind::Shortcuts);
            return true;
        }
        if key(Key::F6) {
            access::cycle_region(s, &ui, !shift);
            return true;
        }
        // All tools.
        if ctrl && !shift && !alt && c.eq_ignore_ascii_case(&'k') {
            let chrome = ui.global::<crate::Chrome>();
            chrome.set_palette_open(!chrome.get_palette_open());
            return true;
        }
        if let Some(handled) = read_aloud::shortcut(self, s, &ui, c, ctrl, shift) {
            return handled;
        }
        if st.get_mode() == Mode::View
            && st.get_tool() == edit::TOOL
            && let Some(handled) = self.edit_keys(s, &ui, c, ctrl, shift)
        {
            return handled;
        }
        if st.get_mode() == Mode::View
            && st.get_tool() == prepare::TOOL
            && let Some(handled) = self.prep_keys(s, &ui, c, ctrl, shift)
        {
            return handled;
        }
        // Vim keys use the Control key itself: Command is the app's on macOS.
        let (vim_ctrl, app_ctrl) = if cfg!(target_os = "macos") {
            (meta, ctrl)
        } else {
            (ctrl, false)
        };
        if !alt
            && !app_ctrl
            && let Some(handled) = self.vim_key(s, &ui, c, vim_ctrl)
        {
            return handled;
        }
        if let Some(handled) = print::shortcut(self, s, &ui, c, ctrl)
            .or_else(|| library::shortcut(self, s, &ui, c, ctrl))
            .or_else(|| reading::shortcut(self, s, &ui, c, ctrl, shift, alt))
        {
            return handled;
        }
        let mode = st.get_mode();
        if alt {
            match () {
                _ if key(Key::LeftArrow) => self.nav(s, &ui, false),
                _ if key(Key::RightArrow) => self.nav(s, &ui, true),
                _ => return false,
            }
            return true;
        }
        let arrow = [
            Key::LeftArrow,
            Key::RightArrow,
            Key::UpArrow,
            Key::DownArrow,
        ]
        .into_iter()
        .any(key);
        if ctrl && arrow && mode == Mode::View && s.tab_ref().is_some_and(|t| t.selected.is_some())
        {
            return self.page_keys(s, &ui, c, ctrl, shift);
        }
        if ctrl {
            let lower = c.to_ascii_lowercase();
            match lower {
                'o' => self.pick_file(),
                't' if !shift => self.pick_file(),
                's' if shift => self.save_as(s, &ui, false),
                's' => self.save(s, &ui, None),
                'z' if shift => self.undo(s, &ui, true),
                'z' => self.undo(s, &ui, false),
                'y' => self.undo(s, &ui, true),
                'w' => {
                    let i = s.active;
                    self.close_tab(s, &ui, i, false);
                }
                'f' if mode == Mode::View && !s.tabs.is_empty() => {
                    self.open_find(s, &ui);
                }
                'e' if mode == Mode::View && !s.tabs.is_empty() => {
                    let tool = if st.get_tool() == edit::TOOL {
                        0
                    } else {
                        edit::TOOL
                    };
                    st.set_tool(tool);
                    self.tool_changed(s, &ui);
                }
                'c' if mode == Mode::View => self.copy_selection(s),
                'a' if mode == Mode::View => self.select_all_text(s, &ui),
                'a' if mode == Mode::Organize => self.grid_select_all(s, &ui),
                _ if mode == Mode::View && !s.tabs.is_empty() => {
                    let op: fn(&mut ViewBridge) = match c {
                        '+' | '=' => |v| v.zoom_in(),
                        '-' | '_' => |v| v.zoom_out(),
                        '0' => |v| v.fit_page(),
                        '1' => |v| v.zoom_to(1.0),
                        '2' => |v| v.fit_width(zoom::MAX),
                        _ => return false,
                    };
                    self.view_op(s, &ui, op);
                }
                _ => return false,
            }
            return true;
        }
        if key(Key::Escape) {
            return self.escape(s, &ui);
        }
        if mode == Mode::Organize {
            if key(Key::Delete) || key(Key::Backspace) {
                self.organize_action(s, &ui, "remove");
                return true;
            }
            return false;
        }
        if mode != Mode::View || s.tabs.is_empty() {
            return false;
        }
        if (key(Key::Delete) || key(Key::Backspace))
            && s.tab_ref().is_some_and(|t| t.selected.is_some())
        {
            self.selection_action(s, &ui, "delete");
            return true;
        }
        if c == '+' || c == '=' {
            self.view_op(s, &ui, |v| v.zoom_in());
            return true;
        }
        if c == '-' {
            self.view_op(s, &ui, |v| v.zoom_out());
            return true;
        }
        if self.page_keys(s, &ui, c, ctrl, shift) {
            return true;
        }
        let Some(t) = s.tab() else { return false };
        if !t.view.key(c) {
            return false;
        }
        self.view_op(s, &ui, |_| {});
        true
    }

    /// Escape: finish what is in progress, else clear the selection, else the tool.
    fn escape(self: &Rc<Self>, s: &mut State, ui: &AppWindow) -> bool {
        let st = ui.global::<AppState>();
        if st.get_mode() == Mode::Organize {
            self.organize_done(s, ui);
            return true;
        }
        if !matches!(s.gesture, Gesture::None) {
            s.gesture = Gesture::None;
            self.overlay_changed(s, ui);
            return true;
        }
        if s.input.area_copy {
            s.input.area_copy = false;
            ui.global::<crate::ViewState>().set_cursor(0);
            return true;
        }
        if st.get_find_visible() {
            st.set_find_visible(false);
            self.find_close(s, ui);
            s.focus_region = 3;
            ui.invoke_focus_view();
            return true;
        }
        if let Some(t) = s.tab()
            && (t.selected.is_some() || t.text_sel.is_some())
        {
            t.selected = None;
            t.text_sel = None;
            self.selection_changed(s, ui);
            return true;
        }
        if st.get_tool() != 0 {
            st.set_tool(0);
            self.tool_changed(s, ui);
            return true;
        }
        false
    }

    /// Applies `op` to the active view and shows the result.
    pub fn view_op(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        op: impl FnOnce(&mut ViewBridge),
    ) {
        let Some(t) = s.tabs.get_mut(s.active) else {
            return;
        };
        op(&mut t.view);
        s.stats.activity(Instant::now());
        self.refresh(s, ui);
        if s.tabs[s.active].view.animating() {
            ui.window().request_redraw();
            if !s.glide.running() {
                let app = self.clone();
                s.glide
                    .start(TimerMode::Repeated, Duration::from_millis(16), move || {
                        app.act(|app, s, ui| match s.tabs.get_mut(s.active) {
                            Some(t) if t.view.animating() => {
                                t.view.step(Instant::now());
                                app.refresh(s, ui);
                            }
                            _ => s.glide.stop(),
                        })
                    });
            }
        }
    }

    /// Updates the page view, the overlay and the status shown for it.
    pub fn refresh(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let Some(t) = s.tabs.get(s.active) else {
            return;
        };
        if t.render_gen != t.session.generation() {
            // Same pages, new content (annotations, form fields): redraw what is shown.
            let renderer = reading::themed(t, renderer_for(s, &t.session));
            let t = &mut s.tabs[s.active];
            t.render_gen = t.session.generation();
            t.view.view.set_renderer(renderer, None);
        }
        let t = &mut s.tabs[s.active];
        let start = Instant::now();
        let arrived = t.view.update(ui, start);
        reading::current_page(t);
        if t.first_pixels.is_some() && !arrived.is_empty() {
            crate::startup::mark("tiles arrived");
        }
        if !t.view.last.tiles.is_empty()
            && t.view.last.missing == 0
            && let Some(at) = t.first_pixels.take()
        {
            crate::startup::mark("first page drawn");
            log::info!(
                "first screen drawn {} ms after opening",
                at.elapsed().as_millis()
            );
        }
        if t.held && !t.view.last.tiles.is_empty() && t.view.last.missing == 0 {
            let uid = t.uid;
            self.release_background(s, ui, uid);
        }
        let t = &mut s.tabs[s.active];
        t.previews_arrived(&arrived);
        t.sync_status(ui);
        reading::sync_page(t, ui);
        if edit::previews_stale(t) {
            t.overlay_dirty = true;
        }
        if t.overlay_dirty || t.overlay_pages != t.view.last.pages {
            overlay::rebuild(s, ui);
            page_text::rebuild(s);
        }
        let t = &mut s.tabs[s.active];
        if ui.global::<AppState>().get_mode() == Mode::Organize {
            organize::previews_arrived(t, ui, &arrived);
        }
        s.stats.update_took(start.elapsed());
        if let Some(at) = s.tabs[s.active].view.last.wake_at {
            let app = self.clone();
            s.settle.start(
                TimerMode::SingleShot,
                at.saturating_duration_since(Instant::now()),
                move || app.act(|app, s, ui| app.refresh(s, ui)),
            );
        }
    }

    /// The overlay must be drawn again (annotations, selection, hits changed).
    pub fn overlay_changed(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        if let Some(t) = s.tab() {
            t.overlay_dirty = true;
        }
        self.refresh(s, ui);
    }

    fn rendering_hooks(self: &Rc<Self>, ui: &AppWindow) {
        let app = self.clone();
        let result = ui.window().set_rendering_notifier(move |state, _| {
            let now = Instant::now();
            match state {
                slint::RenderingState::BeforeRendering => app.before_render(now),
                slint::RenderingState::AfterRendering => app.after_render(now),
                _ => {}
            }
        });
        if let Err(e) = result {
            log::warn!("frame timing unavailable with this renderer: {e:?}");
        }
    }

    fn before_render(self: &Rc<Self>, now: Instant) {
        let Some(ui) = self.ui.upgrade() else { return };
        let Ok(mut s) = self.state.try_borrow_mut() else {
            return;
        };
        let s = &mut *s;
        s.stats.before_render(now);
        let Some(t) = s.tabs.get_mut(s.active) else {
            return;
        };
        let mut moved = false;
        if let Some(bench) = &mut s.bench {
            if !bench.step(now, &mut t.view, &mut s.stats) {
                log::info!("benchmark finished");
                let _ = slint::quit_event_loop();
            }
            moved = true;
        }
        if t.view.animating() {
            t.view.step(now);
            s.stats.activity(now);
            moved = true;
        }
        if moved {
            self.refresh(s, &ui);
        }
    }

    fn after_render(&self, now: Instant) {
        FIRST_FRAME.call_once(|| {
            crate::startup::mark("first frame");
            preopen::warm_fonts();
        });
        let Some(ui) = self.ui.upgrade() else { return };
        let Ok(mut s) = self.state.try_borrow_mut() else {
            return;
        };
        s.stats.after_render(now);
        let more = s.bench.is_some() || s.tabs.get(s.active).is_some_and(|t| t.view.animating());
        if more {
            ui.window().request_redraw();
        }
    }

    /// Window size and position from the settings, the close button, and dropped files.
    fn window_hooks(self: &Rc<Self>, ui: &AppWindow, size: Option<(f32, f32)>) {
        let s = self.state.borrow();
        let w = ui.window();
        if let Some((width, height)) = size {
            w.set_size(slint::LogicalSize::new(width, height));
        } else if let Some(ws) = s.settings.window {
            crate::window_state::apply(ui, &ws);
            crate::window_state::restore(self);
        }
        drop(s);
        let app = self.clone();
        w.on_close_requested(move || app.close_requested());
        crate::dnd::install(self, ui);
    }

    fn close_requested(self: &Rc<Self>) -> slint::CloseRequestResponse {
        let Some(ui) = self.ui.upgrade() else {
            return slint::CloseRequestResponse::HideWindow;
        };
        let Ok(mut s) = self.state.try_borrow_mut() else {
            return slint::CloseRequestResponse::KeepWindowShown;
        };
        let s = &mut *s;
        self.finish_editors(s, &ui);
        if !s.quitting
            && let Some(i) = s.tabs.iter().position(|t| t.session.is_dirty())
        {
            self.select_tab(s, &ui, i);
            self.ask_unsaved(s, &ui, AfterUnsaved::Quit);
            return slint::CloseRequestResponse::KeepWindowShown;
        }
        self.remember_window(s, &ui);
        self.save_settings(s);
        slint::CloseRequestResponse::HideWindow
    }

    pub fn remember_window(&self, s: &mut State, ui: &AppWindow) {
        let w = ui.window();
        let scale = w.scale_factor();
        let size = w.size().to_logical(scale);
        let pos = w.position();
        let maximized = w.is_maximized();
        let old = s.settings.window.unwrap_or_default();
        let ws = pagebara_app_core::settings::WindowState {
            width: if maximized { old.width } else { size.width },
            height: if maximized { old.height } else { size.height },
            x: Some(pos.x),
            y: Some(pos.y),
            maximized,
        };
        crate::window_state::remember(&mut s.settings, ws, crate::window_state::layout(ui));
    }

    /// Quits once every document is saved or discarded.
    pub fn quit(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        s.quitting = true;
        self.remember_window(s, ui);
        self.save_settings(s);
        let _ = ui.window().hide();
        let _ = slint::quit_event_loop();
    }

    pub fn pick_file(self: &Rc<Self>) {
        if let Some(path) = pick::take_answer() {
            return self.open_path(path);
        }
        let app = self.clone();
        let task = slint::spawn_local(async move {
            let picked = rfd::AsyncFileDialog::new()
                .set_title(tr!("Open a PDF"))
                .add_filter(tr!("PDF documents"), &["pdf", "PDF"])
                .pick_files()
                .await;
            for f in picked.unwrap_or_default() {
                app.open_path(f.path().to_owned());
            }
        });
        if let Err(e) = task {
            log::warn!("cannot show the file dialog: {e}");
        }
    }

    pub fn open_path(self: &Rc<Self>, path: PathBuf) {
        self.act(move |app, s, ui| {
            if ui.global::<crate::LibraryState>().get_visible() {
                library::show(app, s, ui, false);
            }
            // An open document is shown again instead of opened twice.
            let abs = std::path::absolute(&path).ok();
            let known = s.tabs.iter().position(|t| {
                t.session
                    .path
                    .as_ref()
                    .and_then(|p| std::path::absolute(p).ok())
                    == abs
            });
            if let Some(i) = known {
                return app.select_tab(s, ui, i);
            }
            ui.global::<AppState>().set_busy(true);
            s.opening = Some(Instant::now());
            app.message(s, &tr!("Opening…"));
            // Form data (FDF, XFDF): the form it belongs to, filled into a copy.
            if pagebara_actions::is_form_data(&path) {
                pagebara_thread::spawn("io-fill", move || {
                    let r = pagebara_actions::fill_form(&path, None);
                    post(move |app| match r {
                        Ok(filled) => app.open_path(filled),
                        Err(e) => app.act(move |app, s, ui| {
                            ui.global::<AppState>().set_busy(false);
                            app.message(s, &tr!("Could not fill in the form: {}.", e));
                        }),
                    });
                });
                return;
            }
            pagebara_thread::spawn("io-open", move || {
                let result = session::open_file(&path);
                post(move |app| app.opened(Some(path), result));
            });
        });
    }

    pub fn opened(self: &Rc<Self>, path: Option<PathBuf>, result: Result<Opened, OpenError>) {
        self.act(move |app, s, ui| {
            ui.global::<AppState>().set_busy(false);
            match result {
                Ok(opened) => {
                    if let Some(p) = &path {
                        s.recent.add(p);
                        show_recent(s, ui);
                        App::report_recent(s, p);
                    }
                    app.clear_message(s, ui);
                    match opened {
                        Opened::Ready(session) => app.add_tab(s, ui, *session),
                        Opened::Locked(pending) => app.ask_password(s, ui, pending),
                    }
                }
                Err(e) => {
                    if let (OpenError::Io(io), Some(p)) = (&e, &path)
                        && io.kind() == std::io::ErrorKind::NotFound
                    {
                        s.recent.remove(p);
                        show_recent(s, ui);
                    }
                    log::warn!("open failed: {e}");
                    app.clear_message(s, ui);
                    let title = match path.as_ref().and_then(|p| p.file_name()) {
                        Some(name) => tr!("Could not open “{}”", name.to_string_lossy()),
                        None => tr!("Could not open the file"),
                    };
                    app.show_message(s, ui, &title, &e.to_string());
                }
            }
        });
    }

    fn ask_password(self: &Rc<Self>, s: &mut State, ui: &AppWindow, pending: PendingUnlock) {
        let st = ui.global::<AppState>();
        let title = pending.title();
        st.set_password_file(title.clone().into());
        st.set_password_error(false);
        st.set_unlocking(false);
        st.set_password_visible(true);
        s.pending = Some(pending);
        if let Some(pw) = s.startup.password.take() {
            self.unlock(s, ui, pw);
        }
    }

    fn unlock(self: &Rc<Self>, s: &mut State, ui: &AppWindow, password: String) {
        let Some(pending) = s.pending.take() else {
            return;
        };
        ui.global::<AppState>().set_unlocking(true);
        pagebara_thread::spawn("io-unlock", move || {
            let result = pending.unlock(&password);
            post_act(move |app, s, ui| {
                let st = ui.global::<AppState>();
                st.set_unlocking(false);
                match result {
                    Ok(session) => {
                        st.set_password_visible(false);
                        app.add_tab(s, ui, session);
                    }
                    Err(pending) => {
                        st.set_password_error(true);
                        s.pending = Some(pending);
                    }
                }
            });
        });
    }

    fn new_view(self: &Rc<Self>, s: &State, ui: &AppWindow, session: &Session) -> ViewBridge {
        let notify = || post(|app| app.act(|app, s, ui| app.refresh(s, ui)));
        let renderer = renderer_for(s, session);
        // The session's sizes: asking the renderer would look up every page again.
        let sizes: Vec<(f64, f64)> = session.geoms.iter().map(|g| g.size()).collect();
        let mut view = ViewBridge::new(ui, &sizes, renderer, notify);
        view.set_r2l(session.reads_right_to_left());
        view.resized(s.view_size.0, s.view_size.1);
        view
    }

    fn add_tab(self: &Rc<Self>, s: &mut State, ui: &AppWindow, session: Session) {
        crate::startup::mark("document read");
        let mut view = self.new_view(s, ui, &session);
        crate::startup::mark("view made");
        let render_gen = session.generation();
        let remembered = session.path.as_deref().and_then(|p| s.settings.zoom_for(p));
        match remembered {
            Some(z) if z > 0.0 => view.zoom_to(z),
            _ => view.fit_width(1.5),
        }
        s.next_uid += 1;
        let tab = Tab {
            uid: s.next_uid,
            workspace: TabWorkspace::default(),
            thumbs: Rc::new(VecModel::from(thumb_rows(&session))),
            thumbs_shown: 0..0,
            outline: Rc::new(VecModel::from(outline_rows(&session))),
            outline_current: None,
            layout_rev: session.layout_rev,
            session,
            view,
            shown_page: None,
            shown_zoom: f64::NAN,
            annots: HashMap::new(),
            annots_rev: u64::MAX,
            selected: None,
            text_sel: None,
            search: Search::default(),
            nav: Nav::default(),
            grid: Grid::default(),
            images: HashMap::new(),
            overlay_dirty: true,
            overlay_pages: Vec::new(),
            comments: Vec::new(),
            comments_rev: None,
            render_gen,
            form: None,
            form_ui: forms::FormUi::default(),
            reading: reading::TabReading::default(),
            edit: edit::TabEdit::default(),
            prep: prepare::TabPrep::default(),
            vim: vim_keys::TabVim::default(),
            first_pixels: s.opening.take(),
            held: true,
        };
        let (app, uid) = (self.clone(), tab.uid);
        Timer::single_shot(HOLD_BACKGROUND, move || {
            app.act(move |app, s, ui| app.release_background(s, ui, uid))
        });
        crate::startup::mark("tab made");
        if let Some(at) = tab.first_pixels {
            log::info!(
                "document read {} ms after opening",
                at.elapsed().as_millis()
            );
        }
        self.leave_tab(s, ui);
        s.tabs.push(tab);
        s.active = s.tabs.len() - 1;
        reading::opened(self, s, ui);
        crate::startup::mark("reading state applied");
        self.restore_tab(s, ui);
        crate::startup::mark("tab shown");
        self.apply_startup(s, ui);
        self.apply_open_at(s, ui);
        crate::startup::mark("document shown");
    }

    /// Starts the background work held for tab `uid` (see [`Tab::held`]).
    fn release_background(self: &Rc<Self>, s: &mut State, ui: &AppWindow, uid: u64) {
        let Some(i) = s.tab_index(uid).filter(|&i| s.tabs[i].held) else {
            return;
        };
        s.tabs[i].held = false;
        crate::startup::mark("background work started");
        // Other tabs start theirs when shown.
        if i == s.active {
            self.doc_changed(s, ui);
            page_text::rebuild(s);
        }
    }

    /// Command-line options that need a document.
    fn apply_startup(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let o = std::mem::take(&mut s.startup);
        let Some(t) = s.tab() else { return };
        if let Some(z) = o.zoom {
            t.view.zoom_to(z);
        }
        if let Some(p) = o.page {
            t.view.view.viewport.go_to_page(p);
        }
        // `--sign` (a file-manager menu): the Sign tool.
        if let Some(tool) = o.tool.or(o.sign.then_some(4)) {
            ui.global::<AppState>().set_tool(tool);
            self.tool_changed(s, ui);
        }
        if o.print {
            print::open(self, s, ui);
        }
        if o.organize {
            self.organize(s, ui);
        }
        if o.bench {
            s.bench = Some(Bench::new());
            ui.window().request_redraw();
        }
        self.refresh(s, ui);
    }

    fn step_tab(self: &Rc<Self>, s: &mut State, ui: &AppWindow, back: bool, reorder: bool) {
        let n = s.tabs.len();
        if n < 2 {
            return;
        }
        let i = if back {
            (s.active + n - 1) % n
        } else {
            (s.active + 1) % n
        };
        if reorder {
            if (back && s.active == 0) || (!back && s.active == n - 1) {
                return;
            }
            s.tabs.swap(s.active, i);
            s.active = i;
            Self::show_tabs(s, ui);
        } else {
            self.select_tab(s, ui, i);
        }
    }

    pub fn select_tab(self: &Rc<Self>, s: &mut State, ui: &AppWindow, i: usize) {
        if i == s.active || i >= s.tabs.len() {
            return;
        }
        self.leave_tab(s, ui);
        s.active = i;
        self.restore_tab(s, ui);
    }

    fn leave_tab(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        if let Some(t) = s.tab() {
            t.workspace = TabWorkspace::capture(ui);
            self.finish_editors(s, ui);
            self.vim_reset(s, ui);
        }
        s.gesture = Gesture::None;
        self.clear_message(s, ui);
    }

    fn restore_tab(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        if let Some(t) = s.tab_ref() {
            let w = &t.workspace;
            let st = ui.global::<AppState>();
            st.set_mode(if w.organize {
                Mode::Organize
            } else {
                Mode::View
            });
            st.set_tool(w.tool);
            st.set_markup_kind(w.markup);
            st.set_shape(w.shape);
            st.set_text_mode(w.text_mode);
            ui.global::<crate::Chrome>().set_group(w.group);
            self.show_tool(s, ui);
        }
        self.show_active(s, ui);
        ui.invoke_focus_view();
        s.focus_region = 3;
    }

    /// Shows the active tab (or the welcome screen) in every part of the window.
    pub fn show_active(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let st = ui.global::<AppState>();
        let vs = ui.global::<crate::ViewState>();
        match s.tabs.get_mut(s.active) {
            Some(t) => {
                t.view.attach(ui);
                t.view.resized(s.view_size.0, s.view_size.1);
                st.set_thumbs(ModelRc::from(t.thumbs.clone()));
                // The list reports only when it scrolls, not when its model changes.
                t.show_thumbs(s.thumbs_on_screen.clone());
                st.set_outline(ModelRc::from(t.outline.clone()));
                st.set_page_count(t.session.page_count() as i32);
                t.shown_page = None;
                t.shown_zoom = f64::NAN;
                t.overlay_dirty = true;
                if st.get_mode() == Mode::Welcome {
                    st.set_mode(Mode::View);
                }
                st.set_find_collapsed(false);
                st.set_find_visible(!t.search.query.is_empty() && !t.vim.hits_hidden);
                st.set_find_text(t.search.query.as_str().into());
                // This tab's "3 of 12", not the last one's.
                search::show_status(&st, &t.search);
                search::show_rows(&st, t);
            }
            None => {
                st.set_mode(Mode::Welcome);
                st.set_thumbs(ModelRc::default());
                st.set_outline(ModelRc::default());
                st.set_comments(ModelRc::default());
                st.set_find_visible(false);
                st.set_find_rows(ModelRc::default());
                st.set_zoom_label(SharedString::default());
                vs.set_overlay(ModelRc::default());
                ViewBridge::detach(ui);
            }
        }
        self.doc_changed(s, ui);
        reading::show(s, ui);
        if ui.global::<AppState>().get_mode() == Mode::Organize {
            self.organize(s, ui);
        }
        self.refresh(s, ui);
    }

    fn show_tabs(s: &State, ui: &AppWindow) {
        let st = ui.global::<AppState>();
        let tabs: Vec<TabInfo> = s
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| TabInfo {
                title: t.session.title().into(),
                folder: t
                    .session
                    .path
                    .as_deref()
                    .and_then(|p| p.parent())
                    .map(|dir| dir.to_string_lossy().as_ref().into())
                    .unwrap_or_default(),
                dirty: t.session.is_dirty(),
                active: i == s.active,
            })
            .collect();
        st.set_active_tab(s.active as i32);
        // Background document updates should leave keyboard focus in the switcher.
        if st.get_tabs().iter().ne(tabs.iter().cloned()) {
            st.set_tabs(ModelRc::new(VecModel::from(tabs)));
        }
    }

    /// Titles, tabs, undo state and side lists after the document or the tab changed.
    pub fn doc_changed(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        edit::sync(s, ui);
        let st = ui.global::<AppState>();
        Self::show_tabs(s, ui);
        let Some(t) = s.tabs.get(s.active) else {
            st.set_title("".into());
            st.set_subtitle("".into());
            st.set_window_title(pagebara_identity::NAME.into());
            st.set_can_undo(false);
            st.set_can_redo(false);
            st.set_dirty(false);
            st.set_protection("".into());
            st.set_page_size("".into());
            signing::sync(self, s, ui);
            ocr::sync(self, s, ui);
            return;
        };
        let ses = &t.session;
        st.set_title(ses.title().into());
        st.set_subtitle(subtitle(ses).into());
        let mark = if ses.is_dirty() { "• " } else { "" };
        st.set_window_title(format!("{mark}{} — {}", ses.title(), pagebara_identity::NAME).into());
        st.set_can_undo(ses.can_undo());
        st.set_can_redo(ses.can_redo());
        st.set_undo_label(ses.undo_label().unwrap_or_default().into());
        st.set_redo_label(ses.redo_label().unwrap_or_default().into());
        st.set_dirty(ses.is_dirty());
        st.set_protection(ses.protection().into());
        st.set_encrypted(ses.encrypted());
        st.set_page_count(ses.page_count() as i32);
        st.set_can_back(!t.nav.back.is_empty());
        st.set_can_forward(!t.nav.forward.is_empty());
        panel::refresh_comments(self, s, ui);
        panel::sync_selection(s, ui);
        forms::sync_fields(s, ui);
        prepare::sync(s, ui);
        signing::sync(self, s, ui);
        redaction::sync(s, ui);
        ocr::sync(self, s, ui);
    }

    /// After an edit, undo or redo: rebuilds what depends on the pages when they changed.
    pub fn after_edit(self: &Rc<Self>, s: &mut State, ui: &AppWindow, pages: bool) {
        if pages {
            self.rebuild_pages(s, ui);
        }
        if let Some(t) = s.tab() {
            t.overlay_dirty = true;
            if let Some(sel) = t.selected
                && t.annot(sel).is_none()
            {
                t.selected = None;
            }
            if pages {
                t.text_sel = None;
                t.selected = None;
            }
        }
        self.doc_changed(s, ui);
        forms::sync_fields(s, ui);
        if pages && ui.global::<AppState>().get_mode() == Mode::Organize {
            self.organize(s, ui);
        }
        self.refresh(s, ui);
    }

    /// New view, thumbnails and outline for changed pages, keeping the position.
    fn rebuild_pages(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let Some(t) = s.tabs.get(s.active) else {
            return;
        };
        let (page, v) = t.view.position();
        let zoom = t.view.view.viewport.zoom();
        let fit = t.view.is_fit_width();
        let mut view = self.new_view(s, ui, &s.tabs[s.active].session);
        if fit {
            view.fit_width(1.5);
        } else {
            view.zoom_to(zoom);
        }
        let n = s.tabs[s.active].session.page_count();
        let page = page.min(n.saturating_sub(1));
        let k = view.view.viewport.k();
        let top = view.view.viewport.layout().page_top(page, k);
        let x = view.view.viewport.scroll().0;
        view.view.viewport.scroll_to(x, top + v * k);
        let t = &mut s.tabs[s.active];
        t.view = view;
        reading::apply_view(t);
        t.render_gen = t.session.generation();
        t.layout_rev = t.session.layout_rev;
        t.thumbs.set_vec(thumb_rows(&t.session));
        let shown = std::mem::take(&mut t.thumbs_shown);
        t.show_thumbs(shown);
        t.outline.set_vec(outline_rows(&t.session));
        t.shown_page = None;
        t.search.invalidate();
        t.grid.selected.retain(|&i| i < n);
        ui.global::<AppState>().set_page_count(n as i32);
        self.restart_search(s, ui);
    }

    pub fn message(self: &Rc<Self>, s: &mut State, text: &str) {
        let Some(ui) = self.ui.upgrade() else { return };
        ui.global::<AppState>().set_message(text.into());
        let weak = self.ui.clone();
        s.message
            .start(TimerMode::SingleShot, MESSAGE_FOR, move || {
                if let Some(ui) = weak.upgrade() {
                    ui.global::<AppState>().set_message(SharedString::default());
                }
            });
    }

    fn clear_message(&self, s: &mut State, ui: &AppWindow) {
        s.message.stop();
        ui.global::<AppState>().set_message(SharedString::default());
    }

    fn screenshot_after(self: &Rc<Self>, path: PathBuf, after: Duration) {
        let weak = self.ui.clone();
        Timer::single_shot(after, move || {
            let Some(ui) = weak.upgrade() else { return };
            crate::screenshot::take(&ui, &path);
            let _ = slint::quit_event_loop();
        });
    }

    /// Goes to page `i` (at point `v` from its top, if given), remembering where we were.
    pub fn jump(self: &Rc<Self>, s: &mut State, ui: &AppWindow, i: usize, v: Option<f64>) {
        let Some(t) = s.tab() else { return };
        if i >= t.session.page_count() {
            return;
        }
        reading::settle(t);
        t.push_nav();
        match v {
            Some(v) => t.view.glide_to(i, v, 24.0),
            None => t.view.glide_to_page(i),
        }
        reading::follow(t, ui, i);
        self.doc_nav_state(s, ui);
        self.view_op(s, ui, |_| {});
    }

    fn doc_nav_state(&self, s: &State, ui: &AppWindow) {
        if let Some(t) = s.tab_ref() {
            let st = ui.global::<AppState>();
            st.set_can_back(!t.nav.back.is_empty());
            st.set_can_forward(!t.nav.forward.is_empty());
        }
    }

    fn nav(self: &Rc<Self>, s: &mut State, ui: &AppWindow, forward: bool) {
        let Some(t) = s.tab() else { return };
        reading::settle(t);
        let here = t.view.position();
        let target = if forward {
            t.nav.forward.pop()
        } else {
            t.nav.back.pop()
        };
        let Some((page, v)) = target else { return };
        if forward {
            t.nav.back.push(here);
        } else {
            t.nav.forward.push(here);
        }
        t.view.glide_to(page, v, 0.0);
        reading::follow(t, ui, page);
        self.doc_nav_state(s, ui);
        self.view_op(s, ui, |_| {});
    }

    fn page_entered(self: &Rc<Self>, s: &mut State, ui: &AppWindow, text: &str) {
        let Some(t) = s.tab_ref() else { return };
        let text = text.trim();
        match t.page_named(text) {
            Some(p) => {
                self.jump(s, ui, p, None);
                ui.invoke_focus_view();
            }
            None => self.message(s, &tr!("There is no page “{}”.", text)),
        }
    }
}

/// A page renderer for the session's current document, form fields shown as they will be
/// in other readers.
fn renderer_for(s: &State, session: &Session) -> Arc<dyn PageRenderer> {
    s.backend.renderer(RenderSource {
        doc: pagebara_app_core::forms::for_display(session.doc()),
        refs: session.tree.refs(),
    })
}

impl Tab {
    /// The page a typed page number or label names: the page with that label, else the
    /// page with that number.
    pub fn page_named(&self, text: &str) -> Option<usize> {
        let text = text.trim();
        let n = self.session.page_count();
        let by_label = (0..n).find(|&i| self.session.page_label(i).eq_ignore_ascii_case(text));
        by_label.or_else(|| {
            text.parse::<usize>()
                .ok()
                .filter(|&p| p >= 1 && p <= n)
                .map(|p| p - 1)
        })
    }

    /// Puts newly rendered previews into the thumbnails shown.
    fn previews_arrived(&mut self, pages: &[usize]) {
        for &p in pages {
            if self.thumbs_shown.contains(&p) {
                let image = self.view.image(&Key::preview(p)).unwrap_or_default();
                self.set_thumb(p, image);
            }
        }
    }

    /// The thumbnails of `pages` are on screen: they get their previews, and the ones that
    /// went off screen let theirs go (the tile cache keeps previews within its budget).
    pub fn show_thumbs(&mut self, pages: std::ops::Range<usize>) {
        let old = std::mem::replace(&mut self.thumbs_shown, pages.clone());
        self.view.view.want_previews(pages.clone());
        let n = self.session.page_count();
        for p in old.start..old.end.min(n) {
            if !pages.contains(&p) {
                self.set_thumb(p, Image::default());
            }
        }
        for p in pages.start..pages.end.min(n) {
            if !old.contains(&p)
                && let Some(image) = self.view.image(&Key::preview(p))
            {
                self.set_thumb(p, image);
            }
        }
    }

    fn set_thumb(&mut self, p: usize, image: Image) {
        if let Some(mut t) = self.thumbs.row_data(p) {
            if t.image == image {
                return;
            }
            t.image = image;
            self.thumbs.set_row_data(p, t);
        }
    }

    fn sync_status(&mut self, ui: &AppWindow) {
        let st = ui.global::<AppState>();
        let page = self.view.current_page;
        if self.shown_page != Some(page) {
            if let Some(old) = self.shown_page {
                self.set_current(old, false);
            }
            self.set_current(page, true);
            self.shown_page = Some(page);
            st.set_current_page(page as i32);
            st.set_page_label(self.session.page_label(page).into());
            let label = self.session.geoms.get(page).map(|g| {
                let (w, h) = g.size();
                session::page_size_label(w, h)
            });
            st.set_page_size(label.unwrap_or_default().into());
            self.sync_outline(page);
        }
        let z = self.view.view.viewport.zoom();
        if z != self.shown_zoom {
            self.shown_zoom = z;
            st.set_zoom_label(zoom::label(z).into());
        }
    }

    fn set_current(&self, page: usize, current: bool) {
        if let Some(mut t) = self.thumbs.row_data(page) {
            t.current = current;
            self.thumbs.set_row_data(page, t);
        }
    }

    /// Highlights the last outline entry at or before `page`. Only the rows highlighted
    /// before and now change; long outlines have thousands.
    fn sync_outline(&mut self, page: usize) {
        let current = self
            .session
            .outline
            .iter()
            .rposition(|o| o.page.is_some_and(|p| p <= page));
        for i in [self.outline_current, current].into_iter().flatten() {
            if let Some(mut row) = self.outline.row_data(i) {
                let want = Some(i) == current;
                if row.current != want {
                    row.current = want;
                    self.outline.set_row_data(i, row);
                }
            }
        }
        self.outline_current = current;
    }
}

pub fn remember_zoom(settings: &mut Settings, t: &Tab) {
    if let Some(p) = &t.session.path {
        let z = if t.view.is_fit_width() {
            0.0
        } else {
            t.view.view.viewport.zoom()
        };
        settings.set_zoom(p, z);
    }
}

pub fn show_recent(s: &State, ui: &AppWindow) {
    let rows: Vec<RecentFile> = s
        .recent
        .paths()
        .iter()
        .map(|p| RecentFile {
            name: p
                .file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default()
                .as_ref()
                .into(),
            folder: p
                .parent()
                .map(Path::to_string_lossy)
                .unwrap_or_default()
                .as_ref()
                .into(),
        })
        .collect();
    ui.global::<AppState>()
        .set_recent(ModelRc::new(VecModel::from(rows)));
}

fn subtitle(s: &Session) -> String {
    let n = s.page_count();
    if s.is_dirty() {
        trn!(
            "{n} page · Unsaved changes",
            "{n} pages · Unsaved changes",
            n
        )
    } else {
        trn!("{n} page", "{n} pages", n)
    }
}

pub fn thumb_rows(s: &Session) -> Vec<Thumb> {
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

fn outline_rows(s: &Session) -> Vec<OutlineRow> {
    s.outline
        .iter()
        .map(|o| OutlineRow {
            title: o.title.as_str().into(),
            depth: o.depth as i32,
            page: o.page.map_or(-1, |p| p as i32),
            current: false,
        })
        .collect()
}

static FIRST_FRAME: std::sync::Once = std::sync::Once::new();

/// Whether the window has drawn a frame (before that, snapshots fail).
pub fn first_frame_drawn() -> bool {
    FIRST_FRAME.is_completed()
}
