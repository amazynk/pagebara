//! FDF and XFDF, cross-checked with pdftk; annotations through XFDF.

#[path = "../../forms/tests/common/mod.rs"]
mod common;

use std::process::Command;

use common::*;
use pagebara_fdf::*;
use pagebara_forms::{Error, Form};
use pagebara_syntax::Document;

fn filled() -> (Document, Form) {
    let mut doc = sample_form("");
    let mut form = Form::load(&doc);
    form.set_text(&mut doc, "name", "Zoë (Ann)").unwrap();
    form.set_text(&mut doc, "notes", "two\nlines").unwrap();
    form.set_checked(&mut doc, "agree", true).unwrap();
    form.select_radio(&mut doc, "color", Some("Blue")).unwrap();
    form.select_choice(&mut doc, "country", "it").unwrap();
    form.select_choices(&mut doc, "langs", &["English", "Spanish"])
        .unwrap();
    form.set_text(&mut doc, "addr.street", "Main <St> & Co")
        .unwrap();
    form.set_text(&mut doc, "a", "2").unwrap();
    (doc, form)
}

/// The values the two forms agree on must match field by field.
fn assert_same(a: &Form, b: &Form) {
    for n in [
        "name",
        "notes",
        "agree",
        "color",
        "country",
        "langs",
        "addr.street",
        "a",
        "total",
    ] {
        assert_eq!(
            a.field(n).unwrap().value,
            b.field(n).unwrap().value,
            "field {n}"
        );
    }
}

#[test]
fn fdf_round_trip() {
    let (_, form) = filled();
    let fdf = export_fdf(&form, Some("form.pdf"));
    assert!(fdf.starts_with(b"%FDF-1.2"));
    // NoExport fields stay out.
    assert!(!fdf.windows(8).any(|w| w == b"(secret)"));
    let mut doc = sample_form("");
    let mut fresh = Form::load(&doc);
    let report = import_fdf(&mut fresh, &mut doc, &fdf).unwrap();
    assert!(report.unknown.is_empty(), "{:?}", report.unknown);
    // The read-only town keeps its value and is reported.
    assert!(
        report
            .rejected
            .iter()
            .any(|(n, e)| n == "addr.town" && *e == Error::ReadOnly("addr.town".into())),
        "{:?}",
        report.rejected
    );
    assert_same(&form, &fresh);
    let bytes = doc.save_incremental().unwrap();
    qpdf_check(&bytes, "fdf-import");
    assert!(import_fdf(&mut fresh, &mut doc, b"not fdf").is_err());
}

#[test]
fn fdf_with_pdftk() {
    let Some(pdftk) = tool("pdftk") else {
        eprintln!("pdftk not found; skipped");
        return;
    };
    let (_, form) = filled();
    // pdftk fills the original file with our FDF.
    let doc = sample_form("");
    let src = scratch("fdf-pdftk-src.pdf");
    std::fs::write(&src, doc.source()).unwrap();
    let fdf_path = scratch("fdf-pdftk.fdf");
    std::fs::write(&fdf_path, export_fdf(&form, None)).unwrap();
    let out = scratch("fdf-pdftk-out.pdf");
    let st = Command::new(&pdftk)
        .arg(&src)
        .arg("fill_form")
        .arg(&fdf_path)
        .arg("output")
        .arg(&out)
        .output()
        .unwrap();
    assert!(
        st.status.success(),
        "{}",
        String::from_utf8_lossy(&st.stderr)
    );
    let filled = std::fs::read(&out).unwrap();
    let v = pdftk_values(&filled, "fdf-pdftk-out").unwrap();
    assert_eq!(value_of(&v, "name"), Some("Zoë (Ann)"));
    assert_eq!(value_of(&v, "addr.street"), Some("Main <St> & Co"));
    assert_eq!(value_of(&v, "country"), Some("it"));

    // And we read pdftk's FDF of that file.
    let generated = scratch("fdf-pdftk-gen.fdf");
    let st = Command::new(&pdftk)
        .arg(&out)
        .arg("generate_fdf")
        .arg("output")
        .arg(&generated)
        .output()
        .unwrap();
    assert!(st.status.success());
    let mut doc = sample_form("");
    let mut fresh = Form::load(&doc);
    let report = import_fdf(&mut fresh, &mut doc, &std::fs::read(&generated).unwrap()).unwrap();
    assert!(report.unknown.is_empty(), "{:?}", report.unknown);
    // (pdftk writes combo box values as empty strings here, so "country" is left out.)
    for n in ["name", "addr.street", "agree", "color"] {
        assert_eq!(
            fresh.field(n).unwrap().value,
            form.field(n).unwrap().value,
            "{n}"
        );
    }
}

#[test]
fn xfdf_round_trip() {
    let (_, form) = filled();
    let x = export_xfdf(&form, &sample_form(""), Some("a&b.pdf"), false);
    assert!(x.contains("<f href=\"a&amp;b.pdf\"/>"));
    assert!(x.contains("<field name=\"addr\">"));
    assert!(x.contains("Main &lt;St&gt; &amp; Co"));
    assert!(x.contains("<value>English</value><value>Spanish</value>"));
    let mut doc = sample_form("");
    let mut fresh = Form::load(&doc);
    let report = import_xfdf(&mut fresh, &mut doc, &x).unwrap();
    assert!(report.unknown.is_empty());
    assert_same(&form, &fresh);
    assert!(import_xfdf(&mut fresh, &mut doc, "<html/>").is_err());
    // Flat names and unknown fields.
    let flat = "<?xml version=\"1.0\"?><xfdf xmlns=\"http://ns.adobe.com/xfdf/\"><fields>\
                <field name=\"addr.street\"><value>Elm</value></field>\
                <field name=\"ghost\"><value>x</value></field></fields></xfdf>";
    let report = import_xfdf(&mut fresh, &mut doc, flat).unwrap();
    assert_eq!(report.unknown, ["ghost"]);
    assert_eq!(fresh.field("addr.street").unwrap().text(), "Elm");

    // pdftk takes our XFDF too.
    if let Some(pdftk) = tool("pdftk") {
        let src = scratch("xfdf-src.pdf");
        std::fs::write(&src, sample_form("").source()).unwrap();
        let xp = scratch("xfdf.xfdf");
        std::fs::write(&xp, export_xfdf(&form, &sample_form(""), None, false)).unwrap();
        let out = scratch("xfdf-out.pdf");
        let st = Command::new(pdftk)
            .arg(&src)
            .arg("fill_form")
            .arg(&xp)
            .arg("output")
            .arg(&out)
            .output()
            .unwrap();
        assert!(
            st.status.success(),
            "{}",
            String::from_utf8_lossy(&st.stderr)
        );
        let v = pdftk_values(&std::fs::read(&out).unwrap(), "xfdf-out").unwrap();
        assert_eq!(value_of(&v, "name"), Some("Zoë (Ann)"));
    }
}

#[test]
fn data_round_trips() {
    let d = FormData {
        file: Some("ä b.pdf".into()),
        fields: vec![
            ("a.b".into(), Value::Text("x (y) \\ z\nü".into())),
            ("a.c".into(), Value::State("On".into())),
            ("d".into(), Value::List(vec!["1".into(), "2".into()])),
        ],
        annotations: Vec::new(),
    };
    assert_eq!(parse_fdf(&write_fdf(&d)).unwrap(), d);
    let x = parse_xfdf(&write_xfdf(&d)).unwrap();
    assert_eq!(x.file, d.file);
    assert_eq!(x.fields[0], d.fields[0]);
    // XFDF has no names: a state comes back as text.
    assert_eq!(x.fields[1], ("a.c".into(), Value::Text("On".into())));
    assert_eq!(x.fields[2], d.fields[2]);
    assert!(parse_fdf(b"%PDF-1.7").is_err());
    assert!(parse_xfdf("<xfdf").is_err());
}

#[test]
fn annotations_through_xfdf() {
    use pagebara_annot::{Annotation, Color, Kind, MarkupKind, Point, Quad, Rect};
    let (mut doc, form) = filled();
    let mut square = Annotation::new(
        Kind::Square {
            interior: Some(Color::Rgb(0.0, 0.0, 1.0)),
        },
        Rect::new(50.0, 50.0, 120.0, 90.0),
    );
    square.contents = Some("Check <this> & that".into());
    square.author = Some("Ann".into());
    let hl = Annotation::new(
        Kind::Markup {
            kind: MarkupKind::Highlight,
            quads: vec![Quad::from_rect(Rect::new(60.0, 700.0, 200.0, 712.0))],
        },
        Rect::default(),
    );
    let ink = Annotation::new(
        Kind::Ink {
            strokes: vec![vec![Point::new(10.0, 10.0), Point::new(40.0, 30.0)]],
        },
        Rect::default(),
    );
    for a in [&square, &hl, &ink] {
        pagebara_annot::add(&mut doc, PAGE, a).unwrap();
    }
    let x = export_xfdf(&form, &doc, None, true);
    assert!(x.contains("<square page=\"0\""), "{x}");
    assert!(x.contains("interior-color=\"#0000FF\""));
    assert!(x.contains("<contents>Check &lt;this&gt; &amp; that</contents>"));
    assert!(x.contains("<highlight page=\"0\""));
    assert!(x.contains("<gesture>10,10;40,30</gesture>"));
    let data = parse_xfdf(&x).unwrap();
    assert_eq!(data.annotations.len(), 3);
    // Imported into the blank form: values and annotations.
    let mut fresh_doc = sample_form("");
    let mut fresh = Form::load(&fresh_doc);
    let report = import_xfdf(&mut fresh, &mut fresh_doc, &x).unwrap();
    assert_eq!(report.annotations, 3);
    assert_same(&form, &fresh);
    let page = fresh_doc.get(PAGE).unwrap();
    let list = pagebara_annot::annotations(&fresh_doc, page.as_dict().unwrap());
    let sq = list
        .iter()
        .find(|a| matches!(a.kind, Kind::Square { .. }))
        .unwrap();
    assert_eq!(sq.contents.as_deref(), Some("Check <this> & that"));
    assert_eq!(sq.author.as_deref(), Some("Ann"));
    assert!(list.iter().any(|a| matches!(a.kind, Kind::Ink { .. })));
    qpdf_check(&fresh_doc.save_incremental().unwrap(), "xfdf-annots");
}
