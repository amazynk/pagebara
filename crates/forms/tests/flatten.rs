//! Flattening the whole form or some fields.

mod common;

use common::*;
use pagebara_forms::*;
use pagebara_syntax::Document;

fn filled() -> (Document, Form) {
    let mut doc = sample_form("");
    let mut form = Form::load(&doc);
    form.set_text(&mut doc, "name", "Ann Example").unwrap();
    form.set_checked(&mut doc, "agree", true).unwrap();
    form.select_radio(&mut doc, "color", Some("Green")).unwrap();
    form.set_text(&mut doc, "addr.street", "1 Main Street")
        .unwrap();
    (doc, form)
}

fn annots(doc: &Document) -> usize {
    let p = doc.get(PAGE).unwrap();
    doc.get_in(p.as_dict().unwrap(), b"Annots")
        .as_array()
        .map_or(0, <[_]>::len)
}

#[test]
fn whole_form() {
    let (mut doc, mut form) = filled();
    let c = form.flatten(&mut doc, None).unwrap();
    assert!(c.fields.len() > 20);
    assert!(!form.has_form() && form.is_empty());
    assert_eq!(annots(&doc), 0);
    let bytes = doc.save_incremental().unwrap();
    qpdf_check(&bytes, "flatten-all");
    let doc = Document::open(bytes).unwrap();
    let cat = doc.catalog().unwrap();
    assert!(!cat.as_dict().unwrap().contains(b"AcroForm"));
    assert!(!Form::load(&doc).has_form());
    if let Some(v) = qpdf_values(doc.source(), "flatten-all") {
        assert!(v.is_empty());
    }
    for (tool, r) in render_all(doc.source(), "flatten-all") {
        assert!(r.any(52.0, 722.0, 150.0, 742.0, is_blue), "{tool}: name");
        assert!(r.any(52.0, 522.0, 64.0, 534.0, is_red), "{tool}: check");
        assert!(r.any(82.0, 482.0, 92.0, 492.0, is_dark), "{tool}: radio");
        assert!(r.any(52.0, 302.0, 150.0, 318.0, is_dark), "{tool}: street");
        // The hidden field is not drawn.
        assert!(
            !r.any(402.0, 442.0, 498.0, 458.0, is_dark),
            "{tool}: hidden"
        );
    }
}

#[test]
fn selected_fields() {
    let (mut doc, mut form) = filled();
    let before = form.fields.len();
    form.flatten(&mut doc, Some(&["name", "addr"])).unwrap();
    // "addr" covers both of its fields; the parent goes with them.
    assert_eq!(form.fields.len(), before - 3);
    assert!(form.field("name").is_none() && form.field("addr.town").is_none());
    assert!(form.field("agree").is_some_and(|f| f.is_checked()));
    assert_eq!(
        form.flatten(&mut doc, Some(&["nope"])),
        Err(Error::NoSuchField("nope".into()))
    );
    let bytes = doc.save_incremental().unwrap();
    qpdf_check(&bytes, "flatten-some");
    let doc = Document::open(bytes).unwrap();
    let form = Form::load(&doc);
    assert_eq!(form.fields.len(), before - 3);
    let names: Vec<String> = qpdf_values(doc.source(), "flatten-some")
        .unwrap_or_default()
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert!(!names.iter().any(|n| n == "name" || n.starts_with("addr")));
    for (tool, r) in render_all(doc.source(), "flatten-some") {
        assert!(r.any(52.0, 722.0, 150.0, 742.0, is_blue), "{tool}: name");
        assert!(r.any(52.0, 522.0, 64.0, 534.0, is_red), "{tool}: check");
    }
}

#[test]
fn flatten_need_appearances_form() {
    // Fields without appearances get them before they are drawn.
    let mut doc = sample_form("/NeedAppearances true");
    let mut form = Form::load(&doc);
    edit(
        &mut doc,
        form.field("addr.town").unwrap().widgets[0].id,
        |d| {
            d.remove(b"AP");
        },
    );
    form.flatten(&mut doc, None).unwrap();
    let bytes = doc.save_incremental().unwrap();
    qpdf_check(&bytes, "flatten-needapp");
    for (tool, r) in render_all(&bytes, "flatten-needapp") {
        assert!(r.any(52.0, 272.0, 150.0, 288.0, is_dark), "{tool}: town");
    }
}
