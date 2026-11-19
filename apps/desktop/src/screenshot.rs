//! Saving window snapshots (`--screenshot`, scripts).

use std::path::Path;

use slint::ComponentHandle;

use crate::AppWindow;

/// Saves the window as PNG; returns whether it worked.
pub fn take(ui: &AppWindow, path: &Path) -> bool {
    pagebara_ui::snapshot::take(ui.window(), path)
}
