//! Hostile files found by corpus runs and fuzzing: each must render quickly and within
//! bounded memory.

#[path = "common/pdf.rs"]
mod pdf;

use std::time::{Duration, Instant};

use pagebara_render::{PixelRect, RenderRequest, Renderer};
use pagebara_syntax::Document;
use pdf::Pdf;

/// Renders every page at `scale`, failing when that takes longer than `budget`.
fn render_within(data: Vec<u8>, scale: f32, budget: Duration) -> Vec<Vec<u8>> {
    let r = Renderer::new(Document::open(data).expect("opens"));
    let start = Instant::now();
    let pages = (0..r.page_count())
        .map(|i| {
            let (w, h) = r.page_size(i).unwrap();
            let rect = PixelRect {
                x: 0,
                y: 0,
                w: (w * scale).ceil() as u32,
                h: (h * scale).ceil() as u32,
            };
            r.render(i, &RenderRequest::new(scale, rect)).unwrap().data
        })
        .collect();
    assert!(start.elapsed() < budget, "took {:?}", start.elapsed());
    pages
}

/// A tiling pattern whose cell draws a form that fills with the same pattern (PDFium
/// bug 451265): every level multiplied the cells drawn, and rendering never ended.
#[test]
fn self_painting_pattern_ends() {
    let mut p = Pdf::new();
    // Objects 3 (form A), 4 (form B), 5 (pattern) refer to each other; the forms have no
    // bounding box to clip with.
    let form_a = 3;
    let form_b = 4;
    let pattern = 5;
    p.stream(
        &format!("/Subtype /Form /Resources << /XObject << /B {form_b} 0 R >> >>"),
        b"/B Do",
    );
    p.stream(
        &format!("/Subtype /Form /Resources << /Pattern << /P {pattern} 0 R >> >>"),
        b"q /Pattern cs /P scn 0 0 224.72 160.4 re f Q",
    );
    // The fill reaches just past the cell on each side, into its neighbours.
    p.stream(
        &format!(
            "/PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 225 161] /Resources << /XObject << /A {form_a} 0 R >> >>"
        ),
        b"/A Do",
    );
    p.page(
        (300.0, 300.0),
        &format!("/XObject << /A {form_a} 0 R >>"),
        "1 0 0 1 20 20 cm /A Do",
    );
    for scale in [0.5, 2.0] {
        render_within(p.clone().finish(), scale, Duration::from_secs(10));
    }
}

/// A 2 × 2 image with a 34862 × 4332 soft mask (pdf.js issue 16263) was enlarged to the
/// mask's size, 600 MB of colour, and rendered for half a minute.
#[test]
fn tiny_image_with_huge_soft_mask() {
    let (mw, mh) = (34_862usize, 4_332usize);
    // A 1-bit mask: alternate rows on and off, Flate-compressed.
    let row = mw.div_ceil(8);
    let mut raw = Vec::with_capacity(row * mh);
    for y in 0..mh {
        raw.extend(std::iter::repeat_n(if y % 2 == 0 { 0xff } else { 0 }, row));
    }
    let mask = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6);
    let mut p = Pdf::new();
    let m = p.stream(
        &format!(
            "/Type /XObject /Subtype /Image /Width {mw} /Height {mh} /ColorSpace /DeviceGray /BitsPerComponent 1 /Filter /FlateDecode"
        ),
        &mask,
    );
    let img = p.stream(
        &format!(
            "/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8 /SMask {m} 0 R"
        ),
        &[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0],
    );
    let content: String = (0..8)
        .map(|i| format!("q 200 0 0 20 10 {} cm /I Do Q ", 10 + i * 30))
        .collect();
    p.page(
        (300.0, 300.0),
        &format!("/XObject << /I {img} 0 R >>"),
        &content,
    );
    render_within(p.finish(), 1.0, Duration::from_secs(5));
}
