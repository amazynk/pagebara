//! Pagebara commands in the application menu supplied by the window backend.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSMenu, NSMenuItem, NSMenuItemValidation};
use objc2_foundation::{NSObject, NSObjectProtocol, NSString};

use super::{guarded, menu_item};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(isize)]
pub enum AppMenuCommand {
    About,
    Settings,
    Finder,
    Updates,
    Quit,
}

impl AppMenuCommand {
    fn for_item(item: &NSMenuItem) -> Option<Self> {
        [
            Self::About,
            Self::Settings,
            Self::Finder,
            Self::Updates,
            Self::Quit,
        ]
        .into_iter()
        .find(|cmd| *cmd as isize == item.tag())
    }
}

struct Callbacks {
    title: fn(AppMenuCommand) -> String,
    enabled: Box<dyn Fn(AppMenuCommand) -> bool>,
    run: Box<dyn Fn(AppMenuCommand)>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "PagebaraAppMenuTarget"]
    #[thread_kind = MainThreadOnly]
    #[ivars = Callbacks]
    struct MenuTarget;

    unsafe impl NSObjectProtocol for MenuTarget {}

    unsafe impl NSMenuItemValidation for MenuTarget {
        #[unsafe(method(validateMenuItem:))]
        fn validate(&self, item: &NSMenuItem) -> bool {
            guarded("validate application menu", || {
                AppMenuCommand::for_item(item).is_some_and(|cmd| {
                    // The user can change the interface language while the app is running.
                    let title = NSString::from_str(&(self.ivars().title)(cmd));
                    if item.title() != title {
                        item.setTitle(&title);
                    }
                    (self.ivars().enabled)(cmd)
                })
            }).unwrap_or(false)
        }
    }

    impl MenuTarget {
        #[unsafe(method(pagebaraMenuAction:))]
        fn perform(&self, item: &NSMenuItem) {
            guarded("application menu", || {
                if let Some(cmd) = AppMenuCommand::for_item(item)
                    && (self.ivars().enabled)(cmd)
                {
                    (self.ivars().run)(cmd);
                }
            });
        }
    }
);

// NSMenuItem references its target weakly.
thread_local! {
    static TARGET: RefCell<Option<Retained<MenuTarget>>> = const { RefCell::new(None) };
}

fn app_menu(mtm: MainThreadMarker) -> Option<Retained<NSMenu>> {
    let bar = NSApplication::sharedApplication(mtm).mainMenu()?;
    bar.itemAtIndex(0)?.submenu()
}

/// Connects the About and Quit items and adds Pagebara’s Settings and Finder commands.
/// Call on the main thread after the window has shown.
pub fn install_app_menu(
    title: fn(AppMenuCommand) -> String,
    has_updates: bool,
    enabled: impl Fn(AppMenuCommand) -> bool + 'static,
    run: impl Fn(AppMenuCommand) + 'static,
) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let Some(menu) = app_menu(mtm) else {
        return;
    };
    let action = sel!(pagebaraMenuAction:);
    if menu
        .itemArray()
        .iter()
        .any(|item| item.action() == Some(action))
    {
        return;
    }
    menu.setAutoenablesItems(true);
    let callbacks = Callbacks {
        title,
        enabled: Box::new(enabled),
        run: Box::new(run),
    };
    let target = mtm.alloc::<MenuTarget>().set_ivars(callbacks);
    // SAFETY: MenuTarget inherits the default NSObject initializer.
    let target: Retained<MenuTarget> = unsafe { msg_send![super(target), init] };
    // Slint and winit both put About first, but use different action selectors for it.
    for (index, item) in menu.itemArray().iter().enumerate() {
        let cmd = if index == 0 && !item.isSeparatorItem() {
            AppMenuCommand::About
        } else if item.action() == Some(sel!(terminate:)) {
            AppMenuCommand::Quit
        } else {
            continue;
        };
        item.setTitle(&NSString::from_str(&title(cmd)));
        item.setTag(cmd as isize);
        // SAFETY: the target implements this menu action and TARGET keeps it alive.
        unsafe {
            item.setTarget(Some(&target));
            item.setAction(Some(action));
        }
    }
    let at = menu
        .itemArray()
        .iter()
        .position(|i| i.isSeparatorItem())
        .map_or(menu.numberOfItems(), |i| i as isize + 1);
    let mut next = at;
    for cmd in [
        AppMenuCommand::Settings,
        AppMenuCommand::Finder,
        AppMenuCommand::Updates,
    ] {
        if cmd == AppMenuCommand::Updates && !has_updates {
            continue;
        }
        let item = menu_item(mtm, &title(cmd), action, &target, cmd as isize);
        if cmd == AppMenuCommand::Settings {
            item.setKeyEquivalent(&NSString::from_str(","));
        }
        menu.insertItem_atIndex(&item, next);
        next += 1;
    }
    menu.insertItem_atIndex(&NSMenuItem::separatorItem(mtm), next);
    TARGET.with(|slot| slot.replace(Some(target)));
}

/// Selects an enabled application menu item by its displayed title. Used by UI scripts.
pub fn perform_app_menu_item(title: &str) -> Result<(), String> {
    let menu = MainThreadMarker::new()
        .and_then(app_menu)
        .ok_or("no application menu")?;
    menu.update();
    let i = menu.indexOfItemWithTitle(&NSString::from_str(title));
    if i < 0 {
        return Err(format!("no application menu item named {title:?}"));
    }
    if !menu.itemAtIndex(i).is_some_and(|item| item.isEnabled()) {
        return Err(format!("application menu item {title:?} is disabled"));
    }
    menu.performActionForItemAtIndex(i);
    Ok(())
}
