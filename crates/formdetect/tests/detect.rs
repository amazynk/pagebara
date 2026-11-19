//! Detection on the printed sample form: what is found, how it is named, and that the
//! accepted fields fill and save.

use pagebara_formdetect::{accept, detect, detect_all};
use pagebara_forms::{FieldKind, Form};
use pagebara_syntax::Document;

fn find<'a>(
    list: &'a [pagebara_formdetect::Suggestion],
    name: &str,
) -> &'a pagebara_formdetect::Suggestion {
    list.iter().find(|s| s.name == name).unwrap_or_else(|| {
        panic!(
            "no {name:?} in {:#?}",
            list.iter()
                .map(|s| (&s.name, s.kind, s.rect))
                .collect::<Vec<_>>()
        )
    })
}

#[test]
fn finds_the_printed_form_fields() {
    let doc = Document::open(pagebara_samples::printed_form()).unwrap();
    let list = detect(&doc, 0);
    let names: Vec<&str> = list.iter().map(|s| s.name.as_str()).collect();
    let name = find(&list, "Full name");
    assert_eq!(name.kind, FieldKind::Text);
    assert!((name.rect.x0 - 140.0).abs() < 1.0 && (name.rect.x1 - 520.0).abs() < 1.0);
    assert!(
        name.rect.y0 >= 710.0 && name.rect.height() >= 12.0,
        "{:?}",
        name.rect
    );
    assert_eq!(find(&list, "E-mail").kind, FieldKind::Text);
    let phone = find(&list, "Phone");
    assert!((phone.rect.y0 - 643.0).abs() < 1.0, "{:?}", phone.rect);
    let town = find(&list, "Town");
    assert!(
        town.rect.y1 < 640.0,
        "the label stays clear: {:?}",
        town.rect
    );
    assert_eq!(find(&list, "Weekdays").kind, FieldKind::CheckBox);
    assert_eq!(find(&list, "Weekends").kind, FieldKind::CheckBox);
    let post = find(&list, "Postcode");
    assert_eq!(post.comb, Some(6));
    let dob = find(&list, "Date of birth");
    assert!(dob.date);
    let notes = find(&list, "Notes");
    assert!(notes.multiline);
    assert_eq!(find(&list, "Signature").kind, FieldKind::Signature);
    assert!(find(&list, "Signature").rect.height() > 20.0);
    // The rule under the heading is not a field; nothing else is found.
    assert!(!names.contains(&"Thank you"), "{names:?}");
    assert_eq!(list.len(), 10, "{names:?}");
    // Top to bottom.
    assert_eq!(list[0].name, "Full name");
    assert_eq!(detect_all(&doc), list);
}

#[test]
fn accepted_fields_fill_and_are_not_found_again() {
    let mut doc = Document::open(pagebara_samples::printed_form()).unwrap();
    let list = detect(&doc, 0);
    let mut form = Form::load(&doc);
    let names = accept(&mut form, &mut doc, &list).unwrap();
    assert_eq!(names.len(), list.len());
    form.set_text(&mut doc, "Full name", "Ada Lovelace")
        .unwrap();
    form.set_text(&mut doc, "Postcode", "SW1A2A").unwrap();
    form.set_checked(&mut doc, "Weekends", true).unwrap();
    form.set_text(&mut doc, "Date of birth", "1815-12-10")
        .unwrap();
    let f = form.field("Full name").unwrap();
    assert_eq!(f.alternate_name.as_deref(), Some("Full name"));
    let doc = Document::open(doc.save_incremental().unwrap()).unwrap();
    assert!(detect(&doc, 0).is_empty());
    let form = Form::load(&doc);
    assert_eq!(form.field("Full name").unwrap().text(), "Ada Lovelace");
    assert!(form.field("Postcode").unwrap().is_comb());
}
