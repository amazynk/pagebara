//! Native OS calls for desktop integration, behind one safe API. Each call is a no-op (or
//! answers "unsupported") on platforms without the feature.
//!
//! - macOS (`macos.rs`): files opened from Finder (`application:openURLs:`), the Dock menu
//!   (New Window and recent files), the app's recent documents (`NSDocumentController`) and
//!   the default app for a content type (`NSWorkspace`, macOS 12+, confirmed by the user).
//! - Windows (`windows.rs`): `SHAddToRecentDocs`, the taskbar jump list
//!   (`ICustomDestinationList`), taskbar progress (`ITaskbarList3`), the process's
//!   AppUserModelID and `SHChangeNotify` after association changes.
//!
//! Only those two modules allow `unsafe`; the menu model and the URL filtering are here.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

mod dock;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::app_menu::{AppMenuCommand, install_app_menu, perform_app_menu_item};
#[cfg(target_os = "macos")]
pub use macos::menu_bar::{
    BarCallbacks, BarItem, BarMenu, BarRole, WindowCommand, install_menu_bar, menu_bar_titles,
    retitle_menu_bar,
};
#[cfg(windows)]
mod windows;

/// Most recent files shown in the jump list or the Dock menu.
pub const MAX_RECENT: usize = 10;

/// Adds `path` to the system's recent documents (Windows: the shell's list, Start and
/// jump lists; macOS: the app's Open Recent list and its Dock menu). Any thread.
pub fn note_recent(path: &Path) {
    #[cfg(windows)]
    windows::note_recent(path);
    #[cfg(target_os = "macos")]
    macos::note_recent(path);
    let _ = path;
}

/// Empties the app's own recent documents. `true` where that list is the app's alone
/// (macOS); Windows' list is shared by all apps and never cleared.
pub fn clear_recent() -> bool {
    #[cfg(target_os = "macos")]
    return macos::clear_recent();
    #[cfg(not(target_os = "macos"))]
    false
}

/// Windows: the AppUserModelID that groups the windows and the jump list of this app.
/// Call once at startup, before a window shows.
pub fn set_app_user_model_id(id: &str) {
    #[cfg(windows)]
    windows::set_app_user_model_id(id);
    let _ = id;
}

/// The taskbar jump list (Windows: a "New window" task and a "Recent" category) or the
/// Dock menu's recent files (macOS). Files that do not exist are left out.
pub fn set_jump_list(recent: &[PathBuf], new_window_label: &str) -> Result<(), String> {
    let items = recent_items(recent, |p| p.is_file());
    *lock(&DOCK.recent) = items.clone();
    *lock(&DOCK.new_window_label) = new_window_label.to_owned();
    #[cfg(windows)]
    return windows::set_jump_list(&items, new_window_label);
    #[cfg(not(windows))]
    Ok(())
}

/// Removes the jump list (Windows) or empties the Dock menu's recent files (macOS).
pub fn clear_jump_list() {
    lock(&DOCK.recent).clear();
    #[cfg(windows)]
    windows::clear_jump_list();
}

/// Refreshes Finder Services on macOS and shell associations on Windows.
pub fn shell_assoc_changed() {
    #[cfg(target_os = "macos")]
    objc2_app_kit::NSUpdateDynamicServices();
    #[cfg(windows)]
    windows::shell_assoc_changed();
}

/// Windows: progress on the window's taskbar button (`value` 0.0–1.0; `None` hides it).
/// `hwnd` is the window's handle. Call from the window's thread.
pub fn taskbar_progress(hwnd: isize, value: Option<f64>) {
    #[cfg(windows)]
    windows::taskbar_progress(hwnd, value);
    let _ = (hwnd, value);
}

/// macOS: files opened from Finder (or dropped on the Dock icon) go to `on_open`, also on a
/// cold start; the Dock menu offers New Window (`on_new_window`) and the recent files
/// given to [`set_jump_list`] when `dock_menu` is on. Call on the main thread after the
/// event loop exists (after the first Slint window is made) and before it runs.
pub fn install_open_handler(
    on_open: Box<dyn Fn(Vec<PathBuf>)>,
    on_new_window: Box<dyn Fn()>,
    dock_menu: bool,
) {
    set_dock_menu_enabled(dock_menu);
    #[cfg(target_os = "macos")]
    macos::install_open_handler(on_open, on_new_window);
    #[cfg(not(target_os = "macos"))]
    drop((on_open, on_new_window));
}

/// macOS: shows or hides the app's items in the Dock menu.
pub fn set_dock_menu_enabled(on: bool) {
    DOCK.enabled.store(on, Ordering::Relaxed);
}

/// macOS: the app that opens content of type `uti` (such as `com.adobe.pdf`).
pub fn default_app_for_type(uti: &str) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    return macos::default_app_for_type(uti);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = uti;
        None
    }
}

/// macOS 12+: makes `app` (an .app bundle) the default for `uti`; the system asks the user
/// to confirm. `done` runs on some thread once that is settled.
pub fn set_default_app_for_type(
    uti: &str,
    app: &Path,
    done: Box<dyn FnOnce(Result<(), String>) + Send>,
) {
    #[cfg(target_os = "macos")]
    macos::set_default_app_for_type(uti, app, done);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (uti, app);
        done(Err("unsupported on this system".into()));
    }
}

/// macOS: the app bundle this process runs from (`None` outside an .app).
pub fn bundle_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    return macos::bundle_path();
    #[cfg(not(target_os = "macos"))]
    None
}

/// macOS: centres the close, minimise and zoom buttons of the app's windows with a
/// transparent title bar in a bar `height` points tall at the top of the window, the first
/// one `inset` points in. For a window whose title bar is merged into the app's own bar;
/// call it again when the window changes size or title. Main thread; a no-op elsewhere.
pub fn place_window_buttons(height: f64, inset: f64) {
    #[cfg(target_os = "macos")]
    macos::place_window_buttons(height, inset);
    let _ = (height, inset);
}

/// The Dock menu's state, shared with the main thread's menu callback.
struct Dock {
    enabled: AtomicBool,
    recent: Mutex<Vec<PathBuf>>,
    new_window_label: Mutex<String>,
}

static DOCK: Dock = Dock {
    enabled: AtomicBool::new(false),
    recent: Mutex::new(Vec::new()),
    new_window_label: Mutex::new(String::new()),
};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A file's title in menus: its name.
pub(crate) fn title(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string_lossy().into_owned())
}

/// `recent` without duplicates or missing files, most recent first, at most
/// [`MAX_RECENT`].
fn recent_items(recent: &[PathBuf], exists: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for p in recent {
        if out.len() == MAX_RECENT {
            break;
        }
        if !out.contains(p) && exists(p) {
            out.push(p.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dock::{MenuItem, dock_menu, file_paths, menu_items};

    #[test]
    fn recent_items_skip_duplicates_and_missing_files() {
        let p = |s: &str| PathBuf::from(s);
        let list = [p("/a.pdf"), p("/gone.pdf"), p("/b.pdf"), p("/a.pdf")];
        let got = recent_items(&list, |x| x != Path::new("/gone.pdf"));
        assert_eq!(got, [p("/a.pdf"), p("/b.pdf")]);
        let many: Vec<_> = (0..20).map(|i| p(&format!("/{i}.pdf"))).collect();
        assert_eq!(recent_items(&many, |_| true).len(), MAX_RECENT);
    }

    #[test]
    fn menu_has_new_window_then_recent_files() {
        let items = menu_items("", &[PathBuf::from("/docs/lease.pdf")]);
        assert_eq!(
            items,
            [
                MenuItem::NewWindow("New Window".into()),
                MenuItem::Separator,
                MenuItem::Recent("lease.pdf".into(), 0),
            ]
        );
        assert_eq!(menu_items("Neues Fenster", &[]).len(), 1);
    }

    #[test]
    fn dock_menu_follows_the_setting() {
        set_dock_menu_enabled(false);
        assert!(dock_menu().is_empty());
        set_dock_menu_enabled(true);
        assert!(matches!(dock_menu().first(), Some(MenuItem::NewWindow(_))));
        set_dock_menu_enabled(false);
    }

    #[test]
    fn only_file_urls_open() {
        let got = file_paths([
            (true, Some("/a b.pdf".to_owned())),
            (false, Some("/x".to_owned())),
            (true, None),
            (true, Some(String::new())),
        ]);
        assert_eq!(got, [PathBuf::from("/a b.pdf")]);
    }

    #[test]
    fn unsupported_calls_answer() {
        if cfg!(any(windows, target_os = "macos")) {
            return;
        }
        assert!(!clear_recent());
        assert!(bundle_path().is_none());
        assert!(default_app_for_type("com.adobe.pdf").is_none());
        let (tx, rx) = std::sync::mpsc::channel();
        set_default_app_for_type(
            "com.adobe.pdf",
            Path::new("/x.app"),
            Box::new(move |r| tx.send(r).unwrap()),
        );
        assert!(rx.recv().unwrap().is_err());
        assert!(set_jump_list(&[], "New window").is_ok());
    }
}
