//! Form data exchange: FDF (ISO 32000-2 §12.7.8) and XFDF (ISO 19444-1). Field values are
//! read into and written from [`FormData`], exported from a [`Form`] and imported into one
//! (as a reader filling the form would, so appearances are rebuilt); XFDF also carries
//! the document's annotations.
//!
//! ```standalone_crate,no_run
//! # fn f(doc: &mut pagebara_syntax::Document) {
//! let mut form = pagebara_forms::Form::load(doc);
//! let xfdf = pagebara_fdf::export_xfdf(&form, doc, Some("form.pdf"), true);
//! let report = pagebara_fdf::import_xfdf(&mut form, doc, &xfdf).unwrap();
//! # }
//! ```

mod annots;
mod fdf;
mod text;
mod xfdf;
mod xml;

use pagebara_forms::{Change, Field, FieldKind, FieldValue, Form};
use pagebara_syntax::Document;

pub use annots::AnnotData;
pub use fdf::{parse_fdf, write_fdf};
pub use xfdf::{parse_xfdf, write_xfdf};

/// Field trees and XML nested deeper than this are treated as broken.
pub(crate) const MAX_DEPTH: usize = 64;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum Error {
    #[error("not a valid {0} file")]
    BadData(&'static str),
    #[error(transparent)]
    Form(#[from] pagebara_forms::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// A field value as exchanged.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Text(String),
    /// A button state (a name in FDF).
    State(String),
    /// Several selected options.
    List(Vec<String>),
}

/// The contents of an FDF or XFDF file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FormData {
    /// The PDF the data belongs to.
    pub file: Option<String>,
    /// Fully qualified field names and values, in file order.
    pub fields: Vec<(String, Value)>,
    /// Annotations (XFDF only).
    pub annotations: Vec<AnnotData>,
}

/// What an import did.
#[derive(Debug, Default)]
pub struct ImportReport {
    pub change: Change,
    /// Names in the data that the form does not have.
    pub unknown: Vec<String>,
    /// Values the form refused (read-only fields, invalid options, ...).
    pub rejected: Vec<(String, pagebara_forms::Error)>,
    /// Annotations added.
    pub annotations: usize,
}

/// The value exported for a field, or `None` for fields that carry no data.
fn export_value(f: &Field) -> Option<Value> {
    if f.flags.no_export() {
        return None;
    }
    Some(match f.kind {
        FieldKind::Text => Value::Text(f.text()),
        FieldKind::CheckBox | FieldKind::Radio => Value::State(match &f.value {
            FieldValue::State(s) => s.clone(),
            _ => "Off".to_owned(),
        }),
        FieldKind::ComboBox | FieldKind::ListBox => match &f.value {
            FieldValue::Choices(c) if c.len() > 1 || f.is_multi_select() => Value::List(c.clone()),
            v => Value::Text(v.display()),
        },
        _ => return None,
    })
}

/// The form's values (and, with `doc` and `annotations`, the document's annotations).
pub fn from_form(form: &Form, doc: Option<&Document>, file: Option<&str>) -> FormData {
    FormData {
        file: file.map(str::to_owned),
        fields: form
            .fields
            .iter()
            .filter_map(|f| Some((f.name.clone(), export_value(f)?)))
            .collect(),
        annotations: doc.map(annots::collect).unwrap_or_default(),
    }
}

/// The form's values as FDF, naming `file` as the PDF they belong to.
pub fn export_fdf(form: &Form, file: Option<&str>) -> Vec<u8> {
    write_fdf(&from_form(form, None, file))
}

/// The form's values as XFDF, with the document's annotations when `annotations`.
pub fn export_xfdf(form: &Form, doc: &Document, file: Option<&str>, annotations: bool) -> String {
    write_xfdf(&from_form(form, annotations.then_some(doc), file))
}

/// Imports the field values of an FDF file.
pub fn import_fdf(form: &mut Form, doc: &mut Document, data: &[u8]) -> Result<ImportReport> {
    let d = parse_fdf(data)?;
    apply(form, doc, &d)
}

/// Imports the field values and annotations of an XFDF document.
pub fn import_xfdf(form: &mut Form, doc: &mut Document, data: &str) -> Result<ImportReport> {
    let d = parse_xfdf(data)?;
    apply(form, doc, &d)
}

/// Fills the form with `data`'s values and adds its annotations.
pub fn apply(form: &mut Form, doc: &mut Document, data: &FormData) -> Result<ImportReport> {
    let mut report = ImportReport::default();
    if data.fields.iter().any(|(n, _)| form.field(n).is_some()) {
        form.check_editable(doc)?;
    }
    for (name, v) in &data.fields {
        let Some(f) = form.field(name) else {
            report.unknown.push(name.clone());
            continue;
        };
        let kind = f.kind;
        let result = match (kind, v) {
            (FieldKind::Text, Value::Text(s) | Value::State(s)) => form.set_text(doc, name, s),
            (FieldKind::Text, Value::List(l)) => form.set_text(doc, name, &l.join(", ")),
            (FieldKind::CheckBox | FieldKind::Radio, Value::Text(s) | Value::State(s)) => {
                if s.is_empty() || s == "Off" {
                    form.select_radio(doc, name, None)
                } else {
                    form.select_radio(doc, name, Some(s)).or_else(|e| {
                        let yes = ["Yes", "On", "1", "true"].contains(&s.as_str());
                        if kind == FieldKind::CheckBox && yes {
                            form.set_checked(doc, name, true)
                        } else {
                            Err(e)
                        }
                    })
                }
            }
            (FieldKind::ComboBox | FieldKind::ListBox, Value::Text(s) | Value::State(s)) => {
                if s.is_empty() {
                    form.select_choices(doc, name, &[])
                } else {
                    form.select_choice(doc, name, s)
                }
            }
            (FieldKind::ComboBox | FieldKind::ListBox, Value::List(l)) => {
                let l: Vec<&str> = l.iter().map(String::as_str).collect();
                form.select_choices(doc, name, &l)
            }
            _ => Err(pagebara_forms::Error::WrongKind {
                field: name.clone(),
                expected: "fillable",
            }),
        };
        match result {
            Ok(c) => report.change.merge(c),
            Err(e) => report.rejected.push((name.clone(), e)),
        }
    }
    report.annotations = annots::add_all(doc, &data.annotations);
    Ok(report)
}

/// Fields grouped by name parts, for the hierarchical formats.
#[derive(Default)]
pub(crate) struct Tree {
    pub kids: Vec<(String, Tree)>,
    pub value: Option<Value>,
}

pub(crate) fn tree(fields: &[(String, Value)]) -> Tree {
    let mut root = Tree::default();
    for (name, v) in fields {
        let mut node = &mut root;
        for part in name.split('.') {
            let i = match node.kids.iter().position(|(k, _)| k == part) {
                Some(i) => i,
                None => {
                    node.kids.push((part.to_owned(), Tree::default()));
                    node.kids.len() - 1
                }
            };
            node = &mut node.kids[i].1;
        }
        node.value = Some(v.clone());
    }
    root
}
