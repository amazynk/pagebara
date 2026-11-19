//! Small helpers for editing objects that may be direct or indirect.

use pagebara_syntax::{Dict, Document, ObjRef, Object};

/// Applies `f` to a copy of the object `o` names. An indirect object is written back when
/// `f` reports a change; a direct one is returned for its holder to store instead.
pub(crate) fn modify(
    doc: &mut Document,
    o: &Object,
    f: impl FnOnce(&mut Document, &mut Object) -> bool,
) -> Option<Object> {
    match o {
        Object::Ref(r) => {
            let mut v = doc.get(*r).map(|o| (*o).clone())?;
            if f(doc, &mut v) {
                doc.set(*r, v);
            }
            None
        }
        o => {
            let mut v = o.clone();
            f(doc, &mut v).then_some(v)
        }
    }
}

/// Like [`modify`] for `holder[key]`, storing a changed direct value back into `holder`.
/// Returns whether `holder` changed.
pub(crate) fn modify_in(
    doc: &mut Document,
    holder: &mut Dict,
    key: &[u8],
    f: impl FnOnce(&mut Document, &mut Object) -> bool,
) -> bool {
    let Some(o) = holder.get(key).cloned() else {
        return false;
    };
    match modify(doc, &o, f) {
        Some(v) => {
            holder.set(key, v);
            true
        }
        None => false,
    }
}

pub(crate) fn catalog_ref(doc: &Document) -> Option<ObjRef> {
    doc.trailer().get(b"Root")?.as_objref()
}

/// The catalog dictionary, creating one when the file has none usable.
pub(crate) fn catalog(doc: &mut Document) -> (ObjRef, Dict) {
    if let Some(r) = catalog_ref(doc)
        && let Some(d) = doc.get(r).and_then(|o| o.as_dict().cloned())
    {
        return (r, d);
    }
    let mut d = Dict::new();
    d.set(b"Type", Object::name(b"Catalog"));
    let r = doc.add(d.clone());
    let mut t = doc.trailer().clone();
    t.set(b"Root", r);
    doc.set_trailer(t);
    (r, d)
}

/// Edits the catalog through `f`, writing it back when `f` reports a change.
pub(crate) fn update_catalog(doc: &mut Document, f: impl FnOnce(&mut Document, &mut Dict) -> bool) {
    let (r, mut d) = catalog(doc);
    if f(doc, &mut d) {
        doc.set(r, d);
    }
}

/// A dictionary from an object, resolved.
pub(crate) fn dict_of(doc: &Document, o: &Object) -> Option<Dict> {
    doc.resolve(o).as_dict().cloned()
}

pub(crate) fn as_dict_mut(o: &mut Object) -> Option<&mut Dict> {
    match o {
        Object::Dict(d) => Some(d),
        Object::Stream(s) => Some(&mut s.dict),
        _ => None,
    }
}
