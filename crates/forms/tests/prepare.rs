//! Preparing forms: every kind of field created on a blank page, properties, renaming,
//! duplicating, copies across pages, moving, removing, tab order and tagging; saved,
//! reopened, checked with qpdf and drawn by other renderers.

mod common;

#[allow(unused_imports)]
use common::sample_form as _;

use common::*;
use pagebara_annot::{Color, Rect};
use pagebara_forms::*;
use pagebara_syntax::{Document, Object};

fn blank() -> Document {
    Document::open(one_page(
        400,
        500,
        "BT /F1 12 Tf 30 470 Td (Application) Tj ET",
    ))
    .unwrap()
}

fn reopen(doc: &Document, name: &str, incremental: bool) -> Document {
    let bytes = if incremental {
        doc.save_incremental().unwrap()
    } else {
        doc.save_full().unwrap()
    };
    qpdf_check(&bytes, name);
    Document::open(bytes).unwrap()
}

fn add(form: &mut Form, doc: &mut Document, kind: FieldKind, rect: Rect) -> String {
    form.add_field(doc, &NewField::new(kind, 0, rect))
        .unwrap()
        .1
}

/// Every widget has a normal appearance stream (or one per state).
fn all_have_appearances(doc: &Document, form: &Form) {
    for f in &form.fields {
        for w in &f.widgets {
            let d = doc.get(w.id).unwrap();
            let ap = doc.get_in(d.as_dict().unwrap(), b"AP");
            let n = doc.get_in(ap.as_dict().expect("no /AP"), b"N");
            match &*n {
                Object::Stream(_) => {}
                Object::Dict(states) => {
                    assert!(states.len() >= 2, "{}", f.name);
                    for (_, v) in states.iter() {
                        assert!(doc.resolve(v).as_stream().is_some(), "{}", f.name);
                    }
                }
                o => panic!("{}: /N is {o:?}", f.name),
            }
        }
    }
}

#[test]
fn creates_every_kind_of_field() {
    let mut doc = blank();
    let mut form = Form::load(&doc);
    assert!(!form.has_form());
    let text = add(
        &mut form,
        &mut doc,
        FieldKind::Text,
        Rect::new(30.0, 420.0, 230.0, 440.0),
    );
    assert_eq!(text, "Text1");
    let check = add(
        &mut form,
        &mut doc,
        FieldKind::CheckBox,
        Rect::new(30.0, 390.0, 44.0, 404.0),
    );
    let radio = add(
        &mut form,
        &mut doc,
        FieldKind::Radio,
        Rect::new(30.0, 360.0, 44.0, 374.0),
    );
    let (_, second) = form
        .add_widget(
            &mut doc,
            &radio,
            0,
            Rect::new(60.0, 360.0, 74.0, 374.0),
            None,
        )
        .unwrap();
    let mut combo = NewField::new(FieldKind::ComboBox, 0, Rect::new(30.0, 320.0, 180.0, 340.0));
    combo.props.options = ["Red", "Green"]
        .iter()
        .map(|s| ChoiceOption {
            export: s.to_lowercase(),
            display: (*s).into(),
        })
        .collect();
    combo.props.editable = true;
    let combo = form.add_field(&mut doc, &combo).unwrap().1;
    let mut list = NewField::new(FieldKind::ListBox, 0, Rect::new(200.0, 280.0, 350.0, 340.0));
    list.props.options = ["One", "Two", "Three"]
        .iter()
        .map(|s| ChoiceOption {
            export: (*s).into(),
            display: (*s).into(),
        })
        .collect();
    list.props.multi_select = true;
    let list = form.add_field(&mut doc, &list).unwrap().1;
    let mut button = NewField::new(
        FieldKind::PushButton,
        0,
        Rect::new(30.0, 240.0, 130.0, 265.0),
    );
    button.props.caption = "Reset".into();
    button.props.action = ButtonAction::ResetForm;
    let button = form.add_field(&mut doc, &button).unwrap().1;
    let sig = add(
        &mut form,
        &mut doc,
        FieldKind::Signature,
        Rect::new(30.0, 150.0, 230.0, 200.0),
    );
    let mut date = NewField::new(FieldKind::Text, 0, Rect::new(250.0, 420.0, 370.0, 440.0));
    date.props.format = Format::Date("dd.mm.yyyy".into());
    let date = form.add_field(&mut doc, &date).unwrap().1;
    assert_eq!(date, "Date1");
    let mut comb = NewField::new(FieldKind::Text, 0, Rect::new(250.0, 390.0, 370.0, 410.0));
    comb.props.comb = true;
    comb.props.max_len = Some(6);
    comb.name = "postcode".into();
    form.add_field(&mut doc, &comb).unwrap();
    let mut multi = NewField::new(FieldKind::Text, 0, Rect::new(250.0, 200.0, 370.0, 260.0));
    multi.props.multiline = true;
    multi.name = "notes".into();
    form.add_field(&mut doc, &multi).unwrap();
    let mut pw = NewField::new(FieldKind::Text, 0, Rect::new(250.0, 160.0, 370.0, 180.0));
    pw.props.password = true;
    pw.name = "pin".into();
    form.add_field(&mut doc, &pw).unwrap();
    assert_eq!(form.fields.len(), 11);
    assert!(!form.need_appearances);
    all_have_appearances(&doc, &form);
    // Names must be free.
    assert_eq!(
        form.add_field(
            &mut doc,
            &NewField {
                name: text.clone(),
                ..NewField::new(FieldKind::Text, 0, Rect::new(0.0, 0.0, 9.0, 9.0))
            }
        ),
        Err(Error::NameTaken(text.clone()))
    );
    assert!(matches!(
        form.add_field(
            &mut doc,
            &NewField {
                name: "a..b".into(),
                ..NewField::new(FieldKind::Text, 0, Rect::new(0.0, 0.0, 9.0, 9.0))
            }
        ),
        Err(Error::BadName(_))
    ));

    // Saved and reopened: the kinds, flags and options are as made, and every field fills.
    let mut doc = reopen(&doc, "prepared", false);
    let mut form = Form::load(&doc);
    let kind = |n: &str| form.field(n).unwrap().kind;
    assert_eq!(kind(&text), FieldKind::Text);
    assert_eq!(kind(&check), FieldKind::CheckBox);
    assert_eq!(kind(&radio), FieldKind::Radio);
    assert_eq!(kind(&combo), FieldKind::ComboBox);
    assert_eq!(kind(&list), FieldKind::ListBox);
    assert_eq!(kind(&button), FieldKind::PushButton);
    assert_eq!(kind(&sig), FieldKind::Signature);
    assert!(form.field("postcode").unwrap().is_comb());
    assert!(form.field("notes").unwrap().is_multiline());
    assert!(form.field("pin").unwrap().is_password());
    assert!(form.field(&combo).unwrap().is_editable_combo());
    assert!(form.field(&list).unwrap().is_multi_select());
    assert_eq!(
        form.field(&radio).unwrap().export_values(),
        ["Choice1", "Choice2"]
    );
    assert_eq!(
        form.field(&text).unwrap().alternate_name.as_deref(),
        Some("Text1")
    );
    assert_eq!(form.button_action(&doc, &button), ButtonAction::ResetForm);
    assert!(form.script_warnings().is_empty());

    form.set_text(&mut doc, &text, "Ada Lovelace").unwrap();
    form.set_checked(&mut doc, &check, true).unwrap();
    form.click(&mut doc, second).unwrap();
    form.select_choice(&mut doc, &combo, "Blue").unwrap();
    form.select_choices(&mut doc, &list, &["One", "Three"])
        .unwrap();
    form.set_text(&mut doc, &date, "2024-03-07").unwrap();
    assert_eq!(
        form.display_text(form.field(&date).unwrap()).text,
        "07.03.2024"
    );
    form.set_text(&mut doc, "postcode", "SW1A2A").unwrap();
    let doc = reopen(&doc, "prepared-filled", true);
    if let Some(v) = qpdf_values(&doc.save_full().unwrap(), "prepared-values") {
        assert_eq!(value_of(&v, &text), Some("Ada Lovelace"));
        assert_eq!(value_of(&v, &radio), Some("/Choice2"));
        assert_eq!(value_of(&v, &combo), Some("Blue"));
    }
    // Other renderers show the typed text without building appearances themselves.
    for (who, r) in render_all(&doc.save_full().unwrap(), "prepared-render") {
        assert!(
            r.any(30.0, 420.0, 230.0, 440.0, is_dark),
            "{who}: text field is empty"
        );
    }
}

#[test]
fn properties_round_trip() {
    let mut doc = blank();
    let mut form = Form::load(&doc);
    let name = add(
        &mut form,
        &mut doc,
        FieldKind::Text,
        Rect::new(30.0, 420.0, 230.0, 440.0),
    );
    let mut p = form.props(&doc, &name).unwrap();
    p.tooltip = "Your full name".into();
    p.required = true;
    p.default_value = "Anon".into();
    p.font = "Cour".into();
    p.font_size = 11.0;
    p.text_color = Color::Rgb(0.0, 0.0, 1.0);
    p.background = Some(Color::Rgb(1.0, 1.0, 0.8));
    p.border_color = Some(Color::Rgb(1.0, 0.0, 0.0));
    p.border_width = 2.0;
    p.border_style = BorderKind::Dashed;
    p.align = 2;
    p.max_len = Some(20);
    p.format = Format::Number {
        decimals: 2,
        sep: 2,
        currency: "€".into(),
    };
    form.set_props(&mut doc, &name, &p).unwrap();
    let doc2 = reopen(&doc, "props", true);
    let form2 = Form::load(&doc2);
    assert_eq!(form2.props(&doc2, &name).unwrap(), p);
    let f = form2.field(&name).unwrap();
    assert!(f.flags.required());
    assert_eq!(f.max_len, Some(20));
    assert_eq!(f.default_value, FieldValue::Text("Anon".into()));
    // The format is ours: no script warning, and values are shown formatted.
    assert!(form2.script_warnings().is_empty());
    form.set_text(&mut doc, &name, "1234,5").unwrap();
    assert_eq!(
        form.display_text(form.field(&name).unwrap()).text,
        "€1.234,50"
    );
    // Resetting gives the default.
    form.reset(&mut doc, None).unwrap();
    assert_eq!(form.field(&name).unwrap().text(), "Anon");
    // A comb needs a length.
    p.comb = true;
    p.max_len = None;
    assert!(form.set_props(&mut doc, &name, &p).is_err());

    // Check boxes: mark style and export value.
    let check = add(
        &mut form,
        &mut doc,
        FieldKind::CheckBox,
        Rect::new(30.0, 390.0, 44.0, 404.0),
    );
    let mut cp = form.props(&doc, &check).unwrap();
    cp.check_style = '8';
    form.set_props(&mut doc, &check, &cp).unwrap();
    let w = form.field(&check).unwrap().widgets[0].id;
    form.set_export(&mut doc, w, "Agreed").unwrap();
    form.set_checked(&mut doc, &check, true).unwrap();
    let f = form.field(&check).unwrap();
    assert_eq!(f.value, FieldValue::State("Agreed".into()));
    assert_eq!(form.props(&doc, &check).unwrap().check_style, '8');
    assert!(form.set_export(&mut doc, w, "Off").is_err());
}

#[test]
fn rename_duplicate_copy_move_remove() {
    let mut doc = Document::open(two_pages()).unwrap();
    let mut form = Form::load(&doc);
    let name = add(
        &mut form,
        &mut doc,
        FieldKind::Text,
        Rect::new(30.0, 420.0, 230.0, 440.0),
    );
    form.rename(&mut doc, &name, "person.name").unwrap();
    assert!(form.field("person.name").is_some());
    assert_eq!(
        form.field("person.name").unwrap().alternate_name.as_deref(),
        Some("person.name")
    );
    let other = add(
        &mut form,
        &mut doc,
        FieldKind::Text,
        Rect::new(30.0, 380.0, 230.0, 400.0),
    );
    assert!(matches!(
        form.rename(&mut doc, &other, "person.name"),
        Err(Error::NameTaken(_))
    ));
    assert!(matches!(
        form.rename(&mut doc, &other, "person"),
        Err(Error::NameTaken(_))
    ));
    form.rename(&mut doc, &other, "person.town").unwrap();
    // Copies on the second page show the same value.
    form.copy_to_pages(&mut doc, "person.name", &[0, 1])
        .unwrap();
    let f = form.field("person.name").unwrap();
    assert_eq!(f.widgets.len(), 2);
    assert_eq!(f.widgets[1].page, Some(1));
    form.set_text(&mut doc, "person.name", "Grace").unwrap();
    all_have_appearances(&doc, &form);
    // A duplicate is a new field.
    let (_, dup) = form.duplicate(&mut doc, "person.town", 0.0, -40.0).unwrap();
    assert_eq!(dup, "person.town1");
    assert_eq!(form.field(&dup).unwrap().widgets[0].rect.y0, 340.0);
    // Moving redraws.
    let w = form.field(&dup).unwrap().widgets[0].id;
    let c = form
        .set_rects(&mut doc, &[(w, Rect::new(100.0, 100.0, 200.0, 130.0))])
        .unwrap();
    assert_eq!(c.widgets, [w]);
    assert_eq!(
        form.field(&dup).unwrap().widgets[0].rect,
        Rect::new(100.0, 100.0, 200.0, 130.0)
    );
    let doc2 = reopen(&doc, "structure", true);
    let form2 = Form::load(&doc2);
    assert_eq!(form2.field("person.name").unwrap().text(), "Grace");
    assert_eq!(form2.field("person.name").unwrap().widgets.len(), 2);
    // Removing a copy keeps the field; removing the field takes the parent too.
    let copy = form.field("person.name").unwrap().widgets[1].id;
    form.remove_widget(&mut doc, copy).unwrap();
    assert_eq!(form.field("person.name").unwrap().widgets.len(), 1);
    form.remove_field(&mut doc, "person.name").unwrap();
    form.remove_field(&mut doc, "person.town").unwrap();
    form.remove_field(&mut doc, &dup).unwrap();
    assert!(form.fields.is_empty());
    let doc3 = reopen(&doc, "removed", true);
    let cat = doc3.catalog().unwrap();
    let af = doc3.get_in(cat.as_dict().unwrap(), b"AcroForm");
    assert_eq!(
        doc3.get_in(af.as_dict().unwrap(), b"Fields")
            .as_array()
            .unwrap()
            .len(),
        0
    );
    let page = doc3.get(PAGE).unwrap();
    assert!(
        doc3.get_in(page.as_dict().unwrap(), b"Annots")
            .as_array()
            .unwrap_or_default()
            .is_empty()
    );
}

#[test]
fn tab_order_modes() {
    let mut doc = blank();
    let mut form = Form::load(&doc);
    let a = add(
        &mut form,
        &mut doc,
        FieldKind::Text,
        Rect::new(200.0, 400.0, 300.0, 420.0),
    );
    let b = add(
        &mut form,
        &mut doc,
        FieldKind::Text,
        Rect::new(30.0, 400.0, 130.0, 420.0),
    );
    let c = add(
        &mut form,
        &mut doc,
        FieldKind::Text,
        Rect::new(30.0, 300.0, 130.0, 320.0),
    );
    let id = |form: &Form, n: &str| form.field(n).unwrap().widgets[0].id;
    let (wa, wb, wc) = (id(&form, &a), id(&form, &b), id(&form, &c));
    assert_eq!(form.tab_order(0), [wa, wb, wc]);
    form.set_tab_order(&mut doc, 0, &TabOrder::Rows).unwrap();
    assert_eq!(form.tab_order(0), [wb, wa, wc]);
    assert_eq!(form.tab_mode(0), TabOrder::Rows);
    form.set_tab_order(&mut doc, 0, &TabOrder::Columns).unwrap();
    assert_eq!(form.tab_order(0), [wb, wc, wa]);
    form.set_tab_order(&mut doc, 0, &TabOrder::Manual(vec![wc, wa]))
        .unwrap();
    assert_eq!(form.tab_order(0), [wc, wa, wb]);
    let doc2 = reopen(&doc, "tabs", true);
    assert_eq!(
        Form::load(&doc2).tab_mode(0),
        TabOrder::Manual(vec![wc, wa, wb])
    );
}

#[test]
fn tagged_documents_get_form_elements() {
    let mut doc = blank();
    let root = add_obj(
        &mut doc,
        "<</Type/StructTreeRoot/K[]/ParentTree<</Nums[]>>/ParentTreeNextKey 0>>",
    );
    edit(&mut doc, pagebara_syntax::ObjRef::new(1, 0), |d| {
        d.set(b"StructTreeRoot", root);
        d.set(b"MarkInfo", o("<</Marked true>>"));
    });
    let mut form = Form::load(&doc);
    let mut nf = NewField::new(FieldKind::Text, 0, Rect::new(30.0, 420.0, 230.0, 440.0));
    nf.props.tooltip = "First name".into();
    let name = form.add_field(&mut doc, &nf).unwrap().1;
    let w = form.field(&name).unwrap().widgets[0].id;
    let wd = doc.get(w).unwrap();
    assert_eq!(wd.as_dict().unwrap().int(b"StructParent"), Some(0));
    let r = doc.get(root).unwrap();
    let kids = r
        .as_dict()
        .unwrap()
        .get(b"K")
        .unwrap()
        .as_array()
        .unwrap()
        .to_vec();
    assert_eq!(kids.len(), 1);
    let el = doc.get(kids[0].as_objref().unwrap()).unwrap();
    assert_eq!(el.as_dict().unwrap().name(b"S"), Some(&b"Form"[..]));
    let page = doc.get(PAGE).unwrap();
    assert_eq!(page.as_dict().unwrap().name(b"Tabs"), Some(&b"S"[..]));
    reopen(&doc, "tagged", true);
    // A copy on the same page splits the merged field; the element follows the widget.
    form.add_widget(
        &mut doc,
        &name,
        0,
        Rect::new(30.0, 380.0, 230.0, 400.0),
        None,
    )
    .unwrap();
    let f = form.field(&name).unwrap();
    assert_ne!(f.widgets[0].id, f.id);
    let el = doc.get(kids[0].as_objref().unwrap()).unwrap();
    let k = el
        .as_dict()
        .unwrap()
        .get(b"K")
        .unwrap()
        .as_dict()
        .unwrap()
        .clone();
    assert_eq!(
        k.get(b"Obj").and_then(Object::as_objref),
        Some(f.widgets[0].id)
    );
    form.remove_field(&mut doc, &name).unwrap();
    let r = doc.get(root).unwrap();
    assert!(
        r.as_dict()
            .unwrap()
            .get(b"K")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    reopen(&doc, "untagged", true);
}

#[test]
fn button_icons() {
    let mut doc = blank();
    let mut form = Form::load(&doc);
    let mut nf = NewField::new(
        FieldKind::PushButton,
        0,
        Rect::new(30.0, 300.0, 130.0, 360.0),
    );
    nf.props.caption = "Go".into();
    let name = form.add_field(&mut doc, &nf).unwrap().1;
    let px = [255u8, 0, 0, 255].repeat(16);
    let img = pagebara_annot::embed_image(
        &mut doc,
        pagebara_annot::RgbaImage {
            width: 4,
            height: 4,
            pixels: &px,
        },
    )
    .unwrap();
    form.set_button_icon(&mut doc, &name, Some((img, 4, 4)))
        .unwrap();
    let w = &form.field(&name).unwrap().widgets[0];
    assert!(w.mk.icon.is_some());
    assert_eq!(w.mk.text_position, 2);
    let doc = reopen(&doc, "icon", true);
    for (who, r) in render_all(&doc.save_full().unwrap(), "icon-render") {
        assert!(r.any(30.0, 320.0, 130.0, 360.0, is_red), "{who}: no icon");
    }
}

fn add_obj(doc: &mut Document, s: &str) -> pagebara_syntax::ObjRef {
    common::add(doc, s)
}

/// Two 400 x 500 pages (page objects 3 and 6).
fn two_pages() -> Vec<u8> {
    let mut doc = blank();
    let content = common::add_stream(&mut doc, "<<>>", b"");
    let p2 = common::add(
        &mut doc,
        &format!(
            "<</Type/Page/Parent 2 0 R/MediaBox[0 0 400 500]/Resources<<>>/Contents {} 0 R>>",
            content.num
        ),
    );
    edit(&mut doc, pagebara_syntax::ObjRef::new(2, 0), |d| {
        d.set(b"Kids", o(&format!("[3 0 R {} 0 R]", p2.num)));
        d.set(b"Count", 2);
    });
    doc.save_full().unwrap()
}
