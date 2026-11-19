//! The Android entry point: `android_main`, which android-activity calls on its own thread
//! once `MainActivity` (a `NativeActivity`) has loaded this library.

use std::rc::Rc;

use slint::android::android_activity::{MainEvent, PollEvent};

use crate::platform::android::Android;

// Exported under this exact name for android-activity to find, hence `no_mangle`, which
// Rust counts as unsafe. Nothing else unsafe happens here.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
fn android_main(app: slint::android::AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag(crate::APP_NAME),
    );
    crate::init_threads();
    let platform = match Android::new(app.clone()) {
        Ok(p) => Rc::new(p),
        Err(e) => {
            log::error!("cannot reach the activity: {e}");
            return;
        }
    };
    crate::platform::android::install(&platform);
    // Coming back to the app (from a picker, or with a document from another app) may
    // bring something for it.
    let result = slint::android::init_with_event_listener(app, |event| {
        if let PollEvent::Main(MainEvent::Resume { .. } | MainEvent::GainedFocus) = event {
            crate::platform::android::wake();
        }
    });
    if let Err(e) = result {
        log::error!("cannot start the UI: {e}");
        return;
    }
    if let Err(e) = crate::run(platform, crate::Options::default()) {
        log::error!("{e}");
    }
}
