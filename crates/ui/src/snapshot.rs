//! Saving window snapshots as PNG (screenshots, scripts).

use std::path::Path;

use slint::{Rgba8Pixel, SharedPixelBuffer};

/// Saves the window as PNG; returns whether it worked.
pub fn take(window: &slint::Window, path: &Path) -> bool {
    let saved = window
        .take_snapshot()
        .map_err(|e| e.to_string())
        .and_then(|buf| save_png(path, &buf).map_err(|e| e.to_string()));
    match saved {
        Ok(()) => {
            log::info!("screenshot saved to {}", path.display());
            true
        }
        Err(e) => {
            log::error!("screenshot failed: {e}");
            false
        }
    }
}

pub fn save_png(
    path: &Path,
    buf: &SharedPixelBuffer<Rgba8Pixel>,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let png = pagebara_png::encode(
        buf.width(),
        buf.height(),
        pagebara_png::ColorType::Rgba,
        buf.as_bytes(),
    )?;
    std::fs::write(path, png)?;
    Ok(())
}
