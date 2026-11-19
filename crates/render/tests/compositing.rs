//! Compositing rules checked against what other readers (and the standard) do, found by
//! comparing renderings with MuPDF.

#[path = "common/pdf.rs"]
mod pdf;

use pagebara_render::{PixelRect, RenderRequest, Renderer};
use pagebara_syntax::Document;
use pdf::Pdf;

/// Renders page 1 at 1 pixel per point; returns the RGBA of pixel (x, y), y from the top.
fn pixel(data: Vec<u8>, x: u32, y: u32) -> [u8; 4] {
    let r = Renderer::new(Document::open(data).unwrap());
    let (w, h) = r.page_size(0).unwrap();
    let rect = PixelRect {
        x: 0,
        y: 0,
        w: w as u32,
        h: h as u32,
    };
    let img = r.render(0, &RenderRequest::new(1.0, rect)).unwrap();
    let o = ((y * rect.w + x) * 4) as usize;
    img.data[o..o + 4].try_into().unwrap()
}

/// `B` with transparency: the stroke knocks out the fill beneath it (ISO 32000-2,
/// 11.7.4.4), so the inner half of a translucent stroke shows the page, not the fill.
#[test]
fn fill_and_stroke_is_one_object() {
    let mut p = Pdf::new();
    p.page(
        (100.0, 100.0),
        "/ExtGState<</T<</CA 0.5/ca 0.5>>>>",
        "/T gs 20 w 1 0 0 rg 0 1 0 RG 20 20 60 60 re B",
    );
    let file = p.finish();
    // Inside the rectangle and the stroke's inner half: green at half over white.
    let c = pixel(file.clone(), 50, 25);
    assert!(
        c[0].abs_diff(128) <= 3 && c[1] == 255 && c[2].abs_diff(128) <= 3,
        "{c:?}"
    );
    // Inside, away from the stroke: red at half over white.
    let c = pixel(file.clone(), 50, 50);
    assert!(c[0] == 255 && c[1].abs_diff(128) <= 3, "{c:?}");
    // Separate fill and stroke operators composite normally.
    let mut p = Pdf::new();
    p.page(
        (100.0, 100.0),
        "/ExtGState<</T<</CA 0.5/ca 0.5>>>>",
        "/T gs 20 w 1 0 0 rg 0 1 0 RG 20 20 60 60 re f 20 20 60 60 re S",
    );
    let c = pixel(p.finish(), 50, 25);
    assert!(c[0].abs_diff(128) <= 3 && c[1].abs_diff(191) <= 3, "{c:?}");
}

/// An inline image giving both abbreviated and full keys uses the abbreviations, its own
/// keys (the PDF Association's inline image abbreviation test).
#[test]
fn inline_image_abbreviations_win() {
    for content in [
        "q 100 0 0 100 0 0 cm BI /W 1 /H 1 /BPC 8 /CS /G /D [0 1] /Decode [1 0] ID \u{0} EI Q",
        "q 100 0 0 100 0 0 cm BI /W 1 /H 1 /BPC 8 /CS /G /ColorSpace /DeviceRGB ID \u{0}~~ EI Q",
    ] {
        let mut p = Pdf::new();
        p.page((100.0, 100.0), "", content);
        assert_eq!(pixel(p.finish(), 50, 50), [0, 0, 0, 255], "{content}");
    }
}

/// An axial shading whose axis has no length paints nothing (as PDFium and MuPDF).
#[test]
fn zero_length_axis_paints_nothing() {
    let mut p = Pdf::new();
    p.page(
        (100.0, 100.0),
        "/Shading<</S<</ShadingType 2/ColorSpace/DeviceRGB/Coords[50 50 50 50]/Extend[true true]/Function<</FunctionType 2/Domain[0 1]/C0[1 0 0]/C1[0 0 1]/N 1>>>>>>",
        "/S sh",
    );
    assert_eq!(pixel(p.finish(), 20, 20), [255; 4]);
}

/// A form drawn under a soft mask is masked as one isolated group: a Screen blend inside
/// it does not reach the page (PDFium's smask_blend case, which MuPDF draws alike).
#[test]
fn soft_masked_form_is_an_isolated_group() {
    let mut p = Pdf::new();
    let mask = p.stream(
        "/Type/XObject/Subtype/Form/BBox[0 0 100 100]",
        b"0 g 0 0 100 100 re f",
    );
    let inner = p.stream(
        "/Type/XObject/Subtype/Form/BBox[0 0 100 100]",
        b"0 g 0 0 100 100 re f",
    );
    let outer = p.stream(
        &format!(
            "/Type/XObject/Subtype/Form/BBox[0 0 100 100]/Resources<</ExtGState<</S<</BM/Screen/ca 0.6>>>>/XObject<</I {inner} 0 R>>>>"
        ),
        b"/S gs /I Do",
    );
    p.page(
        (100.0, 100.0),
        &format!(
            "/ExtGState<</M<</SMask<</Type/Mask/S/Alpha/G {mask} 0 R>>>>>>/XObject<</F {outer} 0 R>>"
        ),
        "/M gs /F Do",
    );
    // Black at 0.6 over white: 102 (screening white would leave white).
    let c = pixel(p.finish(), 50, 50);
    assert!(
        c[0].abs_diff(102) <= 2 && c[0] == c[1] && c[1] == c[2],
        "{c:?}"
    );
}
