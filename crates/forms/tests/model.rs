//! The field model: tree, names, inheritance, flags, values, widgets, tab order, scripts
//! and XFA detection; robustness on public files.

mod common;

use common::*;
use pagebara_annot::Color;
use pagebara_forms::*;
use pagebara_syntax::Document;

#[test]
fn reads_every_kind() {
    let doc = sample_form("");
    let form = Form::load(&doc);
    assert!(form.has_form());
    let kinds: Vec<(&str, FieldKind)> = form
        .fields
        .iter()
        .map(|f| (f.name.as_str(), f.kind))
        .collect();
    for (n, k) in [
        ("name", FieldKind::Text),
        ("agree", FieldKind::CheckBox),
        ("color", FieldKind::Radio),
        ("country", FieldKind::ComboBox),
        ("langs", FieldKind::ListBox),
        ("submit", FieldKind::PushButton),
        ("sig", FieldKind::Signature),
        ("addr.street", FieldKind::Text),
        ("addr.town", FieldKind::Text),
    ] {
        assert!(kinds.contains(&(n, k)), "{n} {k:?} in {kinds:?}");
    }
    let name = form.field("name").unwrap();
    assert!(name.flags.required());
    assert_eq!(name.alternate_name.as_deref(), Some("Your name"));
    assert_eq!(name.widgets.len(), 1);
    let w = &name.widgets[0];
    assert_eq!(w.page, Some(0));
    assert_eq!(w.mk.border, Some(Color::Rgb(0.0, 0.0, 0.0)));
    assert_eq!(w.mk.background, Some(Color::Rgb(1.0, 1.0, 0.8)));
    assert_eq!(
        DefaultAppearance::parse(name.da.as_ref().unwrap().as_bytes()).size,
        12.0
    );

    let zip = form.field("zip").unwrap();
    assert!(zip.is_comb());
    assert_eq!(zip.max_len, Some(5));
    assert_eq!(zip.quadding, 1);
    assert!(form.field("pin").unwrap().is_password());
    assert!(form.field("notes").unwrap().is_multiline());

    // Inherited from the parent: FT and DA; the value is the child's own.
    let town = form.field("addr.town").unwrap();
    assert_eq!(town.partial_name, "town");
    assert!(town.is_read_only());
    assert_eq!(town.text(), "Springfield");
    assert_eq!(town.da.as_deref(), Some("/Helv 9 Tf 0 g"));
    // A field without its own DA takes the form's.
    assert_eq!(
        form.field("pin").unwrap().da.as_deref(),
        Some("/Helv 0 Tf 0 g")
    );

    let color = form.field("color").unwrap();
    assert_eq!(color.widgets.len(), 3);
    assert_eq!(color.export_values(), ["Red", "Green", "Blue"]);
    assert_eq!(color.default_value, FieldValue::State("1".into()));
    assert!(color.flags.contains(FieldFlags::NO_TOGGLE_TO_OFF));
    let size = form.field("size").unwrap();
    assert!(size.flags.contains(FieldFlags::RADIOS_IN_UNISON));
    assert_eq!(size.export_values(), ["S", "L"]);

    let country = form.field("country").unwrap();
    assert_eq!(country.options.len(), 3);
    assert_eq!(country.options[1].display, "Germany");
    assert_eq!(country.value, FieldValue::Choices(vec!["de".into()]));
    assert_eq!(country.selection(), [1]);
    assert!(form.field("city").unwrap().is_editable_combo());
    let langs = form.field("langs").unwrap();
    assert!(langs.is_multi_select());
    assert_eq!(langs.top_index, 1);
    let submit = form.field("submit").unwrap();
    assert_eq!(submit.widgets[0].mk.caption.as_deref(), Some("Submit"));
    assert_eq!(submit.widgets[0].border.kind, BorderKind::Beveled);
    assert_eq!(submit.widgets[0].border.width, 2.0);
    assert_eq!(form.field("rot").unwrap().widgets[0].mk.rotation, 90);
    assert!(form.field("secret").unwrap().widgets[0].is_hidden());
    assert!(form.field("secret").unwrap().flags.no_export());
    assert_eq!(
        form.field("news").unwrap().widgets[0].on_state.as_deref(),
        Some("On")
    );
    assert_eq!(form.signature_fields().len(), 1);
    assert_eq!(form.fields_on_page(0).len(), form.fields.len());
}

#[test]
fn scripts_are_reported() {
    let doc = sample_form("");
    let form = Form::load(&doc);
    assert!(form.has_scripts());
    let a = form.field("a").unwrap();
    assert!(matches!(
        a.scripts.format.as_ref().unwrap().native,
        Some(Native::Number(_))
    ));
    let total = form.field("total").unwrap();
    assert!(matches!(
        &total.scripts.calculate.as_ref().unwrap().native,
        Some(Native::Calculate { op: CalcOp::Sum, fields }) if fields == &["a", "b"]
    ));
    assert_eq!(form.calculation_order, [total.id]);
    let warnings = form.script_warnings();
    assert_eq!(
        warnings,
        [ScriptWarning {
            field: "custom".into(),
            triggers: vec!["K".into()]
        }]
    );
}

#[test]
fn tab_order() {
    let doc = sample_form("");
    let form = Form::load(&doc);
    let annots = form.tab_order(0);
    // Annotation order by default; hidden widgets are skipped.
    let secret = form.field("secret").unwrap().widgets[0].id;
    assert!(!annots.contains(&secret));
    assert_eq!(annots[0], form.field("name").unwrap().widgets[0].id);

    let mut doc = doc;
    edit(&mut doc, PAGE, |d| {
        d.set(b"Tabs", pagebara_syntax::Object::name(b"R"))
    });
    let form = Form::load(&doc);
    let rows = form.tab_order(0);
    let y = |r| {
        form.fields
            .iter()
            .flat_map(|f| &f.widgets)
            .find(|w| w.id == r)
            .unwrap()
            .rect
    };
    for pair in rows.windows(2) {
        let (a, b) = (y(pair[0]), y(pair[1]));
        assert!(
            a.y1 > b.y1 - 4.0,
            "rows go top to bottom: {a:?} before {b:?}"
        );
    }
    // On the same row, left to right: "name" (x 50) before "submit" (x 400).
    let pos = |n: &str| {
        rows.iter()
            .position(|r| *r == form.field(n).unwrap().widgets[0].id)
            .unwrap()
    };
    assert!(pos("name") < pos("submit"));
    edit(&mut doc, PAGE, |d| {
        d.set(b"Tabs", pagebara_syntax::Object::name(b"C"))
    });
    let form = Form::load(&doc);
    let cols = form.tab_order(0);
    let pos = |n: &str| {
        cols.iter()
            .position(|r| *r == form.field(n).unwrap().widgets[0].id)
            .unwrap()
    };
    assert!(pos("name") < pos("a") && pos("a") < pos("rot"));
}

#[test]
fn xfa_detection() {
    // With AcroForm fields: usable, XFA dropped on the first edit.
    let mut doc = sample_form("/XFA[(template) 1 0 R]");
    let mut form = Form::load(&doc);
    assert_eq!(form.xfa, Xfa::WithAcroForm);
    let c = form.set_text(&mut doc, "name", "Ann").unwrap();
    assert!(c.xfa_removed);
    assert_eq!(Form::load(&doc).xfa, Xfa::None);

    // XFA alone.
    let mut doc = Document::open(one_page(200, 200, "")).unwrap();
    let af = add(&mut doc, "<</Fields[]/XFA 1 0 R>>");
    edit(&mut doc, pagebara_syntax::ObjRef::new(1, 0), |d| {
        d.set(b"AcroForm", af)
    });
    let mut form = Form::load(&doc);
    assert_eq!(form.xfa, Xfa::XfaOnly);
    assert_eq!(
        form.generate_appearances(&mut doc, false),
        Err(Error::XfaOnly)
    );
}

#[test]
fn no_form() {
    let mut doc = Document::open(one_page(200, 200, "")).unwrap();
    let mut form = Form::load(&doc);
    assert!(!form.has_form() && form.is_empty());
    assert_eq!(form.set_text(&mut doc, "x", "y"), Err(Error::NoForm));
}

#[test]
fn cycles_and_orphans() {
    let mut doc = Document::open(one_page(300, 300, "")).unwrap();
    // A field whose kid points back at it, and a widget only on the page.
    let a = add(&mut doc, "<</FT/Tx/T(loop)>>");
    let kid = add(
        &mut doc,
        &format!(
            "<</T(k)/Parent {} 0 R/Kids[{} 0 R]/Subtype/Widget/Rect[0 0 10 10]>>",
            a.num, a.num
        ),
    );
    edit(&mut doc, a, |d| {
        d.set(b"Kids", vec![pagebara_syntax::Object::Ref(kid)])
    });
    let orphan = add(
        &mut doc,
        "<</Type/Annot/Subtype/Widget/FT/Tx/T(orphan)/Rect[20 20 120 40]>>",
    );
    let doc = finish(doc, "", &[a], &[kid, orphan]);
    let form = Form::load(&doc);
    let names: Vec<&str> = form.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(names.contains(&"orphan"), "{names:?}");
    assert!(names.contains(&"loop.k"), "{names:?}");
}

/// Public files: the form model must load without panicking; signed PR570 manual's
/// signature is described.
#[test]
fn public_files() {
    let files = [
        "/home/user/projects/ols/LuaManualEn.pdf",
        "/home/user/projects/rubidium/rfs_12pg.pdf",
        "/home/user/projects/pr570/PR570-OSM.pdf",
        "/home/user/projects/fileservice/dunt_api.pdf",
    ];
    for f in files {
        let Ok(bytes) = std::fs::read(f) else {
            eprintln!("{f} missing; skipped");
            continue;
        };
        let Ok(doc) = Document::open(bytes) else {
            continue;
        };
        let form = Form::load(&doc);
        let _ = form.signatures(&doc);
        let _ = export_json(&form);
        if f.ends_with("PR570-OSM.pdf") {
            let sigs = form.signatures(&doc);
            assert_eq!(sigs.len(), 1);
            let s = &sigs[0];
            assert_eq!(s.field, "Signature2");
            assert!(s.signed);
            let br = s.byte_range.as_ref().unwrap();
            assert!(br.well_formed, "{br:?}");
            assert!(br.covers_revision, "{br:?}");
            eprintln!("PR570 signature: {s:?}");
        }
    }
}
