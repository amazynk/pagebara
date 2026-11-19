//! Flattening a document: what viewers draw on top of pages or switch on and off becomes
//! plain page content.
//!
//! - **Annotations** are drawn with their appearance streams (generated where missing for
//!   the kinds we can draw) and removed; links are kept unless asked.
//! - **Form fields** are drawn with their values and removed, the form with them.
//!   Signature fields stay unless asked (flattening them breaks the signatures; signed
//!   documents get a warning either way).
//! - **Optional content** is applied (what the default configuration hides is deleted,
//!   the rest stays without its layer) or merged (everything stays, without layers).
//! - **Transparency**, optionally: areas painted with transparency are rendered and drawn
//!   as images in their place.

#![forbid(unsafe_code)]

mod annots;
mod oc;
mod transparency;

use pagebara_syntax::Document;
use serde::{Deserialize, Serialize};

/// What to do with optional content.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Layers {
    /// Leave layers as they are.
    #[default]
    Keep,
    /// Delete what the default configuration hides; the rest stays, without layers.
    Apply,
    /// Keep everything, visible or not, without layers.
    Merge,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Options {
    pub annotations: bool,
    /// With `annotations`: flatten links too (they stop working).
    pub links: bool,
    pub forms: bool,
    /// With `forms`: flatten signed signature fields too (empty ones always are).
    pub signatures: bool,
    pub layers: Layers,
    /// Render areas painted with transparency at this resolution (dpi).
    pub transparency: Option<f32>,
}

impl Default for Options {
    fn default() -> Self {
        Options::all()
    }
}

impl Options {
    /// "Flatten all": annotations (links kept), fields (signatures kept), layers applied.
    pub fn all() -> Options {
        Options {
            annotations: true,
            links: false,
            forms: true,
            signatures: false,
            layers: Layers::Apply,
            transparency: None,
        }
    }

    pub fn nothing() -> Options {
        Options {
            annotations: false,
            links: false,
            forms: false,
            signatures: false,
            layers: Layers::Keep,
            transparency: None,
        }
    }
}

/// What [`flatten`] did.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Report {
    pub annotations: usize,
    /// Annotations left because there is nothing to draw them with (3D, media, …).
    pub annotations_kept: usize,
    pub fields: usize,
    /// Layers removed.
    pub layers: usize,
    /// Pieces of hidden content deleted.
    pub hidden_removed: usize,
    /// Areas rendered for transparency.
    pub transparent_areas: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the document's security settings do not allow changes")]
    NotPermitted,
    #[error("form: {0}")]
    Form(String),
    #[error("annotation: {0}")]
    Annot(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// The signed signature fields (their names), to warn before flattening.
pub fn signed_fields(doc: &Document) -> Vec<String> {
    let form = pagebara_forms::Form::load(doc);
    form.fields
        .iter()
        .filter(|f| f.kind == pagebara_forms::FieldKind::Signature)
        .filter(|f| {
            doc.get(f.id)
                .and_then(|o| o.as_dict().map(|d| d.contains(b"V")))
                .unwrap_or(false)
        })
        .map(|f| f.name.clone())
        .collect()
}

/// What the document has that flattening would change: annotations (links apart), links,
/// form fields, layers, pages painting with transparency.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Contents {
    pub annotations: usize,
    pub links: usize,
    pub fields: usize,
    pub layers: usize,
    pub signed: Vec<String>,
}

pub fn contents(doc: &Document) -> Contents {
    let (annotations, links) = annots::count(doc);
    Contents {
        annotations,
        links,
        fields: pagebara_forms::Form::load(doc).fields.len(),
        layers: oc::group_count(doc),
        signed: signed_fields(doc),
    }
}

/// Flattens `doc` in place.
pub fn flatten(doc: &mut Document, opts: &Options) -> Result<Report> {
    if !doc.permissions().modify {
        return Err(Error::NotPermitted);
    }
    let mut report = Report::default();
    let signed = signed_fields(doc);
    if !signed.is_empty() {
        report.warnings.push(if opts.forms && opts.signatures {
            format!(
                "The document is signed ({}); flattening the signatures invalidates them.",
                signed.join(", ")
            )
        } else {
            format!(
                "The document is signed ({}); any change makes the signatures show as modified.",
                signed.join(", ")
            )
        })
    }
    if opts.layers != Layers::Keep {
        oc::annotations(doc, opts.layers, &mut report);
    }
    if opts.forms {
        annots::forms(doc, opts.signatures, &mut report)?;
    }
    if opts.annotations {
        annots::flatten(doc, opts.links, &mut report);
    }
    if opts.layers != Layers::Keep {
        oc::content(doc, opts.layers, &mut report);
    }
    if let Some(dpi) = opts.transparency {
        transparency::flatten(doc, dpi.clamp(36.0, 1200.0), &mut report);
    }
    Ok(report)
}
