//! Destinations (ISO 32000-2 §12.3.2) and the name trees holding named ones.

use std::collections::{HashMap, HashSet};

use pagebara_syntax::{Dict, Document, ObjRef, Object};

use crate::edit::{as_dict_mut, dict_of, modify, modify_in};
use crate::names::walk_tree;

const MAX_DEPTH: u32 = 64;

/// Where a destination leads.
#[derive(Debug, PartialEq)]
pub(crate) enum Target {
    Page(ObjRef),
    Named(Vec<u8>),
    /// A page number (remote destinations) or something malformed.
    Other,
}

pub(crate) fn target(doc: &Document, dest: &Object) -> Target {
    let d = doc.resolve(dest);
    match &*d {
        Object::Array(a) => match a.first() {
            Some(Object::Ref(r)) => Target::Page(*r),
            _ => Target::Other,
        },
        Object::Name(n) => Target::Named(n.as_bytes().to_vec()),
        Object::String(s) => Target::Named(s.bytes.clone()),
        Object::Dict(d) => match doc.get_in(d, b"D").as_array().and_then(|a| a.first()) {
            Some(Object::Ref(r)) => Target::Page(*r),
            _ => Target::Other,
        },
        _ => Target::Other,
    }
}

/// The destination of an outline item or link: `/Dest`, or `/D` of a `/GoTo` action.
pub(crate) fn dest_of(doc: &Document, d: &Dict) -> Option<Target> {
    if let Some(dest) = d.get(b"Dest") {
        return Some(target(doc, dest));
    }
    let a = doc.get_in(d, b"A");
    let a = a.as_dict()?;
    (a.name(b"S") == Some(b"GoTo")).then(|| target(doc, a.get(b"D").unwrap_or(&Object::Null)))
}

/// Every named destination, from the catalog's `/Dests` and the `/Names` `/Dests` tree.
pub(crate) fn named(doc: &Document) -> HashMap<Vec<u8>, Object> {
    let mut out = HashMap::new();
    let Some(cat) = doc.catalog() else { return out };
    let Some(cat) = cat.as_dict() else { return out };
    if let Some(d) = doc.get_in(cat, b"Dests").as_dict() {
        for (k, v) in d.iter() {
            out.insert(k.as_bytes().to_vec(), (*doc.resolve(v)).clone());
        }
    }
    if let Some(names) = dict_of(doc, cat.get(b"Names").unwrap_or(&Object::Null))
        && let Some(root) = names.get(b"Dests")
    {
        let mut seen = HashSet::new();
        walk_tree(doc, root, b"Names", 0, &mut seen, &mut |k, v| {
            if let Some(k) = k.as_string() {
                out.entry(k.to_vec())
                    .or_insert_with(|| (*doc.resolve(v)).clone());
            }
        });
    }
    out
}

/// An explicit destination array leading to `page` in the view of destination `dest`
/// (an array, or a dictionary with `/D`), read in `doc`. Other references are dropped.
pub(crate) fn explicit(doc: &Document, dest: &Object, page: ObjRef) -> Option<Object> {
    let d = doc.resolve(dest);
    let a = match &*d {
        Object::Dict(dd) => doc.get_in(dd, b"D").as_array()?.to_vec(),
        o => o.as_array()?.to_vec(),
    };
    let mut out: Vec<Object> = vec![page.into()];
    out.extend(
        a.into_iter()
            .skip(1)
            .map(|o| (*doc.resolve(&o)).clone())
            .filter(|o| !matches!(o, Object::Ref(_) | Object::Array(_) | Object::Dict(_))),
    );
    if out.len() == 1 {
        out.push(Object::name(b"Fit"));
    }
    Some(out.into())
}

/// Removes from the catalog's named destinations every entry `gone` rejects, returning the
/// removed names.
pub(crate) fn prune_named(
    doc: &mut Document,
    gone: &impl Fn(&Document, &Object) -> bool,
) -> HashSet<Vec<u8>> {
    let mut removed = HashSet::new();
    crate::edit::update_catalog(doc, |doc, cat| {
        let a = modify_in(doc, cat, b"Dests", |doc, o| {
            let Some(d) = as_dict_mut(o) else {
                return false;
            };
            let before = d.len();
            d.0.retain(|(k, v)| {
                let keep = !gone(doc, v);
                if !keep {
                    removed.insert(k.as_bytes().to_vec());
                }
                keep
            });
            d.len() != before
        });
        let b = modify_in(doc, cat, b"Names", |doc, o| {
            let Some(names) = as_dict_mut(o) else {
                return false;
            };
            let mut seen = HashSet::new();
            modify_in(doc, names, b"Dests", |doc, node| {
                prune_tree(doc, node, 0, &mut seen, gone, &mut removed)
            })
        });
        a || b
    });
    removed
}

/// Prunes a name tree node held by value; indirect kids are rewritten in place.
fn prune_tree(
    doc: &mut Document,
    node: &mut Object,
    depth: u32,
    seen: &mut HashSet<u32>,
    gone: &impl Fn(&Document, &Object) -> bool,
    removed: &mut HashSet<Vec<u8>>,
) -> bool {
    let Some(n) = as_dict_mut(node) else {
        return false;
    };
    if depth > MAX_DEPTH {
        return false;
    }
    let mut changed = modify_in(doc, n, b"Names", |doc, a| {
        let Object::Array(a) = a else { return false };
        let before = a.len();
        let mut kept = Vec::with_capacity(a.len());
        for p in a.as_chunks::<2>().0 {
            if gone(doc, &p[1]) {
                if let Some(k) = doc.resolve(&p[0]).as_string() {
                    removed.insert(k.to_vec());
                }
            } else {
                kept.extend_from_slice(p);
            }
        }
        *a = kept;
        a.len() != before
    });
    if changed {
        set_limits(doc, n);
    }
    changed |= modify_in(doc, n, b"Kids", |doc, kids| {
        let Object::Array(kids) = kids else {
            return false;
        };
        let mut changed = false;
        for k in kids.iter_mut() {
            if k.as_objref().is_some_and(|r| !seen.insert(r.num)) {
                continue;
            }
            if let Some(v) = modify(doc, k, |doc, o| {
                prune_tree(doc, o, depth + 1, seen, gone, removed)
            }) {
                *k = v;
                changed = true;
            }
        }
        changed
    });
    changed
}

/// Recomputes a leaf's `/Limits` from its (sorted) keys.
fn set_limits(doc: &Document, n: &mut Dict) {
    if !n.contains(b"Limits") {
        return;
    }
    let keys: Vec<Object> = doc
        .get_in(n, b"Names")
        .as_array()
        .map(|a| a.iter().step_by(2).cloned().collect())
        .unwrap_or_default();
    match (keys.first(), keys.last()) {
        (Some(a), Some(b)) => n.set(b"Limits", vec![a.clone(), b.clone()]),
        _ => {
            n.remove(b"Limits");
        }
    }
}
