//! Copying the document outline (ISO 32000-2 §12.3.3) along with pages: items whose
//! destinations were copied are kept, with their ancestors, in their hierarchy.

use std::collections::{HashMap, HashSet};

use pagebara_syntax::{Dict, Document, ObjRef, Object};

use crate::dest::{Target, dest_of, explicit, target};
use crate::edit::update_catalog;
use crate::xfer::{Place, Xfer};

const MAX_DEPTH: u32 = 64;

/// An outline item to write.
#[derive(Debug)]
pub(crate) struct Item {
    pub title: Object,
    pub dest: Option<Object>,
    /// `/C`, `/F` and `/SE`, already in the target.
    pub extra: Vec<(&'static [u8], Object)>,
    pub open: bool,
    pub kids: Vec<Item>,
}

impl Item {
    pub fn new(title: &[u8], page: ObjRef) -> Item {
        Item {
            title: Object::string(title),
            dest: Some(vec![page.into(), Object::name(b"Fit")].into()),
            extra: Vec::new(),
            open: false,
            kids: Vec::new(),
        }
    }
}

/// The source's outline items that lead to copied pages, or have descendants that do.
/// Named destinations become explicit.
pub(crate) fn collect(dst: &mut Document, x: &mut Xfer) -> Vec<Item> {
    let src = x.src;
    let Some(first) = src
        .catalog()
        .and_then(|c| {
            let c = c.as_dict()?;
            src.get_in(c, b"Outlines").as_dict()?.get(b"First").cloned()
        })
        .and_then(|f| f.as_objref())
    else {
        return Vec::new();
    };
    let named = crate::dest::named(src);
    siblings(dst, x, &named, Some(first), 0, &mut HashSet::new())
}

fn siblings(
    dst: &mut Document,
    x: &mut Xfer,
    named: &HashMap<Vec<u8>, Object>,
    first: Option<ObjRef>,
    depth: u32,
    seen: &mut HashSet<u32>,
) -> Vec<Item> {
    let src = x.src;
    let mut out = Vec::new();
    let mut cur = first;
    while let Some(r) = cur {
        if depth > MAX_DEPTH || !seen.insert(r.num) {
            break;
        }
        let Some(d) = src.get(r).and_then(|o| o.as_dict().cloned()) else {
            break;
        };
        cur = d.get(b"Next").and_then(Object::as_objref);
        let first_kid = d.get(b"First").and_then(Object::as_objref);
        let kids = siblings(dst, x, named, first_kid, depth + 1, seen);
        let dest = destination(x, named, &d);
        if dest.is_none() && kids.is_empty() {
            continue;
        }
        let mut extra = Vec::new();
        for key in [&b"C"[..], b"F"] {
            if let Some(v) = d.get(key) {
                extra.push((key, (*src.resolve(v)).clone()));
            }
        }
        if let Some(se) = d.get(b"SE").and_then(Object::as_objref)
            && let Some(se) = x.object(dst, se)
        {
            extra.push((b"SE", se.into()));
        }
        out.push(Item {
            title: (*src.get_in(&d, b"Title")).clone(),
            dest,
            extra,
            open: src.get_in(&d, b"Count").as_int().is_none_or(|c| c > 0),
            kids,
        });
    }
    out
}

/// An item's destination in the target, if its page was copied.
fn destination(x: &Xfer, named: &HashMap<Vec<u8>, Object>, d: &Dict) -> Option<Object> {
    let src = x.src;
    let raw = match d.get(b"Dest") {
        Some(dest) => dest.clone(),
        None => src.get_in(d, b"A").as_dict()?.get(b"D")?.clone(),
    };
    let (page, dest) = match dest_of(src, d)? {
        Target::Page(p) => (p, raw),
        Target::Named(n) => {
            let v = named.get(&n)?;
            match target(src, v) {
                Target::Page(p) => (p, v.clone()),
                _ => return None,
            }
        }
        Target::Other => return None,
    };
    match x.page(page) {
        Place::Kept(p) => explicit(src, &dest, p),
        _ => None,
    }
}

/// Adds `items` at the end of the target's outline, creating it if needed.
pub(crate) fn append(dst: &mut Document, items: Vec<Item>) {
    if items.is_empty() {
        return;
    }
    let existing = dst
        .catalog()
        .and_then(|c| c.as_dict()?.get(b"Outlines")?.as_objref());
    let (root, mut root_dict) =
        match existing.and_then(|r| Some((r, dst.get(r)?.as_dict()?.clone()))) {
            Some(x) => x,
            None => {
                let mut d = Dict::new();
                d.set(b"Type", Object::name(b"Outlines"));
                (dst.add(Object::Null), d)
            }
        };
    let last = root_dict
        .get(b"Last")
        .and_then(Object::as_objref)
        .filter(|r| dst.get(*r).is_some_and(|o| o.as_dict().is_some()));
    let (first, new_last, visible) = write_level(dst, root, items, last);
    if let Some(l) = last
        && let Some(mut d) = dst.get(l).and_then(|o| o.as_dict().cloned())
    {
        d.set(b"Next", first);
        dst.set(l, d);
    } else {
        root_dict.set(b"First", first);
    }
    root_dict.set(b"Last", new_last);
    let before = root_dict.int(b"Count").unwrap_or(0).max(0);
    root_dict.set(b"Count", before + visible);
    dst.set(root, root_dict);
    update_catalog(dst, |_, cat| {
        let stale = cat.get(b"Outlines") != Some(&Object::Ref(root));
        if stale {
            cat.set(b"Outlines", root);
        }
        stale
    });
}

/// Writes one level under `parent`; returns its first and last items and how many items
/// show when `parent` is open.
fn write_level(
    dst: &mut Document,
    parent: ObjRef,
    items: Vec<Item>,
    prev: Option<ObjRef>,
) -> (ObjRef, ObjRef, i64) {
    let refs: Vec<ObjRef> = items.iter().map(|_| dst.add(Object::Null)).collect();
    let mut visible = items.len() as i64;
    for (i, item) in items.into_iter().enumerate() {
        let mut d = Dict::new();
        d.set(b"Title", item.title);
        d.set(b"Parent", parent);
        if let Some(p) = if i == 0 { prev } else { Some(refs[i - 1]) } {
            d.set(b"Prev", p);
        }
        if let Some(n) = refs.get(i + 1) {
            d.set(b"Next", *n);
        }
        if let Some(dest) = item.dest {
            d.set(b"Dest", dest);
        }
        for (k, v) in item.extra {
            d.set(k, v);
        }
        if !item.kids.is_empty() {
            let (f, l, n) = write_level(dst, refs[i], item.kids, None);
            d.set(b"First", f);
            d.set(b"Last", l);
            d.set(b"Count", if item.open { n } else { -n });
            if item.open {
                visible += n;
            }
        }
        dst.set(refs[i], d);
    }
    (refs[0], refs[refs.len() - 1], visible)
}
