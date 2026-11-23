//! User settings, kept as a small JSON file in the per-user config directory.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::page_theme::PageTheme;

const FILE: &str = "settings.json";
/// Documents whose zoom is remembered.
const MAX_ZOOMS: usize = 200;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
    HighContrastDark,
    HighContrastLight,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WindowState {
    pub width: f32,
    pub height: f32,
    /// Position of the window on screen, when known.
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub maximized: bool,
}

/// Desktop integration choices.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SystemSettings {
    /// Opened files go to the system's recent-files list (file managers, the GTK picker).
    pub report_recent: bool,
    /// Look for a new version once a day (only when the build has an update address).
    pub check_updates: bool,
    /// When updates were last looked for (seconds since 1970).
    pub last_update_check: u64,
    /// A version the user put off; automatic checks stay quiet about it.
    pub skipped_version: Option<String>,
    /// Later launches hand their files to the running window.
    pub single_instance: bool,
    /// Files from later launches open in a window of their own instead of a tab.
    pub open_in_new_window: bool,
    /// Starting the app reopens the documents open when it was closed.
    pub restore_session: bool,
    /// The documents open when the app was closed (with `restore_session`).
    pub session: Vec<SessionDoc>,
    /// Windows: the taskbar jump list; macOS: the Dock menu.
    pub jump_list: bool,
    /// Long jobs show their progress on the taskbar or launcher icon.
    pub progress: bool,
    /// A notification when a long job ends while the window is in the background.
    pub notifications: bool,
    /// The "Set up system integration" card was closed or used.
    pub setup_card_done: bool,
}

/// A document to reopen: its path and the page shown (0-based).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionDoc {
    pub path: PathBuf,
    pub page: usize,
}

impl Default for SystemSettings {
    fn default() -> Self {
        SystemSettings {
            report_recent: true,
            check_updates: false,
            last_update_check: 0,
            skipped_version: None,
            single_instance: true,
            open_in_new_window: false,
            restore_session: false,
            session: Vec::new(),
            jump_list: true,
            progress: true,
            notifications: false,
            setup_card_done: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub window: Option<WindowState>,
    /// The window per screen layout (see the desktop app's `window_state`).
    pub screens: BTreeMap<String, WindowState>,
    pub system: SystemSettings,
    pub sidebar_visible: bool,
    /// 0 pages, 1 outline, 2 comments.
    pub sidebar_tab: u8,
    pub theme: ThemeChoice,
    /// Written into annotations and replies.
    pub author: String,
    /// Last zoom per document path; `0` means "fit width".
    pub zoom: BTreeMap<String, f64>,
    /// Order of `zoom`'s keys, most recent last, to forget the oldest.
    pub zoom_order: Vec<String>,
    pub flatten_signatures: bool,
    /// Smooth scrolling and other motion: `None` follows the system.
    pub reduce_motion: Option<bool>,
    /// Size of the app's own text, 1.0 normal.
    pub text_scale: f32,
    /// Read-aloud speed, 1.0 normal.
    pub speech_rate: f32,
    /// The read-aloud voice (an id the speech engine gave); `None` picks one.
    pub speech_voice: Option<String>,
    /// The colours of the pages on phones and tablets (the desktop app keeps them per
    /// document).
    pub page_theme: PageTheme,
    /// The UI language (a code of `pagebara_i18n::LANGUAGES`); `None` follows the system.
    pub language: Option<String>,
    /// Misspelled words are underlined in text being typed.
    pub check_spelling: bool,
    /// Vim keys over the pages (`crate::vim`).
    pub vim_keys: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            window: None,
            screens: BTreeMap::new(),
            system: SystemSettings::default(),
            sidebar_visible: true,
            sidebar_tab: 0,
            theme: ThemeChoice::System,
            author: default_author(),
            zoom: BTreeMap::new(),
            zoom_order: Vec::new(),
            flatten_signatures: false,
            reduce_motion: None,
            text_scale: 1.0,
            speech_rate: 1.0,
            speech_voice: None,
            page_theme: PageTheme::Normal,
            language: None,
            check_spelling: true,
            vim_keys: false,
        }
    }
}

fn default_author() -> String {
    ["USER", "USERNAME"]
        .iter()
        .find_map(|v| std::env::var(v).ok().filter(|s| !s.is_empty()))
        .unwrap_or_default()
}

impl Settings {
    pub fn default_path() -> Option<PathBuf> {
        pagebara_identity::config_dir().map(|d| d.join(FILE))
    }

    /// The settings at `path`; defaults when missing or unreadable.
    pub fn load(path: &Path) -> Settings {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        crate::session::write_atomic(path, &json)
    }

    pub fn zoom_for(&self, doc: &Path) -> Option<f64> {
        self.zoom.get(&key(doc)).copied()
    }

    pub fn set_zoom(&mut self, doc: &Path, zoom: f64) {
        let k = key(doc);
        self.zoom_order.retain(|o| *o != k);
        self.zoom_order.push(k.clone());
        self.zoom.insert(k, zoom);
        while self.zoom_order.len() > MAX_ZOOMS {
            let old = self.zoom_order.remove(0);
            self.zoom.remove(&old);
        }
    }
}

fn key(p: &Path) -> String {
    std::path::absolute(p)
        .unwrap_or_else(|_| p.to_owned())
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_zoom_cap() {
        let dir = std::env::temp_dir().join(format!("pagebara-settings-{}", std::process::id()));
        let path = dir.join(FILE);
        let mut s = Settings::load(&path);
        assert_eq!(s, Settings::default());
        s.theme = ThemeChoice::Dark;
        s.author = "Ada".into();
        s.page_theme = PageTheme::Sepia;
        for i in 0..(MAX_ZOOMS + 5) {
            s.set_zoom(Path::new(&format!("/d/{i}.pdf")), 1.25);
        }
        s.save(&path).unwrap();
        let t = Settings::load(&path);
        assert_eq!(t.theme, ThemeChoice::Dark);
        assert_eq!(t.author, "Ada");
        assert_eq!(t.page_theme, PageTheme::Sepia);
        assert_eq!(t.zoom.len(), MAX_ZOOMS);
        assert_eq!(t.zoom_for(Path::new("/d/0.pdf")), None);
        assert_eq!(t.zoom_for(Path::new("/d/204.pdf")), Some(1.25));
        std::fs::write(&path, b"{\"theme\": \"light\", \"unknown\": 1}").unwrap();
        assert_eq!(Settings::load(&path).theme, ThemeChoice::Light);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
