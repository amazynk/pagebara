//! Where per-user data lives: the platform's directories, or one folder given with
//! `--profile` (tests and recordings keep the real settings and signatures untouched).

use std::path::PathBuf;

use pagebara_app_core::recent::RecentFiles;
use pagebara_app_core::settings::Settings;
use pagebara_signatures::Library;

#[derive(Debug, Clone)]
pub struct Profile {
    pub settings_path: Option<PathBuf>,
    pub recent_path: Option<PathBuf>,
    pub signatures_dir: Option<PathBuf>,
    /// The user's trusted certificates (PEM), next to the settings.
    pub trust_path: Option<PathBuf>,
    /// Signing IDs made in the app (PKCS #12).
    pub ids_dir: Option<PathBuf>,
    /// Downloaded text recognition models.
    pub models_dir: Option<PathBuf>,
    /// The system's recent-files list (`recently-used.xbel`); with `--profile`, a copy in
    /// the profile, so test runs leave the user's list alone.
    pub os_recent_path: Option<PathBuf>,
    /// With `--profile`, system integration (`xdg-mime`'s choice included) writes into
    /// the profile, not the user's configuration.
    pub xdg_home: Option<PathBuf>,
    /// Downloaded updates.
    pub updates_dir: Option<PathBuf>,
    /// The `--profile` folder.
    pub dir: Option<PathBuf>,
}

impl Profile {
    pub fn new(dir: Option<PathBuf>) -> Profile {
        match dir {
            Some(d) => Profile {
                settings_path: Some(d.join("settings.json")),
                recent_path: Some(d.join("recent.txt")),
                signatures_dir: Some(d.join("signatures")),
                trust_path: Some(d.join("trusted-certificates.pem")),
                ids_dir: Some(d.join("ids")),
                models_dir: Some(d.join("ocr-models")),
                os_recent_path: Some(d.join("recently-used.xbel")),
                xdg_home: Some(d.join("xdg")),
                updates_dir: Some(d.join("updates")),
                dir: Some(d),
            },
            None => {
                let data = Library::default_dir().and_then(|d| d.parent().map(PathBuf::from));
                let settings_path = Settings::default_path();
                Profile {
                    trust_path: settings_path
                        .as_ref()
                        .and_then(|p| p.parent())
                        .map(|d| d.join("trusted-certificates.pem")),
                    settings_path,
                    recent_path: None,
                    signatures_dir: Library::default_dir(),
                    ids_dir: data.as_ref().map(|d| d.join("ids")),
                    models_dir: data.map(|d| d.join("ocr-models")),
                    os_recent_path: pagebara_os::recent_xbel::default_path(),
                    xdg_home: None,
                    updates_dir: pagebara_identity::cache_dir().map(|d| d.join("updates")),
                    dir: None,
                }
            }
        }
    }

    pub fn settings(&self) -> Settings {
        self.settings_path
            .as_deref()
            .map(Settings::load)
            .unwrap_or_default()
    }

    pub fn recent(&self) -> RecentFiles {
        match &self.recent_path {
            Some(p) => RecentFiles::load(p.clone()),
            None => RecentFiles::load_default(),
        }
    }

    pub fn library(&self) -> Result<Library, String> {
        let dir = self
            .signatures_dir
            .clone()
            .ok_or("no per-user data directory on this platform")?;
        Library::open(dir).map_err(|e| e.to_string())
    }
}
