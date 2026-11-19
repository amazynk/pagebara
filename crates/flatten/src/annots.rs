//! Annotations and form fields into page content.

use pagebara_forms::{FieldKind, Form};
use pagebara_pages::PageTree;
use pagebara_syntax::{Document, ObjRef, Object};

use crate::{Error, Report, Result};

/// Kinds whose appearance we can generate when it is missing.
const DRAWABLE: [&[u8]; 14] = [
    b"Text",
    b"FreeText",
    b"Line",
    b"Square",
    b"Circle",
    b"Polygon",
    b"PolyLine",
    b"Highlight",
    b"Underline",
    b"Squiggly",
    b"StrikeOut",
    b"Ink",
    b"Stamp",
    b"Caret",
];

fn annot_refs(doc: &Document, page: ObjRef) -> Vec<ObjRef> {
    let Some(d) = doc.get(page).and_then(|o| o.as_dict().cloned()) else {
        return Vec::new();
    };
    doc.get_in(&d, b"Annots")
        .as_array()
        .unwrap_or_default()
        .iter()
        .filter_map(Object::as_objref)
        .collect()
}

fn subtype(doc: &Document, r: ObjRef) -> Option<Vec<u8>> {
    let o = doc.get(r)?;
    Some(o.as_dict()?.name(b"Subtype")?.to_vec())
}

fn has_appearance(doc: &Document, r: ObjRef) -> bool {
    doc.get(r).is_some_and(|o| {
        o.as_dict().is_some_and(|d| {
            doc.get_in(d, b"AP")
                .as_dict()
                .is_some_and(|ap| ap.contains(b"N"))
        })
    })
}

/// Annotations (without links, popups and widgets) and links.
pub(crate) fn count(doc: &Document) -> (usize, usize) {
    let (mut a, mut l) = (0, 0);
    for p in PageTree::load(doc).pages() {
        for r in annot_refs(doc, p.r) {
            match subtype(doc, r).as_deref() {
                Some(b"Link") => l += 1,
                Some(b"Popup" | b"Widget") => {}
                _ => a += 1,
            }
        }
    }
    (a, l)
}

pub(crate) fn forms(doc: &mut Document, signatures: bool, report: &mut Report) -> Result<()> {
    let mut form = Form::load(doc);
    if form.fields.is_empty() {
        return Ok(());
    }
    // Signed signature fields stay unless asked; empty ones are flattened like the rest.
    let signed = crate::signed_fields(doc);
    let names: Vec<String> = form
        .fields
        .iter()
        .filter(|f| signatures || f.kind != FieldKind::Signature || !signed.contains(&f.name))
        .map(|f| f.name.clone())
        .collect();
    if names.is_empty() {
        return Ok(());
    }
    // Ancestors are flattened through their kids; only leaves are named.
    let leaves: Vec<&str> = names
        .iter()
        .filter(|n| {
            !names.iter().any(|o| {
                o.len() > n.len() && o.starts_with(n.as_str()) && o.as_bytes()[n.len()] == b'.'
            })
        })
        .map(String::as_str)
        .collect();
    let change = form
        .flatten(doc, Some(&leaves))
        .map_err(|e| Error::Form(e.to_string()))?;
    report.fields += change.fields.len();
    Ok(())
}

pub(crate) fn flatten(doc: &mut Document, links: bool, report: &mut Report) {
    let pages: Vec<ObjRef> = PageTree::load(doc).pages().iter().map(|p| p.r).collect();
    for page in pages {
        for r in annot_refs(doc, page) {
            // Gone already: a popup or reply of one flattened before.
            if !annot_refs(doc, page).contains(&r) {
                continue;
            }
            let Some(sub) = subtype(doc, r) else { continue };
            match sub.as_slice() {
                b"Popup" => continue,
                b"Link" if !links => continue,
                // Signature widgets that stayed (and orphan widgets) keep working.
                b"Widget" => continue,
                _ => {}
            }
            // Nothing to draw it with (a link just goes).
            if !has_appearance(doc, r) && !DRAWABLE.contains(&sub.as_slice()) && sub != b"Link" {
                report.annotations_kept += 1;
                continue;
            }
            match pagebara_annot::flatten(doc, page, r) {
                Ok(()) => report.annotations += 1,
                Err(e) => report
                    .warnings
                    .push(format!("an annotation could not be flattened: {e}")),
            }
        }
    }
}
