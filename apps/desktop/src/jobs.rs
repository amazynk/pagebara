//! Long jobs seen from outside the window: progress on the taskbar button (Windows) or the
//! launcher icon (Linux, the Unity launcher API that GNOME docks and KDE read), and a
//! system notification when a job ends (Linux). Each has its setting. With `--profile`
//! both are written to `jobs.log` in the profile instead, so test runs never reach the
//! desktop.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use slint::ComponentHandle;

use crate::AppWindow;
use crate::profile::Profile;

/// The last percentage shown, to send only changes.
static SHOWN: Mutex<Option<u32>> = Mutex::new(None);

fn log_path(profile: &Profile) -> Option<PathBuf> {
    profile.dir.as_ref().map(|d| d.join("jobs.log"))
}

fn log(path: &Path, line: &str) {
    let r = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| writeln!(f, "{line}"));
    if let Err(e) = r {
        log::warn!("{}: {e}", path.display());
    }
}

/// Shows `value` (0–1) as the job's progress; `None` hides it.
pub fn progress(profile: &Profile, enabled: bool, ui: &AppWindow, value: Option<f64>) {
    let pct = value.map(|v| (v.clamp(0.0, 1.0) * 100.0).round() as u32);
    {
        let mut shown = SHOWN.lock().unwrap_or_else(|e| e.into_inner());
        if *shown == pct || (!enabled && shown.is_none()) {
            return;
        }
        *shown = if enabled { pct } else { None };
    }
    let pct = if enabled { pct } else { None };
    if let Some(p) = log_path(profile) {
        return log(
            &p,
            &format!("progress {}", pct.map_or("off".into(), |p| p.to_string())),
        );
    }
    if cfg!(windows) {
        let value = pct.map(|p| f64::from(p) / 100.0);
        ui.window().with_winit_window(|w| {
            use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(h) = w.window_handle()
                && let RawWindowHandle::Win32(h) = h.as_raw()
            {
                pagebara_native::taskbar_progress(h.hwnd.get(), value);
            }
        });
    } else if cfg!(all(unix, not(target_os = "macos"))) {
        launcher_progress(pct);
    }
}

use slint::winit_030::WinitWindowAccessor;

/// `com.canonical.Unity.LauncherEntry.Update` on the session bus.
fn launcher_progress(pct: Option<u32>) {
    let props = match pct {
        Some(p) => format!(
            "{{'progress': <{:.2}>, 'progress-visible': <true>}}",
            f64::from(p) / 100.0
        ),
        None => "{'progress-visible': <false>}".into(),
    };
    run(
        "gdbus",
        &[
            "emit",
            "--session",
            "--object-path",
            &format!(
                "/com/canonical/unity/launcherentry/{}",
                pagebara_identity::APP_ID.replace(['.', '-'], "_")
            ),
            "--signal",
            "com.canonical.Unity.LauncherEntry.Update",
            &format!("application://{}.desktop", pagebara_identity::APP_ID),
            &props,
        ],
    );
}

/// Runs a helper in the background (not found: nothing happens).
fn run(program: &str, args: &[&str]) {
    let mut c = Command::new(program);
    c.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Ok(mut child) = c.spawn() {
        pagebara_thread::spawn("job-helper", move || {
            let _ = child.wait();
        });
    }
}

/// A notification that a job ended. In the window, only while it is in the background.
pub fn notify(profile: &Profile, enabled: bool, title: &str, body: &str) {
    if !enabled {
        return;
    }
    if let Some(p) = log_path(profile) {
        return log(&p, &format!("notify {title}: {body}"));
    }
    if !cfg!(all(unix, not(target_os = "macos"))) {
        return;
    }
    let name = pagebara_identity::NAME;
    let id = pagebara_identity::APP_ID;
    if which("notify-send") {
        run("notify-send", &["-a", name, "-i", id, title, body]);
    } else {
        run(
            "gdbus",
            &[
                "call",
                "--session",
                "--dest",
                "org.freedesktop.Notifications",
                "--object-path",
                "/org/freedesktop/Notifications",
                "--method",
                "org.freedesktop.Notifications.Notify",
                name,
                "0",
                id,
                title,
                body,
                "[]",
                &format!("{{'desktop-entry': <'{id}'>}}"),
                "-1",
            ],
        );
    }
}

/// Whether the window has the keyboard focus (notifications are for when it has not).
pub fn focused(ui: &AppWindow) -> bool {
    ui.window()
        .with_winit_window(|w| w.has_focus())
        .unwrap_or(true)
}

fn which(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(program).is_file()))
}
