//! Name and number trees (ISO 32000-2 §7.9.6, §7.9.7), read whole and written flat.

use std::collections::{BTreeMap, HashSet};

use pagebara_syntax::{Dict, Document, Object, PdfString};

use crate::edit::{as_dict_mut, update_catalog};

const MAX_DEPTH: u32 = 64;

/// Calls `f` with each key and value of the name (`leaves` = `Names`) or number (`Nums`)
/// tree at `node`.
pub(crate) fn walk_tree(
    doc: &Document,
    node: &Object,
    leaves: &[u8],
    depth: u32,
    seen: &mut HashSet<u32>,
    f: &mut impl FnMut(&Object, &Object),
) {
    if depth > MAX_DEPTH || node.as_objref().is_some_and(|r| !seen.insert(r.num)) {
        return;
    }
    let n = doc.resolve(node);
    let Some(n) = n.as_dict() else { return };
    if let Some(a) = doc.get_in(n, leaves).as_array() {
        for p in a.as_chunks::<2>().0 {
            f(&doc.resolve(&p[0]), &p[1]);
        }
    }
    if let Some(kids) = doc.get_in(n, b"Kids").as_array() {
        for k in kids {
            walk_tree(doc, k, leaves, depth + 1, seen, f);
        }
    }
}

/// A name tree's entries in tree order; later duplicates are dropped.
pub(crate) fn read_names(doc: &Document, root: &Object) -> Vec<(Vec<u8>, Object)> {
    let mut out = Vec::new();
    let mut keys = HashSet::new();
    walk_tree(doc, root, b"Names", 0, &mut HashSet::new(), &mut |k, v| {
        if let Some(k) = k.as_string()
            && keys.insert(k.to_vec())
        {
            out.push((k.to_vec(), v.clone()));
        }
    });
    out
}

pub(crate) fn read_nums(doc: &Document, root: &Object) -> BTreeMap<i64, Object> {
    let mut out = BTreeMap::new();
    walk_tree(doc, root, b"Nums", 0, &mut HashSet::new(), &mut |k, v| {
        if let Some(k) = k.as_int() {
            out.entry(k).or_insert_with(|| v.clone());
        }
    });
    out
}

/// A single-node name tree, sorted as the format requires.
pub(crate) fn names_tree(mut entries: Vec<(Vec<u8>, Object)>) -> Dict {
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut a = Vec::with_capacity(entries.len() * 2);
    for (k, v) in entries {
        a.push(Object::String(PdfString {
            bytes: k,
            hex: false,
        }));
        a.push(v);
    }
    let mut d = Dict::new();
    d.set(b"Names", a);
    d
}

pub(crate) fn nums_tree(entries: &BTreeMap<i64, Object>) -> Dict {
    let mut a = Vec::with_capacity(entries.len() * 2);
    for (k, v) in entries {
        a.push(Object::Int(*k));
        a.push(v.clone());
    }
    let mut d = Dict::new();
    d.set(b"Nums", a);
    d
}

/// `base`, or `base#2`, `base#3`... whichever `taken` does not hold. UTF-16 text strings
/// get the suffix in UTF-16.
pub(crate) fn unique(base: &[u8], taken: impl Fn(&[u8]) -> bool) -> Vec<u8> {
    if !taken(base) {
        return base.to_vec();
    }
    let utf16 = base.starts_with(b"\xfe\xff");
    (2u32..)
        .map(|n| {
            let mut k = base.to_vec();
            let suffix = format!("#{n}");
            if utf16 {
                k.extend(suffix.encode_utf16().flat_map(u16::to_be_bytes));
            } else {
                k.extend(suffix.bytes());
            }
            k
        })
        .find(|k| !taken(k))
        .expect("an unused suffix")
}

/// Adds entries to the catalog's `/Names` tree `key`, renaming those whose key is taken
/// (by the tree or by `also_taken`). Returns the keys used.
pub(crate) fn add_names(
    doc: &mut Document,
    key: &[u8],
    entries: Vec<(Vec<u8>, Object)>,
    also_taken: &HashSet<Vec<u8>>,
) -> Vec<Vec<u8>> {
    if entries.is_empty() {
        return Vec::new();
    }
    let names = doc
        .catalog()
        .and_then(|c| c.as_dict()?.get(b"Names").cloned())
        .unwrap_or_default();
    let names = doc.resolve(&names).as_dict().cloned().unwrap_or_default();
    let old = names.get(key).cloned();
    let mut all = old.as_ref().map(|o| read_names(doc, o)).unwrap_or_default();
    let mut taken: HashSet<Vec<u8>> = all.iter().map(|e| e.0.clone()).collect();
    taken.extend(also_taken.iter().cloned());
    let mut used = Vec::with_capacity(entries.len());
    for (k, v) in entries {
        let k = unique(&k, |k| taken.contains(k));
        taken.insert(k.clone());
        used.push(k.clone());
        all.push((k, v));
    }
    let tree = names_tree(all);
    let tree_ref = match old.as_ref().and_then(Object::as_objref) {
        Some(r) => {
            doc.set(r, tree);
            None
        }
        None => Some(doc.add(tree)),
    };
    if let Some(t) = tree_ref {
        update_catalog(doc, |doc, cat| {
            let mut holder = cat.get(b"Names").cloned().unwrap_or(Dict::new().into());
            if let Some(r) = holder.as_objref() {
                let mut d = doc.get(r).map(|o| (*o).clone()).unwrap_or_default();
                if let Some(d2) = as_dict_mut(&mut d) {
                    d2.set(key, t);
                } else {
                    d = {
                        let mut n = Dict::new();
                        n.set(key, t);
                        n.into()
                    };
                }
                doc.set(r, d);
                return false;
            }
            match as_dict_mut(&mut holder) {
                Some(d) => d.set(key, t),
                None => {
                    let mut n = Dict::new();
                    n.set(key, t);
                    holder = n.into();
                }
            }
            cat.set(b"Names", holder);
            true
        });
    }
    used
}
