//! pagebara for phones and tablets: the Slint UI of `ui/`, driven by `pagebara-app-core` (documents,
//! tiles, search, forms, signatures) through `pagebara-ui`'s page view. Platform services
//! (pickers, sharing, where files live) are behind [`platform::Platform`].

mod app;
pub mod platform;
mod view;

#[cfg(target_os = "android")]
mod android;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub mod script;

use std::path::PathBuf;
use std::rc::Rc;

/// The UI compiled from `ui/*.slint`. Generated code: allowed to use unsafe.
#[allow(unsafe_code, missing_debug_implementations, clippy::all)]
mod ui {
    slint::include_modules!();
}

pub use ui::*;

/// The product name shown to users; one place to change it.
pub const APP_NAME: &str = "pagebara";

/// How to start.
#[derive(Debug, Default)]
pub struct Options {
    /// Documents to open.
    pub files: Vec<PathBuf>,
    /// Keeps settings, recent files and signatures in this folder.
    pub profile: Option<PathBuf>,
    /// Dark or light, else the system's.
    pub dark: Option<bool>,
    /// A UI script to run (desktop hosts).
    pub script: Option<PathBuf>,
}

/// Shows the window and runs until it closes.
pub fn run(
    platform: Rc<dyn platform::Platform>,
    opts: Options,
) -> Result<(), slint::PlatformError> {
    run_sized(platform, opts, None)
}

/// As [`run`], in a window of `size` logical pixels (desktop hosts).
pub fn run_sized(
    platform: Rc<dyn platform::Platform>,
    opts: Options,
    size: Option<(f32, f32)>,
) -> Result<(), slint::PlatformError> {
    use slint::ComponentHandle;
    let ui = MobileWindow::new()?;
    if let Some((w, h)) = size {
        ui.window().set_size(slint::LogicalSize::new(w, h));
    }
    let app = app::App::start(&ui, platform, opts);
    let result = ui.run();
    drop(app);
    result
}

/// Threads for rendering and text work; call once at start.
pub fn init_threads() {
    if let Err(e) = pagebara_thread::rayon_pool(pagebara_thread::GLOBAL_POOL).build_global() {
        log::warn!("rayon global pool: {e}");
    }
}
