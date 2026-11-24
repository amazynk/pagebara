//! Pagebara's own menus in the menu bar (File, Tools, View…), after the application menu
//! the window backend supplies, and the Window and Help menus the system looks after.
//!
//! The app says what each menu holds and answers, per command, what it is called now and
//! whether it can run now; the system asks before it shows an item.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSMenu, NSMenuItem, NSMenuItemValidation};
use objc2_foundation::{NSObject, NSObjectProtocol, NSString};

use super::{guarded, menu_item};

/// An item the system carries out itself, for the window in front.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowCommand {
    /// Minimise (⌘M).
    Minimize,
    Zoom,
    BringAllToFront,
}

/// One line of a menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BarItem {
    /// A command of the app, by its id.
    Command(String),
    Window(WindowCommand),
    Separator,
}

/// What a menu is to the system.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarRole {
    Plain,
    /// The system adds the open windows.
    Window,
    /// The system adds its search field.
    Help,
}

/// One menu of the bar. `key` names it to [`BarCallbacks::menu_title`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BarMenu {
    pub key: String,
    pub role: BarRole,
    pub items: Vec<BarItem>,
}

/// A command's title and whether it can run now; `None` for an id that is not a command.
pub type CommandNow = dyn Fn(&str) -> Option<(String, bool)>;

pub struct BarCallbacks {
    /// A menu's title, by its key, in the language of the interface now.
    pub menu_title: Box<dyn Fn(&str) -> String>,
    pub window_title: fn(WindowCommand) -> String,
    pub command: Box<CommandNow>,
    pub run: Box<dyn Fn(&str)>,
}

impl std::fmt::Debug for BarCallbacks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BarCallbacks").finish_non_exhaustive()
    }
}

struct Ivars {
    /// The command of each item's tag.
    ids: Vec<String>,
    callbacks: BarCallbacks,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "PagebaraMenuBarTarget"]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    struct BarTarget;

    unsafe impl NSObjectProtocol for BarTarget {}

    unsafe impl NSMenuItemValidation for BarTarget {
        #[unsafe(method(validateMenuItem:))]
        fn validate(&self, item: &NSMenuItem) -> bool {
            guarded("validate menu bar", || {
                let Some((title, enabled)) = self.id_of(item).and_then(|id| (self.ivars().callbacks.command)(id))
                else {
                    return false;
                };
                // A command's name can change (Switch to dark mode), and so can the language.
                let title = NSString::from_str(&title);
                if item.title() != title {
                    item.setTitle(&title);
                }
                enabled
            })
            .unwrap_or(false)
        }
    }

    impl BarTarget {
        #[unsafe(method(pagebaraBarAction:))]
        fn perform(&self, item: &NSMenuItem) {
            guarded("menu bar", || {
                if let Some(id) = self.id_of(item) {
                    (self.ivars().callbacks.run)(id);
                }
            });
        }
    }
);

impl BarTarget {
    fn id_of(&self, item: &NSMenuItem) -> Option<&str> {
        let i = usize::try_from(item.tag()).ok()?;
        self.ivars().ids.get(i).map(String::as_str)
    }
}

/// The menus as they are in the bar. NSMenuItem references its target weakly, so the target
/// is kept here with them.
struct Bar {
    target: Retained<BarTarget>,
    /// Each menu with its item in the bar, its key and its role.
    menus: Vec<(Retained<NSMenuItem>, Retained<NSMenu>, String, BarRole)>,
    window_items: Vec<(Retained<NSMenuItem>, WindowCommand)>,
    /// The bar the menus were last put in.
    host: Option<Retained<NSMenu>>,
}

thread_local! {
    static BAR: RefCell<Option<Bar>> = const { RefCell::new(None) };
}

/// Puts `menus` in the bar after the application menu. Call on the main thread once the
/// window backend has installed its menu, and again whenever it may have installed a new
/// one: the same menus are then moved into the new bar (`menus` and `callbacks` of a later
/// call are not used).
pub fn install_menu_bar(menus: impl FnOnce() -> (Vec<BarMenu>, BarCallbacks)) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let Some(bar) = app.mainMenu() else {
        return;
    };
    BAR.with(|slot| {
        let mut slot = slot.borrow_mut();
        let built = slot.get_or_insert_with(|| {
            let (menus, callbacks) = menus();
            build(mtm, menus, callbacks)
        });
        for (top, menu, _, role) in &built.menus {
            if bar.indexOfItem(top) < 0 {
                // An item is in one menu at a time: out of the bar the backend replaced.
                if let Some(old) = &built.host
                    && old.indexOfItem(top) >= 0
                {
                    old.removeItem(top);
                }
                bar.addItem(top);
            }
            match role {
                BarRole::Window => app.setWindowsMenu(Some(menu)),
                BarRole::Help => app.setHelpMenu(Some(menu)),
                BarRole::Plain => {}
            }
        }
        built.host = Some(bar);
    });
}

fn build(mtm: MainThreadMarker, menus: Vec<BarMenu>, callbacks: BarCallbacks) -> Bar {
    let ids: Vec<String> = menus
        .iter()
        .flat_map(|m| &m.items)
        .filter_map(|i| match i {
            BarItem::Command(id) => Some(id.clone()),
            _ => None,
        })
        .collect();
    let title_of = |id: &str| (callbacks.command)(id).map(|(t, _)| t).unwrap_or_default();
    let titles: Vec<String> = ids.iter().map(|id| title_of(id)).collect();
    let menu_titles: Vec<String> = menus
        .iter()
        .map(|m| (callbacks.menu_title)(&m.key))
        .collect();
    let window_title = callbacks.window_title;
    let target = mtm.alloc::<BarTarget>().set_ivars(Ivars { ids, callbacks });
    // SAFETY: BarTarget inherits the default NSObject initializer.
    let target: Retained<BarTarget> = unsafe { msg_send![super(target), init] };
    let action = sel!(pagebaraBarAction:);
    let mut built = Vec::new();
    let mut window_items = Vec::new();
    let mut tag = 0;
    for (m, title) in menus.into_iter().zip(menu_titles) {
        let title = NSString::from_str(&title);
        let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &title);
        menu.setAutoenablesItems(true);
        for item in &m.items {
            match item {
                BarItem::Separator => menu.addItem(&NSMenuItem::separatorItem(mtm)),
                BarItem::Command(_) => {
                    menu.addItem(&menu_item(mtm, &titles[tag], action, &target, tag as isize));
                    tag += 1;
                }
                BarItem::Window(cmd) => {
                    let item = window_item(mtm, *cmd, &window_title(*cmd));
                    menu.addItem(&item);
                    window_items.push((item, *cmd));
                }
            }
        }
        let top = NSMenuItem::new(mtm);
        top.setTitle(&title);
        top.setSubmenu(Some(&menu));
        built.push((top, menu, m.key, m.role));
    }
    Bar {
        target,
        menus: built,
        window_items,
        host: None,
    }
}

/// An item whose action goes to the window in front or the application, as in every
/// AppKit app's Window menu.
fn window_item(mtm: MainThreadMarker, cmd: WindowCommand, title: &str) -> Retained<NSMenuItem> {
    let (action, key) = match cmd {
        WindowCommand::Minimize => (sel!(performMiniaturize:), "m"),
        WindowCommand::Zoom => (sel!(performZoom:), ""),
        WindowCommand::BringAllToFront => (sel!(arrangeInFront:), ""),
    };
    // SAFETY: each selector is a standard action of NSWindow or NSApplication taking the
    // sender; with no target AppKit sends it along the responder chain, and disables the
    // item when nothing answers.
    unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(action),
            &NSString::from_str(key),
        )
    }
}

/// Gives the menus and the Window menu's items their titles again, after the language of
/// the interface has changed. (A command's title follows when its menu is next opened.)
pub fn retitle_menu_bar() {
    if MainThreadMarker::new().is_none() {
        return;
    }
    BAR.with(|slot| {
        let slot = slot.borrow();
        let Some(bar) = slot.as_ref() else {
            return;
        };
        let callbacks = &bar.target.ivars().callbacks;
        for (top, menu, key, _) in &bar.menus {
            let title = NSString::from_str(&(callbacks.menu_title)(key));
            menu.setTitle(&title);
            top.setTitle(&title);
        }
        for (item, cmd) in &bar.window_items {
            item.setTitle(&NSString::from_str(&(callbacks.window_title)(*cmd)));
        }
    });
}

/// The titles of the bar's own menus with their items' (an empty title is a separator), as
/// the system would show them now. Used by UI scripts and tests.
pub fn menu_bar_titles() -> Vec<(String, Vec<String>)> {
    BAR.with(|slot| {
        slot.borrow().as_ref().map_or_else(Vec::new, |bar| {
            bar.menus
                .iter()
                .map(|(_, menu, _, _)| {
                    menu.update();
                    let items = menu
                        .itemArray()
                        .iter()
                        .map(|i| {
                            if i.isSeparatorItem() {
                                String::new()
                            } else {
                                i.title().to_string()
                            }
                        })
                        .collect();
                    (menu.title().to_string(), items)
                })
                .collect()
        })
    })
}
