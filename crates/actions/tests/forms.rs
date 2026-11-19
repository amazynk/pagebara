//! Form tools on the command line: FDF and XFDF export and import, field detection.

use std::path::PathBuf;

use pagebara_actions::run_pro;
use pagebara_forms::Form;
use pagebara_syntax::Document;

fn samples(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pagebara-forms-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    pagebara_samples::write_all(&dir).unwrap();
    dir
}

fn args(s: &[&str]) -> Vec<String> {
    s.iter().map(|a| a.to_string()).collect()
}

fn form_of(path: &std::path::Path) -> (Document, Form) {
    let d = Document::open(std::fs::read(path).unwrap()).unwrap();
    let f = Form::load(&d);
    (d, f)
}

#[test]
fn fdf_and_xfdf_round_trip() {
    let dir = samples("fdf");
    // A filled copy to export from.
    let (mut d, mut f) = form_of(&dir.join("form.pdf"));
    f.set_text(&mut d, "name", "Ada Lovelace").unwrap();
    f.set_checked(&mut d, "newsletter", true).unwrap();
    std::fs::write(dir.join("filled.pdf"), d.save_full().unwrap()).unwrap();
    for (export, import, ext) in [
        ("fdf-export", "fdf-import", "fdf"),
        ("xfdf-export", "xfdf-import", "xfdf"),
    ] {
        let r = run_pro(export, &args(&["--json", "filled.pdf"]), &dir).unwrap();
        let data = dir.join(format!("filled.{ext}"));
        assert_eq!(r.outputs, vec![data.clone()]);
        let j: serde_json::Value = serde_json::from_str(r.json.as_deref().unwrap()).unwrap();
        assert_eq!(j["files"][0]["fields"], 11);
        let data_name = format!("filled.{ext}");
        let r = run_pro(import, &args(&["--json", "form.pdf", &data_name]), &dir).unwrap();
        let out = dir.join(format!(
            "form-filled{}.pdf",
            if ext == "fdf" { "" } else { " (2)" }
        ));
        assert_eq!(r.outputs, vec![out.clone()]);
        let (_, f2) = form_of(&out);
        assert_eq!(f2.field("name").unwrap().text(), "Ada Lovelace");
        assert!(f2.field("newsletter").unwrap().is_checked());
    }
    // With --data, and errors.
    assert!(run_pro("fdf-import", &args(&["form.pdf"]), &dir).is_err());
    assert!(run_pro("fdf-export", &args(&["lease.pdf"]), &dir).is_err());
    let r = run_pro(
        "xfdf-import",
        &args(&["--data", "filled.xfdf", "--out", "x.pdf", "form.pdf"]),
        &dir,
    )
    .unwrap();
    assert_eq!(r.outputs, vec![dir.join("x.pdf")]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn detect_fields_adds_or_lists() {
    let dir = samples("detect");
    let r = run_pro(
        "detect-fields",
        &args(&["--list", "--json", "printed-form.pdf"]),
        &dir,
    )
    .unwrap();
    assert!(r.outputs.is_empty());
    let j: serde_json::Value = serde_json::from_str(r.json.as_deref().unwrap()).unwrap();
    let fields = j["files"][0]["fields"].as_array().unwrap();
    assert_eq!(fields.len(), 10);
    assert!(
        fields
            .iter()
            .any(|f| f["name"] == "Postcode" && f["comb"] == 6)
    );
    assert!(
        r.messages
            .iter()
            .any(|m| m.contains("signature \"Signature\""))
    );
    let r = run_pro("detect-fields", &args(&["printed-form.pdf"]), &dir).unwrap();
    assert_eq!(r.outputs, vec![dir.join("printed-form-fields.pdf")]);
    let (_, f) = form_of(&r.outputs[0]);
    assert_eq!(f.fields.len(), 10);
    assert!(f.field("Weekdays").is_some());
    // A page without anything to fill.
    assert!(run_pro("detect-fields", &args(&["report.pdf"]), &dir).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
