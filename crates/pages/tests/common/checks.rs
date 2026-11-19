//! Consistency checks on documents after page operations.

use std::collections::{BTreeMap, HashSet};

use pagebara_pages::PageTree;
use pagebara_syntax::{Document, ObjRef, Object};

use super::{catalog, dict};

/// Every reference reachable from the trailer leads to an object.
pub fn check_refs(doc: &Document) {
    let mut seen = HashSet::new();
    let mut stack: Vec<(Object, String)> = doc
        .trailer()
        .iter()
        .map(|(k, v)| {
            (
                v.clone(),
                format!("trailer/{}", String::from_utf8_lossy(k.as_bytes())),
            )
        })
        .collect();
    while let Some((o, path)) = stack.pop() {
        match o {
            Object::Ref(r) => {
                let o = doc.get(r);
                assert!(o.is_some(), "{path} -> {r:?} dangles");
                if seen.insert(r.num) {
                    stack.push(((*o.unwrap()).clone(), format!("{r:?}")));
                }
            }
            Object::Array(a) => stack.extend(a.into_iter().map(|o| (o, path.clone()))),
            Object::Dict(d) => stack.extend(d.0.into_iter().map(|(k, v)| {
                (
                    v,
                    format!("{path}/{}", String::from_utf8_lossy(k.as_bytes())),
                )
            })),
            Object::Stream(s) => stack.extend(s.dict.0.into_iter().map(|(k, v)| {
                (
                    v,
                    format!("{path}/{}", String::from_utf8_lossy(k.as_bytes())),
                )
            })),
            _ => {}
        }
    }
}

fn nums(doc: &Document, node: &Object, out: &mut BTreeMap<i64, Object>) {
    let n = doc.resolve(node);
    let n = n.as_dict().unwrap();
    if let Some(a) = doc.get_in(n, b"Nums").as_array() {
        for p in a.chunks(2) {
            out.insert(p[0].as_int().unwrap(), p[1].clone());
        }
    }
    if let Some(k) = doc.get_in(n, b"Kids").as_array() {
        for k in k {
            nums(doc, k, out);
        }
    }
}

/// The structure tree as `S(page indices)` lines in tree order, after checking that
/// parents, pages, object references, the parent tree and the ID tree are consistent.
pub fn check_structure(doc: &Document) -> Vec<String> {
    let tree = PageTree::load(doc);
    let index = tree.index();
    let cat = catalog(doc);
    let Some(root_ref) = cat.get(b"StructTreeRoot").and_then(Object::as_objref) else {
        return vec![];
    };
    let root = dict(doc, root_ref);
    let page_of = |o: Option<&Object>| -> Option<usize> {
        let r = o?.as_objref()?;
        Some(
            *index
                .get(&r.num)
                .unwrap_or_else(|| panic!("{r:?} is not a page")),
        )
    };
    let mut elems = HashSet::new();
    let mut out = Vec::new();
    fn walk(
        doc: &Document,
        r: ObjRef,
        (parent, parent_pg): (ObjRef, Option<usize>),
        depth: usize,
        page_of: &dyn Fn(Option<&Object>) -> Option<usize>,
        elems: &mut HashSet<u32>,
        out: &mut Vec<String>,
    ) {
        assert!(elems.insert(r.num), "{r:?} twice");
        let d = dict(doc, r);
        assert_eq!(
            d.get(b"P").and_then(Object::as_objref),
            Some(parent),
            "P of {r:?}"
        );
        let pg = page_of(d.get(b"Pg")).or(parent_pg);
        let mut pages = Vec::new();
        let at = out.len();
        out.push(String::new());
        let k = d.get(b"K").map(|k| (*doc.resolve(k)).clone());
        let items = match (&k, d.get(b"K")) {
            (Some(Object::Array(a)), _) => a.clone(),
            (Some(_), Some(k)) => vec![k.clone()],
            _ => vec![],
        };
        for item in items {
            match &*doc.resolve(&item) {
                Object::Int(_) => pages.push(pg.expect("an MCID needs a page")),
                Object::Dict(m) if m.name(b"Type") == Some(b"MCR") => {
                    pages.push(page_of(m.get(b"Pg")).or(pg).expect("MCR page"));
                }
                Object::Dict(m) if m.name(b"Type") == Some(b"OBJR") => {
                    let o = m
                        .get(b"Obj")
                        .and_then(Object::as_objref)
                        .expect("OBJR /Obj");
                    assert!(doc.get(o).is_some(), "OBJR {o:?} dangles");
                    if let Some(p) = page_of(m.get(b"Pg")).or(pg) {
                        pages.push(p);
                    }
                }
                Object::Dict(_) => walk(
                    doc,
                    item.as_objref().expect("indirect element"),
                    (r, pg),
                    depth + 1,
                    page_of,
                    elems,
                    out,
                ),
                o => panic!("odd kid {o:?}"),
            }
        }
        pages.dedup();
        let s = String::from_utf8_lossy(d.name(b"S").unwrap()).into_owned();
        out[at] = format!("{}{s}{pages:?}", "  ".repeat(depth));
    }
    let k = root.get(b"K").map(|k| (*doc.resolve(k)).clone());
    let tops = match k {
        Some(Object::Array(a)) => a,
        Some(_) => vec![root.get(b"K").unwrap().clone()],
        None => vec![],
    };
    for t in tops {
        walk(
            doc,
            t.as_objref().unwrap(),
            (root_ref, None),
            0,
            &page_of,
            &mut elems,
            &mut out,
        );
    }

    // The parent tree: every key in use has an entry and every entry a user.
    let mut entries = BTreeMap::new();
    if let Some(pt) = root.get(b"ParentTree") {
        nums(doc, pt, &mut entries);
    }
    for v in entries.values() {
        match &*doc.resolve(v) {
            Object::Array(a) => {
                for e in a {
                    if let Some(e) = e.as_objref() {
                        assert!(elems.contains(&e.num), "parent tree -> {e:?}");
                    }
                }
            }
            _ => assert!(
                elems.contains(&v.as_objref().unwrap().num),
                "parent tree -> {v:?}"
            ),
        }
    }
    let mut used = HashSet::new();
    for p in tree.pages() {
        let d = dict(doc, p.r);
        if let Some(k) = d.int(b"StructParents") {
            assert!(entries.contains_key(&k), "page key {k} has no entry");
            assert!(used.insert(k), "key {k} used twice");
        }
        for a in doc.get_in(&d, b"Annots").as_array().unwrap_or_default() {
            if let Some(k) = doc
                .resolve(a)
                .as_dict()
                .and_then(|a| a.int(b"StructParent"))
            {
                assert!(entries.contains_key(&k), "annotation key {k} has no entry");
                assert!(used.insert(k), "key {k} used twice");
            }
        }
    }
    for k in entries.keys() {
        assert!(used.contains(k), "parent tree key {k} unused");
    }
    if let Some(max) = entries.keys().last() {
        assert!(root.int(b"ParentTreeNextKey").unwrap() > *max);
    }
    if let Some(ids) = root.get(b"IDTree") {
        let ids = doc.resolve(ids);
        let a = doc.get_in(ids.as_dict().unwrap(), b"Names");
        for p in a.as_array().unwrap().chunks(2) {
            assert!(
                elems.contains(&p[1].as_objref().unwrap().num),
                "ID tree -> {:?}",
                p[1]
            );
        }
    }
    out
}

/// Each article thread as the page indices of its beads, after checking the bead chain
/// and the pages' `/B` arrays.
pub fn check_threads(doc: &Document) -> Vec<Vec<usize>> {
    let index = PageTree::load(doc).index();
    let cat = catalog(doc);
    let threads = doc.get_in(&cat, b"Threads");
    let mut out = Vec::new();
    for t in threads.as_array().unwrap_or_default() {
        let tr = t.as_objref().unwrap();
        let first = dict(doc, tr).get(b"F").and_then(Object::as_objref).unwrap();
        let mut pages = Vec::new();
        let mut cur = first;
        let mut prev = None;
        loop {
            let b = dict(doc, cur);
            if prev.is_none() {
                assert_eq!(b.get(b"T").and_then(Object::as_objref), Some(tr));
            }
            if let Some(p) = prev {
                assert_eq!(b.get(b"V").and_then(Object::as_objref), Some(p));
            }
            let page = b.get(b"P").and_then(Object::as_objref).unwrap();
            pages.push(index[&page.num]);
            let pd = dict(doc, page);
            let on_page = doc.get_in(&pd, b"B");
            assert!(
                on_page.as_array().unwrap().contains(&Object::Ref(cur)),
                "bead not in /B"
            );
            prev = Some(cur);
            cur = b.get(b"N").and_then(Object::as_objref).unwrap();
            if cur == first {
                break;
            }
            assert!(pages.len() < 1000);
        }
        assert_eq!(dict(doc, first).get(b"V").and_then(Object::as_objref), prev);
        out.push(pages);
    }
    out
}

/// The full consistency check after an operation: tree, references, structure, threads.
pub fn check_all(doc: &Document) {
    super::check_tree(doc);
    check_refs(doc);
    check_structure(doc);
    check_threads(doc);
}
