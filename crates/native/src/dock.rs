//! The Dock menu's model and opened-URL filtering (used by `macos.rs`; plain logic, tested
//! everywhere).
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::path::PathBuf;
use std::sync::atomic::Ordering;

use crate::{DOCK, lock, title};

/// One entry of the Dock menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MenuItem {
    NewWindow(String),
    Separator,
    /// A recent file: its title and its index in the recent list.
    Recent(String, usize),
}

/// The Dock menu: nothing when turned off, else New Window and the recent files.
pub(crate) fn dock_menu() -> Vec<MenuItem> {
    if !DOCK.enabled.load(Ordering::Relaxed) {
        return Vec::new();
    }
    let label = lock(&DOCK.new_window_label).clone();
    menu_items(&label, &lock(&DOCK.recent))
}

pub(crate) fn menu_items(label: &str, recent: &[PathBuf]) -> Vec<MenuItem> {
    let label = if label.is_empty() {
        "New Window"
    } else {
        label
    };
    let mut items = vec![MenuItem::NewWindow(label.to_owned())];
    if !recent.is_empty() {
        items.push(MenuItem::Separator);
    }
    items.extend(
        recent
            .iter()
            .enumerate()
            .map(|(i, p)| MenuItem::Recent(title(p), i)),
    );
    items
}

/// The recent file at `index` of the Dock menu's list.
pub(crate) fn dock_recent(index: usize) -> Option<PathBuf> {
    lock(&DOCK.recent).get(index).cloned()
}

/// Local paths among opened URLs (`file_url`: whether the URL is a `file:` URL; `path`:
/// its path); other schemes are ignored.
pub(crate) fn file_paths(urls: impl IntoIterator<Item = (bool, Option<String>)>) -> Vec<PathBuf> {
    urls.into_iter()
        .filter(|(file, _)| *file)
        .filter_map(|(_, p)| p.filter(|p| !p.is_empty()).map(PathBuf::from))
        .collect()
}
