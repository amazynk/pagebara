//! Settings > System integration and `--integration`: the registrations of
//! `pagebara-integration` together with the app's own integration settings, for this user —
//! or, with `--profile`, inside the profile (its own XDG folders and registry file).

use std::path::PathBuf;

use pagebara_app_core::settings::{Settings, SystemSettings};
use pagebara_integration::{Env, Group, Integration, Kind, Os, Status, catalog};

use pagebara_i18n::{tr, tr_noop};

use crate::profile::Profile;

/// The executable that registrations start (an AppImage itself, not its mount).
pub fn exe() -> PathBuf {
    std::env::var_os("APPIMAGE")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_default()
}

pub fn open(profile: &Profile) -> Integration {
    match &profile.dir {
        Some(d) => Integration::new(Env::sandbox(d, Os::current(), exe())),
        None => {
            let manifest = pagebara_identity::data_dir()
                .unwrap_or_default()
                .join("integration.json");
            #[allow(unused_mut)]
            let mut env = Env::system(manifest);
            #[cfg(target_os = "macos")]
            {
                env.defaults = Box::new(mac::Defaults);
            }
            Integration::new(env)
        }
    }
}

/// One line of the settings page.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub id: &'static str,
    pub title: String,
    pub explain: String,
    pub group: Group,
    pub kind: Kind,
    pub status: String,
    pub on: bool,
    /// The switch can be used (not unavailable, not provided by the package).
    pub toggle: bool,
    /// A button instead of (or besides) the switch.
    pub button: String,
}

/// The value of an integration setting.
pub fn setting(s: &SystemSettings, id: &str) -> Option<bool> {
    Some(match id {
        "recent" => s.report_recent,
        "jump-list" => s.jump_list,
        "single-instance" => s.single_instance,
        "new-window" => s.open_in_new_window,
        "restore-session" => s.restore_session,
        "progress" => s.progress,
        "notifications" => s.notifications,
        _ => return None,
    })
}

pub fn set_setting(s: &mut SystemSettings, id: &str, on: bool) -> bool {
    let field = match id {
        "recent" => &mut s.report_recent,
        "jump-list" => &mut s.jump_list,
        "single-instance" => &mut s.single_instance,
        "new-window" => &mut s.open_in_new_window,
        "restore-session" => &mut s.restore_session,
        "progress" => &mut s.progress,
        "notifications" => &mut s.notifications,
        _ => return false,
    };
    *field = on;
    if id == "restore-session" && !on {
        s.session.clear();
    }
    true
}

/// Settings that do nothing on this system, with the reason.
fn setting_unavailable(os: Os, id: &str) -> Option<&'static str> {
    match (os, id) {
        (Os::Linux, "jump-list") => Some(tr_noop!(
            "The launcher's menu offers “New Window” from the app's launcher entry."
        )),
        _ => catalog::unavailable(os, id),
    }
}

pub fn rows(int: &Integration, s: &SystemSettings) -> Vec<Row> {
    let os = int.env.os;
    int.items()
        .into_iter()
        .map(|item| {
            let mut row = Row {
                id: item.id,
                title: item.title.to_owned(),
                explain: item.explain.clone(),
                group: item.group,
                kind: item.kind,
                status: String::new(),
                on: false,
                toggle: true,
                button: String::new(),
            };
            match item.kind {
                Kind::Setting => match setting_unavailable(os, item.id) {
                    Some(why) => {
                        row.toggle = false;
                        row.status = pagebara_i18n::translate(why, &[]);
                    }
                    None => row.on = setting(s, item.id).unwrap_or(item.default_on),
                },
                Kind::Button => {
                    row.toggle = false;
                    row.button = tr!("Open Default apps");
                }
                Kind::Registration => {
                    let st = int.status(item.id);
                    row.on = st.is_on();
                    row.status = st.text();
                    match st {
                        Status::Unavailable(_) | Status::Provided(_) => row.toggle = false,
                        Status::Partial(_) | Status::Outdated => row.button = tr!("Repair"),
                        _ => {}
                    }
                }
            }
            row
        })
        .collect()
}

/// What turning a setting off removes from the system (the app's recent entries).
pub fn setting_turned_off(profile: &Profile, id: &str) {
    match id {
        "recent" => {
            if let Some(list) = &profile.os_recent_path
                && cfg!(all(unix, not(target_os = "macos")))
                && let Err(e) = pagebara_os::recent_xbel::remove_app(list, pagebara_identity::NAME)
            {
                log::warn!("could not remove the app's recent files: {e}");
            }
            if profile.dir.is_none() {
                pagebara_native::clear_recent();
            }
        }
        "jump-list" if profile.dir.is_none() => pagebara_native::clear_jump_list(),
        _ => {}
    }
}

/// Everything off: registrations undone, the recent entries and jump list removed.
pub fn remove_all(
    int: &mut Integration,
    profile: &Profile,
    settings: &mut Settings,
) -> Result<(), String> {
    let r = int.remove_all();
    if r.as_ref().is_ok_and(|o| o.shell_changed) && profile.dir.is_none() {
        pagebara_native::shell_assoc_changed();
    }
    for id in ["recent", "jump-list"] {
        set_setting(&mut settings.system, id, false);
        setting_turned_off(profile, id);
    }
    r.map(drop).map_err(|e| e.to_string())
}

/// Turns an item on or off (a registration or a setting). `settings` is saved by the caller.
pub fn set(
    int: &mut Integration,
    profile: &Profile,
    settings: &mut Settings,
    id: &str,
    on: bool,
) -> Result<(), pagebara_integration::Error> {
    if setting(&settings.system, id).is_some() {
        if let Some(why) = setting_unavailable(int.env.os, id) {
            return Err(pagebara_integration::Error::Unavailable(
                pagebara_i18n::translate(why, &[]),
            ));
        }
        set_setting(&mut settings.system, id, on);
        if !on {
            setting_turned_off(profile, id);
        }
        return Ok(());
    }
    let out = if on { int.enable(id) } else { int.disable(id) }?;
    if out.shell_changed && profile.dir.is_none() {
        pagebara_native::shell_assoc_changed();
    }
    Ok(())
}

/// Windows: asks for administrator rights to run `--integration CMD ITEM` (the Windows
/// Search filter is registered machine-wide). Waits for it.
pub fn elevated(cmd: &str, id: &str) -> Result<(), String> {
    if !cfg!(windows) {
        return Err(tr!("needs administrator rights"));
    }
    let exe = exe().to_string_lossy().replace('\'', "''");
    let mut c = std::process::Command::new("powershell");
    c.args([
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        &format!(
            "Start-Process -FilePath '{exe}' -ArgumentList '--integration','{cmd}','{id}' -Verb RunAs -Wait -WindowStyle Hidden"
        ),
    ]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: the app has no console, so PowerShell would open one of its own.
        c.creation_flags(0x0800_0000);
    }
    let status = c.status().map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(tr!("administrator rights were not given"))
    }
}

/// `pagebara --integration status|enable|disable|remove-all`; returns the exit code.
pub fn cli(cmd: &str, items: &[PathBuf], json: bool, profile_dir: Option<PathBuf>) -> i32 {
    let profile = Profile::new(profile_dir);
    let mut settings = profile.settings();
    let mut int = open(&profile);
    let ids: Vec<String> = items
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let save = |settings: &Settings| {
        if let Some(p) = &profile.settings_path
            && let Err(e) = settings.save(p)
        {
            eprintln!("could not save the settings: {e}");
            return false;
        }
        true
    };
    match cmd {
        "status" => {
            let rows = rows(&int, &settings.system);
            if json {
                for r in rows {
                    let v = serde_json::json!({
                        "id": r.id, "title": r.title, "group": r.group.title(),
                        "on": r.on, "available": r.toggle || !r.button.is_empty(),
                        "status": r.status,
                    });
                    println!("{v}");
                }
            } else {
                for r in rows {
                    let state = match (r.on, r.toggle || !r.button.is_empty()) {
                        _ if r.kind == Kind::Button => "button",
                        (true, _) => "on",
                        (false, true) => "off",
                        (false, false) => "n/a",
                    };
                    let note = if r.status.is_empty() || r.status == "On" || r.status == "Off" {
                        String::new()
                    } else {
                        format!("  ({})", r.status)
                    };
                    println!("{:<18} {:<6} {}{note}", r.id, state, r.title);
                }
                for (id, why) in catalog::SKIPPED {
                    println!("{id:<18} {:<6} {why}", "-");
                }
            }
            0
        }
        "enable" | "disable" if !ids.is_empty() => {
            let mut code = 0;
            for id in &ids {
                match set(&mut int, &profile, &mut settings, id, cmd == "enable") {
                    Ok(()) => println!("{id}: {}", if cmd == "enable" { "on" } else { "off" }),
                    Err(pagebara_integration::Error::NeedsAdmin) => {
                        eprintln!(
                            "{id}: needs administrator rights (run this from an elevated prompt)"
                        );
                        code = 1;
                    }
                    Err(e) => {
                        eprintln!("{id}: {e}");
                        code = 1;
                    }
                }
            }
            if !save(&settings) {
                code = 1;
            }
            code
        }
        "remove-all" => {
            let r = remove_all(&mut int, &profile, &mut settings);
            let saved = save(&settings);
            match r {
                Ok(()) if saved => {
                    println!("all system integration removed");
                    0
                }
                Ok(()) => 1,
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            }
        }
        _ => {
            eprintln!(
                "usage: --integration status [--json] | enable ITEM... | disable ITEM... | remove-all"
            );
            2
        }
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::sync::mpsc;
    use std::time::Duration;

    /// The default app through LaunchServices; the system asks the user to confirm.
    #[derive(Debug)]
    pub struct Defaults;

    impl pagebara_integration::DefaultApps for Defaults {
        fn current(&self, ty: &str) -> Option<String> {
            pagebara_native::default_app_for_type(ty).map(|p| p.to_string_lossy().into_owned())
        }

        fn set(&self, ty: &str, app: &str) -> Result<(), String> {
            let (tx, rx) = mpsc::channel();
            pagebara_native::set_default_app_for_type(
                ty,
                std::path::Path::new(app),
                Box::new(move |r| {
                    let _ = tx.send(r);
                }),
            );
            rx.recv_timeout(Duration::from_secs(120))
                .unwrap_or_else(|_| Err("no answer".into()))
        }

        fn unset(&self, _: &str, _: &str) -> Result<(), String> {
            Err(pagebara_i18n::tr!(
                "macOS always has a default app: choose another one in Finder's Get Info."
            ))
        }

        fn available(&self) -> Result<(), String> {
            pagebara_native::bundle_path().map(drop).ok_or_else(|| {
                pagebara_i18n::tr!("Start the app from its .app bundle to make it the default.")
            })
        }

        fn ours(&self) -> Option<String> {
            pagebara_native::bundle_path().map(|p| p.to_string_lossy().into_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The context-menu actions run headless exactly where `pagebara-actions` has them.
    #[test]
    fn menu_actions_match_the_actions_crate() {
        for a in pagebara_integration::MENU_ACTIONS {
            if let Some(i) = a.args.iter().position(|x| *x == "--action") {
                let name = a.args[i + 1];
                assert!(pagebara_actions::Action::parse(name).is_some(), "{name}");
            }
        }
    }

    #[test]
    fn every_setting_item_has_a_setting() {
        let mut s = SystemSettings::default();
        for os in [Os::Linux, Os::Windows, Os::MacOs] {
            for item in catalog::items(os) {
                if item.kind == Kind::Setting {
                    assert_eq!(setting(&s, item.id), Some(item.default_on), "{}", item.id);
                    assert!(set_setting(&mut s, item.id, !item.default_on));
                    assert!(set_setting(&mut s, item.id, item.default_on));
                }
            }
        }
    }
}
