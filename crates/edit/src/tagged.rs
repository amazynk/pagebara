//! The structure tree: keeping its text alternatives in step with edited content, and the
//! languages it gives text.

use std::collections::{HashMap, HashSet};

use pagebara_syntax::{Dict, Document, ObjRef, Object, PdfString};

/// Sets `/ActualText` (and `/Alt` when it repeated the old text) of structure elements whose
/// content on `page` is all in the marked-content sequences `mcids`.
pub(crate) fn update_text(doc: &mut Document, page: ObjRef, mcids: &[i64], old: &str, new: &str) {
    if mcids.is_empty() {
        return;
    }
    let Some(root) = doc.catalog().and_then(|c| {
        c.as_dict()
            .map(|d| (*doc.get_in(d, b"StructTreeRoot")).clone())
    }) else {
        return;
    };
    let Some(k) = root.as_dict().and_then(|d| d.get(b"K")).cloned() else {
        return;
    };
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    walk(doc, &k, None, page, mcids, &mut found, &mut seen, 0);
    let norm = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    for r in found {
        let Some(mut d) = doc.get(r).and_then(|o| o.as_dict().cloned()) else {
            continue;
        };
        let mut changed = false;
        if d.contains(b"ActualText") {
            d.set(b"ActualText", Object::String(text_string(new)));
            changed = true;
        }
        let alt = doc.get_in(&d, b"Alt").as_string().map(decode);
        if alt.is_some_and(|a| norm(&a) == norm(old)) {
            d.set(b"Alt", Object::String(text_string(new)));
            changed = true;
        }
        if changed {
            doc.set(r, Object::Dict(d));
        }
    }
}

/// MCIDs on `page` under a structure node: (in `mcids`, not in `mcids`).
#[allow(clippy::too_many_arguments)]
fn walk(
    doc: &Document,
    node: &Object,
    pg: Option<ObjRef>,
    page: ObjRef,
    mcids: &[i64],
    found: &mut Vec<ObjRef>,
    seen: &mut HashSet<ObjRef>,
    depth: u32,
) -> (usize, usize) {
    if depth > 64 {
        return (0, 0);
    }
    match node {
        Object::Int(n) => match pg {
            Some(p) if p == page => {
                if mcids.contains(n) {
                    (1, 0)
                } else {
                    (0, 1)
                }
            }
            _ => (0, 0),
        },
        Object::Array(a) => {
            let mut t = (0, 0);
            for k in a {
                let (i, o) = walk(doc, k, pg, page, mcids, found, seen, depth + 1);
                t.0 += i;
                t.1 += o;
            }
            t
        }
        Object::Ref(r) => {
            if !seen.insert(*r) {
                return (0, 0);
            }
            let Some(o) = doc.get(*r) else { return (0, 0) };
            let t = walk(doc, &o, pg, page, mcids, found, seen, depth + 1);
            let is_elem = o.as_dict().is_some_and(|d| {
                d.contains(b"S") && (d.contains(b"ActualText") || d.contains(b"Alt"))
            });
            if is_elem && t.0 > 0 && t.1 == 0 {
                found.push(*r);
            }
            t
        }
        Object::Dict(d) => {
            let pg = d.get(b"Pg").and_then(Object::as_objref).or(pg);
            match d.name(b"Type") {
                Some(b"MCR") => {
                    let n = doc.get_in(d, b"MCID").as_int();
                    match (n, pg) {
                        (Some(n), Some(p)) if p == page && !d.contains(b"Stm") => {
                            if mcids.contains(&n) { (1, 0) } else { (0, 1) }
                        }
                        _ => (0, 0),
                    }
                }
                Some(b"OBJR") => (0, 0),
                _ => match d.get(b"K") {
                    Some(k) => walk(doc, k, pg, page, mcids, found, seen, depth + 1),
                    None => (0, 0),
                },
            }
        }
        _ => (0, 0),
    }
}

/// A text string: PDFDocEncoding-compatible ASCII as is, otherwise UTF-16BE.
fn text_string(s: &str) -> PdfString {
    if s.bytes().all(|b| (0x20..0x7f).contains(&b)) {
        return PdfString {
            bytes: s.as_bytes().to_vec(),
            hex: false,
        };
    }
    let mut b = vec![0xFE, 0xFF];
    for u in s.encode_utf16() {
        b.extend_from_slice(&u.to_be_bytes());
    }
    PdfString {
        bytes: b,
        hex: true,
    }
}

fn decode(b: &[u8]) -> String {
    match b.strip_prefix(&[0xFE, 0xFF]) {
        Some(u) => char::decode_utf16(u.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c)))
            .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect(),
        None => b
            .iter()
            .map(|&c| pagebara_syntax::text::pdfdoc_char(c))
            .collect(),
    }
}

/// The document's natural language: the catalog's `/Lang`.
pub(crate) fn document_lang(doc: &Document) -> Option<String> {
    let cat = doc.catalog()?;
    lang_of(doc, cat.as_dict()?)
}

/// A dictionary's `/Lang`.
fn lang_of(doc: &Document, d: &Dict) -> Option<String> {
    lang_string(doc.get_in(d, b"Lang").as_string()?)
}

/// A `/Lang` value, when not empty.
pub(crate) fn lang_string(b: &[u8]) -> Option<String> {
    let s = decode(b);
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_owned())
}

/// The languages of the marked content on `page` by MCID: the `/Lang` of each sequence's
/// structure element or its nearest ancestor with one.
pub(crate) fn mcid_langs(doc: &Document, page: ObjRef) -> HashMap<i64, String> {
    let mut out = HashMap::new();
    let tree = || -> Option<(Object, i64)> {
        let key = doc.get(page)?.as_dict()?.int(b"StructParents")?;
        let cat = doc.catalog()?;
        let root = doc.get_in(cat.as_dict()?, b"StructTreeRoot");
        Some((root.as_dict()?.get(b"ParentTree")?.clone(), key))
    };
    let Some(elems) = tree().and_then(|(t, key)| num_tree_get(doc, &t, key, 0)) else {
        return out;
    };
    let elems = doc.resolve(&elems);
    let mut cache: HashMap<ObjRef, Option<String>> = HashMap::new();
    for (mcid, e) in elems.as_array().unwrap_or_default().iter().enumerate() {
        let Some(r) = e.as_objref() else { continue };
        let lang = cache.entry(r).or_insert_with(|| element_lang(doc, r));
        if let Some(l) = lang {
            out.insert(mcid as i64, l.clone());
        }
    }
    out
}

/// `/Lang` of a structure element or its nearest ancestor with one.
fn element_lang(doc: &Document, mut r: ObjRef) -> Option<String> {
    for _ in 0..64 {
        let o = doc.get(r)?;
        let d = o.as_dict()?;
        if d.name(b"Type") == Some(b"StructTreeRoot") {
            return None;
        }
        if let Some(l) = lang_of(doc, d) {
            return Some(l);
        }
        r = d.get(b"P")?.as_objref()?;
    }
    None
}

/// The value for `key` in the number tree at `node`.
fn num_tree_get(doc: &Document, node: &Object, key: i64, depth: u32) -> Option<Object> {
    if depth > 32 {
        return None;
    }
    let n = doc.resolve(node);
    let n = n.as_dict()?;
    if let Some(a) = doc.get_in(n, b"Nums").as_array() {
        return a
            .as_chunks::<2>()
            .0
            .iter()
            .find(|p| doc.resolve(&p[0]).as_int() == Some(key))
            .map(|p| p[1].clone());
    }
    for k in doc.get_in(n, b"Kids").as_array()? {
        let kid = doc.resolve(k);
        let limits = kid.as_dict().and_then(|d| {
            let l = doc.get_in(d, b"Limits");
            let l = l.as_array()?;
            Some((l.first()?.as_int()?, l.get(1)?.as_int()?))
        });
        if limits.is_none_or(|(lo, hi)| (lo..=hi).contains(&key))
            && let Some(v) = num_tree_get(doc, k, key, depth + 1)
        {
            return Some(v);
        }
    }
    None
}
