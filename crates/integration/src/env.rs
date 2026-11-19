//! Where integration writes: the real per-user locations, or a sandbox folder (`--profile`
//! runs and tests), which also stands in for the Windows registry.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::Os;
use crate::registry::{FileRegistry, Registry};

/// The default app per file type: `xdg-mime` on Linux; the app supplies the macOS one.
pub trait DefaultApps: Send {
    /// The app that opens `ty` (a MIME type or a UTI) now.
    fn current(&self, ty: &str) -> Option<String>;
    fn set(&self, ty: &str, app: &str) -> Result<(), String>;
    /// Drops `app` as the default for `ty` when no earlier choice is known.
    fn unset(&self, ty: &str, app: &str) -> Result<(), String>;
    /// Whether this works here (`xdg-mime` installed and so on).
    fn available(&self) -> Result<(), String>;
    /// This app as the system names it, when that differs from the plan's name (macOS: the
    /// bundle path).
    fn ours(&self) -> Option<String> {
        None
    }
}

/// No default-app support.
#[derive(Debug)]
pub struct NoDefaults(pub &'static str);

impl DefaultApps for NoDefaults {
    fn current(&self, _: &str) -> Option<String> {
        None
    }
    fn set(&self, _: &str, _: &str) -> Result<(), String> {
        Err(pagebara_i18n::translate(self.0, &[]))
    }
    fn unset(&self, _: &str, _: &str) -> Result<(), String> {
        Err(pagebara_i18n::translate(self.0, &[]))
    }
    fn available(&self) -> Result<(), String> {
        Err(pagebara_i18n::translate(self.0, &[]))
    }
}

/// `xdg-mime` with an environment (pointed at the sandbox in tests), and the
/// `mimeapps.list` it writes.
#[derive(Debug, Clone)]
pub struct XdgDefaults {
    pub xdg: pagebara_os::default_app::XdgMime,
    pub mimeapps: PathBuf,
    pub path: Vec<PathBuf>,
}

impl XdgDefaults {
    fn run(&self, args: &[&str]) -> Result<String, String> {
        let mut c = Command::new(&self.xdg.program);
        c.envs(self.xdg.envs.iter().map(|(k, v)| (k, v)));
        let out = c
            .args(args)
            .output()
            .map_err(|e| format!("xdg-mime: {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
        } else {
            Err(format!(
                "xdg-mime {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }
}

impl DefaultApps for XdgDefaults {
    fn current(&self, ty: &str) -> Option<String> {
        self.run(&["query", "default", ty])
            .ok()
            .filter(|s| !s.is_empty())
    }

    fn set(&self, ty: &str, app: &str) -> Result<(), String> {
        self.run(&["default", app, ty]).map(drop)
    }

    fn unset(&self, ty: &str, app: &str) -> Result<(), String> {
        crate::linux::remove_mime_default(&self.mimeapps, ty, app).map_err(|e| e.to_string())
    }

    fn available(&self) -> Result<(), String> {
        let p = &self.xdg.program;
        let found = if p.is_absolute() {
            p.is_file()
        } else {
            find_program(&self.path, &p.to_string_lossy()).is_some()
        };
        if found {
            Ok(())
        } else {
            Err(pagebara_i18n::tr!("xdg-mime (xdg-utils) is not installed."))
        }
    }
}

/// Everything integration needs to know about the system it writes to.
pub struct Env {
    pub os: Os,
    /// The executable that menus and registrations start (the AppImage itself for one).
    pub exe: PathBuf,
    pub home: PathBuf,
    /// `XDG_DATA_HOME` (Linux).
    pub data_home: PathBuf,
    /// `XDG_CONFIG_HOME` (Linux).
    pub config_home: PathBuf,
    /// `XDG_DATA_DIRS`: where installed packages put desktop files.
    pub data_dirs: Vec<PathBuf>,
    /// `XDG_CONFIG_DIRS`: system defaults (Thunar's `uca.xml`).
    pub config_dirs: Vec<PathBuf>,
    /// `%APPDATA%` (Windows: the Send To folder).
    pub appdata: PathBuf,
    /// The record of what was registered.
    pub manifest: PathBuf,
    /// `PATH`, for finding file managers and helper programs.
    pub path: Vec<PathBuf>,
    /// Environment for helper programs (`update-desktop-database`).
    pub child_env: Vec<(OsString, OsString)>,
    pub flatpak: bool,
    pub registry: Box<dyn Registry>,
    pub defaults: Box<dyn DefaultApps>,
    /// The Windows Explorer extension DLL.
    pub shell_dll: PathBuf,
}

impl std::fmt::Debug for Env {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Env")
            .field("os", &self.os)
            .field("exe", &self.exe)
            .field("home", &self.home)
            .field("manifest", &self.manifest)
            .finish_non_exhaustive()
    }
}

fn var_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn var_paths(name: &str, default: &[&str]) -> Vec<PathBuf> {
    match std::env::var_os(name).filter(|v| !v.is_empty()) {
        Some(v) => std::env::split_paths(&v).collect(),
        None => default.iter().map(PathBuf::from).collect(),
    }
}

pub fn find_program(path: &[PathBuf], name: &str) -> Option<PathBuf> {
    path.iter().map(|d| d.join(name)).find(|p| p.is_file())
}

impl Env {
    /// The real locations of this user. `manifest` lives in the app's data folder.
    pub fn system(manifest: PathBuf) -> Env {
        let os = Os::current();
        let home = var_path("HOME")
            .or_else(|| var_path("USERPROFILE"))
            .unwrap_or_default();
        let exe = var_path("APPIMAGE")
            .or_else(|| std::env::current_exe().ok())
            .unwrap_or_default();
        let data_home = var_path("XDG_DATA_HOME").unwrap_or_else(|| home.join(".local/share"));
        let config_home = var_path("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
        let path = var_paths("PATH", &[]);
        let defaults: Box<dyn DefaultApps> = match os {
            Os::Linux => Box::new(XdgDefaults {
                xdg: pagebara_os::default_app::XdgMime::system(),
                mimeapps: config_home.join("mimeapps.list"),
                path: path.clone(),
            }),
            Os::Windows => Box::new(NoDefaults(pagebara_i18n::tr_noop!(
                "Windows lets only you choose default apps."
            ))),
            Os::MacOs => Box::new(NoDefaults(pagebara_i18n::tr_noop!("unavailable"))),
        };
        #[cfg(windows)]
        let registry: Box<dyn Registry> = Box::new(crate::registry::WindowsRegistry);
        #[cfg(not(windows))]
        let registry: Box<dyn Registry> = Box::new(FileRegistry::new());
        Env {
            os,
            shell_dll: exe
                .parent()
                .map(|d| d.join(pagebara_identity::SHELL_DLL))
                .unwrap_or_default(),
            exe,
            data_dirs: var_paths("XDG_DATA_DIRS", &["/usr/local/share", "/usr/share"]),
            config_dirs: var_paths("XDG_CONFIG_DIRS", &["/etc/xdg"]),
            appdata: var_path("APPDATA").unwrap_or_else(|| home.join("AppData/Roaming")),
            home,
            data_home,
            config_home,
            manifest,
            path,
            child_env: Vec::new(),
            flatpak: std::env::var_os("FLATPAK_ID").is_some(),
            registry,
            defaults,
        }
    }

    /// Everything inside `dir`: home, XDG folders (as `dir/xdg/{data,config}`, where the
    /// app's `--profile` keeps `xdg-mime`'s choice), a registry file and the manifest.
    /// Never touches the user's own settings.
    pub fn sandbox(dir: &Path, os: Os, exe: PathBuf) -> Env {
        let data_home = dir.join("xdg/data");
        let config_home = dir.join("xdg/config");
        let none = dir.join("xdg/none");
        let child_env: Vec<(OsString, OsString)> = vec![
            ("HOME".into(), dir.join("home").into()),
            ("XDG_DATA_HOME".into(), data_home.clone().into()),
            ("XDG_CONFIG_HOME".into(), config_home.clone().into()),
            ("XDG_DATA_DIRS".into(), none.clone().into()),
            ("XDG_CONFIG_DIRS".into(), none.clone().into()),
            ("XDG_CURRENT_DESKTOP".into(), "X-Generic".into()),
            ("DE".into(), "generic".into()),
        ];
        let path = var_paths("PATH", &[]);
        let defaults: Box<dyn DefaultApps> = match os {
            Os::Linux => Box::new(XdgDefaults {
                xdg: pagebara_os::default_app::XdgMime {
                    program: "xdg-mime".into(),
                    envs: child_env.clone(),
                },
                mimeapps: config_home.join("mimeapps.list"),
                path: path.clone(),
            }),
            Os::Windows => Box::new(NoDefaults(pagebara_i18n::tr_noop!(
                "Windows lets only you choose default apps."
            ))),
            Os::MacOs => Box::new(NoDefaults(pagebara_i18n::tr_noop!("unavailable"))),
        };
        Env {
            os,
            shell_dll: exe
                .parent()
                .map(|d| d.join(pagebara_identity::SHELL_DLL))
                .unwrap_or_default(),
            exe,
            home: dir.join("home"),
            data_home,
            config_home,
            data_dirs: vec![none.clone()],
            config_dirs: vec![none],
            appdata: dir.join("home/AppData/Roaming"),
            manifest: dir.join("integration.json"),
            path,
            child_env,
            flatpak: false,
            registry: Box::new(FileRegistry::open(dir.join("registry.json"))),
            defaults,
        }
    }

    pub fn find(&self, program: &str) -> Option<PathBuf> {
        find_program(&self.path, program)
    }

    /// Runs a helper program (when installed) with the child environment; failures are
    /// only logged.
    pub fn run_helper(&self, program: &str, args: &[&Path]) {
        let Some(p) = self.find(program) else { return };
        let mut c = Command::new(p);
        c.envs(self.child_env.iter().map(|(k, v)| (k, v)));
        match c.args(args).output() {
            Ok(o) if !o.status.success() => {
                log::warn!("{program}: {}", String::from_utf8_lossy(&o.stderr).trim())
            }
            Ok(_) => {}
            Err(e) => log::warn!("{program}: {e}"),
        }
    }
}
