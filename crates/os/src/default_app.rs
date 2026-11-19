//! Becoming the default PDF app. Linux: `xdg-mime default` (only when the user asks).
//! Windows does not let apps set themselves as default: the Settings page for this app
//! opens instead. macOS: Finder's Get Info sets it (the public API for it is C only).

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::process::Command;

/// The types the app registers for (see `packaging/linux/app.desktop.in`).
pub const MIME_TYPES: [&str; 2] = ["application/pdf", "application/x-pdf"];

/// How this platform lets the user make the app the default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    /// The app sets it (Linux).
    Direct,
    /// The app opens the system's settings page (Windows).
    SystemSettings,
    /// The user does it; show these steps.
    Instructions(&'static str),
}

pub fn support() -> Support {
    if cfg!(windows) {
        Support::SystemSettings
    } else if cfg!(target_os = "macos") {
        Support::Instructions(
            "In Finder, select a PDF, choose File > Get Info, pick this app under \
             \"Open with\", then click Change All.",
        )
    } else if std::env::var_os("FLATPAK_ID").is_some() || XdgMime::system().find().is_none() {
        Support::Instructions(
            "Choose this app for PDF documents under Default Applications in your system \
             settings, or in your file manager's \"Open With\" menu.",
        )
    } else {
        Support::Direct
    }
}

/// The desktop file's ID, as `xdg-mime` names apps.
pub fn desktop_id() -> String {
    format!("{}.desktop", pagebara_identity::APP_ID)
}

/// Runs `xdg-mime`, with the environment given (tests point it at temporary XDG folders).
#[derive(Clone, Debug)]
pub struct XdgMime {
    pub program: PathBuf,
    pub envs: Vec<(OsString, OsString)>,
}

impl XdgMime {
    pub fn system() -> XdgMime {
        XdgMime {
            program: "xdg-mime".into(),
            envs: Vec::new(),
        }
    }

    fn find(&self) -> Option<PathBuf> {
        if self.program.is_absolute() {
            return self.program.is_file().then(|| self.program.clone());
        }
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|d| d.join(&self.program))
            .find(|p| p.is_file())
    }

    fn command(&self) -> Command {
        let mut c = Command::new(&self.program);
        c.envs(self.envs.iter().map(|(k, v)| (k, v)));
        c
    }

    /// Whether this app opens PDFs by default; `None` when that cannot be told.
    pub fn is_default(&self) -> Option<bool> {
        let out = self
            .command()
            .args(["query", "default", MIME_TYPES[0]])
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim() == desktop_id())
    }

    /// Makes this app the default for PDFs.
    pub fn make_default(&self) -> io::Result<()> {
        let status = self
            .command()
            .arg("default")
            .arg(desktop_id())
            .args(MIME_TYPES)
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!("xdg-mime failed ({status})")))
        }
    }
}

/// Windows: the Settings page where the user picks default apps, at this app's entry
/// (Windows 11; earlier versions show the general page).
pub fn settings_uri() -> String {
    let name: String = pagebara_identity::NAME
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    format!("ms-settings:defaultapps?registeredAppMachine={name}")
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// `xdg-mime` against temporary XDG folders, never the user's own settings.
    #[test]
    fn xdg_mime_default_in_a_temporary_home() {
        if XdgMime::system().find().is_none() {
            eprintln!("skipped: no xdg-mime");
            return;
        }
        let home = std::env::temp_dir().join(format!("pagebara-mime-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let apps = home.join("data/applications");
        std::fs::create_dir_all(&apps).unwrap();
        std::fs::create_dir_all(home.join("config")).unwrap();
        // Another app is the default to begin with.
        std::fs::write(
            home.join("config/mimeapps.list"),
            "[Default Applications]\napplication/pdf=other.desktop\n",
        )
        .unwrap();
        std::fs::write(
            apps.join("other.desktop"),
            "[Desktop Entry]\nType=Application\nName=o\nExec=/bin/sh %U\nMimeType=application/pdf;\n",
        )
        .unwrap();
        std::fs::write(
            apps.join(desktop_id()),
            "[Desktop Entry]\nType=Application\nName=x\nExec=/bin/sh %U\nMimeType=application/pdf;\n",
        )
        .unwrap();
        let env = |k: &str, v: PathBuf| (OsString::from(k), v.into_os_string());
        let x = XdgMime {
            program: "xdg-mime".into(),
            envs: vec![
                env("HOME", home.clone()),
                env("XDG_CONFIG_HOME", home.join("config")),
                env("XDG_DATA_HOME", home.join("data")),
                env("XDG_DATA_DIRS", home.join("none")),
                env("XDG_CONFIG_DIRS", home.join("none")),
                (
                    OsString::from("XDG_CURRENT_DESKTOP"),
                    OsString::from("X-Generic"),
                ),
                (OsString::from("DE"), OsString::from("generic")),
            ],
        };
        assert_eq!(x.is_default(), Some(false));
        x.make_default().unwrap();
        assert_eq!(x.is_default(), Some(true));
        let list = std::fs::read_to_string(home.join("config/mimeapps.list")).unwrap();
        assert!(
            list.contains(&format!("application/x-pdf={}", desktop_id())),
            "{list}"
        );
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn settings_page() {
        assert!(settings_uri().starts_with("ms-settings:defaultapps?registeredAppMachine="));
    }
}
