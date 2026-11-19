//! Thin wrappers over the engine shared by the workers.

use pagebara_render::{PixelRect, RenderRequest, Renderer, RgbaImage};
use pagebara_syntax::{Dict, Document, Error};

/// Why a file did not open.
#[derive(Debug)]
pub enum OpenError {
    Password,
    Other(String),
}

/// Opens a file the way the app does (the empty password is tried automatically).
pub fn open(bytes: Vec<u8>) -> Result<Document, OpenError> {
    match Document::open(bytes) {
        Ok(d) => Ok(d),
        Err(Error::PasswordRequired(_)) => Err(OpenError::Password),
        Err(e) => Err(OpenError::Other(e.to_string())),
    }
}

/// Renders a whole page at `dpi` on white, shrunk to fit `max_side` pixels.
pub fn render_page(
    r: &Renderer,
    page: usize,
    dpi: f32,
    max_side: f32,
) -> Result<RgbaImage, String> {
    let (w, h) = r.page_size(page).ok_or_else(|| format!("no page {page}"))?;
    let scale = (dpi / 72.0).min(max_side / w.max(h).max(1.0));
    let rect = PixelRect {
        x: 0,
        y: 0,
        w: ((w * scale).ceil() as u32).max(1),
        h: ((h * scale).ceil() as u32).max(1),
    };
    r.render(page, &RenderRequest::new(scale, rect))
        .map_err(|e| e.to_string())
}

/// A page's resources, its own or inherited.
pub fn resources(doc: &Document, page: &Dict) -> Dict {
    let mut cur = page.clone();
    for _ in 0..64 {
        if let Some(r) = doc.get_in(&cur, b"Resources").as_dict() {
            return r.clone();
        }
        let parent = doc.get_in(&cur, b"Parent");
        let Some(p) = parent.as_dict() else { break };
        cur = p.clone();
    }
    Dict::new()
}

/// Mean absolute difference per channel (0–255) of two images of one size, over RGB.
pub fn mean_abs_diff(a: &RgbaImage, b: &RgbaImage) -> Option<f64> {
    if (a.width, a.height) != (b.width, b.height) || a.data.is_empty() {
        return None;
    }
    let sum: u64 = a
        .data
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.data.as_chunks::<4>().0)
        .map(|(p, q)| (0..3).map(|i| u64::from(p[i].abs_diff(q[i]))).sum::<u64>())
        .sum();
    Some(sum as f64 / (a.data.len() / 4 * 3) as f64)
}
