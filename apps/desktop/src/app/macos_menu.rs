//! The macOS menu bar: the application menu's commands, shared with the settings dialogs,
//! and the menus after it (File, Tools, View…, Window, Help), which hold the commands of the
//! All tools list under the same names.

use pagebara_i18n::tr;
use pagebara_native::{AppMenuCommand, BarCallbacks, BarItem, BarMenu, BarRole, WindowCommand};
use slint::{ComponentHandle, Model};

use super::post_act;
use crate::{AppState, AppWindow, DialogKind, KeyboardOwner, PaletteItem, PaletteState};

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

/// The Help menu's commands. In the All tools list they are under the app's name.
const HELP: [&str; 2] = ["shortcuts", "discord"];
/// Commands of the All tools list that the application menu has already.
const IN_APP_MENU: [&str; 2] = ["settings", "about"];
/// Commands that start a new part of their menu.
const SEPARATOR_BEFORE: [&str; 6] = ["save", "print", "close", "organize", "sidebar", "theme"];
const WINDOW_KEY: &str = "window";
const HELP_KEY: &str = "help";

/// The menus after the application menu: one for each group of the All tools list, in the
/// list's order and named by the group's first command, then Window and Help.
fn bar_menus(items: &[PaletteItem]) -> Vec<BarMenu> {
    let mut menus: Vec<BarMenu> = Vec::new();
    let mut group = None;
    for item in items {
        let id = item.id.as_str();
        if HELP.contains(&id) || IN_APP_MENU.contains(&id) {
            continue;
        }
        if group != Some(&item.group) {
            group = Some(&item.group);
            menus.push(BarMenu {
                key: id.to_owned(),
                role: BarRole::Plain,
                items: Vec::new(),
            });
        }
        let Some(menu) = menus.last_mut() else {
            continue;
        };
        if SEPARATOR_BEFORE.contains(&id) && !menu.items.is_empty() {
            menu.items.push(BarItem::Separator);
        }
        menu.items.push(BarItem::Command(id.to_owned()));
    }
    menus.push(BarMenu {
        key: WINDOW_KEY.to_owned(),
        role: BarRole::Window,
        items: vec![
            BarItem::Window(WindowCommand::Minimize),
            BarItem::Window(WindowCommand::Zoom),
            BarItem::Separator,
            BarItem::Window(WindowCommand::BringAllToFront),
        ],
    });
    menus.push(BarMenu {
        key: HELP_KEY.to_owned(),
        role: BarRole::Help,
        items: HELP
            .iter()
            .map(|id| BarItem::Command((*id).to_owned()))
            .collect(),
    });
    menus
}

fn window_title(cmd: WindowCommand) -> String {
    match cmd {
        WindowCommand::Minimize => tr!("Minimize"),
        WindowCommand::Zoom => tr!("Zoom"),
        WindowCommand::BringAllToFront => tr!("Bring All to Front"),
    }
}

fn commands(ui: &AppWindow) -> Vec<PaletteItem> {
    ui.global::<PaletteState>().get_items().iter().collect()
}

/// A menu's title: Window, Help, or the group of the command the menu is named by.
fn menu_title(ui: &AppWindow, key: &str) -> String {
    match key {
        WINDOW_KEY => tr!("Window"),
        HELP_KEY => tr!("Help"),
        _ => commands(ui)
            .iter()
            .find(|i| i.id == key)
            .map(|i| i.group.to_string())
            .unwrap_or_default(),
    }
}

/// A command's name now, and whether its menu item can be chosen: the command is enabled
/// and the document has the keyboard (no dialog, no field being typed in), as for its key.
fn command(ui: &AppWindow, id: &str) -> Option<(String, bool)> {
    let item = commands(ui).into_iter().find(|i| i.id == id)?;
    let on = item.enabled && ui.get_keyboard_owner() == KeyboardOwner::Document;
    Some((item.title.to_string(), on))
}

fn install_bar(ui: &AppWindow) {
    pagebara_native::install_menu_bar(|| {
        let (titles, state) = (ui.as_weak(), ui.as_weak());
        let callbacks = BarCallbacks {
            menu_title: Box::new(move |key| {
                titles
                    .upgrade()
                    .map(|ui| menu_title(&ui, key))
                    .unwrap_or_default()
            }),
            window_title,
            command: Box::new(move |id| state.upgrade().and_then(|ui| command(&ui, id))),
            run: Box::new(|id| {
                let id = id.to_owned();
                post_act(move |_app, _s, ui| {
                    if command(ui, &id).is_some_and(|(_, on)| on) {
                        crate::os::raise(ui);
                        ui.global::<PaletteState>().invoke_run(id.as_str().into());
                    }
                });
            }),
        };
        (bar_menus(&commands(ui)), callbacks)
    });
}

/// The menus' titles in the language the interface has just changed to.
pub fn retitle() {
    pagebara_native::retitle_menu_bar();
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
    // After the application menu: the backend's bar must be there for both.
    install_bar(ui);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, group: &str) -> PaletteItem {
        PaletteItem {
            id: id.into(),
            group: group.into(),
            ..PaletteItem::default()
        }
    }

    /// Each group of the All tools list is a menu, named by its first command; the
    /// commands the application menu has are left out, and the app's own group becomes
    /// Help, after Window.
    #[test]
    fn the_bar_follows_the_all_tools_list() {
        let items = [
            item("open", "File"),
            item("save", "File"),
            item("print", "File"),
            item("reflow", "View"),
            item("theme", "View"),
            item("shortcuts", "Pagebara"),
            item("settings", "Pagebara"),
            item("discord", "Pagebara"),
            item("about", "Pagebara"),
        ];
        let menus = bar_menus(&items);
        let keys: Vec<&str> = menus.iter().map(|m| m.key.as_str()).collect();
        assert_eq!(keys, ["open", "reflow", WINDOW_KEY, HELP_KEY]);
        let cmd = |id: &str| BarItem::Command(id.to_owned());
        assert_eq!(
            menus[0].items,
            [
                cmd("open"),
                BarItem::Separator,
                cmd("save"),
                BarItem::Separator,
                cmd("print")
            ]
        );
        assert_eq!(
            menus[1].items,
            [cmd("reflow"), BarItem::Separator, cmd("theme")]
        );
        assert_eq!(menus[2].role, BarRole::Window);
        assert_eq!(menus[3].role, BarRole::Help);
        assert_eq!(menus[3].items, [cmd("shortcuts"), cmd("discord")]);
        // No command is in the bar twice, and none of the application menu's is in it.
        let all: Vec<&BarItem> = menus.iter().flat_map(|m| &m.items).collect();
        for id in IN_APP_MENU {
            assert!(!all.contains(&&cmd(id)), "{id}");
        }
    }

    /// A menu never starts with a separator, even when its first command is one that
    /// starts a new part.
    #[test]
    fn a_menu_does_not_start_with_a_separator() {
        let menus = bar_menus(&[item("save", "File"), item("close", "File")]);
        assert_eq!(
            menus[0].items.first(),
            Some(&BarItem::Command("save".to_owned()))
        );
        assert_eq!(menus[0].items.len(), 3);
    }
}
