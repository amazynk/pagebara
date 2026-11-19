//! One printed sheet: the page rendered at the printer's resolution where [`super::place`]
//! puts it, on white paper.

use std::sync::atomic::AtomicBool;

use pagebara_render::{Annotations, PixelRect, RenderError, RenderRequest, Renderer};
use pagebara_syntax::{Document, ObjRef, Object};

use super::{PT, Placement};

/// Sheets larger than this many pixels are rendered at a lower resolution.
const MAX_PIXELS: f64 = 80e6;

/// A rendered sheet: opaque grey (1 byte a pixel) or RGB (3 bytes) rows.
#[derive(Clone, Debug)]
pub struct Sheet {
    pub width: u32,
    pub height: u32,
    pub grey: bool,
    pub data: Vec<u8>,
    /// The sheet's size in points.
    pub size_pt: (f64, f64),
}

impl Sheet {
    /// Premultiplied (opaque) RGBA, for showing the sheet.
    pub fn to_rgba(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.width as usize * self.height as usize * 4);
        if self.grey {
            for &v in &self.data {
                out.extend([v, v, v, 255]);
            }
        } else {
            for p in self.data.as_chunks::<3>().0 {
                out.extend([p[0], p[1], p[2], 255]);
            }
        }
        out
    }
}

/// The document as printed: form fields with appearances when `forms`, and without the
/// annotations (or the form fields) left out.
pub fn prepare_document(
    doc: &Document,
    pages: &[ObjRef],
    annotations: bool,
    forms: bool,
) -> Document {
    let mut d = if forms {
        crate::forms::for_display(doc)
    } else {
        doc.clone()
    };
    if annotations && forms {
        return d;
    }
    for &p in pages {
        let Some(mut page) = d.get(p).and_then(|o| o.as_dict().cloned()) else {
            continue;
        };
        let list = d.get_in(&page, b"Annots");
        let Some(list) = list.as_array() else {
            continue;
        };
        let kept: Vec<Object> = list
            .iter()
            .filter(|a| {
                let widget = d
                    .resolve(a)
                    .as_dict()
                    .is_some_and(|a| a.name(b"Subtype") == Some(b"Widget"));
                if widget { forms } else { annotations }
            })
            .cloned()
            .collect();
        page.set(b"Annots", Object::Array(kept));
        d.set(p, Object::Dict(page));
    }
    d
}

/// Renders page `page` onto a sheet at `dpi`, as `placed` says.
pub fn render_sheet(
    renderer: &Renderer,
    page: usize,
    placed: &Placement,
    dpi: u32,
    grey: bool,
    cancel: Option<&AtomicBool>,
) -> Result<Sheet, String> {
    let mut px_per_pt = f64::from(dpi.max(18)) / PT;
    let area = placed.sheet.0 * placed.sheet.1 * px_per_pt * px_per_pt;
    if area > MAX_PIXELS {
        px_per_pt *= (MAX_PIXELS / area).sqrt();
    }
    let width = ((placed.sheet.0 * px_per_pt).round() as u32).max(1);
    let height = ((placed.sheet.1 * px_per_pt).round() as u32).max(1);
    let channels = if grey { 1 } else { 3 };
    let mut data = vec![255u8; width as usize * height as usize * channels];

    // The page's pixels on the sheet, and the part of them that is on the paper.
    let ox = (placed.x * px_per_pt).round() as i64;
    let oy = (placed.y * px_per_pt).round() as i64;
    let pw = (placed.w * px_per_pt).round() as i64;
    let ph = (placed.h * px_per_pt).round() as i64;
    let x0 = ox.max(0);
    let y0 = oy.max(0);
    let x1 = (ox + pw).min(i64::from(width));
    let y1 = (oy + ph).min(i64::from(height));
    if x1 > x0 && y1 > y0 {
        // Layers as printing shows them: print-only stamps on, view-only ones off.
        let layers = pagebara_render::OptionalContentConfig::for_print(renderer.document());
        let rect = PixelRect {
            x: (x0 - ox) as i32,
            y: (y0 - oy) as i32,
            w: (x1 - x0) as u32,
            h: (y1 - y0) as u32,
        };
        let req = RenderRequest {
            scale: (placed.scale * px_per_pt) as f32,
            rect,
            rotation: if placed.rotate { 90 } else { 0 },
            background: Some([255; 4]),
            annotations: Annotations::Print,
            optional_content: Some(&layers),
            cancel,
            straight_alpha: true,
        };
        let img = renderer.render(page, &req).map_err(|e| match e {
            // Not translated: the print dialog compares it.
            RenderError::Cancelled => "Cancelled".to_string(),
            e => format!("page {}: {e:?}", page + 1),
        })?;
        let row = width as usize * channels;
        for y in 0..rect.h as usize {
            let src = &img.data[y * rect.w as usize * 4..(y + 1) * rect.w as usize * 4];
            let at = (y0 as usize + y) * row + x0 as usize * channels;
            let dst = &mut data[at..at + rect.w as usize * channels];
            for (d, s) in dst.chunks_exact_mut(channels).zip(src.as_chunks::<4>().0) {
                if grey {
                    let l = (u32::from(s[0]) * 299 + u32::from(s[1]) * 587 + u32::from(s[2]) * 114)
                        / 1000;
                    d[0] = l as u8;
                } else {
                    d.copy_from_slice(&s[..3]);
                }
            }
        }
    }
    Ok(Sheet {
        width,
        height,
        grey,
        data,
        size_pt: placed.sheet,
    })
}
