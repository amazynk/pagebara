//! Handing a print file to the operating system.
//!
//! Linux and macOS: CUPS, through its command-line tools (`lpstat -l -p -d` lists printers,
//! `lp` submits with the copies, sides, media and collation options), found on `PATH`.
//!
//! Windows: the spooler, through GDI (see `super::win32`): the sheets are drawn on the
//! printer's device context, with copies, duplex and paper in its `DEVMODE`.

use pagebara_i18n::tr;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::{Duplex, Paper};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Printer {
    /// The queue the system knows it by (for `lp -d`).
    pub name: String,
    /// What people call it ("Office laser"), when the system says; else empty.
    pub description: String,
    pub is_default: bool,
    /// Accepting jobs (not disabled).
    pub enabled: bool,
}

/// How jobs reach printers here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Backend {
    /// CUPS `lp`, at this path.
    Cups(PathBuf),
    Windows,
    /// No way to print; only saving the print file.
    None,
}

impl Backend {
    pub fn detect() -> Backend {
        if cfg!(windows) {
            return Backend::Windows;
        }
        match find_on_path("lp") {
            Some(lp) => Backend::Cups(lp),
            None => Backend::None,
        }
    }

    /// Whether copies and duplex can be asked of the printer (else copies go in the file).
    pub fn takes_options(&self) -> bool {
        matches!(self, Backend::Cups(_) | Backend::Windows)
    }

    pub fn printers(&self) -> Result<Vec<Printer>, String> {
        match self {
            Backend::Cups(lp) => {
                let lpstat = lp.with_file_name("lpstat");
                let lpstat = if lpstat.exists() {
                    lpstat
                } else {
                    PathBuf::from("lpstat")
                };
                // `lpstat` exits with an error when there are no printers; its output
                // still says so.
                let out = Command::new(lpstat)
                    .args(["-l", "-p", "-d"])
                    .env("LC_ALL", "C")
                    .output()
                    .map_err(|e| tr!("Could not list printers: {}", e))?;
                let text = String::from_utf8_lossy(&out.stdout);
                Ok(parse_lpstat(&text))
            }
            #[cfg(windows)]
            Backend::Windows => super::win32::printers(),
            #[cfg(not(windows))]
            Backend::Windows => Err(tr!("Not on Windows.")),
            Backend::None => Ok(Vec::new()),
        }
    }

    /// Prints `file`; returns what the system said (a job id).
    pub fn submit(&self, job: &Job) -> Result<String, String> {
        match self {
            Backend::Cups(lp) => {
                let out = Command::new(lp)
                    .args(lp_args(job))
                    .env("LC_ALL", "C")
                    .output()
                    .map_err(|e| tr!("Could not run lp: {}", e))?;
                let said = String::from_utf8_lossy(&out.stdout).trim().to_owned();
                if out.status.success() {
                    Ok(said)
                } else {
                    let err = String::from_utf8_lossy(&out.stderr).trim().to_owned();
                    Err(if err.is_empty() {
                        tr!("lp failed ({})", out.status)
                    } else {
                        err
                    })
                }
            }
            #[cfg(windows)]
            Backend::Windows => super::win32::submit(job, None),
            #[cfg(not(windows))]
            Backend::Windows => Err(tr!("Not on Windows.")),
            Backend::None => Err(tr!("There is no print system on this computer.")),
        }
    }
}

/// A print job.
#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    pub printer: String,
    pub file: PathBuf,
    pub title: String,
    pub copies: u32,
    pub duplex: Duplex,
    pub paper: Paper,
    /// Collate copies (1, 2, 3, 1, 2, 3).
    pub collate: bool,
}

/// The arguments of `lp` for a job.
pub fn lp_args(job: &Job) -> Vec<String> {
    let mut a = vec![
        "-d".into(),
        job.printer.clone(),
        "-n".into(),
        job.copies.max(1).to_string(),
        "-o".into(),
        format!("sides={}", job.duplex.sides()),
    ];
    if let Some(m) = job.paper.media() {
        a.extend(["-o".into(), format!("media={m}")]);
    }
    if job.collate && job.copies > 1 {
        a.extend(["-o".into(), "collate=true".into()]);
    }
    a.extend(["-t".into(), job.title.clone(), "--".into()]);
    a.push(job.file.to_string_lossy().into_owned());
    a
}

/// Printers, their descriptions and the default from `lpstat -l -p -d` (in the C locale).
pub fn parse_lpstat(text: &str) -> Vec<Printer> {
    let mut printers: Vec<Printer> = Vec::new();
    let mut default = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("printer ") {
            let mut w = rest.split_whitespace();
            if let Some(name) = w.next() {
                printers.push(Printer {
                    name: name.to_owned(),
                    description: String::new(),
                    is_default: false,
                    enabled: !rest.contains("disabled"),
                });
            }
        } else if let Some(d) = line.trim_start().strip_prefix("Description:") {
            // `lpstat -l`: the printer above's description.
            if let Some(p) = printers.last_mut() {
                p.description = d.trim().to_owned();
            }
        } else if let Some(d) = line.strip_prefix("system default destination:") {
            default = Some(d.trim().to_owned());
        }
    }
    for p in &mut printers {
        p.is_default = default.as_deref() == Some(p.name.as_str());
    }
    // The default first.
    printers.sort_by_key(|p| !p.is_default);
    printers
}

/// An executable called `name` on `PATH` (what `which` does).
pub fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| is_executable(p))
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}
