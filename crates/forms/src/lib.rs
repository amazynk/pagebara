//! Interactive forms (ISO 32000-2 §12.7): the field tree of a
//! [`Document`](pagebara_syntax::Document), filling with generated appearance streams (so
//! every reader shows what was typed), signature field information, flattening, JSON
//! export (FDF and XFDF are in `pagebara-fdf`), and the common Acrobat format and calculation helpers run
//! natively in place of a JavaScript engine. Forms are also prepared here: fields created,
//! changed, moved and removed, with tab order and tagging for accessibility.
//!
//! [`Form::load`] reads the form; the editing methods change the document in place (keep a
//! clone of it for undo) and keep the [`Form`] in step.

mod appearance;
mod content;
mod da;
mod fill;
mod flags;
mod flatten;
mod fonts;
mod form;
mod json;
mod model;
mod prepare;
mod props;
mod script;
mod signature;
mod tag;
mod text;

pub use da::DefaultAppearance;
pub use flags::FieldFlags;
pub use fonts::{SharedFont, SimpleFont};
pub use form::{Change, Form, ScriptWarning};
pub use json::export_json;
pub use model::{Border, BorderKind, ChoiceOption, Field, FieldKind, FieldValue, Mk, Widget, Xfa};
pub use prepare::{NewField, TabOrder};
pub use props::{ButtonAction, CHECK_STYLES, FieldProps, Format, STANDARD_FONTS};
pub use script::{
    CalcOp, Date, Formatted, Native, NumberFormat, Script, Scripts, document_scripts, format_date,
    parse_date, parse_number,
};
pub use signature::{ByteRangeCheck, SignatureInfo};
pub use text::DateTime;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum Error {
    #[error("the document has no interactive form")]
    NoForm,
    #[error("this is an XFA-only form, which is not supported")]
    XfaOnly,
    #[error("the document's security settings do not allow filling forms")]
    NotPermitted,
    #[error("no field named {0:?}")]
    NoSuchField(String),
    #[error("field {0:?} is read-only")]
    ReadOnly(String),
    #[error("field {field:?} is not a {expected} field")]
    WrongKind {
        field: String,
        expected: &'static str,
    },
    #[error("field {field:?} takes at most {max} characters")]
    TooLong { field: String, max: usize },
    #[error("{value:?} is not an option of field {field:?}")]
    NotAnOption { field: String, value: String },
    #[error("field {0:?} allows only one selection")]
    NotMultiSelect(String),
    #[error("field {field:?} rejects {value:?}: {reason}")]
    Rejected {
        field: String,
        value: String,
        reason: &'static str,
    },
    #[error("not a valid {0} file")]
    BadData(&'static str),
    #[error("there is already a field named {0:?}")]
    NameTaken(String),
    #[error("{0:?} is not a valid field name")]
    BadName(String),
}

pub type Result<T> = std::result::Result<T, Error>;
