//! The optional content groups (layers) stamps are drawn in.

use pagebara_syntax::{Dict, Document, ObjRef, Object};

use crate::Visibility;

/// Our marker on a group we made: the stamp kind it is for.
pub(crate) const MARK: &[u8] = b"PagebaraStamp";

fn catalog(doc: &Document) -> Option<(ObjRef, Dict)> {
    let r = doc.trailer().get(b"Root")?.as_objref()?;
    Some((r, doc.get(r)?.as_dict()?.clone()))
}

/// The `/OCProperties` dictionary (made direct) or a new one.
fn properties(doc: &Document, cat: &Dict) -> Dict {
    doc.get_in(cat, b"OCProperties")
        .as_dict()
        .cloned()
        .unwrap_or_default()
}

fn store(doc: &mut Document, cat_ref: ObjRef, mut cat: Dict, props: Dict) {
    match cat.get(b"OCProperties").and_then(Object::as_objref) {
        Some(r) if doc.get(r).is_some_and(|o| o.as_dict().is_some()) => doc.set(r, props),
        _ => {
            cat.set(b"OCProperties", props);
            doc.set(cat_ref, cat);
        }
    }
}

fn refs(doc: &Document, d: &Dict, key: &[u8]) -> Vec<Object> {
    doc.get_in(d, key)
        .as_array()
        .map(<[Object]>::to_vec)
        .unwrap_or_default()
}

/// All groups listed in `/OCProperties /OCGs` with their names.
pub(crate) fn groups(doc: &Document) -> Vec<(ObjRef, String, Option<Vec<u8>>)> {
    let Some((_, cat)) = catalog(doc) else {
        return Vec::new();
    };
    let props = properties(doc, &cat);
    refs(doc, &props, b"OCGs")
        .iter()
        .filter_map(Object::as_objref)
        .filter_map(|r| {
            let o = doc.get(r)?;
            let d = o.as_dict()?;
            let name = doc
                .get_in(d, b"Name")
                .as_string()
                .map(pdf_text)
                .unwrap_or_default();
            Some((r, name, d.name(MARK).map(<[u8]>::to_vec)))
        })
        .collect()
}

/// Text of a PDF text string (UTF-16 with BOM, UTF-8 with BOM, or PDFDocEncoding).
pub(crate) fn pdf_text(b: &[u8]) -> String {
    if let Some(u) = b.strip_prefix(&[0xfe, 0xff]) {
        let units: Vec<u16> = u
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_be_bytes(*c))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if let Some(u) = b.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        return String::from_utf8_lossy(u).into_owned();
    }
    b.iter()
        .map(|&c| pagebara_syntax::text::pdfdoc_char(c))
        .collect()
}

/// A PDF text string for `s`.
pub(crate) fn text_string(s: &str) -> Object {
    match pagebara_syntax::text::pdfdoc_encode(s) {
        Some(b) => Object::string(&b),
        None => {
            let mut b = vec![0xfe, 0xff];
            for u in s.encode_utf16() {
                b.extend_from_slice(&u.to_be_bytes());
            }
            Object::string(&b)
        }
    }
}

/// Our group named `name` for stamps of `kind`, made if needed, its visibility set.
pub(crate) fn group(doc: &mut Document, name: &str, kind: &[u8], vis: Visibility) -> ObjRef {
    let found = groups(doc)
        .into_iter()
        .find(|(_, n, k)| n == name && k.as_deref() == Some(kind))
        .map(|g| g.0);
    let (view, print) = match vis {
        Visibility::Always => (true, true),
        Visibility::ViewOnly => (true, false),
        Visibility::PrintOnly => (false, true),
    };
    let on_off = |b: bool| Object::name(if b { b"ON" } else { b"OFF" });
    let mut usage = Dict::new();
    let mut v = Dict::new();
    v.set(b"ViewState", on_off(view));
    let mut p = Dict::new();
    p.set(b"PrintState", on_off(print));
    usage.set(b"View", v);
    usage.set(b"Print", p);
    let mut pe = Dict::new();
    pe.set(
        b"Subtype",
        Object::name(if kind == b"Watermark" { b"FG" } else { b"HF" }),
    );
    usage.set(b"PageElement", pe);
    let mut g = Dict::new();
    g.set(b"Type", Object::name(b"OCG"));
    g.set(b"Name", text_string(name));
    g.set(b"Usage", usage);
    g.set(MARK, Object::name(kind));
    let r = match found {
        Some(r) => {
            doc.set(r, g);
            r
        }
        None => doc.add(g),
    };
    let Some((cat_ref, cat)) = catalog(doc) else {
        return r;
    };
    let mut props = properties(doc, &cat);
    let mut ocgs = refs(doc, &props, b"OCGs");
    if !ocgs.iter().any(|o| o.as_objref() == Some(r)) {
        ocgs.push(r.into());
    }
    props.set(b"OCGs", ocgs);
    let mut d = doc
        .get_in(&props, b"D")
        .as_dict()
        .cloned()
        .unwrap_or_default();
    let without = |list: Vec<Object>| -> Vec<Object> {
        list.into_iter()
            .filter(|o| o.as_objref() != Some(r))
            .collect()
    };
    let mut on = without(refs(doc, &d, b"ON"));
    let mut off = without(refs(doc, &d, b"OFF"));
    if view {
        on.push(r.into());
    } else {
        off.push(r.into());
    }
    d.set(b"ON", on);
    d.set(b"OFF", off);
    let mut order = refs(doc, &d, b"Order");
    if !order.iter().any(|o| o.as_objref() == Some(r)) {
        order.push(r.into());
    }
    d.set(b"Order", order);
    // Viewers that apply usage switch the group per event.
    let mut auto: Vec<Object> = refs(doc, &d, b"AS")
        .into_iter()
        .map(|o| strip_from_as(doc, o, r))
        .filter(|o| !o.is_null())
        .collect();
    if vis != Visibility::Always {
        for (event, cat) in [(&b"View"[..], &b"View"[..]), (b"Print", b"Print")] {
            let mut a = Dict::new();
            a.set(b"Event", Object::name(event));
            a.set(b"OCGs", vec![Object::from(r)]);
            a.set(b"Category", vec![Object::name(cat)]);
            auto.push(a.into());
        }
    }
    if auto.is_empty() {
        d.remove(b"AS");
    } else {
        d.set(b"AS", auto);
    }
    props.set(b"D", d);
    store(doc, cat_ref, cat, props);
    r
}

/// An `/AS` entry without group `r` (null when nothing is left in it).
fn strip_from_as(doc: &Document, o: Object, r: ObjRef) -> Object {
    let Some(mut d) = doc.resolve(&o).as_dict().cloned() else {
        return o;
    };
    let list: Vec<Object> = refs(doc, &d, b"OCGs")
        .into_iter()
        .filter(|g| g.as_objref() != Some(r))
        .collect();
    if list.is_empty() {
        return Object::Null;
    }
    d.set(b"OCGs", list);
    Object::Dict(d)
}

/// Takes group `r` out of the document's optional content configuration.
pub(crate) fn drop_group(doc: &mut Document, r: ObjRef) {
    let Some((cat_ref, cat)) = catalog(doc) else {
        return;
    };
    let mut props = properties(doc, &cat);
    let keep = |list: Vec<Object>| -> Vec<Object> {
        list.into_iter()
            .filter(|o| o.as_objref() != Some(r))
            .collect()
    };
    let ocgs = keep(refs(doc, &props, b"OCGs"));
    let mut d = doc
        .get_in(&props, b"D")
        .as_dict()
        .cloned()
        .unwrap_or_default();
    for key in [&b"ON"[..], b"OFF", b"Order", b"Locked"] {
        if d.contains(key) {
            let l = keep(refs(doc, &d, key));
            d.set(key, l);
        }
    }
    if d.contains(b"AS") {
        let auto: Vec<Object> = refs(doc, &d, b"AS")
            .into_iter()
            .map(|o| strip_from_as(doc, o, r))
            .filter(|o| !o.is_null())
            .collect();
        if auto.is_empty() {
            d.remove(b"AS");
        } else {
            d.set(b"AS", auto);
        }
    }
    if ocgs.is_empty() {
        let mut cat = cat;
        cat.remove(b"OCProperties");
        doc.set(cat_ref, cat);
        return;
    }
    props.set(b"OCGs", ocgs);
    props.set(b"D", d);
    store(doc, cat_ref, cat, props);
}
