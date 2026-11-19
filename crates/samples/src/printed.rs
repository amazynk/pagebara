//! A printed form without fields, for field detection: labels with lines, underscores,
//! boxes (one with its label inside), check boxes, a comb of boxes, a notes box and a
//! signature line with its label below.

use std::fmt::Write as _;

use super::{Pdf, lit};

pub fn printed_form() -> Vec<u8> {
    let mut ops = String::new();
    let text = |ops: &mut String, size: f64, x: f64, y: f64, s: &str| {
        let _ = writeln!(ops, "BT /F1 {size} Tf {x} {y} Td {} Tj ET", lit(s));
    };
    text(&mut ops, 20.0, 72.0, 760.0, "Volunteer sign-up");
    ops.push_str("0.8 w 0 0 0 RG\n");
    // A label and a drawn line.
    text(&mut ops, 11.0, 72.0, 712.0, "Full name:");
    ops.push_str("140 710 m 520 710 l S\n");
    // Underscores typed after the label.
    text(
        &mut ops,
        11.0,
        72.0,
        680.0,
        "E-mail: ______________________________",
    );
    // A box with the label beside it.
    text(&mut ops, 11.0, 72.0, 648.0, "Phone");
    ops.push_str("140 642 200 20 re S\n");
    // A box with its label printed in the top of it.
    ops.push_str("360 600 160 50 re S\n");
    text(&mut ops, 8.0, 364.0, 640.0, "Town");
    // Check boxes.
    ops.push_str("72 600 10 10 re S\n");
    text(&mut ops, 11.0, 88.0, 601.0, "Weekdays");
    ops.push_str("180 600 10 10 re S\n");
    text(&mut ops, 11.0, 196.0, 601.0, "Weekends");
    // A comb of six boxes.
    text(&mut ops, 11.0, 72.0, 566.0, "Postcode");
    for i in 0..6 {
        let _ = writeln!(ops, "{} 560 16 18 re S", 140 + i * 16);
    }
    // A date.
    text(&mut ops, 11.0, 72.0, 530.0, "Date of birth:");
    ops.push_str("150 528 m 260 528 l S\n");
    // Notes.
    text(&mut ops, 11.0, 72.0, 500.0, "Notes");
    ops.push_str("72 400 448 90 re S\n");
    // A signature line with its label below.
    ops.push_str("72 330 m 280 330 l S\n");
    text(&mut ops, 9.0, 72.0, 318.0, "Signature");
    // A rule under a heading is not a field.
    text(&mut ops, 14.0, 72.0, 280.0, "Thank you");
    ops.push_str("72 277 m 150 277 l S\n");
    let mut pdf = Pdf::default();
    let catalog = pdf.add("<</Type/Catalog/Pages 2 0 R>>".into());
    pdf.add("<</Type/Pages/Kids[3 0 R]/Count 1>>".into());
    pdf.add(
        "<</Type/Page/Parent 2 0 R/MediaBox[0 0 612 792]/Resources<</Font<<\
         /F1<</Type/Font/Subtype/Type1/BaseFont/Helvetica/Encoding/WinAnsiEncoding>>>>>>\
         /Contents 4 0 R>>"
            .into(),
    );
    pdf.stream(&ops);
    let info = pdf.add("<</Title(Volunteer sign-up)/Producer(pagebara samples)>>".into());
    pdf.finish(catalog, info)
}
