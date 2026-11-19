use pagebara_syntax::Document;

use super::*;
use crate::test_pdf;

fn open() -> (Document, Box<dyn FormSession>) {
    let doc = Document::open(test_pdf::form()).unwrap();
    let form = EngineForms.load(&doc);
    (doc, form)
}

fn value(form: &dyn FormSession, name: &str) -> String {
    form.field(name).unwrap().value
}

#[test]
fn lists_every_kind_of_field() {
    let (doc, form) = open();
    let fields = form.fields();
    let kinds: Vec<(&str, FieldKind)> = fields.iter().map(|f| (f.name.as_str(), f.kind)).collect();
    assert_eq!(
        kinds,
        [
            ("name", FieldKind::Text),
            ("notes", FieldKind::Text),
            ("code", FieldKind::Text),
            ("agree", FieldKind::CheckBox),
            ("size", FieldKind::Radio),
            ("colour", FieldKind::ComboBox),
            ("sizes", FieldKind::ListBox),
            ("signature", FieldKind::Signature),
        ]
    );
    assert_eq!(fields[0].label, "Full name");
    assert!(fields[1].multiline);
    assert_eq!(fields[2].comb, Some(5));
    assert_eq!(fields[4].widgets.len(), 2);
    assert_eq!(fields[5].options, ["Red", "Green", "Blue"]);
    assert!(fields[6].multi_select);
    assert!(form.can_fill(&doc));
    assert_eq!(form.xfa(), Xfa::None);
    assert_eq!(form.tab_order(0).len(), 9);
}

#[test]
fn fills_saves_and_reads_back() {
    let (mut doc, mut form) = open();
    form.set_text(&mut doc, "name", "Ada Lovelace").unwrap();
    form.set_text(&mut doc, "notes", "Line one\nLine two")
        .unwrap();
    form.set_text(&mut doc, "code", "AB12").unwrap();
    let e = form.set_text(&mut doc, "code", "ABCDEF").unwrap_err();
    assert!(e.text.contains("at most 5"), "{}", e.text);
    let agree = form.field("agree").unwrap().widgets[0].id;
    let c = form.click(&mut doc, agree).unwrap();
    assert_eq!(c.fields, ["agree"]);
    form.select_radio(&mut doc, "size", Some("L")).unwrap();
    form.select_choice(&mut doc, "colour", "Green").unwrap();
    form.select_choices(&mut doc, "sizes", &["Small", "x"])
        .unwrap();

    let saved = Document::open(doc.save_full().unwrap()).unwrap();
    let back = EngineForms.load(&saved);
    assert_eq!(value(&*back, "name"), "Ada Lovelace");
    assert_eq!(value(&*back, "notes"), "Line one\nLine two");
    assert_eq!(value(&*back, "code"), "AB12");
    assert!(back.field("agree").unwrap().checked);
    let size = back.field("size").unwrap();
    assert_eq!(size.value, "L");
    assert!(!size.widgets[0].on && size.widgets[1].on);
    assert_eq!(value(&*back, "colour"), "Green");
    assert_eq!(value(&*back, "sizes"), "Small, Extra large");
}

#[test]
fn reset_and_flatten() {
    let (mut doc, mut form) = open();
    form.set_text(&mut doc, "name", "Ada").unwrap();
    form.reset(&mut doc).unwrap();
    assert_eq!(value(&*form, "name"), "");
    form.set_text(&mut doc, "name", "Ada").unwrap();
    form.flatten(&mut doc).unwrap();
    let after = EngineForms.load(&doc);
    assert!(after.fields().is_empty());
    assert!(!after.can_fill(&doc));
}

#[test]
fn signature_fields_are_not_verified() {
    let (doc, form) = open();
    let sigs = form.signatures(&doc);
    assert_eq!(sigs.len(), 1);
    assert_eq!(signature_status(&sigs[0]), "Signature: not signed yet");
    let mut signed = sigs[0].clone();
    signed.signed = true;
    signed.signer = Some("Ada".into());
    assert_eq!(signature_status(&signed), "Signature: checking… · by Ada");
}

#[test]
fn display_copy_has_appearances_and_leaves_the_document() {
    let (doc, _) = open();
    let shown = for_display(&doc);
    let has_ap = |d: &Document| {
        let w = d
            .get(pagebara_syntax::ObjRef {
                num: 6,
                generation: 0,
            })
            .unwrap();
        w.as_dict().unwrap().get(b"AP").is_some()
    };
    assert!(has_ap(&shown));
    assert!(!has_ap(&doc));
}

#[test]
fn errors_name_the_problem() {
    let (mut doc, mut form) = open();
    let e = form.set_text(&mut doc, "nope", "x").unwrap_err();
    assert_eq!(e.text, "No field named \"nope\".");
    assert!(!e.permission);
}

#[test]
fn prepares_fields_and_exchanges_data() {
    let mut doc = Document::open(test_pdf::form()).unwrap();
    let mut form = EngineForms.load(&doc);
    let tool = crate::form_prep::Tool::Date;
    let nf = tool.new_field(0, tool.rect_at(300.0, 300.0));
    let (_, name) = form.add_field(&mut doc, &nf).unwrap();
    assert_eq!(name, "Date1");
    form.set_text(&mut doc, &name, "2024-01-02").unwrap();
    let mut p = form.props(&doc, &name).unwrap();
    p.tooltip = "When".into();
    form.set_props(&mut doc, &name, &p).unwrap();
    assert_eq!(form.field(&name).unwrap().label, "When");
    form.rename(&mut doc, &name, "when").unwrap();
    let w = form.field("when").unwrap().widgets[0].id;
    form.set_rects(&mut doc, &[(w, [10.0, 10.0, 110.0, 30.0])])
        .unwrap();
    assert_eq!(
        form.field("when").unwrap().widgets[0].rect,
        [10.0, 10.0, 110.0, 30.0]
    );
    let fdf = form.export_fdf(None);
    let xfdf = form.export_xfdf(&doc, None, false);
    form.clear_all(&mut doc).unwrap();
    assert_eq!(value(&*form, "when"), "");
    let s = form.import_data(&mut doc, &fdf).unwrap();
    assert!(s.unknown.is_empty(), "{:?}", s.unknown);
    assert_eq!(value(&*form, "when"), "2024-01-02");
    form.clear_all(&mut doc).unwrap();
    form.import_data(&mut doc, xfdf.as_bytes()).unwrap();
    assert_eq!(value(&*form, "when"), "2024-01-02");
    assert!(form.import_data(&mut doc, b"garbage").is_err());
    form.remove_field(&mut doc, "when").unwrap();
    assert!(form.field("when").is_none());
}
