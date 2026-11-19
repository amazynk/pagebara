//! Opt-in update checks (`pagebara_update`): hidden unless the build has an update address and
//! key, off until the user turns them on (then once a day), or "Check now" in Settings.
//! An AppImage is replaced and restarted; the Windows installer and macOS disk image are
//! downloaded, verified and opened; other installs are told where updates come from.

use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use pagebara_i18n::tr;
use pagebara_update::{Check, Config, Install, Update};
use slint::ComponentHandle;

use super::{App, State, post_act};
use crate::{AppState, AppWindow, DialogKind};

/// Automatic checks are this far apart.
const EVERY: u64 = 24 * 3600;

#[derive(Debug)]
pub struct UpdateState {
    config: Option<Config>,
    install: Install,
    busy: bool,
    offered: Option<Update>,
}

impl Default for UpdateState {
    fn default() -> Self {
        UpdateState {
            config: config(),
            install: Install::detect(),
            busy: false,
            offered: None,
        }
    }
}

/// From the app's identity; debug builds also take `PAGEBARA_UPDATE_URL` and
/// `PAGEBARA_UPDATE_KEY` (for trying it against a local server).
fn config() -> Option<Config> {
    let version = env!("CARGO_PKG_VERSION");
    #[cfg(debug_assertions)]
    if let (Ok(url), Ok(key)) = (
        std::env::var("PAGEBARA_UPDATE_URL"),
        std::env::var("PAGEBARA_UPDATE_KEY"),
    ) {
        return Config::new(&url, &key, version).ok();
    }
    Config::from_identity(version)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// At startup: shows the setting when updates are set up, and checks when due.
pub fn start(s: &mut State, ui: &AppWindow) {
    ui.global::<AppState>()
        .set_updates_available(s.update.config.is_some());
    let sys = &s.settings.system;
    if sys.check_updates && now().saturating_sub(sys.last_update_check) >= EVERY {
        check(s, ui, false);
    }
}

pub fn show_settings(s: &State, ui: &AppWindow) {
    let st = ui.global::<AppState>();
    st.set_updates_available(s.update.config.is_some());
    st.set_check_updates(s.settings.system.check_updates);
}

pub fn action(app: &Rc<App>, s: &mut State, ui: &AppWindow, what: &str) {
    let st = ui.global::<AppState>();
    match what {
        "check-updates" => {
            s.settings.system.check_updates = st.get_check_updates();
            app.save_settings(s);
            start(s, ui);
        }
        "check-now" => check(s, ui, true),
        "install-update" => install(app, s, ui),
        "skip-update" => {
            s.settings.system.skipped_version =
                s.update.offered.as_ref().map(|u| u.version.clone());
            app.save_settings(s);
            st.set_dialog(DialogKind::None);
        }
        "update-later" => st.set_dialog(DialogKind::None),
        _ => log::warn!("unknown system action {what}"),
    }
}

/// Looks for a new version on a background thread; `manual` reports every outcome.
fn check(s: &mut State, ui: &AppWindow, manual: bool) {
    let Some(config) = s.update.config.clone() else {
        return;
    };
    if s.update.busy {
        return;
    }
    s.update.busy = true;
    let st = ui.global::<AppState>();
    st.set_update_status(tr!("Checking for updates…").into());
    let install = s.update.install.clone();

    pagebara_thread::spawn("int-update", move || {
        let result = fetch(&config, &install);
        post_act(move |app, s, ui| checked(app, s, ui, result, manual));
    });
}

#[cfg(feature = "network")]
fn fetch(config: &Config, install: &Install) -> Result<Check, pagebara_update::Error> {
    pagebara_update::net::check(config, install)
}

#[cfg(not(feature = "network"))]
fn fetch(_: &Config, _: &Install) -> Result<Check, pagebara_update::Error> {
    Err(pagebara_update::Error::Network(tr!(
        "this build has no network support"
    )))
}

fn checked(
    app: &Rc<App>,
    s: &mut State,
    ui: &AppWindow,
    result: Result<Check, pagebara_update::Error>,
    manual: bool,
) {
    s.update.busy = false;
    s.settings.system.last_update_check = now();
    app.save_settings(s);
    let st = ui.global::<AppState>();
    match result {
        Ok(Check::Available(u)) => {
            st.set_update_status(tr!("Version {} is available.", u.version).into());
            let skipped = s.settings.system.skipped_version.as_deref() == Some(&u.version);
            if manual || !skipped {
                show_offer(s, ui, &u);
            }
            s.update.offered = Some(u);
        }
        Ok(Check::UpToDate) | Err(pagebara_update::Error::Older { .. }) => {
            st.set_update_status(
                tr!(
                    "{} {} is the latest version.",
                    pagebara_identity::NAME,
                    env!("CARGO_PKG_VERSION")
                )
                .into(),
            );
        }
        Err(e) => {
            log::warn!("update check: {e}");
            st.set_update_status(tr!("Could not check for updates: {}", e).into());
        }
    }
}

fn show_offer(s: &State, ui: &AppWindow, u: &Update) {
    let st = ui.global::<AppState>();
    st.set_update_version(u.version.as_str().into());
    st.set_update_notes(u.notes.as_str().into());
    let (can, how) = match (&u.download, &s.update.install) {
        (Some(_), Install::AppImage { .. }) => (true, tr!("Install and restart")),
        (Some(_), Install::Msi { .. }) => (true, tr!("Download and install")),
        // TRANSLATORS: the verb, on the button that downloads an update
        (Some(_), Install::Dmg { .. }) => (true, tr!("Download")),
        _ => (
            false,
            pagebara_i18n::translate(s.update.install.instructions(), &[]),
        ),
    };
    st.set_update_can_install(can);
    // The dialog shows download progress and errors here.
    st.set_update_status("".into());
    st.set_update_how(how.into());
    st.set_dialog(DialogKind::Update);
}

fn install(app: &Rc<App>, s: &mut State, ui: &AppWindow) {
    let st = ui.global::<AppState>();
    let (Some(u), Some(dir)) = (s.update.offered.clone(), s.profile.updates_dir.clone()) else {
        return;
    };
    let Some(d) = u.download.clone() else { return };
    if s.update.busy {
        return;
    }
    let restarts = !matches!(s.update.install, Install::Dmg { .. });
    if restarts && s.tabs.iter().any(|t| t.session.is_dirty()) {
        st.set_dialog(DialogKind::None);
        return app.show_message(
            s,
            ui,
            &tr!("Save your documents first"),
            &tr!(
                "The app restarts to finish the update. Save or close the documents with changes, then install again."
            ),
        );
    }
    s.update.busy = true;
    st.set_update_status(tr!("Downloading…").into());
    let install = s.update.install.clone();
    let staged = install.staging_path(&d, &dir);
    pagebara_thread::spawn("int-update", move || {
        let size = d.size.unwrap_or(0).max(1);
        let mut last = 0;
        let r = download(&d, &staged, &mut |n| {
            let pct = (n * 100 / size).min(100);
            if pct != last {
                last = pct;
                post_act(move |_, _, ui| {
                    ui.global::<AppState>()
                        .set_update_status(tr!("Downloading… {}%", pct).into());
                });
            }
        })
        .and_then(|()| Ok(pagebara_update::install::apply(&install, &staged)?));
        post_act(move |app, s, ui| installed(app, s, ui, r));
    });
}

#[cfg(feature = "network")]
fn download(
    d: &pagebara_update::Download,
    to: &std::path::Path,
    progress: &mut dyn FnMut(u64),
) -> Result<(), pagebara_update::Error> {
    pagebara_update::net::download(d, to, progress)
}

#[cfg(not(feature = "network"))]
fn download(
    _: &pagebara_update::Download,
    _: &std::path::Path,
    _: &mut dyn FnMut(u64),
) -> Result<(), pagebara_update::Error> {
    Err(pagebara_update::Error::Network(tr!(
        "this build has no network support"
    )))
}

fn installed(
    app: &Rc<App>,
    s: &mut State,
    ui: &AppWindow,
    r: Result<pagebara_update::install::Applied, pagebara_update::Error>,
) {
    use pagebara_update::install::Applied;
    s.update.busy = false;
    let st = ui.global::<AppState>();
    match r {
        Ok(Applied::Restart(exe)) => {
            // The new process becomes the instance and reopens the documents.
            s.instance = None;
            let files: Vec<_> = s
                .tabs
                .iter()
                .filter_map(|t| t.session.path.clone())
                .collect();
            match std::process::Command::new(&exe).args(&files).spawn() {
                Ok(_) => app.quit(s, ui),
                Err(e) => st.set_update_status(tr!("Installed; restart the app ({}).", e).into()),
            }
        }
        Ok(Applied::InstallerStarted) => app.quit(s, ui),
        Ok(Applied::Opened) => {
            st.set_dialog(DialogKind::None);
            app.show_message(
                s,
                ui,
                &tr!("Update downloaded"),
                &tr!("Drag the app from the disk image to Applications, replacing this version."),
            );
        }
        Err(e) => {
            log::warn!("update: {e}");
            st.set_update_status(tr!("The update failed: {}", e).into());
        }
    }
}
