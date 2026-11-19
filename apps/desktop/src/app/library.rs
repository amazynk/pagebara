//! The library screen: recent documents and folders the user added, searched with a
//! full-text index kept up to date in the background.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use pagebara_app_core::library::{self, Hit, Index, LibraryConfig};
use pagebara_i18n::{tr, trn};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use super::{App, State, post_act};
use crate::{AppWindow, LibraryDoc, LibraryHit, LibraryState};

const MAX_HITS: usize = 200;
/// Progress is shown at most this often.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

#[derive(Debug, Default)]
pub struct Library {
    /// Where the folder list and the index are kept.
    dir: Option<PathBuf>,
    config: LibraryConfig,
    index: Arc<Index>,
    loaded: bool,
    /// The running update's cancel flag.
    indexing: Option<Arc<AtomicBool>>,
    /// Bumped by each update; only the latest one's result is kept.
    update_serial: u64,
    docs: Vec<PathBuf>,
    pub hits: Vec<Hit>,
    search_serial: u64,
    /// Open this page once the document has opened.
    pending: Option<(PathBuf, usize)>,
}

impl Library {
    pub fn new(profile: &crate::profile::Profile) -> Self {
        // A `--profile` folder holds everything; else the per-user cache directory.
        let dir = if profile.recent_path.is_some() {
            profile
                .settings_path
                .as_ref()
                .and_then(|p| p.parent())
                .map(|d| d.join("library"))
        } else {
            library::default_dir()
        };
        Library {
            config: dir.as_deref().map(LibraryConfig::load).unwrap_or_default(),
            dir,
            ..Library::default()
        }
    }

    pub fn is_indexing(&self) -> bool {
        self.indexing.is_some()
    }

    pub fn doc_count(&self) -> usize {
        self.docs.len()
    }
}

pub fn wire(app: &Rc<App>, ui: &AppWindow) {
    let ls = ui.global::<LibraryState>();
    macro_rules! on {
        ($cb:ident, |$app:ident, $s:ident, $ui:ident $(, $arg:ident)*| $body:expr) => {{
            let a = app.clone();
            ls.$cb(move |$($arg),*| {
                a.act(move |$app, $s, $ui| {
                    let _ = (&$app, &$ui);
                    $body
                })
            });
        }};
    }
    on!(on_show, |app, s, ui, v| show(app, s, ui, v));
    on!(on_add_folder, |app, _s, _ui| pick_folder(app));
    on!(on_remove_folder, |app, s, ui, i| {
        if let Some(f) = s.library.config.folders.get(i.max(0) as usize).cloned() {
            s.library.config.remove(&f);
            save_config(s);
            update(app, s, ui);
        }
    });
    on!(on_search, |app, s, ui, text| search(app, s, ui, &text));
    on!(on_open_doc, |app, s, _ui, i| {
        if let Some(p) = s.library.docs.get(i.max(0) as usize).cloned() {
            open_at(app, s, p, None);
        }
    });
    on!(on_open_hit, |app, s, _ui, i| open_hit(
        app,
        s,
        i.max(0) as usize
    ));
    on!(on_reindex, |app, s, ui| update(app, s, ui));
    on!(on_cancel, |_app, s, _ui| {
        if let Some(c) = &s.library.indexing {
            c.store(true, Ordering::Relaxed);
        }
    });
}

/// Keys while the library is shown: Esc closes it; Ctrl+L toggles it anywhere.
pub fn shortcut(app: &Rc<App>, s: &mut State, ui: &AppWindow, c: char, ctrl: bool) -> Option<bool> {
    use slint::platform::Key;
    let visible = ui.global::<LibraryState>().get_visible();
    if ctrl && c.eq_ignore_ascii_case(&'l') {
        show(app, s, ui, !visible);
        return Some(true);
    }
    if !visible {
        return None;
    }
    if c == char::from(Key::Escape) {
        show(app, s, ui, false);
        return Some(true);
    }
    // Opening files still works; other shortcuts are for the document view.
    if ctrl && c.eq_ignore_ascii_case(&'o') {
        app.pick_file();
        return Some(true);
    }
    Some(false)
}

pub fn show(app: &Rc<App>, s: &mut State, ui: &AppWindow, visible: bool) {
    let ls = ui.global::<LibraryState>();
    ls.set_visible(visible);
    if !visible {
        ui.invoke_focus_view();
        return;
    }
    show_folders(s, ui);
    update(app, s, ui);
}

fn show_folders(s: &State, ui: &AppWindow) {
    let rows: Vec<SharedString> = s
        .library
        .config
        .folders
        .iter()
        .map(|f| f.to_string_lossy().as_ref().into())
        .collect();
    ui.global::<LibraryState>()
        .set_folders(ModelRc::new(VecModel::from(rows)));
}

fn save_config(s: &State) {
    if let Some(dir) = &s.library.dir
        && let Err(e) = s.library.config.save(dir)
    {
        log::warn!("could not save the library folders: {e}");
    }
}

pub fn add_folder(app: &Rc<App>, s: &mut State, ui: &AppWindow, folder: &Path) {
    if s.library.config.add(folder) {
        save_config(s);
    }
    show_folders(s, ui);
    update(app, s, ui);
}

fn pick_folder(app: &Rc<App>) {
    let app = app.clone();
    let task = slint::spawn_local(async move {
        let picked = rfd::AsyncFileDialog::new()
            .set_title(tr!("Add a folder to the library"))
            .pick_folder()
            .await;
        if let Some(f) = picked {
            let path = f.path().to_owned();
            app.act(move |app, s, ui| add_folder(app, s, ui, &path));
        }
    });
    if let Err(e) = task {
        log::warn!("cannot show the folder dialog: {e}");
    }
}

/// The documents: recent ones first, then those in the folders.
fn known_docs(s: &State) -> Vec<PathBuf> {
    let mut docs: Vec<PathBuf> = s.recent.paths().to_vec();
    for p in s.library.index.files.keys() {
        if !docs.contains(p) {
            docs.push(p.clone());
        }
    }
    docs
}

fn show_docs(s: &mut State, ui: &AppWindow) {
    s.library.docs = known_docs(s);
    let rows: Vec<LibraryDoc> = s
        .library
        .docs
        .iter()
        .map(|p| {
            let pages = match s.library.index.files.get(p) {
                // TRANSLATORS: in the library list, a password-protected document
                Some(f) if f.error.is_some() => tr!("locked"),
                Some(f) => trn!("{n} page", "{n} pages", f.pages.len()),
                None => String::new(),
            };
            LibraryDoc {
                name: p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
                    .into(),
                folder: p
                    .parent()
                    .map(|d| d.to_string_lossy().into_owned())
                    .unwrap_or_default()
                    .into(),
                pages: pages.into(),
            }
        })
        .collect();
    ui.global::<LibraryState>()
        .set_docs(ModelRc::new(VecModel::from(rows)));
}

/// Brings the index up to date in the background (restarting a running update).
pub fn update(_app: &Rc<App>, s: &mut State, ui: &AppWindow) {
    if let Some(c) = s.library.indexing.take() {
        c.store(true, Ordering::Relaxed);
    }
    show_docs(s, ui);
    let cancel = Arc::new(AtomicBool::new(false));
    s.library.indexing = Some(cancel.clone());
    s.library.update_serial += 1;
    let serial = s.library.update_serial;
    let ls = ui.global::<LibraryState>();
    ls.set_indexing(true);
    ls.set_progress(0.0);
    ls.set_status(tr!("Looking for documents…").into());
    let folders = s.library.config.folders.clone();
    let recent = s.recent.paths().to_vec();
    let dir = s.library.dir.clone();
    let loaded = s.library.loaded;
    let mut index = (*s.library.index).clone();
    pagebara_thread::spawn("library", move || {
        if !loaded && let Some(d) = &dir {
            index = Index::load(d);
        }
        let files = library::collect(&folders, &recent);
        let mut last = Instant::now();
        let stats = index.update(&files, &cancel, |done, total| {
            if last.elapsed() >= PROGRESS_EVERY {
                last = Instant::now();
                post_act(move |_app, _s, ui| {
                    let ls = ui.global::<LibraryState>();
                    ls.set_progress(done as f32 / total.max(1) as f32);
                    ls.set_status(tr!("Indexing {} of {} documents…", done, total).into());
                });
            }
        });
        if let Some(d) = &dir
            && let Err(e) = index.save(d)
        {
            log::warn!("could not save the library index: {e}");
        }
        let count = index.files.len();
        let index = Arc::new(index);
        post_act(move |app, s, ui| {
            // A newer update replaced this one.
            if s.library.update_serial != serial {
                return;
            }
            s.library.indexing = None;
            s.library.index = index;
            s.library.loaded = true;
            let ls = ui.global::<LibraryState>();
            ls.set_indexing(false);
            ls.set_progress(1.0);
            let status = if stats.cancelled {
                trn!(
                    "Indexing stopped · {n} document indexed ({} new or changed)",
                    "Indexing stopped · {n} documents indexed ({} new or changed)",
                    count,
                    stats.read
                )
            } else {
                trn!(
                    "Up to date · {n} document indexed ({} new or changed)",
                    "Up to date · {n} documents indexed ({} new or changed)",
                    count,
                    stats.read
                )
            };
            ls.set_status(status.into());
            show_docs(s, ui);
            let q = ls.get_query().to_string();
            if !q.trim().is_empty() {
                search(app, s, ui, &q);
            }
        });
    });
}

pub fn search(_app: &Rc<App>, s: &mut State, ui: &AppWindow, text: &str) {
    s.library.search_serial += 1;
    let serial = s.library.search_serial;
    let ls = ui.global::<LibraryState>();
    if ls.get_query() != text {
        ls.set_query(text.into());
    }
    if text.trim().is_empty() {
        s.library.hits.clear();
        ls.set_hits(ModelRc::default());
        return;
    }
    let index = s.library.index.clone();
    let query = text.to_owned();
    pagebara_thread::spawn("library-find", move || {
        let hits = index.search(&query, MAX_HITS);
        post_act(move |_app, s, ui| {
            if s.library.search_serial != serial {
                return;
            }
            let rows: Vec<LibraryHit> = hits
                .iter()
                .map(|h| LibraryHit {
                    name: h
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                        .into(),
                    page: h.page as i32,
                    label: (h.page + 1).to_string().into(),
                    snippet: h.snippet.as_str().into(),
                })
                .collect();
            s.library.hits = hits;
            ui.global::<LibraryState>()
                .set_hits(ModelRc::new(VecModel::from(rows)));
        });
    });
}

pub fn open_hit(app: &Rc<App>, s: &mut State, i: usize) {
    if let Some(h) = s.library.hits.get(i).cloned() {
        open_at(app, s, h.path, Some(h.page));
    }
}

/// Opens `path` (or shows it, if open) at `page`, and closes the library.
fn open_at(app: &Rc<App>, s: &mut State, path: PathBuf, page: Option<usize>) {
    let Some(ui) = app.ui.upgrade() else { return };
    ui.global::<LibraryState>().set_visible(false);
    ui.invoke_focus_view();
    let abs = std::path::absolute(&path).ok();
    let open = s.tabs.iter().position(|t| {
        t.session
            .path
            .as_ref()
            .and_then(|p| std::path::absolute(p).ok())
            == abs
    });
    match open {
        Some(i) => {
            app.select_tab(s, &ui, i);
            if let Some(p) = page {
                app.jump(s, &ui, p, None);
            }
        }
        None => {
            s.library.pending = page.map(|p| (path.clone(), p));
            app.open_path(path);
        }
    }
}

/// After a document opened: goes to the page a library hit asked for.
pub fn opened(_app: &Rc<App>, s: &mut State, _ui: &AppWindow) {
    let Some((path, page)) = s.library.pending.take() else {
        return;
    };
    let Some(t) = s.tabs.get_mut(s.active) else {
        return;
    };
    let same = t
        .session
        .path
        .as_ref()
        .and_then(|p| std::path::absolute(p).ok())
        == std::path::absolute(&path).ok();
    if same {
        t.view.view.viewport.go_to_page(page);
    } else {
        s.library.pending = Some((path, page));
    }
}
