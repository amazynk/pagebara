//! Generated documents covering what pages draw: shared forms, inline images, clipping,
//! marked content, Type 3 and CID fonts, rotated pages and offset crop boxes.

use super::{HELV, Pdf, flate};

/// One page of text, paths and an image in a 300 × 200 page.
pub fn basic() -> Vec<u8> {
    let mut pdf = Pdf::new();
    let img = pdf.stream(
        "/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8",
        &[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0],
    );
    let content = "q 0 0 1 rg 20 20 60 40 re f Q\n\
        q 1 0 0 RG 4 w 100 30 m 180 30 l S Q\n\
        BT /F1 18 Tf 20 150 Td (Hello World) Tj ET\n\
        BT /F1 12 Tf 20 120 Td (Second line) Tj 0 -14 Td (Third line here) Tj ET\n\
        q 60 0 0 40 220 20 cm /Im1 Do Q\n";
    pdf.page(
        &format!("{HELV} /XObject << /Im1 {img} 0 R >>"),
        content,
        "",
    );
    pdf.finish()
}

/// A form drawn twice on page 1 and once on page 2.
pub fn shared_form() -> Vec<u8> {
    let mut pdf = Pdf::new();
    let form = pdf.stream(
        &format!("/Type /XObject /Subtype /Form /BBox [0 0 100 50] /Resources << {HELV} >>"),
        b"0 0.6 0 rg 0 0 100 50 re f BT /F1 14 Tf 0 0 0 rg 10 20 Td (In form) Tj ET",
    );
    let res = format!("{HELV} /XObject << /Fm1 {form} 0 R >>");
    pdf.page(
        &res,
        "q 1 0 0 1 20 20 cm /Fm1 Do Q q 1 0 0 1 150 120 cm /Fm1 Do Q BT /F1 12 Tf 20 180 Td (Page one) Tj ET",
        "",
    );
    pdf.page(&res, "q 1 0 0 1 100 70 cm /Fm1 Do Q", "");
    pdf.finish()
}

/// An inline image, clipping paths and a painted path that also clips.
pub fn inline_and_clip() -> Vec<u8> {
    let mut pdf = Pdf::new();
    let content = "q 40 0 0 40 20 20 cm BI /W 2 /H 2 /CS /RGB /BPC 8 /F /AHx ID 0000FFFF000000FF00FFFF00> EI Q\n\
        q 100 100 80 60 re W n 0 0 1 rg 90 90 120 90 re f Q\n\
        q 1 0 0 rg 200 20 m 280 20 l 240 90 l h W f 0 g 190 10 100 100 re f Q\n\
        BT /F1 12 Tf 20 180 Td (Clipped page) Tj ET";
    pdf.page(HELV, content, "");
    pdf.finish()
}

/// Tagged content: MCIDs, `/ActualText` and optional content.
pub fn marked() -> Vec<u8> {
    let mut pdf = Pdf::new();
    let ocg = pdf.add("<< /Type /OCG /Name (Layer) >>");
    pdf.catalog = format!(
        "/MarkInfo << /Marked true >> /OCProperties << /OCGs [{ocg} 0 R] /D << /ON [{ocg} 0 R] >> >>"
    );
    let content = "/P << /MCID 0 >> BDC BT /F1 14 Tf 20 150 Td (Tagged text) Tj ET EMC\n\
        /Span << /MCID 1 /ActualText (fi) >> BDC BT /F1 14 Tf 20 120 Td (f i) Tj ET EMC\n\
        /OC /L1 BDC 0 0 1 rg 20 20 80 40 re f EMC\n\
        /Artifact BMC 0.5 g 150 20 100 30 re f EMC";
    pdf.page(
        &format!("{HELV} /Properties << /L1 {ocg} 0 R >>"),
        content,
        "/StructParents 0",
    );
    pdf.finish()
}

/// A Type 3 font of two box glyphs.
pub fn type3() -> Vec<u8> {
    let mut pdf = Pdf::new();
    let a = pdf.stream("", b"500 0 0 0 400 600 d1 0 0 400 600 re f");
    let b = pdf.stream("", b"500 0 0 0 400 300 d1 0 0 400 300 re f");
    let font = pdf.add(format!(
        "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 400 600] /FontMatrix [0.001 0 0 0.001 0 0] \
         /CharProcs << /a {a} 0 R /b {b} 0 R >> /Encoding << /Type /Encoding /Differences [97 /a /b] >> \
         /FirstChar 97 /LastChar 98 /Widths [500 500] >>"
    ));
    pdf.page(
        &format!("/Font << /T3 {font} 0 R >>"),
        "BT /T3 20 Tf 20 100 Td (abba) Tj ET",
        "",
    );
    pdf.finish()
}

/// A composite font with two-byte codes and a ToUnicode map (not embedded).
pub fn cid() -> Vec<u8> {
    let mut pdf = Pdf::new();
    let cmap = b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap \
        /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def \
        /CMapName /Adobe-Identity-UCS def /CMapType 2 def \
        1 begincodespacerange <0000> <FFFF> endcodespacerange \
        1 beginbfrange <0021> <007E> <0021> endbfrange \
        1 beginbfchar <0003> <0020> endbfchar \
        endcmap CMapName currentdict /CMap defineresource pop end end";
    let tu = pdf.stream("", cmap);
    let desc = pdf.add(
        "<< /Type /FontDescriptor /FontName /Arial /Flags 32 /FontBBox [-665 -325 2000 1006] \
         /ItalicAngle 0 /Ascent 905 /Descent -212 /CapHeight 716 /StemV 80 >>",
    );
    let cidfont = pdf.add(format!(
        "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Arial /CIDSystemInfo << /Registry (Adobe) \
         /Ordering (Identity) /Supplement 0 >> /FontDescriptor {desc} 0 R /DW 600 /CIDToGIDMap /Identity >>"
    ));
    let font = pdf.add(format!(
        "<< /Type /Font /Subtype /Type0 /BaseFont /Arial /Encoding /Identity-H \
         /DescendantFonts [{cidfont} 0 R] /ToUnicode {tu} 0 R >>"
    ));
    // "CID text" as two-byte codes equal to their Unicode, the space as 3.
    pdf.page(
        &format!("/Font << /C0 {font} 0 R >>"),
        "BT /C0 16 Tf 20 100 Td <00430049004400030074006500780074> Tj ET",
        "",
    );
    pdf.finish()
}

/// Rotated pages and a crop box away from the origin, contents split in two streams.
pub fn rotated_cropped() -> Vec<u8> {
    let mut pdf = Pdf::new();
    for rot in [90, 180, 270] {
        pdf.page(
            HELV,
            "BT /F1 16 Tf 30 100 Td (Turned page) Tj ET 0 0 1 rg 30 30 50 30 re f",
            &format!("/Rotate {rot}"),
        );
    }
    let a = pdf.stream("", b"q 1 0 0 rg 60 60 40 40 re f");
    let b = pdf.add(super::stream_body(
        "/Filter /FlateDecode",
        &flate(b"Q BT /F1 12 Tf 70 120 Td (Cropped) Tj ET"),
    ));
    let p = pdf.add(format!(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /CropBox [50 50 250 180] \
         /Resources << {HELV} >> /Contents [{a} 0 R {b} 0 R] >>"
    ));
    pdf.pages_push(p);
    pdf.finish()
}

/// Nested forms, a shading and a pattern fill.
pub fn nested() -> Vec<u8> {
    let mut pdf = Pdf::new();
    let inner = pdf.stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 40 40]",
        b"1 0 0 rg 0 0 40 40 re f",
    );
    let outer = pdf.stream(
        &format!("/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Resources << /XObject << /In {inner} 0 R >> >>"),
        b"0 0 1 rg 0 0 100 100 re f q 1 0 0 1 30 30 cm /In Do Q",
    );
    let sh = pdf.add(
        "<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 100 0] \
         /Function << /FunctionType 2 /Domain [0 1] /C0 [1 1 0] /C1 [0 1 1] /N 1 >> /Extend [true true] >>",
    );
    pdf.page(
        &format!("/XObject << /Out {outer} 0 R >> /Shading << /Sh1 {sh} 0 R >>"),
        "q 1 0 0 1 20 20 cm /Out Do Q q 150 20 100 60 re W n 1 0 0 1 150 0 cm /Sh1 sh Q",
        "",
    );
    pdf.finish()
}

pub fn all() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("basic", basic()),
        ("shared-form", shared_form()),
        ("inline-clip", inline_and_clip()),
        ("marked", marked()),
        ("type3", type3()),
        ("cid", cid()),
        ("rotated-cropped", rotated_cropped()),
        ("nested", nested()),
    ]
}
