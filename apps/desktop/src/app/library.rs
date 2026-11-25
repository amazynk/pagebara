//! The library screen: recent documents and folders the user added, searched by name,
//! title and text with an index kept up to date in the background.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use pagebara_app_core::library::{self, Found, Index, LibraryConfig, Thumbnail};
use pagebara_i18n::{tr, trn};
use slint::{ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};

use super::{App, State, post_act};
use crate::{AppWindow, LibraryFolder, LibraryRow, LibraryState};

/// At most this many documents for each kind of match.
const MAX_FOUND: usize = 500;
/// Progress is shown at most this often.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);
/// While indexing, the documents read so far are listed and searched this often.
const SNAPSHOT_EVERY: Duration = Duration::from_secs(2);

/// Pictures of first pages are this many pixels on their longer side (sharp on 2x screens).
const THUMB_PX: u32 = 104;
/// At most this many pictures are kept in memory; the others are read again from the disk.
const MAX_THUMBS: usize = 600;
/// Threads making pictures.
const THUMB_THREADS: usize = 3;

/// What a row of the list opens: a document, at a page.
type Target = (PathBuf, Option<usize>);

/// The files to make pictures of, in order, and those being made.
#[derive(Debug, Default)]
struct ThumbQueue {
    wanted: Vec<PathBuf>,
    busy: HashSet<PathBuf>,
}

/// Pictures of the documents' first pages, made on a few threads for the rows in view.
#[derive(Debug, Default)]
struct Thumbs {
    /// Made ones, with when they were last shown.
    made: HashMap<PathBuf, (Image, u64)>,
    tick: u64,
    /// Files without a picture (damaged).
    failed: HashSet<PathBuf>,
    queue: Arc<(Mutex<ThumbQueue>, Condvar)>,
    started: bool,
    /// The rows in view, and a few around them.
    shown: (usize, usize),
}

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
    /// What each row of the list opens (none for headings).
    targets: Vec<Option<Target>>,
    /// The documents matching the last search.
    pub found: Vec<Found>,
    search_serial: u64,
    /// Stops the running search, when another one starts.
    searching: Option<Arc<AtomicBool>>,
    /// Enter was pressed while searching: open the first match once found.
    open_found: bool,
    /// Open this page once the document has opened, and find the text there.
    pending: Option<(PathBuf, usize, String)>,
    thumbs: Thumbs,
    /// Files read again by the last update, for scripts.
    pub changed: usize,
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

    /// How many pictures of first pages are made, for scripts.
    pub fn thumbnails(&self) -> usize {
        self.thumbs.made.len()
    }

    /// How many documents the list shows without a search: the recent ones and those in
    /// the folders.
    pub fn doc_count(&self, recent: &[PathBuf]) -> usize {
        let folders = &self.config.folders;
        let inside = |p: &PathBuf| folders.iter().any(|f| p.starts_with(f));
        let indexed = self.index.files.keys().filter(|p| inside(p)).count();
        indexed
            + recent
                .iter()
                .filter(|p| !inside(p) || !self.index.files.contains_key(*p))
                .count()
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
    on!(on_open_row, |app, s, ui, i| open_row(
        app,
        s,
        ui,
        i.max(0) as usize
    ));
    on!(on_accept, |app, s, ui| accept(app, s, ui));
    on!(on_reindex, |app, s, ui| update(app, s, ui));
    on!(on_cancel, |_app, s, _ui| {
        if let Some(c) = &s.library.indexing {
            c.store(true, Ordering::Relaxed);
        }
    });
    on!(on_rows_shown, |_app, s, ui, first, end| {
        s.library.thumbs.shown = (first.max(0) as usize, end.max(0) as usize);
        show_thumbs(s, ui);
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
    let files = &s.library.index.files;
    let rows: Vec<LibraryFolder> = s
        .library
        .config
        .folders
        .iter()
        .map(|f| {
            let docs = files
                .range(f.clone()..)
                .take_while(|(p, _)| p.starts_with(f))
                .count();
            LibraryFolder {
                name: f
                    .file_name()
                    .map_or_else(|| f.to_string_lossy(), |n| n.to_string_lossy())
                    .as_ref()
                    .into(),
                path: short(f.parent().unwrap_or(f)).into(),
                docs: trn!("{n} document", "{n} documents", docs).into(),
            }
        })
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
        let picked = super::pick::dialog(&app)
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

/// A folder, with the home folder as ~ on Linux and macOS.
fn short(dir: &Path) -> String {
    #[cfg(unix)]
    if let Some(home) = std::env::home_dir()
        && let Ok(rest) = dir.strip_prefix(&home)
    {
        return Path::new("~").join(rest).to_string_lossy().into_owned();
    }
    dir.to_string_lossy().into_owned()
}

/// Where a document is: in a library folder, the folders below it (with the library
/// folder's name first when there are several); else its folder.
fn place(path: &Path, folders: &[PathBuf]) -> String {
    let parent = path.parent().unwrap_or(Path::new(""));
    let Some(f) = folders.iter().find(|f| path.starts_with(f)) else {
        return short(parent);
    };
    let below = parent.strip_prefix(f).unwrap_or(Path::new(""));
    if folders.len() == 1 {
        return below.to_string_lossy().into_owned();
    }
    let name = f
        .file_name()
        .map_or_else(|| f.to_string_lossy(), |n| n.to_string_lossy());
    Path::new(name.as_ref())
        .join(below)
        .to_string_lossy()
        .trim_end_matches(std::path::MAIN_SEPARATOR)
        .to_owned()
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn heading(name: String, side: String) -> LibraryRow {
    LibraryRow {
        kind: 0,
        label: name.clone().into(),
        name: name.into(),
        side: side.into(),
        ..LibraryRow::default()
    }
}

fn locked(s: &State, path: &Path) -> bool {
    s.library
        .index
        .files
        .get(path)
        .is_some_and(|f| f.error.is_some())
}

/// A document: its name without `.pdf`, where it is and its pages.
fn doc_row(s: &State, path: &Path) -> LibraryRow {
    let pages = match s.library.index.files.get(path) {
        // TRANSLATORS: in the library list, a password-protected document
        Some(f) if f.error.is_some() => tr!("locked"),
        Some(f) => trn!("{n} page", "{n} pages", f.pages.len()),
        None => String::new(),
    };
    let place = place(path, &s.library.config.folders);
    let detail = [place, pages]
        .into_iter()
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    LibraryRow {
        kind: 1,
        label: tr!("Open {}", file_name(path)).into(),
        name: library::stem(path).into(),
        detail: detail.into(),
        locked: locked(s, path),
        ..LibraryRow::default()
    }
}

/// A document found: with the text around the match, the match's page and how many pages
/// match.
fn found_row(s: &State, f: &Found) -> LibraryRow {
    let Some(page) = f.page else {
        return doc_row(s, &f.path);
    };
    let (before, rest) = f.snippet.split_at(f.mark.start);
    let (matched, after) = rest.split_at(f.mark.len());
    let more = if f.pages > 1 {
        trn!("{n} page matches", "{n} pages match", f.pages)
    } else {
        String::new()
    };
    LibraryRow {
        kind: 2,
        label: tr!("Open {} at page {}", file_name(&f.path), page + 1).into(),
        name: library::stem(&f.path).into(),
        detail: before.into(),
        matched: matched.into(),
        after: after.into(),
        side: tr!("page {}", page + 1).into(),
        side2: more.into(),
        locked: locked(s, &f.path),
        ..LibraryRow::default()
    }
}

/// Adds a heading and its rows, as one card.
fn push_group(
    rows: &mut Vec<LibraryRow>,
    targets: &mut Vec<Option<Target>>,
    head: LibraryRow,
    items: Vec<(LibraryRow, Target)>,
) {
    if items.is_empty() {
        return;
    }
    rows.push(head);
    targets.push(None);
    let n = items.len();
    for (i, (mut r, t)) in items.into_iter().enumerate() {
        r.first = i == 0;
        r.last = i + 1 == n;
        rows.push(r);
        targets.push(Some(t));
    }
}

/// A group's count; `+` when the search stopped at the most it lists.
fn count(n: usize) -> String {
    if n >= MAX_FOUND {
        format!("{n}+")
    } else {
        n.to_string()
    }
}

/// Fills the list: without a search, the recent documents and those in the folders (by
/// name); with one, the documents found.
fn show_list(s: &mut State, ui: &AppWindow) {
    let ls = ui.global::<LibraryState>();
    let mut rows = Vec::new();
    let mut targets = Vec::new();
    // Counts are left out while indexing, as they keep growing.
    let counting = s.library.indexing.is_none();
    if ls.get_query().trim().is_empty() {
        let recent: Vec<_> = s
            .recent
            .paths()
            .iter()
            .map(|p| (doc_row(s, p), (p.clone(), None)))
            .collect();
        push_group(
            &mut rows,
            &mut targets,
            heading(tr!("Recent"), String::new()),
            recent,
        );
        let folders = &s.library.config.folders;
        let mut inside: Vec<(String, &PathBuf)> = s
            .library
            .index
            .files
            .keys()
            .filter(|p| folders.iter().any(|f| p.starts_with(f)))
            .map(|p| (file_name(p).to_lowercase(), p))
            .collect();
        inside.sort();
        let n = if counting {
            inside.len().to_string()
        } else {
            String::new()
        };
        let inside: Vec<_> = inside
            .into_iter()
            .map(|(_, p)| (doc_row(s, p), (p.clone(), None)))
            .collect();
        push_group(
            &mut rows,
            &mut targets,
            heading(tr!("In your folders"), n),
            inside,
        );
    } else {
        let (named, text): (Vec<&Found>, Vec<&Found>) =
            s.library.found.iter().partition(|f| f.named);
        let group = |list: &[&Found]| -> Vec<(LibraryRow, Target)> {
            list.iter()
                .map(|f| (found_row(s, f), (f.path.clone(), f.page)))
                .collect()
        };
        let (n, t) = (count(named.len()), count(text.len()));
        let (named, text) = (group(&named), group(&text));
        push_group(
            &mut rows,
            &mut targets,
            heading(tr!("Name or title"), n),
            named,
        );
        push_group(
            &mut rows,
            &mut targets,
            heading(tr!("In the text"), t),
            text,
        );
    }
    if !rows.is_empty() {
        // Room under the last row.
        rows.push(LibraryRow {
            kind: 3,
            ..LibraryRow::default()
        });
        targets.push(None);
    }
    // The current row stays on its document (the list is filled again as indexing goes
    // on), else it is the first document.
    let first = rows.iter().position(|r| r.kind == 1 || r.kind == 2);
    let old = usize::try_from(ls.get_current()).ok();
    let was = old.and_then(|i| s.library.targets.get(i).cloned().flatten());
    let current = was
        .and_then(|w| match old {
            Some(i) if targets.get(i).is_some_and(|t| t.as_ref() == Some(&w)) => Some(i),
            _ => targets.iter().position(|t| t.as_ref() == Some(&w)),
        })
        .or(first);
    s.library.targets = targets;
    ls.set_rows(ModelRc::new(VecModel::from(rows)));
    ls.set_current(current.map_or(-1, |i| i as i32));
    show_thumbs(s, ui);
}

/// Hands the rows in view their pictures, and asks the threads for the missing ones.
fn show_thumbs(s: &mut State, ui: &AppWindow) {
    let (first, end) = s.library.thumbs.shown;
    let end = end.min(s.library.targets.len());
    let first = first.min(end);
    let mut images = Vec::with_capacity(end - first);
    let mut wanted = Vec::new();
    let t = &mut s.library.thumbs;
    t.tick += 1;
    for target in &s.library.targets[first..end] {
        let Some((path, _)) = target else {
            images.push(Image::default());
            continue;
        };
        match t.made.get_mut(path) {
            Some((image, used)) => {
                *used = t.tick;
                images.push(image.clone());
            }
            None => {
                let locked = s
                    .library
                    .index
                    .files
                    .get(path)
                    .is_some_and(|f| f.error.is_some());
                if !locked && !t.failed.contains(path) && !wanted.contains(path) {
                    wanted.push(path.clone());
                }
                images.push(Image::default());
            }
        }
    }
    let ls = ui.global::<LibraryState>();
    ls.set_thumbs(ModelRc::new(VecModel::from(images)));
    ls.set_thumbs_start(first as i32);
    {
        let (queue, wake) = &*t.queue;
        let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
        q.wanted = wanted.into_iter().filter(|p| !q.busy.contains(p)).collect();
        wake.notify_all();
    }
    if !t.started {
        t.started = true;
        let cache = s.library.dir.as_ref().map(|d| d.join("thumbnails"));
        for _ in 0..THUMB_THREADS {
            let queue = t.queue.clone();
            let cache = cache.clone();
            pagebara_thread::spawn("library-thumbs", move || make_thumbs(&queue, cache));
        }
    }
}

/// A picture-making thread: makes the wanted pictures, first in view first, and waits for
/// more.
fn make_thumbs(queue: &(Mutex<ThumbQueue>, Condvar), cache: Option<PathBuf>) {
    let (lock, wake) = queue;
    loop {
        let path = {
            let mut q = lock.lock().unwrap_or_else(|e| e.into_inner());
            while q.wanted.is_empty() {
                q = wake.wait(q).unwrap_or_else(|e| e.into_inner());
            }
            let path = q.wanted.remove(0);
            q.busy.insert(path.clone());
            path
        };
        let thumb = library::thumbnail(&path, THUMB_PX, cache.as_deref());
        lock.lock()
            .unwrap_or_else(|e| e.into_inner())
            .busy
            .remove(&path);
        post_act(move |_app, s, ui| thumb_made(s, ui, path, thumb));
    }
}

fn thumb_made(s: &mut State, ui: &AppWindow, path: PathBuf, thumb: Option<Thumbnail>) {
    let t = &mut s.library.thumbs;
    match thumb {
        Some(th) => {
            let pixels =
                SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&th.rgba, th.width, th.height);
            t.made
                .insert(path.clone(), (Image::from_rgba8(pixels), t.tick));
            // Forget the pictures shown longest ago.
            if t.made.len() > MAX_THUMBS {
                let mut by_use: Vec<(u64, PathBuf)> =
                    t.made.iter().map(|(p, (_, u))| (*u, p.clone())).collect();
                by_use.sort();
                for (_, p) in by_use.into_iter().take(MAX_THUMBS / 4) {
                    t.made.remove(&p);
                }
            }
        }
        None => {
            t.failed.insert(path.clone());
        }
    }
    let (first, end) = t.shown;
    let in_view = s
        .library
        .targets
        .get(first..end.min(s.library.targets.len()))
        .is_some_and(|rows| rows.iter().flatten().any(|(p, _)| *p == path));
    if in_view {
        show_thumbs(s, ui);
    }
}

/// Shows the top of the list, for new search results.
fn to_top(ui: &AppWindow) {
    let ls = ui.global::<LibraryState>();
    ls.set_serial(ls.get_serial() + 1);
}

/// Takes a newer index: lists and searches it.
fn take_index(app: &Rc<App>, s: &mut State, ui: &AppWindow, index: Arc<Index>) {
    s.library.index = index;
    s.library.loaded = true;
    show_folders(s, ui);
    show_list(s, ui);
    let q = ui.global::<LibraryState>().get_query().to_string();
    if !q.trim().is_empty() {
        search(app, s, ui, &q);
    }
}

/// Brings the index up to date in the background (restarting a running update).
pub fn update(_app: &Rc<App>, s: &mut State, ui: &AppWindow) {
    if let Some(c) = s.library.indexing.take() {
        c.store(true, Ordering::Relaxed);
    }
    show_list(s, ui);
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
    // The files' entries are shared, so this copy is cheap.
    let base = s.library.index.clone();
    pagebara_thread::spawn("library", move || {
        let mut index = if loaded {
            (*base).clone()
        } else {
            let index = dir.as_deref().map(Index::load).unwrap_or_default();
            // What the saved index knows is listed while the files are checked.
            let saved = Arc::new(index.clone());
            post_act(move |app, s, ui| {
                if !s.library.loaded {
                    take_index(app, s, ui, saved);
                }
            });
            index
        };
        let files = library::collect(&folders, &recent);
        let mut last = Instant::now();
        let mut shown = Instant::now();
        let stats = index.update(&files, &cancel, |done, total, index| {
            if last.elapsed() >= PROGRESS_EVERY {
                last = Instant::now();
                let snapshot = (shown.elapsed() >= SNAPSHOT_EVERY).then(|| Arc::new(index.clone()));
                if snapshot.is_some() {
                    shown = Instant::now();
                }
                post_act(move |app, s, ui| {
                    if s.library.update_serial != serial {
                        return;
                    }
                    let ls = ui.global::<LibraryState>();
                    ls.set_progress(done as f32 / total.max(1) as f32);
                    ls.set_status(tr!("Indexing {} of {} documents…", done, total).into());
                    if let Some(index) = snapshot {
                        take_index(app, s, ui, index);
                    }
                });
            }
        });
        if (stats.read > 0 || stats.removed > 0)
            && let Some(d) = &dir
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
            s.library.changed = stats.read;
            let ls = ui.global::<LibraryState>();
            ls.set_indexing(false);
            ls.set_progress(1.0);
            let status = if stats.cancelled {
                trn!(
                    "{n} document · indexing stopped",
                    "{n} documents · indexing stopped",
                    count
                )
            } else {
                trn!(
                    "{n} document · up to date",
                    "{n} documents · up to date",
                    count
                )
            };
            ls.set_status(status.into());
            take_index(app, s, ui, index);
        });
    });
}

/// Finds `text` in the names, titles and text of the documents, in the background (a newer
/// search stops the running one).
pub fn search(_app: &Rc<App>, s: &mut State, ui: &AppWindow, text: &str) {
    s.library.search_serial += 1;
    let serial = s.library.search_serial;
    if let Some(stop) = s.library.searching.take() {
        stop.store(true, Ordering::Relaxed);
    }
    s.library.open_found = false;
    let ls = ui.global::<LibraryState>();
    if ls.get_query() != text {
        ls.set_query(text.into());
    }
    if text.trim().is_empty() {
        s.library.found.clear();
        ls.set_searching(false);
        ls.set_current(-1);
        show_list(s, ui);
        to_top(ui);
        return;
    }
    ls.set_searching(true);
    let stop = Arc::new(AtomicBool::new(false));
    s.library.searching = Some(stop.clone());
    let index = s.library.index.clone();
    let recent = s.recent.paths().to_vec();
    let query = text.to_owned();
    pagebara_thread::spawn("library-find", move || {
        let Some(found) = index.find(&query, &recent, MAX_FOUND, &stop) else {
            return;
        };
        post_act(move |app, s, ui| {
            if s.library.search_serial != serial {
                return;
            }
            s.library.searching = None;
            s.library.found = found;
            let ls = ui.global::<LibraryState>();
            ls.set_searching(false);
            // New results start at the first one.
            ls.set_current(-1);
            show_list(s, ui);
            to_top(ui);
            if std::mem::take(&mut s.library.open_found) {
                let first = ls.get_current();
                if first >= 0 {
                    open_row(app, s, ui, first as usize);
                }
            }
        });
    });
}

/// Enter in the search field: opens the current row (the first match), once found.
fn accept(app: &Rc<App>, s: &mut State, ui: &AppWindow) {
    let ls = ui.global::<LibraryState>();
    if ls.get_query().trim().is_empty() {
        return;
    }
    if s.library.searching.is_some() {
        s.library.open_found = true;
    } else if let Ok(i) = usize::try_from(ls.get_current()) {
        open_row(app, s, ui, i);
    }
}

/// Opens what row `i` of the list shows.
pub fn open_row(app: &Rc<App>, s: &mut State, ui: &AppWindow, i: usize) {
    let Some(Some((path, page))) = s.library.targets.get(i).cloned() else {
        return;
    };
    let query = if page.is_some() {
        ui.global::<LibraryState>().get_query().trim().to_owned()
    } else {
        String::new()
    };
    open_at(app, s, path, page, query);
}

/// Opens the `n`th document found (0-based), for scripts.
pub fn open_found(app: &Rc<App>, s: &mut State, ui: &AppWindow, n: usize) {
    let row = s
        .library
        .targets
        .iter()
        .enumerate()
        .filter(|(_, t)| t.is_some())
        .nth(n)
        .map(|(i, _)| i);
    if let Some(i) = row {
        open_row(app, s, ui, i);
    }
}

/// Opens `path` (or shows it, if open) at `page`, finds `query` there, and closes the
/// library.
fn open_at(app: &Rc<App>, s: &mut State, path: PathBuf, page: Option<usize>, query: String) {
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
                if !query.is_empty() {
                    app.find(s, &ui, &query);
                }
            }
        }
        None => {
            s.library.pending = page.map(|p| (path.clone(), p, query));
            app.open_path(path);
        }
    }
}

/// After a document opened: goes to the page a library match asked for, and finds the
/// text searched for there.
pub fn opened(app: &Rc<App>, s: &mut State, ui: &AppWindow) {
    let Some((path, page, query)) = s.library.pending.take() else {
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
        if !query.is_empty() {
            app.find(s, ui, &query);
        }
    } else {
        s.library.pending = Some((path, page, query));
    }
}
