//! `--script` properties and commands for desktop integration: another launch handing
//! its files over, dropped files, the recent-files list and the default-app choice (both
//! kept inside the `--profile` folder in test runs).

use std::path::PathBuf;
use std::rc::Rc;

use slint::ComponentHandle;

use crate::app::{App, State};
use crate::{AppState, AppWindow};

/// The value of `key` for `assert`, or `None` when it is not one of these.
pub fn query(s: &mut State, ui: &AppWindow, key: &str) -> Option<Result<String, String>> {
    let read = |p: Option<PathBuf>| {
        p.and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default()
    };
    Some(Ok(match key {
        "os.instance" => if s.instance.is_some() {
            "primary"
        } else {
            "alone"
        }
        .to_owned(),
        "os.recent" => read(s.profile.os_recent_path.clone()),
        "os.mimeapps" => read(
            s.profile
                .xdg_home
                .as_ref()
                .map(|h| h.join("config/mimeapps.list")),
        ),
        // `id=on` per line (`n/a` where unavailable), as the settings page shows them.
        "os.integration" => {
            let int = crate::integration::open(&s.profile);
            crate::integration::rows(&int, &s.settings.system)
                .into_iter()
                .map(|r| {
                    let state = match (r.on, r.toggle) {
                        (true, _) => "on",
                        (false, true) => "off",
                        (false, false) => "n/a",
                    };
                    format!("{}={state}\n", r.id)
                })
                .collect()
        }
        "os.integration-status" => ui.global::<AppState>().get_integration_status().to_string(),
        "os.integration-busy" => ui.global::<AppState>().get_integration_busy().to_string(),
        // Files registered inside the profile (its XDG data and config folders).
        "os.registered" => {
            let mut out = Vec::new();
            if let Some(d) = &s.profile.dir {
                let mut stack = vec![d.join("xdg")];
                while let Some(dir) = stack.pop() {
                    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                        let p = e.path();
                        if p.is_dir() {
                            stack.push(p);
                        } else if let Ok(rel) = p.strip_prefix(d) {
                            out.push(rel.to_string_lossy().into_owned());
                        }
                    }
                }
            }
            out.sort();
            out.join("\n")
        }
        "os.jobs" => read(s.profile.dir.as_ref().map(|d| d.join("jobs.log"))),
        "os.settings" => read(s.profile.settings_path.clone()),
        // `os.setting.NAME`: one of the saved integration settings, as JSON.
        k if k.starts_with("os.setting.") => {
            let saved: serde_json::Value = s
                .profile
                .settings_path
                .as_ref()
                .and_then(|p| std::fs::read(p).ok())
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default();
            saved["system"][&k["os.setting.".len()..]].to_string()
        }
        _ => return None,
    }))
}

/// Runs `cmd`, or returns `None` when it is not one of these.
pub fn call(
    app: &Rc<App>,
    s: &mut State,
    ui: &AppWindow,
    cmd: &str,
    args: &[String],
) -> Option<Result<(), String>> {
    Some(match cmd {
        #[cfg(target_os = "macos")]
        "app-menu" => args
            .first()
            .ok_or_else(|| "app-menu needs a title".to_owned())
            .and_then(|title| pagebara_native::perform_app_menu_item(title)),
        // Starts the app again with these arguments and this profile (it hands its files
        // to this window and exits).
        "launch" => match crate::os::relaunch(args, s.profile.dir.as_deref()) {
            Ok(mut child) => {
                pagebara_thread::spawn("test-launch", move || {
                    let status = child.wait();
                    log::info!("the second launch exited: {status:?}");
                });
                Ok(())
            }
            Err(e) => Err(format!("launch: {e}")),
        },
        // `drop PATH X,Y` (or `drop PATH here`, at the script's pointer): a file dropped
        // from the file manager at that window point.
        "drop" => {
            let Some(path) = args.first() else {
                return Some(Err("drop needs a file".into()));
            };
            let st = ui.global::<AppState>();
            let at = match args.get(1).map(String::as_str) {
                Some("here") => Some((st.get_cursor_x(), st.get_cursor_y())),
                Some(p) => p
                    .split_once(',')
                    .and_then(|(x, y)| Some((x.trim().parse().ok()?, y.trim().parse().ok()?))),
                None => None,
            };
            crate::dnd::dropped(app, vec![PathBuf::from(path)], at);
            Ok(())
        }
        "show-setup-card" => {
            ui.global::<AppState>().set_setup_card_visible(true);
            Ok(())
        }
        _ => return None,
    })
}
