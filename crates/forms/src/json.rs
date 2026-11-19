//! A simple JSON export of the form's fields and values.

use crate::form::Form;
use crate::model::{FieldKind, FieldValue};

/// The form's fields and values as JSON: name, type, value (string, list, or null),
/// flags and, for choices and buttons, the possible values.
pub fn export_json(form: &Form) -> String {
    use serde_json::{Value as J, json};
    let fields: Vec<J> = form
        .fields
        .iter()
        .filter(|f| !f.flags.no_export())
        .map(|f| {
            let value = match f.kind {
                FieldKind::CheckBox | FieldKind::Radio => {
                    f.export_value().map_or(J::Null, J::String)
                }
                FieldKind::ComboBox | FieldKind::ListBox => match &f.value {
                    FieldValue::Choices(c) if f.is_multi_select() => json!(c),
                    FieldValue::Choices(c) => c.first().map_or(J::Null, |s| json!(s)),
                    _ => J::Null,
                },
                FieldKind::Text => match &f.value {
                    FieldValue::None => J::Null,
                    v => J::String(v.display()),
                },
                _ => J::Null,
            };
            let mut o = json!({
                "name": f.name,
                "type": f.kind.as_str(),
                "value": value,
                "readOnly": f.is_read_only(),
                "required": f.flags.required(),
            });
            if let Some(tu) = &f.alternate_name {
                o["label"] = json!(tu);
            }
            if f.kind.is_choice() {
                o["options"] = json!(
                    f.options
                        .iter()
                        .map(|c| json!({"value": c.export, "label": c.display}))
                        .collect::<Vec<_>>()
                );
            }
            if matches!(f.kind, FieldKind::CheckBox | FieldKind::Radio) {
                o["choices"] = json!(f.export_values());
            }
            o
        })
        .collect();
    serde_json::to_string_pretty(&json!({ "fields": fields })).unwrap_or_default()
}
