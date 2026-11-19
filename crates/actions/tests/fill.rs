//! Form data files: the PDF they name is filled into a copy; the form stays as it was.

use pagebara_forms::{FieldKind, FieldValue, Form};
use pagebara_syntax::Document;

#[test]
fn fdf_and_xfdf_fill_a_copy() {
    let dir = std::env::temp_dir().join(format!("pagebara-fill-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let form_bytes = pagebara_samples::form();
    std::fs::write(dir.join("form.pdf"), &form_bytes).unwrap();
    let doc = Document::open(form_bytes.clone()).unwrap();
    let name = Form::load(&doc)
        .fields
        .iter()
        .find(|f| f.kind == FieldKind::Text && !f.flags.read_only())
        .map(|f| f.name.clone())
        .expect("a text field");
    let fdf = format!(
        "%FDF-1.2\n1 0 obj\n<< /FDF << /F (form.pdf) /Fields [ << /T ({name}) /V (Hello FDF) >> ] >> >>\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n"
    );
    std::fs::write(dir.join("data.fdf"), fdf).unwrap();
    let out = pagebara_actions::fill_form(&dir.join("data.fdf"), None).unwrap();
    assert_eq!(out.file_name().unwrap(), "form (filled).pdf");
    let filled = Document::open(std::fs::read(&out).unwrap()).unwrap();
    assert_eq!(
        Form::load(&filled).field(&name).unwrap().value,
        FieldValue::Text("Hello FDF".into())
    );
    assert_eq!(std::fs::read(dir.join("form.pdf")).unwrap(), form_bytes);
    // XFDF without a file name: the PDF of the same name.
    std::fs::write(
        dir.join("form.xfdf"),
        format!("<?xml version=\"1.0\"?><xfdf><fields><field name=\"{name}\"><value>Hi</value></field></fields></xfdf>"),
    )
    .unwrap();
    let out = pagebara_actions::fill_form(&dir.join("form.xfdf"), None).unwrap();
    assert_eq!(out.file_name().unwrap(), "form (filled) (2).pdf");
    std::fs::remove_dir_all(dir).unwrap();
}
