//! A flyer for editing page content: a heading, a paragraph of three lines, a picture, a
//! filled box, a rule and a footer, each a separate object.

use std::fmt::Write as _;

use super::{Pdf, lit};

/// The flyer's picture: green hills under a blue sky, 96 × 64 pixels.
fn picture() -> (Vec<u8>, usize, usize) {
    let (w, h) = (96usize, 64usize);
    let mut px = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let hill = 40.0 + 8.0 * ((x as f64) / 12.0).sin();
            let c = if (y as f64) > hill {
                [0x4A, 0xDE - (y as u8), 0x80]
            } else {
                [0x93, 0xC5 + (y as u8) / 2, 0xFD]
            };
            px.extend_from_slice(&c);
        }
    }
    (px, w, h)
}

pub fn flyer() -> Vec<u8> {
    let (px, iw, ih) = picture();
    let packed = pagebara_filters::encode_flate(&px, 9);
    let mut hex = String::with_capacity(packed.len() * 2 + 1);
    for b in packed {
        let _ = write!(hex, "{b:02X}");
    }
    hex.push('>');
    let para = [
        "Join us on Saturday for plants, seeds",
        "and tools from local growers. Entry",
        "is free for members of the garden club.",
    ];
    let mut ops = String::new();
    let _ = writeln!(
        ops,
        "BT /F2 28 Tf 0.1 0.1 0.12 rg 72 700 Td {} Tj ET",
        lit("Spring Garden Fair")
    );
    let _ = writeln!(ops, "BT /F1 12 Tf 0 0 0 rg 16 TL 72 660 Td");
    for (i, l) in para.iter().enumerate() {
        if i > 0 {
            ops.push_str("T* ");
        }
        let _ = writeln!(ops, "{} Tj", lit(l));
    }
    ops.push_str("ET\n");
    ops.push_str("q 200 0 0 150 72 420 cm /Im1 Do Q\n");
    ops.push_str("0.15 0.39 0.92 rg 320 420 200 150 re f\n");
    ops.push_str("2 w 0.2 0.2 0.22 RG 72 390 m 520 390 l S\n");
    let _ = writeln!(
        ops,
        "BT /F1 10 Tf 0.3 0.3 0.33 rg 72 360 Td {} Tj ET",
        lit("Tickets at the gate")
    );
    let mut pdf = Pdf::default();
    let catalog = pdf.add("<</Type/Catalog/Pages 2 0 R/Lang(en-US)>>".into());
    let tree = pdf.add("<</Type/Pages/Kids[3 0 R]/Count 1>>".into());
    let page = pdf.add(
        "<</Type/Page/Parent 2 0 R/MediaBox[0 0 612 792]/Resources<</Font<<\
         /F1<</Type/Font/Subtype/Type1/BaseFont/Helvetica/Encoding/WinAnsiEncoding>>\
         /F2<</Type/Font/Subtype/Type1/BaseFont/Helvetica-Bold/Encoding/WinAnsiEncoding>>>>\
         /XObject<</Im1 5 0 R>>>>/Contents 4 0 R>>"
            .into(),
    );
    let contents = pdf.stream(&ops);
    let image = pdf.add(format!(
        "<</Type/XObject/Subtype/Image/Width {iw}/Height {ih}/ColorSpace/DeviceRGB/BitsPerComponent 8/Filter[/ASCIIHexDecode/FlateDecode]/Length {}>>stream\n{hex}\nendstream",
        hex.len()
    ));
    assert_eq!((catalog, tree, page, contents, image), (1, 2, 3, 4, 5));
    let info = pdf.add("<</Title(Spring Garden Fair)/Producer(pagebara samples)>>".into());
    pdf.finish(catalog, info)
}

#[cfg(test)]
mod tests {
    use pagebara_edit::Kind;

    #[test]
    fn the_flyer_has_separate_objects() {
        let d = pagebara_syntax::Document::open(super::flyer()).unwrap();
        let objs = pagebara_edit::parse(&d, 0).unwrap();
        let kinds: Vec<Kind> = objs.top_level().map(|o| o.kind).collect();
        assert_eq!(
            kinds,
            [
                Kind::Text,
                Kind::Text,
                Kind::Image,
                Kind::Path,
                Kind::Path,
                Kind::Text
            ],
            "{:#?}",
            objs.top_level()
                .map(|o| (o.kind, o.bbox))
                .collect::<Vec<_>>()
        );
        let para = objs.objects[1].text().unwrap();
        assert_eq!(para.lines.len(), 3);
    }
}
