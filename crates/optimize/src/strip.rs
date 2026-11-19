//! Removing optional parts: metadata, thumbnails, private application data, scripts,
//! attached files and comments.

use pagebara_syntax::{Dict, Document, ObjRef, Object};

use crate::Strip;

/// Markup annotations, which "comments" covers; links and widgets stay.
fn is_comment(subtype: &[u8]) -> bool {
    !matches!(
        subtype,
        b"Link" | b"Widget" | b"PrinterMark" | b"TrapNet" | b"Watermark"
    )
}

/// Edits object `r` (a dictionary or stream dictionary) with `f`; `f` says whether it
/// changed anything.
fn edit(doc: &mut Document, r: ObjRef, f: impl FnOnce(&mut Dict) -> bool) -> bool {
    let Some(o) = doc.get(r) else { return false };
    let mut o = (*o).clone();
    let changed = match &mut o {
        Object::Dict(d) => f(d),
        Object::Stream(s) => f(&mut s.dict),
        _ => false,
    };
    if changed {
        doc.set(r, o);
    }
    changed
}

/// Whether an action dictionary (or a chain of them) runs a script.
fn is_script(doc: &Document, a: &Object) -> bool {
    let a = doc.resolve(a);
    let Some(d) = a.as_dict() else { return false };
    d.name(b"S") == Some(b"JavaScript")
}

/// Applies `s`; returns how many parts went.
pub(crate) fn strip(doc: &mut Document, s: &Strip) -> usize {
    if *s == Strip::default() {
        return 0;
    }
    let mut n = 0;
    let cat_ref = doc.trailer().get(b"Root").and_then(Object::as_objref);
    if s.metadata {
        let mut t = doc.trailer().clone();
        if let Some(info) = t.get(b"Info").cloned() {
            let title = doc
                .resolve(&info)
                .as_dict()
                .and_then(|d| d.get(b"Title").cloned());
            match (title, info.as_objref()) {
                (Some(title), Some(r)) => {
                    let mut d = Dict::new();
                    d.set(b"Title", title);
                    doc.set(r, d);
                }
                _ => {
                    t.remove(b"Info");
                    doc.set_trailer(t);
                }
            }
            n += 1;
        }
    }
    for num in doc.object_numbers() {
        let Some(r) = doc.reference(num) else {
            continue;
        };
        let changed = edit(doc, r, |d| {
            let mut c = false;
            if s.metadata && d.remove(b"Metadata").is_some() {
                c = true;
            }
            if s.thumbnails && d.name(b"Type") == Some(b"Page") && d.remove(b"Thumb").is_some() {
                c = true;
            }
            if s.piece_info && d.remove(b"PieceInfo").is_some() {
                c = true;
            }
            c
        });
        n += usize::from(changed);
    }
    if s.javascript {
        n += strip_scripts(doc, cat_ref);
    }
    if s.embedded_files
        && let Some(c) = cat_ref
    {
        let names = doc
            .get(c)
            .and_then(|o| o.as_dict().and_then(|d| d.get(b"Names").cloned()));
        if let Some(nr) = names.as_ref().and_then(Object::as_objref) {
            n += usize::from(edit(doc, nr, |d| d.remove(b"EmbeddedFiles").is_some()));
        }
        n += usize::from(edit(doc, c, |d| {
            let mut ch = d.remove(b"AF").is_some();
            if let Some(Object::Dict(nd)) = d.get_mut(b"Names") {
                ch |= nd.remove(b"EmbeddedFiles").is_some();
            }
            ch |= d.remove(b"Collection").is_some();
            ch
        }));
    }
    if s.embedded_files || s.annotations {
        n += strip_annotations(doc, s);
    }
    n
}

fn strip_scripts(doc: &mut Document, cat: Option<ObjRef>) -> usize {
    let mut n = 0;
    if let Some(c) = cat {
        let names = doc
            .get(c)
            .and_then(|o| o.as_dict().and_then(|d| d.get(b"Names").cloned()));
        if let Some(nr) = names.as_ref().and_then(Object::as_objref) {
            n += usize::from(edit(doc, nr, |d| d.remove(b"JavaScript").is_some()));
        }
        let open = doc
            .get(c)
            .and_then(|o| o.as_dict().and_then(|d| d.get(b"OpenAction").cloned()));
        let open_js = open.as_ref().is_some_and(|a| is_script(doc, a));
        n += usize::from(edit(doc, c, |d| {
            let mut ch = false;
            if let Some(Object::Dict(nd)) = d.get_mut(b"Names") {
                ch |= nd.remove(b"JavaScript").is_some();
            }
            if open_js {
                ch |= d.remove(b"OpenAction").is_some();
            }
            ch
        }));
    }
    for num in doc.object_numbers() {
        let Some(r) = doc.reference(num) else {
            continue;
        };
        let Some(o) = doc.get(r) else { continue };
        let a_js = o
            .as_dict()
            .and_then(|d| d.get(b"A"))
            .is_some_and(|a| is_script(doc, a));
        let next_js = o
            .as_dict()
            .and_then(|d| d.get(b"Next"))
            .is_some_and(|a| is_script(doc, a));
        n += usize::from(edit(doc, r, |d| {
            let mut ch = d.remove(b"AA").is_some();
            if a_js {
                ch |= d.remove(b"A").is_some();
            }
            if next_js {
                ch |= d.remove(b"Next").is_some();
            }
            ch
        }));
    }
    n
}

fn strip_annotations(doc: &mut Document, s: &Strip) -> usize {
    let mut n = 0;
    let pages: Vec<ObjRef> = pagebara_pages::PageTree::load(doc).refs();
    for p in pages {
        let Some(po) = doc.get(p) else { continue };
        let Some(pd) = po.as_dict() else { continue };
        let annots = doc.get_in(pd, b"Annots");
        let Some(list) = annots.as_array() else {
            continue;
        };
        let keep: Vec<Object> = list
            .iter()
            .filter(|a| {
                let ao = doc.resolve(a);
                let st = ao
                    .as_dict()
                    .and_then(|d| d.name(b"Subtype"))
                    .unwrap_or_default();
                let drop = (s.annotations && is_comment(st))
                    || (s.embedded_files && st == b"FileAttachment");
                !drop
            })
            .cloned()
            .collect();
        if keep.len() == list.len() {
            continue;
        }
        n += list.len() - keep.len();
        let mut pd = pd.clone();
        if keep.is_empty() {
            pd.remove(b"Annots");
        } else {
            pd.set(b"Annots", keep);
        }
        doc.set(p, pd);
    }
    n
}
