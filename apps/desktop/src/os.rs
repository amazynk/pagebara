//! Starting up as the one running window or handing the files to it, raising the window,
//! and starting another window of our own.

use std::path::Path;

use pagebara_os::{Instance, InstanceKey, OpenFile, OpenRequest};
use slint::ComponentHandle;
use slint::winit_030::{WinitWindowAccessor, winit};

use crate::AppWindow;
use crate::args::Args;

/// How this launch goes on.
#[derive(Debug)]
// Made once per launch: boxing the Windows pipe listener would gain nothing.
#[allow(clippy::large_enum_variant)]
pub enum Launch {
    /// The running window took the files: exit.
    Forwarded,
    /// Show a window: these files, and the channel for later launches when this is the
    /// first instance.
    Run {
        files: Vec<OpenFile>,
        server: Option<pagebara_os::Server>,
    },
}

/// Decides between handing the files to a running window and opening one.
/// `--new-window` and developer runs never hand over; developer runs take part only with
/// their own `--profile` (so a test can hand files to them, but never the user's launches).
pub fn launch(args: &Args) -> Launch {
    let cwd = std::env::current_dir().unwrap_or_default();
    let (request, skipped) =
        OpenRequest::from_args(&args.files, &cwd, args.page, args.search.as_deref());
    for s in skipped {
        log::warn!("cannot open {s}: only local files can be opened");
    }
    let run = |server| Launch::Run {
        files: request.files.clone(),
        server,
    };
    if args.developer_run() && args.profile.is_none() {
        return run(None);
    }
    // Settings > System integration > One running app.
    if !crate::profile::Profile::new(args.profile.clone())
        .settings()
        .system
        .single_instance
    {
        return run(None);
    }
    let Some(key) = InstanceKey::new(args.profile.as_deref()) else {
        return run(None);
    };
    let forward = !args.new_window && !args.developer_run();
    match Instance::start(&key, forward.then_some(&request)) {
        Instance::Forwarded => Launch::Forwarded,
        Instance::Primary(server) => run(Some(server)),
        Instance::Alone => run(None),
    }
}

/// `PAGEBARA_BACKGROUND` set (scripted runs: tests, screenshots): the window opens without
/// taking the keyboard focus from the app in use, and is never raised.
pub fn background() -> bool {
    std::env::var_os("PAGEBARA_BACKGROUND").is_some()
}

/// Opens the window in the background (see [`background`]); on macOS the app does not come
/// to the front and has no Dock icon.
pub fn in_background(selector: slint::BackendSelector) -> slint::BackendSelector {
    #[cfg(target_os = "macos")]
    let selector = {
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
        let mut builder = winit::event_loop::EventLoop::with_user_event();
        builder
            .with_activation_policy(ActivationPolicy::Accessory)
            .with_activate_ignoring_other_apps(false);
        selector.with_winit_event_loop_builder(builder)
    };
    selector.with_winit_window_attributes_hook(|a| a.with_active(false))
}

/// macOS, `PAGEBARA_TITLEBAR=unified`: the window has no title bar of its own. Its close,
/// minimise and zoom buttons sit in the app's bar, which also moves the window. An
/// experiment: off unless asked for.
pub fn unified_titlebar() -> bool {
    cfg!(target_os = "macos")
        && std::env::var_os("PAGEBARA_TITLEBAR").is_some_and(|v| v == "unified")
}

/// Lets the window's content reach under a transparent title bar (see
/// [`unified_titlebar`]).
pub fn with_unified_titlebar(selector: slint::BackendSelector) -> slint::BackendSelector {
    #[cfg(target_os = "macos")]
    let selector = selector.with_winit_window_attributes_hook(|a| {
        use winit::platform::macos::WindowAttributesExtMacOS;
        let a = a
            .with_titlebar_transparent(true)
            .with_fullsize_content_view(true)
            .with_title_hidden(true);
        if background() {
            a.with_active(false)
        } else {
            a
        }
    });
    selector
}

/// Where the window's own buttons start in the app's bar, and the room they take.
const BUTTONS_INSET: f64 = 20.0;
const BUTTONS_ROOM: f32 = 84.0;

/// Places the window's buttons in the app's bar, `height` logical pixels tall, and tells
/// the bar how much room they take (none in full screen, where they are hidden). Called
/// when the window or the bar changes.
pub fn place_window_buttons(ui: &AppWindow, w: &winit::window::Window) {
    if !unified_titlebar() {
        return;
    }
    let chrome = ui.global::<crate::Chrome>();
    let full = w.fullscreen().is_some();
    chrome.set_titlebar_inset(if full { 0.0 } else { BUTTONS_ROOM });
    if full {
        return;
    }
    pagebara_native::place_window_buttons(f64::from(chrome.get_titlebar_height()), BUTTONS_INSET);
}

/// Wires the bar's part in a merged title bar: dragging the window by it, zooming it
/// with a double click, and keeping the window's buttons in place.
pub fn wire_titlebar(ui: &AppWindow) {
    let chrome = ui.global::<crate::Chrome>();
    chrome.set_unified(unified_titlebar());
    if !unified_titlebar() {
        return;
    }
    let weak = ui.as_weak();
    chrome.on_drag_window(move || {
        if let Some(ui) = weak.upgrade() {
            ui.window().with_winit_window(|w| {
                if let Err(e) = w.drag_window() {
                    log::warn!("moving the window: {e}");
                }
            });
        }
    });
    let weak = ui.as_weak();
    chrome.on_zoom_window(move || {
        if let Some(ui) = weak.upgrade() {
            ui.window()
                .with_winit_window(|w| w.set_maximized(!w.is_maximized()));
        }
    });
    // Now, and again once AppKit has laid the title bar out for what changed (a new
    // title, the window coming up): it puts the buttons back where it has them.
    let weak = ui.as_weak();
    let place = move || {
        for delay in [0, 120] {
            let weak = weak.clone();
            slint::Timer::single_shot(std::time::Duration::from_millis(delay), move || {
                if let Some(ui) = weak.upgrade() {
                    ui.window()
                        .with_winit_window(|w| place_window_buttons(&ui, w));
                }
            });
        }
    };
    place();
    chrome.on_titlebar_changed(place);
}

/// Brings the window to the front (on Wayland, where apps may not raise themselves, the
/// window asks for attention instead).
pub fn raise(ui: &AppWindow) {
    if background() {
        return;
    }
    let w = ui.window();
    if w.is_minimized() {
        w.set_minimized(false);
    }
    w.with_winit_window(|w| {
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let wayland = w
            .window_handle()
            .is_ok_and(|h| matches!(h.as_raw(), RawWindowHandle::Wayland(_)));
        if wayland {
            w.request_user_attention(Some(winit::window::UserAttentionType::Informational));
        } else {
            w.focus_window();
        }
    });
}

/// Starts another window of this app showing `path` at `page` (0-based).
pub fn new_window(path: &Path, page: usize, profile: Option<&Path>) -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    let mut c = std::process::Command::new(exe);
    c.arg("--new-window")
        .arg("--page")
        .arg((page + 1).to_string());
    if let Some(p) = profile {
        c.arg("--profile").arg(p);
    }
    c.arg(path).spawn().map(|_| ())
}

/// Starts this app again (with `--profile` when given) with `args`; used by tests to
/// hand files to the running window.
pub fn relaunch(args: &[String], profile: Option<&Path>) -> std::io::Result<std::process::Child> {
    let exe = std::env::current_exe()?;
    let mut c = std::process::Command::new(exe);
    if let Some(p) = profile {
        c.arg("--profile").arg(p);
    }
    c.args(args).spawn()
}
