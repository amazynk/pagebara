//! AppKit glue: methods added to winit's application delegate (opened files, the Dock
//! menu), recent documents and the default app for a content type.
#![allow(unsafe_code)]

pub(crate) mod app_menu;

use std::cell::RefCell;
use std::ffi::CStr;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Mutex;

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, ffi, sel};
use objc2_app_kit::{
    NSApplication, NSDocumentController, NSMenu, NSMenuItem, NSWindow, NSWindowButton, NSWorkspace,
};
use objc2_foundation::{NSArray, NSBundle, NSError, NSPoint, NSString, NSURL};
use objc2_uniform_type_identifiers::UTType;

use crate::dock::{self, MenuItem};

struct Handlers {
    on_open: Box<dyn Fn(Vec<PathBuf>)>,
    on_new_window: Box<dyn Fn()>,
}

thread_local! {
    static HANDLERS: RefCell<Option<Rc<Handlers>>> = const { RefCell::new(None) };
}

fn handlers() -> Option<Rc<Handlers>> {
    HANDLERS.with(|h| h.borrow().clone())
}

/// Runs a callback from AppKit, never letting a panic unwind into Objective-C.
fn guarded<T>(what: &str, f: impl FnOnce() -> T) -> Option<T> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => Some(v),
        Err(_) => {
            log::error!("panic in the {what} callback");
            None
        }
    }
}

type OpenUrls = extern "C-unwind" fn(&AnyObject, Sel, &AnyObject, &NSArray<NSURL>);
type DockMenu = extern "C-unwind" fn(&AnyObject, Sel, &AnyObject) -> *mut NSMenu;
type MenuAction = extern "C-unwind" fn(&AnyObject, Sel, &NSMenuItem);

extern "C-unwind" fn open_urls(
    _this: &AnyObject,
    _cmd: Sel,
    _app: &AnyObject,
    urls: &NSArray<NSURL>,
) {
    guarded("open URLs", || {
        let paths = dock::file_paths(
            urls.iter()
                .map(|u| (u.isFileURL(), u.path().map(|p| p.to_string()))),
        );
        if let (Some(h), false) = (handlers(), paths.is_empty()) {
            (h.on_open)(paths);
        }
    });
}

extern "C-unwind" fn dock_menu(this: &AnyObject, _cmd: Sel, _app: &AnyObject) -> *mut NSMenu {
    guarded("Dock menu", || build_dock_menu(this))
        .flatten()
        .map(Retained::autorelease_return)
        .unwrap_or(std::ptr::null_mut())
}

extern "C-unwind" fn dock_new_window(_this: &AnyObject, _cmd: Sel, _sender: &NSMenuItem) {
    guarded("New Window", || {
        if let Some(h) = handlers() {
            (h.on_new_window)();
        }
    });
}

extern "C-unwind" fn dock_open_recent(_this: &AnyObject, _cmd: Sel, sender: &NSMenuItem) {
    guarded("open recent", || {
        let path = usize::try_from(sender.tag())
            .ok()
            .and_then(dock::dock_recent);
        if let (Some(h), Some(p)) = (handlers(), path) {
            (h.on_open)(vec![p]);
        }
    });
}

fn build_dock_menu(target: &AnyObject) -> Option<Retained<NSMenu>> {
    let items = dock::dock_menu();
    if items.is_empty() {
        return None;
    }
    let mtm = MainThreadMarker::new()?;
    let menu = NSMenu::new(mtm);
    for item in items {
        let entry = match item {
            MenuItem::Separator => NSMenuItem::separatorItem(mtm),
            MenuItem::NewWindow(title) => {
                menu_item(mtm, &title, sel!(pagebaraDockNewWindow:), target, 0)
            }
            MenuItem::Recent(title, i) => menu_item(
                mtm,
                &title,
                sel!(pagebaraDockOpenRecent:),
                target,
                i as isize,
            ),
        };
        menu.addItem(&entry);
    }
    Some(menu)
}

fn menu_item(
    mtm: MainThreadMarker,
    title: &str,
    action: Sel,
    target: &AnyObject,
    tag: isize,
) -> Retained<NSMenuItem> {
    let empty = NSString::from_str("");
    // SAFETY: `action` names a method added to `target`'s class with the `v@:@` signature
    // a menu action has. Callers keep the target alive for the lifetime of the menu.
    let item = unsafe {
        let item = NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(action),
            &empty,
        );
        item.setTarget(Some(target));
        item
    };
    item.setTag(tag);
    item
}

/// Adds a method to `cls`; `false` when the class has it already (then it is left alone).
fn add_method(cls: &AnyClass, sel: Sel, imp: Imp, types: &CStr) -> bool {
    // SAFETY: `imp` has the C signature `types` describes, with `sel`'s argument count;
    // the runtime allows adding methods to a registered class at any time.
    unsafe {
        ffi::class_addMethod(
            cls as *const AnyClass as *mut AnyClass,
            sel,
            imp,
            types.as_ptr(),
        )
    }
    .as_bool()
}

pub fn install_open_handler(on_open: Box<dyn Fn(Vec<PathBuf>)>, on_new_window: Box<dyn Fn()>) {
    let Some(mtm) = MainThreadMarker::new() else {
        log::warn!("open handler: not on the main thread");
        return;
    };
    HANDLERS.with(|h| {
        *h.borrow_mut() = Some(Rc::new(Handlers {
            on_open,
            on_new_window,
        }))
    });
    let app = NSApplication::sharedApplication(mtm);
    let Some(delegate) = app.delegate() else {
        log::warn!("open handler: the application has no delegate yet");
        return;
    };
    let delegate: &AnyObject = delegate.as_ref();
    let cls = delegate.class();
    // Each function pointer becomes the untyped `Imp`; the runtime calls it only with the
    // signature registered next to it.
    let methods: [(Sel, Imp, &CStr); 4] = [
        (
            sel!(application:openURLs:),
            // SAFETY: same-size function pointer cast; called as `v@:@@`, as declared.
            unsafe { std::mem::transmute::<OpenUrls, Imp>(open_urls) },
            c"v@:@@",
        ),
        (
            sel!(applicationDockMenu:),
            // SAFETY: same-size function pointer cast; called as `@@:@`, as declared.
            unsafe { std::mem::transmute::<DockMenu, Imp>(dock_menu) },
            c"@@:@",
        ),
        (
            sel!(pagebaraDockNewWindow:),
            // SAFETY: same-size function pointer cast; called as `v@:@`, as declared.
            unsafe { std::mem::transmute::<MenuAction, Imp>(dock_new_window) },
            c"v@:@",
        ),
        (
            sel!(pagebaraDockOpenRecent:),
            // SAFETY: same-size function pointer cast; called as `v@:@`, as declared.
            unsafe { std::mem::transmute::<MenuAction, Imp>(dock_open_recent) },
            c"v@:@",
        ),
    ];
    for (sel, imp, types) in methods {
        if !add_method(cls, sel, imp, types) {
            log::warn!("the application delegate already has {sel}; left as it is");
        }
    }
}

/// Runs `f` on the main thread: now when this is it, else soon.
fn on_main(f: impl FnOnce(MainThreadMarker) + Send + 'static) {
    match MainThreadMarker::new() {
        Some(mtm) => f(mtm),
        None => DispatchQueue::main().exec_async(move || {
            if let Some(mtm) = MainThreadMarker::new() {
                f(mtm);
            }
        }),
    }
}

pub fn note_recent(path: &Path) {
    let path = path.to_string_lossy().into_owned();
    on_main(move |mtm| {
        let url = NSURL::fileURLWithPath(&NSString::from_str(&path));
        NSDocumentController::sharedDocumentController(mtm).noteNewRecentDocumentURL(&url);
    });
}

pub fn clear_recent() -> bool {
    on_main(|mtm| {
        let dc = NSDocumentController::sharedDocumentController(mtm);
        // SAFETY: `clearRecentDocuments:` ignores its sender; nil is allowed.
        unsafe { dc.clearRecentDocuments(None) };
    });
    true
}

pub fn default_app_for_type(uti: &str) -> Option<PathBuf> {
    let ty = UTType::typeWithIdentifier(&NSString::from_str(uti))?;
    let url = NSWorkspace::sharedWorkspace().URLForApplicationToOpenContentType(&ty)?;
    url.path().map(|p| PathBuf::from(p.to_string()))
}

pub fn set_default_app_for_type(
    uti: &str,
    app: &Path,
    done: Box<dyn FnOnce(Result<(), String>) + Send>,
) {
    let Some(ty) = UTType::typeWithIdentifier(&NSString::from_str(uti)) else {
        return done(Err(format!("unknown type {uti}")));
    };
    let ws = NSWorkspace::sharedWorkspace();
    if !objc2::available!(macos = 12.0) {
        return done(Err("needs macOS 12 or later".into()));
    }
    let url = NSURL::fileURLWithPath(&NSString::from_str(&app.to_string_lossy()));
    let done = Mutex::new(Some(done));
    let block = RcBlock::new(move |err: *mut NSError| {
        // SAFETY: AppKit passes nil or a valid NSError that lives for this call.
        let err = unsafe { err.as_ref() };
        let r = match err {
            None => Ok(()),
            Some(e) => Err(e.localizedDescription().to_string()),
        };
        if let Some(f) = crate::lock(&done).take() {
            guarded("default app", || f(r));
        }
    });
    ws.setDefaultApplicationAtURL_toOpenContentType_completionHandler(&url, &ty, Some(&block));
}

pub fn bundle_path() -> Option<PathBuf> {
    let p = PathBuf::from(NSBundle::mainBundle().bundlePath().to_string());
    (p.extension().is_some_and(|e| e == "app")).then_some(p)
}

/// Puts the close, minimise and zoom buttons of the app's windows with a transparent
/// title bar (the ones whose content reaches under it) in the middle of a bar `height`
/// points tall at the top of the window, the first one `inset` points in. The title bar's
/// container is made that tall for it. AppKit lays the title bar out again when the window
/// changes size or title, so this is called again then.
pub fn place_window_buttons(height: f64, inset: f64) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    for window in NSApplication::sharedApplication(mtm).windows().iter() {
        if window.titlebarAppearsTransparent() {
            place_buttons(&window, height, inset);
        }
    }
}

fn place_buttons(window: &NSWindow, height: f64, inset: f64) {
    let kinds = [
        NSWindowButton::CloseButton,
        NSWindowButton::MiniaturizeButton,
        NSWindowButton::ZoomButton,
    ];
    let Some(close) = window.standardWindowButton(kinds[0]) else {
        return;
    };
    // The buttons are in the title bar's view, which is in its container.
    // SAFETY: plain getters of views that the window keeps alive.
    let Some(bar) = (unsafe { close.superview() }) else {
        return;
    };
    let Some(container) = (unsafe { bar.superview() }) else {
        return;
    };
    let mut frame = container.frame();
    let top = frame.origin.y + frame.size.height;
    frame.size.height = height;
    frame.origin.y = top - height;
    container.setFrame(frame);
    let size = close.frame().size;
    let step = window
        .standardWindowButton(kinds[1])
        .map_or(20.0, |b| {
            (b.frame().origin.x - close.frame().origin.x).abs()
        })
        .max(size.width);
    let bar = bar.frame().size;
    // Right-to-left systems have them at the other end.
    let mirrored = close.frame().origin.x > bar.width / 2.0;
    for (i, kind) in kinds.into_iter().enumerate() {
        if let Some(b) = window.standardWindowButton(kind) {
            let along = inset + i as f64 * step;
            let x = if mirrored {
                bar.width - along - size.width
            } else {
                along
            };
            b.setFrameOrigin(NSPoint::new(x, (bar.height - size.height) / 2.0));
        }
    }
}
