//! Desktop integration in the window: files handed over by later launches (with their
//! start page and search), the system's recent-files list, becoming the default PDF app,
//! and moving a tab to a window of its own.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::SystemTime;

use pagebara_i18n::{tr, trn};
use pagebara_os::recent_xbel;
use pagebara_os::{OpenFile, Server};
use slint::ComponentHandle;

use super::{App, State, post, post_act};
use crate::{AppState, AppWindow, DialogKind, IntegrationRow};

pub fn wire(app: &Rc<App>, ui: &AppWindow) {
    let st = ui.global::<AppState>();
    let a = app.clone();
    st.on_system_action(move |what| {
        let what = what.to_string();
        a.act(move |app, s, ui| app.system_action(s, ui, &what))
    });
    let a = app.clone();
    st.on_integration_action(move |id, what| {
        let (id, what) = (id.to_string(), what.to_string());
        a.act(move |app, s, ui| app.integration_action(s, ui, &id, &what))
    });
    let a = app.clone();
    st.on_tab_torn_off(move |i| a.act(move |app, s, ui| app.tab_torn_off(s, ui, i as usize)));
}

impl App {
    /// Takes the files of later launches (see `crate::os::launch`).
    pub fn listen(self: &Rc<Self>, server: Server) {
        let registration = server.serve(|req| post(move |app| app.open_request(req.files)));
        self.state.borrow_mut().instance = Some(registration);
    }

    /// Opens `files` (showing the ones already open) and brings the window forward.
    pub fn open_request(self: &Rc<Self>, files: Vec<OpenFile>) {
        self.act(move |app, s, ui| {
            log::info!("{} file(s) from another launch", files.len());
            for f in files {
                match tab_for(s, &f.path) {
                    Some(i) => {
                        app.select_tab(s, ui, i);
                        app.go_to(s, ui, f.page, f.search);
                    }
                    None if s.settings.system.open_in_new_window && !s.tabs.is_empty() => {
                        let page = f.page.unwrap_or(0);
                        if let Err(e) =
                            crate::os::new_window(&f.path, page, s.profile.dir.as_deref())
                        {
                            log::warn!("new window: {e}");
                        }
                    }
                    None => {
                        let path = f.path.clone();
                        s.open_at.push(f);
                        app.open_path(path);
                    }
                }
            }
            crate::os::raise(ui);
        });
    }

    /// The start page and search given for the document just opened.
    pub(super) fn apply_open_at(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let Some(path) = s.tab_ref().and_then(|t| t.session.path.clone()) else {
            return;
        };
        let abs = std::path::absolute(&path).ok();
        let Some(i) = s.open_at.iter().position(|f| Some(&f.path) == abs.as_ref()) else {
            return;
        };
        let f = s.open_at.remove(i);
        self.go_to(s, ui, f.page, f.search);
    }

    fn go_to(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        page: Option<usize>,
        search: Option<String>,
    ) {
        if let (Some(p), Some(t)) = (page, s.tab()) {
            let last = t.session.page_count().saturating_sub(1);
            t.view.view.viewport.go_to_page(p.min(last));
        }
        if let Some(text) = search {
            let st = ui.global::<AppState>();
            st.set_find_visible(true);
            st.set_find_text(text.as_str().into());
            self.find(s, ui, &text);
        }
        self.refresh(s, ui);
    }

    /// Adds `path` to the system's recent files (freedesktop list, Windows' recent
    /// documents, macOS' Open Recent), when the user allows it; refreshes the jump list.
    pub(super) fn report_recent(s: &State, path: &Path) {
        let Ok(abs) = std::path::absolute(path) else {
            return;
        };
        if s.settings.system.report_recent {
            if cfg!(all(unix, not(target_os = "macos"))) {
                if let Some(list) = s.profile.os_recent_path.clone() {
                    let abs = abs.clone();
                    pagebara_thread::spawn("io-recent", move || {
                        let app = recent_xbel::AppInfo::this_app();
                        if let Err(e) = recent_xbel::add(
                            &list,
                            &abs,
                            "application/pdf",
                            &app,
                            SystemTime::now(),
                        ) {
                            log::warn!("could not add to the system's recent files: {e}");
                        }
                    });
                }
            } else if s.profile.dir.is_none() {
                pagebara_native::note_recent(&abs);
            }
        }
        update_jump_list(s);
    }

    /// macOS: files opened from Finder and the Dock menu (see `pagebara_native`).
    pub fn install_open_handler(self: &Rc<Self>) {
        if !cfg!(target_os = "macos") {
            return;
        }
        let dock = {
            let s = self.state.borrow();
            update_jump_list(&s);
            s.settings.system.jump_list
        };
        pagebara_native::install_open_handler(
            Box::new(|paths| {
                let files = paths
                    .into_iter()
                    .map(|path| OpenFile {
                        path,
                        page: None,
                        search: None,
                    })
                    .collect();
                post(move |app| app.open_request(files));
            }),
            Box::new(|| {
                post(|app| {
                    let profile = app.state.borrow().profile.dir.clone();
                    let args = ["--new-window".to_owned()];
                    if let Err(e) = crate::os::relaunch(&args, profile.as_deref()) {
                        log::warn!("new window: {e}");
                    }
                })
            }),
            dock,
        );
    }

    /// Opens the documents open when the app was last closed (Settings > System
    /// integration > Reopen last documents), when this launch named none.
    pub fn restore_session(self: &Rc<Self>) {
        let docs = {
            let mut s = self.state.borrow_mut();
            if !s.settings.system.restore_session || !s.open_at.is_empty() {
                return;
            }
            let docs: Vec<_> = s
                .settings
                .system
                .session
                .iter()
                .filter(|d| d.path.is_file())
                .cloned()
                .collect();
            for d in &docs {
                s.open_at.push(OpenFile {
                    path: d.path.clone(),
                    page: Some(d.page),
                    search: None,
                });
            }
            docs
        };
        for d in docs {
            self.open_path(d.path);
        }
    }

    /// Fills in the System integration line of Settings.
    pub(super) fn show_system_settings(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        super::update::show_settings(s, ui);
        let on = s.settings.system.clone();
        let profile = s.profile.clone();
        ui.global::<AppState>()
            .set_integration_summary(tr!("Checking…").into());
        pagebara_thread::spawn("int-status", move || {
            let int = crate::integration::open(&profile);
            let count = crate::integration::rows(&int, &on)
                .iter()
                .filter(|r| r.on && r.toggle && r.kind == pagebara_integration::Kind::Registration)
                .count();
            post_act(move |_, _, ui| {
                let text = match count {
                    0 => tr!("Nothing is registered with your system yet."),
                    n => trn!(
                        "{n} item is registered with your system.",
                        "{n} items are registered with your system.",
                        n
                    ),
                };
                ui.global::<AppState>().set_integration_summary(text.into());
            });
        });
    }

    /// Shows Settings > System integration (statuses are read in the background).
    pub fn show_integration(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let on = s.settings.system.clone();
        let profile = s.profile.clone();
        ui.global::<AppState>().set_dialog(DialogKind::Integration);
        pagebara_thread::spawn("int-status", move || {
            let int = crate::integration::open(&profile);
            let rows = crate::integration::rows(&int, &on);
            post_act(move |_, _, ui| show_rows(ui, rows));
        });
    }

    pub(super) fn integration_action(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        id: &str,
        what: &str,
    ) {
        let st = ui.global::<AppState>();
        match what {
            "summary" => self.show_system_settings(s, ui),
            "open" => {
                st.set_integration_from_settings(st.get_dialog() == DialogKind::Settings);
                st.set_setup_card_visible(false);
                if !s.settings.system.setup_card_done {
                    s.settings.system.setup_card_done = true;
                    self.save_settings(s);
                }
                st.set_integration_status("".into());
                self.show_integration(s, ui);
            }
            "close" if st.get_integration_from_settings() => {
                self.show_system_settings(s, ui);
                st.set_dialog(DialogKind::Settings);
            }
            "close" => st.set_dialog(DialogKind::None),
            "dismiss-card" => {
                st.set_setup_card_visible(false);
                s.settings.system.setup_card_done = true;
                self.save_settings(s);
            }
            "button" if id.starts_with("default-") => {
                crate::links::open(&pagebara_os::default_app::settings_uri());
            }
            "on" | "off" | "button" | "remove-all" => {
                let on = what != "off";
                // Settings change at once; registrations in the background.
                if what != "remove-all"
                    && crate::integration::setting(&s.settings.system, id).is_some()
                {
                    let mut int = crate::integration::open(&s.profile);
                    let profile = s.profile.clone();
                    let r = crate::integration::set(&mut int, &profile, &mut s.settings, id, on);
                    st.set_integration_status(
                        r.err().map(|e| e.to_string()).unwrap_or_default().into(),
                    );
                    self.save_settings(s);
                    self.integration_changed(s, ui, id);
                    return;
                }
                st.set_integration_busy(true);
                st.set_integration_status(tr!("Working…").into());
                let profile = s.profile.clone();
                let mut settings = s.settings.clone();
                let (id, what) = (id.to_owned(), what.to_owned());
                pagebara_thread::spawn("int-change", move || {
                    let mut int = crate::integration::open(&profile);
                    let r = if what == "remove-all" {
                        crate::integration::remove_all(&mut int, &profile, &mut settings)
                    } else {
                        match crate::integration::set(&mut int, &profile, &mut settings, &id, on) {
                            Err(pagebara_integration::Error::NeedsAdmin) => {
                                let cmd = if on { "enable" } else { "disable" };
                                crate::integration::elevated(cmd, &id)
                            }
                            r => r.map_err(|e| e.to_string()),
                        }
                    };
                    let rows = crate::integration::rows(&int, &settings.system);
                    post_act(move |app, s, ui| {
                        let st = ui.global::<AppState>();
                        st.set_integration_busy(false);
                        st.set_integration_status(match &r {
                            Ok(()) if what == "remove-all" => {
                                tr!("All system integration was removed.").into()
                            }
                            Ok(()) => "".into(),
                            Err(e) => e.as_str().into(),
                        });
                        if what == "remove-all" {
                            s.settings.system.report_recent = settings.system.report_recent;
                            s.settings.system.jump_list = settings.system.jump_list;
                            app.save_settings(s);
                            app.integration_changed(s, ui, "jump-list");
                        }
                        show_rows(ui, rows);
                    });
                });
            }
            _ => {}
        }
    }

    /// Applies a changed integration setting to the running app.
    fn integration_changed(self: &Rc<Self>, s: &mut State, ui: &AppWindow, id: &str) {
        match id {
            "jump-list" => {
                pagebara_native::set_dock_menu_enabled(s.settings.system.jump_list);
                update_jump_list(s);
            }
            "progress" if !s.settings.system.progress => {
                crate::jobs::progress(&s.profile, false, ui, None);
            }
            _ => {}
        }
    }

    /// A long job's progress outside the window (Settings > System integration).
    pub fn job_progress(&self, s: &State, ui: &AppWindow, value: Option<f64>) {
        crate::jobs::progress(&s.profile, s.settings.system.progress, ui, value);
    }

    /// A long job ended: a notification, when the window is in the background.
    pub fn job_done(&self, s: &State, ui: &AppWindow, title: &str, body: &str) {
        self.job_progress(s, ui, None);
        if s.profile.dir.is_some() || !crate::jobs::focused(ui) {
            crate::jobs::notify(&s.profile, s.settings.system.notifications, title, body);
        }
    }

    fn system_action(self: &Rc<Self>, s: &mut State, ui: &AppWindow, what: &str) {
        super::update::action(self, s, ui, what)
    }

    /// A tab dragged off the tab bar opens in a window of its own.
    fn tab_torn_off(self: &Rc<Self>, s: &mut State, ui: &AppWindow, i: usize) {
        if i == s.active {
            self.finish_editors(s, ui);
        }
        let Some(t) = s.tabs.get(i) else { return };
        let Some(path) = t.session.path.clone() else {
            return self.message(
                s,
                &tr!("Save the document first to open it in a new window."),
            );
        };
        if t.session.is_dirty() {
            return self.message(
                s,
                &tr!("Save your changes first to open the document in a new window."),
            );
        }
        let page = t.view.current_page;
        match crate::os::new_window(&path, page, s.profile.dir.as_deref()) {
            Ok(()) => self.close_tab(s, ui, i, true),
            Err(e) => {
                log::warn!("new window: {e}");
                self.message(s, &tr!("Could not open a new window."));
            }
        }
    }
}

fn show_rows(ui: &AppWindow, rows: Vec<crate::integration::Row>) {
    let mut last = None;
    let rows: Vec<IntegrationRow> = rows
        .into_iter()
        .filter(|r| {
            !cfg!(target_os = "macos")
                || pagebara_integration::catalog::unavailable(pagebara_integration::Os::MacOs, r.id)
                    .is_none()
        })
        .map(|r| {
            let group = if last == Some(r.group) {
                String::new()
            } else {
                last = Some(r.group);
                r.group.title().to_owned()
            };
            IntegrationRow {
                id: r.id.into(),
                title: r.title.into(),
                explain: r.explain.into(),
                status: if r.status == "On" || r.status == "Off" {
                    "".into()
                } else {
                    r.status.into()
                },
                on: r.on,
                check: r.kind != pagebara_integration::Kind::Button,
                toggle: r.toggle,
                button: r.button.into(),
                group: group.into(),
            }
        })
        .collect();
    ui.global::<AppState>()
        .set_integration_rows(slint::ModelRc::new(slint::VecModel::from(rows)));
}

/// The taskbar jump list (Windows) or Dock menu (macOS) from the recent files.
fn update_jump_list(s: &State) {
    if s.profile.dir.is_some() || cfg!(all(unix, not(target_os = "macos"))) {
        return;
    }
    if !s.settings.system.jump_list {
        return pagebara_native::clear_jump_list();
    }
    let recent: Vec<PathBuf> = s.recent.paths().to_vec();
    pagebara_thread::spawn("int-jumplist", move || {
        if let Err(e) = pagebara_native::set_jump_list(&recent, &tr!("New window")) {
            log::warn!("jump list: {e}");
        }
    });
}

/// The tab showing `path` (absolute).
fn tab_for(s: &State, path: &PathBuf) -> Option<usize> {
    s.tabs.iter().position(|t| {
        t.session
            .path
            .as_ref()
            .and_then(|p| std::path::absolute(p).ok())
            .as_ref()
            == Some(path)
    })
}
