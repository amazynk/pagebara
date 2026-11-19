//! The JSON export.

mod common;

use common::*;
use pagebara_forms::*;

#[test]
fn json() {
    let mut doc = sample_form("");
    let mut form = Form::load(&doc);
    form.set_text(&mut doc, "name", "Zoë (Ann)").unwrap();
    form.set_checked(&mut doc, "agree", true).unwrap();
    form.select_radio(&mut doc, "color", Some("Blue")).unwrap();
    form.select_choice(&mut doc, "country", "it").unwrap();
    form.select_choices(&mut doc, "langs", &["English", "Spanish"])
        .unwrap();
    form.set_text(&mut doc, "a", "2").unwrap();
    let j: serde_json::Value = serde_json::from_str(&export_json(&form)).unwrap();
    let fields = j["fields"].as_array().unwrap();
    let f = |n: &str| fields.iter().find(|f| f["name"] == n).unwrap().clone();
    assert_eq!(f("name")["value"], "Zoë (Ann)");
    assert_eq!(f("name")["label"], "Your name");
    assert_eq!(f("name")["required"], true);
    assert_eq!(f("color")["value"], "Blue");
    assert_eq!(
        f("color")["choices"],
        serde_json::json!(["Red", "Green", "Blue"])
    );
    assert_eq!(f("agree")["value"], "Yes");
    assert_eq!(f("news")["value"], serde_json::Value::Null);
    assert_eq!(
        f("langs")["value"],
        serde_json::json!(["English", "Spanish"])
    );
    assert_eq!(f("country")["value"], "it");
    assert_eq!(f("country")["options"][2]["label"], "Italy");
    assert_eq!(f("addr.town")["readOnly"], true);
    assert_eq!(f("total")["value"], "2");
    assert!(!fields.iter().any(|f| f["name"] == "secret"));
}
