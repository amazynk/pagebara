//! Filling: values, validation, read-only fields, buttons, choices, reset, calculation;
//! incremental save, reopen, qpdf, pdftk and rendered pixels.

mod common;

use common::*;
use pagebara_forms::*;
use pagebara_syntax::Document;

fn save(doc: &Document, name: &str) -> Document {
    let bytes = doc.save_incremental().unwrap();
    qpdf_check(&bytes, name);
    Document::open(bytes).unwrap()
}

fn widget(form: &Form, name: &str, i: usize) -> pagebara_syntax::ObjRef {
    form.field(name).unwrap().widgets[i].id
}

#[test]
fn text_values_and_validation() {
    let mut doc = sample_form("");
    let mut form = Form::load(&doc);
    let c = form.set_text(&mut doc, "name", "Zoë (O'Brien)").unwrap();
    // The first edit also runs the calculations: the empty total becomes 0.
    assert_eq!(c.fields, ["name", "total"]);
    assert_eq!(c.widgets[0], widget(&form, "name", 0));
    assert_eq!(form.field("total").unwrap().text(), "0");
    assert!(c.missing.is_empty());
    assert_eq!(form.field("name").unwrap().text(), "Zoë (O'Brien)");

    // Single-line fields take no line breaks.
    form.set_text(&mut doc, "name", "a\r\nb").unwrap();
    assert_eq!(form.field("name").unwrap().text(), "a b");
    form.set_text(&mut doc, "notes", "one\rtwo").unwrap();
    assert_eq!(form.field("notes").unwrap().text(), "one\ntwo");

    assert_eq!(
        form.set_text(&mut doc, "zip", "123456"),
        Err(Error::TooLong {
            field: "zip".into(),
            max: 5
        })
    );
    assert_eq!(
        form.set_text(&mut doc, "addr.town", "x"),
        Err(Error::ReadOnly("addr.town".into()))
    );
    assert!(matches!(
        form.set_text(&mut doc, "agree", "x"),
        Err(Error::WrongKind { .. })
    ));
    assert_eq!(
        form.set_text(&mut doc, "nope", "x"),
        Err(Error::NoSuchField("nope".into()))
    );
    // Native keystroke scripts check the format.
    assert!(matches!(
        form.set_text(&mut doc, "a", "twelve"),
        Err(Error::Rejected { .. })
    ));
    assert!(matches!(
        form.set_text(&mut doc, "date", "31.02.2024"),
        Err(Error::Rejected { .. })
    ));
    form.set_text(&mut doc, "date", "05.03.2024").unwrap();

    // Characters beyond WinAnsi are reported.
    let c = form.set_text(&mut doc, "notes", "Жx").unwrap();
    assert_eq!(c.missing, ['Ж']);
    // Password fields show bullets only.
    let c = form.set_text(&mut doc, "pin", "Жx").unwrap();
    assert!(c.missing.is_empty());

    let doc = save(&doc, "fill-text");
    let form = Form::load(&doc);
    assert_eq!(form.field("name").unwrap().text(), "a b");
    assert!(!form.need_appearances);
    if let Some(v) = qpdf_values(doc.source(), "fill-text") {
        assert_eq!(value_of(&v, "name"), Some("a b"));
        assert_eq!(value_of(&v, "date"), Some("05.03.2024"));
    }
}

#[test]
fn buttons() {
    let mut doc = sample_form("");
    let mut form = Form::load(&doc);
    // A check box without appearances gets them, with the default on-state.
    let c = form.set_checked(&mut doc, "agree", true).unwrap();
    assert_eq!(c.fields[0], "agree");
    let agree = form.field("agree").unwrap();
    assert_eq!(agree.value, FieldValue::State("Yes".into()));
    assert_eq!(agree.widgets[0].on_state.as_deref(), Some("Yes"));
    assert!(agree.widgets[0].is_on());

    // Existing appearances keep their on-state name.
    form.set_checked(&mut doc, "news", true).unwrap();
    assert_eq!(
        form.field("news").unwrap().value,
        FieldValue::State("On".into())
    );

    // Radio export values come from /Opt; the on-states are indices.
    form.select_radio(&mut doc, "color", Some("Blue")).unwrap();
    let color = form.field("color").unwrap();
    assert_eq!(color.value, FieldValue::State("2".into()));
    assert_eq!(color.export_value().as_deref(), Some("Blue"));
    let on: Vec<bool> = color.widgets.iter().map(|w| w.is_on()).collect();
    assert_eq!(on, [false, false, true]);
    assert!(matches!(
        form.select_radio(&mut doc, "color", Some("Pink")),
        Err(Error::NotAnOption { .. })
    ));
    // NoToggleToOff: clicking the selected button keeps it on.
    let blue = widget(&form, "color", 2);
    assert!(form.click(&mut doc, blue).unwrap().is_empty());
    assert!(form.field("color").unwrap().is_checked());
    let red = widget(&form, "color", 0);
    form.click(&mut doc, red).unwrap();
    assert_eq!(
        form.field("color").unwrap().export_value().as_deref(),
        Some("Red")
    );

    // RadiosInUnison: both "S" buttons turn on together.
    form.select_radio(&mut doc, "size", Some("S")).unwrap();
    let on: Vec<bool> = form
        .field("size")
        .unwrap()
        .widgets
        .iter()
        .map(|w| w.is_on())
        .collect();
    assert_eq!(on, [true, false, true]);
    // A check box click toggles.
    let agree_w = widget(&form, "agree", 0);
    form.click(&mut doc, agree_w).unwrap();
    assert!(!form.field("agree").unwrap().is_checked());
    form.click(&mut doc, agree_w).unwrap();
    assert!(form.field("agree").unwrap().is_checked());

    let doc = save(&doc, "fill-buttons");
    let form = Form::load(&doc);
    assert!(form.field("agree").unwrap().is_checked());
    assert_eq!(
        form.field("size").unwrap().value,
        FieldValue::State("S".into())
    );
    if let Some(v) = pdftk_values(doc.source(), "fill-buttons") {
        assert_eq!(value_of(&v, "agree"), Some("Yes"));
        // pdftk reports the /Opt export value.
        assert_eq!(value_of(&v, "color"), Some("Red"));
        assert_eq!(value_of(&v, "size"), Some("S"));
    }
}

#[test]
fn choices() {
    let mut doc = sample_form("");
    let mut form = Form::load(&doc);
    // By display text or export value.
    form.select_choice(&mut doc, "country", "Italy").unwrap();
    assert_eq!(
        form.field("country").unwrap().value,
        FieldValue::Choices(vec!["it".into()])
    );
    form.select_choice(&mut doc, "country", "fr").unwrap();
    assert!(matches!(
        form.select_choice(&mut doc, "country", "Spain"),
        Err(Error::NotAnOption { .. })
    ));
    assert_eq!(
        form.select_choices(&mut doc, "country", &["fr", "de"]),
        Err(Error::NotMultiSelect("country".into()))
    );
    // An editable combo box takes any text.
    form.set_text(&mut doc, "city", "Zürich").unwrap();
    assert_eq!(
        form.field("city").unwrap().value,
        FieldValue::Choices(vec!["Zürich".into()])
    );
    form.select_choices(&mut doc, "langs", &["German", "English"])
        .unwrap();
    let langs = form.field("langs").unwrap();
    assert_eq!(langs.selection(), [0, 2]);
    assert_eq!(langs.selected_indices, [0, 2]);

    let doc = save(&doc, "fill-choices");
    let form = Form::load(&doc);
    assert_eq!(form.field("langs").unwrap().selection(), [0, 2]);
    if let Some(v) = pdftk_values(doc.source(), "fill-choices") {
        assert_eq!(value_of(&v, "country"), Some("fr"));
        assert_eq!(value_of(&v, "city"), Some("Zürich"));
    }
    if let Some(v) = qpdf_values(doc.source(), "fill-choices") {
        assert_eq!(value_of(&v, "country"), Some("fr"));
    }
}

#[test]
fn clear_and_reset() {
    let mut doc = sample_form("");
    let mut form = Form::load(&doc);
    form.set_text(&mut doc, "name", "x").unwrap();
    form.set_checked(&mut doc, "news", true).unwrap();
    form.select_radio(&mut doc, "color", Some("Red")).unwrap();
    form.clear(&mut doc, "name").unwrap();
    assert_eq!(form.field("name").unwrap().value, FieldValue::None);
    assert_eq!(
        form.clear(&mut doc, "addr.town"),
        Err(Error::ReadOnly("addr.town".into()))
    );

    // Reset one list: only "color" goes back to its default.
    form.reset(&mut doc, Some(&["color"])).unwrap();
    assert_eq!(
        form.field("color").unwrap().export_value().as_deref(),
        Some("Green")
    );
    assert!(form.field("news").unwrap().is_checked());
    // The whole form: defaults where given, else empty.
    form.select_choice(&mut doc, "country", "it").unwrap();
    let c = form.reset(&mut doc, None).unwrap();
    assert!(c.fields.contains(&"country".to_string()));
    assert_eq!(
        form.field("country").unwrap().value,
        FieldValue::Choices(vec!["fr".into()])
    );
    assert_eq!(
        form.field("news").unwrap().value,
        FieldValue::State("On".into())
    );
    assert_eq!(form.field("addr.town").unwrap().value, FieldValue::None);
    assert_eq!(
        form.reset(&mut doc, Some(&["nope"])),
        Err(Error::NoSuchField("nope".into()))
    );
    save(&doc, "fill-reset");
}

#[test]
fn calculation() {
    let mut doc = sample_form("");
    let mut form = Form::load(&doc);
    form.set_text(&mut doc, "a", "1,250.5").unwrap();
    let c = form.set_text(&mut doc, "b", "$-0.25").unwrap();
    // The read-only total is computed, and shown formatted.
    assert!(c.fields.contains(&"total".to_string()), "{c:?}");
    let total = form.field("total").unwrap();
    assert_eq!(total.text(), "1250.25");
    assert_eq!(form.display_text(total).text, "$1,250.25");
    assert_eq!(form.display_text(form.field("b").unwrap()).text, "-$0.25");
    let doc = save(&doc, "fill-calc");
    if let Some(v) = qpdf_values(doc.source(), "fill-calc") {
        assert_eq!(value_of(&v, "total"), Some("1250.25"));
    }
    // The shown total is in the appearance.
    let form = Form::load(&doc);
    let w = form.field("total").unwrap().widgets[0].id;
    let wd = doc.get(w).unwrap();
    let ap = doc.get_in(wd.as_dict().unwrap(), b"AP");
    let n = doc.get_in(ap.as_dict().unwrap(), b"N");
    let data = doc.decode(n.as_stream().unwrap()).unwrap().data;
    let text = String::from_utf8_lossy(&data);
    assert!(text.contains("($1,250.25) Tj"), "{text}");
}

#[test]
fn need_appearances_and_permissions() {
    let mut doc = sample_form("/NeedAppearances true");
    let mut form = Form::load(&doc);
    assert!(form.need_appearances);
    let c = form.set_text(&mut doc, "name", "x").unwrap();
    // The first edit builds every appearance and clears the flag.
    assert!(c.widgets.len() > 10, "{}", c.widgets.len());
    assert!(!form.need_appearances);
    let doc = save(&doc, "fill-needapp");
    let form = Form::load(&doc);
    assert!(!form.need_appearances);
    for f in &form.fields {
        if f.kind != FieldKind::Signature {
            assert!(
                f.widgets.iter().all(|w| w.has_appearance),
                "{} lacks an appearance",
                f.name
            );
        }
    }

    // Filling is refused when the security settings forbid it.
    let mut doc = sample_form("");
    let perms = pagebara_syntax::Permissions {
        fill_forms: false,
        annotate: false,
        ..pagebara_syntax::Permissions::ALL
    };
    doc.set_password("", "owner", perms).unwrap();
    let bytes = doc.save_full().unwrap();
    let mut doc = Document::open(bytes).unwrap();
    let mut form = Form::load(&doc);
    assert_eq!(
        form.set_text(&mut doc, "name", "x"),
        Err(Error::NotPermitted)
    );
    assert!(doc.unlock_owner("owner"));
    form.set_text(&mut doc, "name", "x").unwrap();
}

#[test]
fn rendered_values() {
    let mut doc = sample_form("");
    let mut form = Form::load(&doc);
    form.set_text(&mut doc, "name", "WWWWWW").unwrap();
    form.set_checked(&mut doc, "agree", true).unwrap();
    form.select_radio(&mut doc, "color", Some("Green")).unwrap();
    let doc = save(&doc, "fill-render");
    for (tool, r) in render_all(doc.source(), "fill-render") {
        // Blue text on the yellowish background of "name" (50..300 x 720..744).
        assert!(
            r.any(55.0, 724.0, 200.0, 740.0, is_blue),
            "{tool}: name text"
        );
        assert!(
            r.any(55.0, 724.0, 295.0, 740.0, |p| p[0] > 240
                && p[1] > 240
                && p[2] < 230
                && p[2] > 180),
            "{tool}: name background"
        );
        // A red cross in the check box.
        assert!(
            r.any(52.0, 522.0, 64.0, 534.0, is_red),
            "{tool}: check mark"
        );
        // The second radio button is on (its original appearance: a dark square).
        assert!(r.any(82.0, 482.0, 92.0, 492.0, is_dark), "{tool}: Green on");
        assert!(!r.any(52.0, 482.0, 62.0, 492.0, is_dark), "{tool}: Red off");
    }
}
