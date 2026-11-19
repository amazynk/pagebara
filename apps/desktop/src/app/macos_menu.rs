//! Commands shared by the macOS application menu and the settings dialogs.

use pagebara_i18n::tr;
use pagebara_native::AppMenuCommand;
use slint::ComponentHandle;

use super::post_act;
use crate::{AppState, AppWindow, DialogKind, KeyboardOwner};

fn title(cmd: AppMenuCommand) -> String {
    match cmd {
        AppMenuCommand::About => tr!("About {}", pagebara_identity::NAME),
        AppMenuCommand::Settings => tr!("Settings…"),
        AppMenuCommand::Finder => tr!("Finder Quick Actions…"),
        AppMenuCommand::Updates => tr!("Check for Updates…"),
        AppMenuCommand::Quit => tr!("Quit {}", pagebara_identity::NAME),
    }
}

fn enabled(ui: &AppWindow, cmd: AppMenuCommand) -> bool {
    ui.get_keyboard_owner() == KeyboardOwner::Document
        || (ui.global::<AppState>().get_dialog() == DialogKind::Settings
            && cmd != AppMenuCommand::Settings)
}

pub fn install(ui: &AppWindow) {
    let weak = ui.as_weak();
    let has_updates = cfg!(feature = "network") && ui.global::<AppState>().get_updates_available();
    pagebara_native::install_app_menu(
        title,
        has_updates,
        move |cmd| weak.upgrade().is_some_and(|ui| enabled(&ui, cmd)),
        |cmd| {
            post_act(move |app, s, ui| {
                if !enabled(ui, cmd) {
                    return;
                }
                crate::os::raise(ui);
                let st = ui.global::<AppState>();
                match cmd {
                    AppMenuCommand::About => app.show_about(s, ui),
                    AppMenuCommand::Settings => app.show_dialog(s, ui, DialogKind::Settings),
                    AppMenuCommand::Finder => app.integration_action(s, ui, "", "open"),
                    AppMenuCommand::Updates => {
                        if st.get_dialog() != DialogKind::Settings {
                            app.show_dialog(s, ui, DialogKind::Settings);
                            // The dialog starts on System, so its section-change callback
                            // does not run on this first visit.
                            app.show_system_settings(s, ui);
                        }
                        st.set_settings_section(2);
                        super::update::action(app, s, ui, "check-now");
                    }
                    AppMenuCommand::Quit => app.continue_quit(s, ui),
                }
            })
        },
    );
}
