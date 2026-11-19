//! Image cases where the rendering disagreed with MuPDF and PDFium's expected images.

#[path = "common/pdf.rs"]
mod pdf;

use pagebara_render::{PixelRect, RenderRequest, Renderer};
use pagebara_syntax::Document;
use pdf::Pdf;

const PALETTE_JP2: &[u8] = include_bytes!("../../codec/jp2/tests/data/palette.jp2");
const GRAY_JP2: &[u8] = include_bytes!("../../codec/jp2/tests/data/gray_rev.jp2");

/// Page 1 at 1 pixel per point, RGBA.
fn render(data: Vec<u8>) -> (u32, Vec<u8>) {
    let r = Renderer::new(Document::open(data).unwrap());
    let (w, h) = r.page_size(0).unwrap();
    let rect = PixelRect {
        x: 0,
        y: 0,
        w: w as u32,
        h: h as u32,
    };
    (
        w as u32,
        r.render(0, &RenderRequest::new(1.0, rect)).unwrap().data,
    )
}

fn jpx_page(jp2: &[u8], space: &str) -> Vec<u8> {
    let info = pagebara_jp2::read_info(jp2).unwrap();
    let mut p = Pdf::new();
    let img = p.stream(
        &format!(
            "/Type/XObject/Subtype/Image/Width {}/Height {}/BitsPerComponent 8/Filter/JPXDecode{space}",
            info.width, info.height
        ),
        jp2,
    );
    p.page(
        (40.0, 40.0),
        &format!("/XObject<</I {img} 0 R>>"),
        "40 0 0 40 0 0 cm /I Do",
    );
    p.finish()
}

/// A one-component JPX image without `/ColorSpace` is grey (it was taken as RGB and
/// came out red; pdf.js issue 5475).
#[test]
fn jpx_without_colour_space_takes_the_files() {
    let (_, px) = render(jpx_page(GRAY_JP2, ""));
    assert!(
        px.as_chunks::<4>()
            .0
            .iter()
            .all(|p| p[0] == p[1] && p[1] == p[2]),
        "not grey"
    );
}

/// Under an `/Indexed` space a JPX image's values are indices into the PDF's palette,
/// not into its own (PDFium's jpxdecode_indexed tests).
#[test]
fn jpx_under_indexed_uses_the_pdf_palette() {
    let blue = "0000ff".repeat(256);
    let (_, px) = render(jpx_page(
        PALETTE_JP2,
        &format!("/ColorSpace[/Indexed/DeviceRGB 255<{blue}>]"),
    ));
    assert!(
        px.as_chunks::<4>().0.iter().all(|p| p[..3] == [0, 0, 255]),
        "the file's palette was used"
    );
}

/// An alpha soft mask is zero outside its group's box even when its transfer function
/// maps zero to more (pdf.js smask_alpha_oob_transfer; MuPDF agrees).
#[test]
fn alpha_mask_outside_its_box_is_zero() {
    let mut p = Pdf::new();
    let g = p.stream(
        "/Type/XObject/Subtype/Form/BBox[10 10 20 20]",
        b"1 g 10 10 10 10 re f",
    );
    p.page(
        (40.0, 40.0),
        &format!(
            "/ExtGState<</M<</SMask<</Type/Mask/S/Alpha/G {g} 0 R/TR<</FunctionType 2/Domain[0 1]/Range[0 1]/N 1/C0[0.5]/C1[1]>>>>>>>>"
        ),
        "/M gs 0 0 1 rg 0 0 40 40 re f",
    );
    let (w, px) = render(p.finish());
    let at = |x: u32, y: u32| &px[((y * w + x) * 4) as usize..][..4];
    assert_eq!(at(2, 2), [255; 4], "outside the box");
    assert_eq!(at(15, 25), [0, 0, 255, 255], "inside the box");
}
