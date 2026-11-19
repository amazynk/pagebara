//! How this copy was installed, and how a verified download replaces it: an AppImage is
//! swapped in place and restarted; the Windows installer and the macOS disk image are
//! handed to the system; .deb and Flatpak installs are updated by the package manager.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::Download;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Install {
    AppImage {
        path: PathBuf,
        arch: String,
    },
    /// A system package (.deb or a distribution's): updated by the package manager.
    Deb,
    Flatpak,
    Msi {
        arch: String,
    },
    Dmg {
        arch: String,
    },
    /// A development build or a copy unpacked by hand.
    Unknown,
}

/// What [`apply`] did; the app finishes the job.
#[derive(Debug, PartialEq, Eq)]
pub enum Applied {
    /// The new version is in place: start this and quit.
    Restart(PathBuf),
    /// The installer is running: quit so it can replace the files.
    InstallerStarted,
    /// The disk image is open for the user to copy the app.
    Opened,
}

impl Install {
    pub fn detect() -> Install {
        let arch = std::env::consts::ARCH.to_owned();
        let exe = std::env::current_exe().unwrap_or_default();
        if cfg!(target_os = "linux") {
            if let Some(p) = std::env::var_os("APPIMAGE").map(PathBuf::from)
                && p.is_file()
            {
                return Install::AppImage { path: p, arch };
            }
            if std::env::var_os("FLATPAK_ID").is_some() {
                return Install::Flatpak;
            }
            if exe.starts_with("/usr") {
                return Install::Deb;
            }
            return Install::Unknown;
        }
        let dev = exe.components().any(|c| c.as_os_str() == "target");
        if cfg!(windows) && !dev {
            Install::Msi { arch }
        } else if cfg!(target_os = "macos")
            && exe.to_string_lossy().contains(".app/Contents/MacOS/")
        {
            Install::Dmg { arch }
        } else {
            Install::Unknown
        }
    }

    /// The manifest's key for this install's download; `None` when the app does not
    /// download its own updates.
    pub fn download_key(&self) -> Option<String> {
        match self {
            Install::AppImage { arch, .. } => Some(format!("linux-{arch}-appimage")),
            Install::Msi { arch } => Some(format!("windows-{arch}-msi")),
            Install::Dmg { arch } => Some(format!("macos-{arch}-dmg")),
            _ => None,
        }
    }

    /// What to tell the user when the app cannot install the update itself (shown through
    /// [`pagebara_i18n::translate`]).
    pub fn instructions(&self) -> &'static str {
        match self {
            Install::Deb => pagebara_i18n::tr_noop!(
                "Update it with your system's package manager or software center."
            ),
            Install::Flatpak => pagebara_i18n::tr_noop!(
                "Update it from your software center, or run \"flatpak update\"."
            ),
            _ => pagebara_i18n::tr_noop!("Download the new version from the project's web page."),
        }
    }

    /// Where the download goes before [`apply`]: next to the AppImage (so the swap is one
    /// rename on the same file system), else in `cache`.
    pub fn staging_path(&self, d: &Download, cache: &Path) -> PathBuf {
        match self {
            Install::AppImage { path, .. } => {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                path.with_file_name(format!(".{name}.update"))
            }
            _ => cache.join(file_name(&d.url)),
        }
    }
}

/// The last part of `url`'s path, made safe as a file name.
fn file_name(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let last = path.rsplit('/').next().unwrap_or("");
    let clean: String = last
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .collect();
    let clean = clean.trim_start_matches('.');
    if clean.is_empty() {
        "update".to_owned()
    } else {
        clean.to_owned()
    }
}

/// Installs the verified download at `staged` (see [`Install::staging_path`]).
pub fn apply(install: &Install, staged: &Path) -> io::Result<Applied> {
    match install {
        Install::AppImage { path, .. } => {
            replace_appimage(path, staged)?;
            Ok(Applied::Restart(path.clone()))
        }
        Install::Msi { .. } => {
            Command::new("msiexec").arg("/i").arg(staged).spawn()?;
            Ok(Applied::InstallerStarted)
        }
        Install::Dmg { .. } => {
            Command::new("open").arg(staged).spawn()?;
            Ok(Applied::Opened)
        }
        _ => Err(io::Error::other(install.instructions())),
    }
}

/// Swaps the AppImage for the new one in one rename (a running AppImage keeps its old
/// inode, so the swap is safe while it runs).
pub fn replace_appimage(target: &Path, staged: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(target)
            .map(|m| m.permissions().mode())
            .unwrap_or(0o755)
            | 0o111;
        std::fs::set_permissions(staged, std::fs::Permissions::from_mode(mode & 0o7777))?;
    }
    std::fs::rename(staged, target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_keys() {
        assert_eq!(
            file_name("https://x/r/pagebara-0.2.0.msi?sig=1"),
            "pagebara-0.2.0.msi"
        );
        assert_eq!(file_name("https://x/../..%2f"), "2f");
        assert_eq!(file_name("https://x/"), "update");
        let a = Install::AppImage {
            path: "/opt/pagebara.AppImage".into(),
            arch: "x86_64".into(),
        };
        assert_eq!(a.download_key().unwrap(), "linux-x86_64-appimage");
        let d = Download {
            url: "https://x/new.AppImage".into(),
            sha256: String::new(),
            size: None,
        };
        assert_eq!(
            a.staging_path(&d, Path::new("/cache")),
            Path::new("/opt/.pagebara.AppImage.update")
        );
        assert_eq!(Install::Deb.download_key(), None);
    }

    #[cfg(unix)]
    #[test]
    fn appimage_swap_keeps_it_executable() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join(format!("pagebara-upd-swap-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let (target, staged) = (d.join("app.AppImage"), d.join(".app.AppImage.update"));
        std::fs::write(&target, b"old").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(&staged, b"new").unwrap();
        let install = Install::AppImage {
            path: target.clone(),
            arch: "x86_64".into(),
        };
        assert_eq!(
            apply(&install, &staged).unwrap(),
            Applied::Restart(target.clone())
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert!(!staged.exists());
        std::fs::remove_dir_all(d).unwrap();
    }
}
