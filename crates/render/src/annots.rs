//! Annotation appearances (§12.5.5): the normal appearance (by `/AS` state) mapped into
//! `/Rect` by the algorithm of §12.5.5 (bounding box of the transformed `/BBox`).

use std::sync::Arc;

use pagebara_content::oc::OptionalContent;
use pagebara_content::{Matrix, Rect};
use pagebara_syntax::{ObjRef, Object};

use crate::list::Prepared;
use crate::{Annotations, Inner};

const HIDDEN: i64 = 2;
const PRINT: i64 = 4;
const NO_VIEW: i64 = 32;

pub fn page_annotations(
    r: &Inner,
    _page: usize,
    page_ref: ObjRef,
    mode: &Annotations,
    oc: &OptionalContent,
) -> Vec<(Arc<Prepared>, Matrix)> {
    let doc = &r.doc;
    let mut out = Vec::new();
    let Some(page) = doc.get(page_ref) else {
        return out;
    };
    let Some(page) = page.as_dict() else {
        return out;
    };
    let annots = doc.get_in(page, b"Annots");
    let Some(annots) = annots.as_array() else {
        return out;
    };
    for a in annots {
        if let (Annotations::ViewExcept(skip), Some(rf)) = (mode, a.as_objref())
            && skip.contains(&rf)
        {
            continue;
        }
        let d = doc.resolve(a);
        let Some(d) = d.as_dict() else { continue };
        let flags = doc.get_in(d, b"F").as_int().unwrap_or(0);
        let shown = match mode {
            Annotations::None => false,
            Annotations::Print => flags & PRINT != 0 && flags & HIDDEN == 0,
            _ => flags & (HIDDEN | NO_VIEW) == 0,
        };
        if !shown || d.name(b"Subtype") == Some(b"Popup") {
            continue;
        }
        if let Some(o) = d.get(b"OC")
            && !oc.visible(doc, o)
        {
            continue;
        }
        let Some(rect) = Rect::from_object(&doc.get_in(d, b"Rect")) else {
            continue;
        };
        let ap = doc.get_in(d, b"AP");
        let Some(ap) = ap.as_dict() else { continue };
        let Some(n) = ap.get(b"N") else { continue };
        // A stream, or a dictionary of states selected by /AS.
        let form: Object = match &*doc.resolve(n) {
            Object::Stream(_) => n.clone(),
            Object::Dict(states) => {
                let Some(state) = d.name(b"AS") else { continue };
                match states.get(state) {
                    Some(f) => f.clone(),
                    None => continue,
                }
            }
            _ => continue,
        };
        let fs = doc.resolve(&form);
        let Some(fs) = fs.as_stream() else { continue };
        let Some(bbox) = Rect::from_object(&doc.get_in(&fs.dict, b"BBox")) else {
            continue;
        };
        let m = Matrix::from_object(&doc.get_in(&fs.dict, b"Matrix")).unwrap_or_default();
        let t = bbox.transform(&m);
        if t.width() <= 0.0 || t.height() <= 0.0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
            continue;
        }
        let a = Matrix::translate(-t.x0, -t.y0)
            .then(&Matrix::scale(
                rect.width() / t.width(),
                rect.height() / t.height(),
            ))
            .then(&Matrix::translate(rect.x0, rect.y0));
        if let Some(list) = r.form_list(&form, a) {
            out.push((list, Matrix::IDENTITY));
        }
    }
    out
}
