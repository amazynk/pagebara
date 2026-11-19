//! `--thumbnail IN OUT SIZE`: page 1 as a PNG for file managers (the freedesktop
//! thumbnailer in `packaging/linux/app.thumbnailer.in`). No window, no GPU, one page.

use std::io;
use std::path::Path;

use pagebara_render::{PixelRect, RenderRequest, Renderer};
use pagebara_syntax::Document;

/// Writes the thumbnail; returns the exit code.
pub fn write(input: &Path, output: &Path, size: u32) -> i32 {
    match render(input, size).and_then(|png| std::fs::write(output, png)) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("pagebara: {}: {e}", input.display());
            1
        }
    }
}

fn render(input: &Path, size: u32) -> io::Result<Vec<u8>> {
    let doc = Document::open(std::fs::read(input)?).map_err(|e| io::Error::other(e.to_string()))?;
    let r = Renderer::new(doc);
    let (w, h) = r.page_size(0).ok_or_else(|| io::Error::other("no pages"))?;
    let scale = size as f32 / w.max(h).max(1.0);
    let (pw, ph) = (
        ((w * scale).round() as u32).clamp(1, size),
        ((h * scale).round() as u32).clamp(1, size),
    );
    let req = RenderRequest::new(
        scale,
        PixelRect {
            x: 0,
            y: 0,
            w: pw,
            h: ph,
        },
    );
    let img = r
        .render(0, &req)
        .map_err(|e| io::Error::other(format!("{e:?}")))?;
    pagebara_png::encode(
        img.width,
        img.height,
        pagebara_png::ColorType::Rgba,
        &img.data,
    )
    .map_err(io::Error::other)
}
