//! Font style reading and bold/italic variants.

mod common;

use common::*;
use pagebara_syntax::Document;
use pagebara_typeset::{FontClass, VariantSource, find_variant, font_style};

fn style_of(body: &str) -> pagebara_typeset::FontStyle {
    let (doc, f) = doc_with_font(&[], &|_| body.to_string(), b"x");
    let d = doc.get(f).unwrap().as_dict().unwrap().clone();
    font_style(&doc, &d)
}

#[test]
fn reads_names_flags_and_descriptors() {
    let s = style_of("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-BoldOblique >>");
    assert_eq!(
        (s.family.as_str(), s.weight, s.italic, s.class),
        ("Helvetica", 700, true, FontClass::Sans)
    );
    let s = style_of(
        "<< /Type /Font /Subtype /TrueType /BaseFont /ABCDEF+TimesNewRomanPS-BoldItalicMT >>",
    );
    assert_eq!(s.base_font, "TimesNewRomanPS-BoldItalicMT");
    assert!(s.subset && s.is_bold() && s.italic);
    assert_eq!(s.class, FontClass::Serif);
    let s = style_of("<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>");
    assert_eq!(s.class, FontClass::Mono);
    let s = style_of(
        "<< /Type /Font /Subtype /TrueType /BaseFont /Foo /FontDescriptor << /Type /FontDescriptor \
         /FontName /Foo /Flags 34 /FontWeight 600 /ItalicAngle -12 /XHeight 480 >> >>",
    );
    assert_eq!(
        (s.weight, s.italic, s.class, s.x_height),
        (600, true, FontClass::Serif, Some(480.0))
    );
}

#[test]
fn variants_prefer_real_faces() {
    // A document with Helvetica and Helvetica-Bold: bold comes from the document.
    let mut p = Pdf::new();
    let a = p.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    let b = p.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold >>");
    p.page(&format!("/Font << /F1 {a} 0 R /F2 {b} 0 R >>"), "");
    let doc = Document::open(p.finish()).unwrap();
    let base = font_style(
        &doc,
        doc.get(doc.reference(a as u32).unwrap())
            .unwrap()
            .as_dict()
            .unwrap(),
    );
    let v = find_variant(&doc, None, &base, true, false);
    assert_eq!(
        v.source,
        VariantSource::Document(doc.reference(b as u32).unwrap())
    );
    assert!(v.synthetic.is_none());
    // Bold italic: the standard font.
    let v = find_variant(&doc, None, &base, true, true);
    assert_eq!(v.source, VariantSource::Standard("Helvetica-BoldOblique"));
    // Same style.
    assert_eq!(
        find_variant(&doc, None, &base, false, false).source,
        VariantSource::Same
    );
}

#[test]
fn variants_from_installed_family_or_synthetic() {
    let Some(_) = system(DEJAVU) else { return };
    let db = db();
    let (doc, f) = doc_with_font(
        &[],
        &|_| {
            "<< /Type /Font /Subtype /TrueType /BaseFont /ABCDEF+DejaVuSans /FontDescriptor << /Type /FontDescriptor /FontName /ABCDEF+DejaVuSans /Flags 32 /FontFile2 99 0 R >> >>".into()
        },
        b"x",
    );
    let mut base = font_style(&doc, doc.get(f).unwrap().as_dict().unwrap());
    base.embedded = true;
    let v = find_variant(&doc, Some(&db), &base, true, false);
    match &v.source {
        VariantSource::System(face) => {
            assert_eq!(face.family, "DejaVu Sans");
            assert!(face.weight >= 600 && !face.italic);
        }
        other => panic!("{other:?}"),
    }
    // An unknown family gets synthetic styling, marked as such.
    let mut odd = base.clone();
    odd.family = "NoSuchFamilyXyz".into();
    let v = find_variant(&doc, Some(&db), &odd, true, true);
    assert_eq!(v.source, VariantSource::Base);
    assert!(v.synthetic.bold && v.synthetic.italic);
}
