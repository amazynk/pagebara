//! Article threads (ISO 32000-2 §12.4.3): beads on pages that remain are kept, in order,
//! and threads left without beads are dropped.

use std::collections::HashSet;

use pagebara_syntax::{Dict, Document, ObjRef, Object};

use crate::edit::update_catalog;
use crate::xfer::{Place, Xfer};

/// Beads followed per thread before the chain is taken to be broken.
const MAX_BEADS: usize = 1 << 20;

/// Keeps the threads of `x.src` in the target: in place, beads on removed pages are
/// unlinked; for copies, threads come along with the beads on copied pages.
pub(crate) fn carry(dst: &mut Document, x: &mut Xfer) {
    let src = x.src;
    let Some(cat) = src.catalog() else { return };
    let Some(cat) = cat.as_dict() else { return };
    let list = src.get_in(cat, b"Threads");
    let Some(list) = list.as_array() else { return };
    let in_place = x.is_in_place();

    let mut threads = Vec::new();
    let mut on_pages: Vec<(ObjRef, ObjRef)> = Vec::new();
    for t in list {
        let Some(td) = src.resolve(t).as_dict().cloned() else {
            continue;
        };
        let beads = chain(src, &td);
        let mut kept = Vec::new();
        for (r, d) in beads {
            match d.get(b"P").and_then(Object::as_objref).map(|p| x.page(p)) {
                Some(Place::Kept(p)) => kept.push((r, d, p)),
                _ if in_place => dst.delete(r),
                _ => x.leave(r),
            }
        }
        if kept.is_empty() {
            if in_place && let Some(r) = t.as_objref() {
                dst.delete(r);
            }
            continue;
        }
        let tr = x.target(dst, t.as_objref());
        let targets: Vec<ObjRef> = kept.iter().map(|b| x.target(dst, Some(b.0))).collect();
        let n = targets.len();
        for (i, (_, old, page)) in kept.iter().enumerate() {
            let mut d = if in_place { old.clone() } else { Dict::new() };
            d.set(b"Type", Object::name(b"Bead"));
            d.set(b"T", tr);
            d.set(b"N", targets[(i + 1) % n]);
            d.set(b"V", targets[(i + n - 1) % n]);
            d.set(b"P", *page);
            if let Some(rect) = old.get(b"R") {
                let rect = x.value(dst, rect);
                d.set(b"R", rect);
            }
            if !(in_place && &d == old) {
                dst.set(targets[i], d);
            }
            on_pages.push((*page, targets[i]));
        }
        let mut d = Dict::new();
        for (k, v) in td.iter() {
            let v = match k.as_bytes() {
                b"F" => targets[0].into(),
                _ => x.value(dst, v),
            };
            d.set(k.as_bytes(), v);
        }
        d.set(b"F", targets[0]);
        if !(in_place && d == td) {
            dst.set(tr, d);
        }
        threads.push(Object::Ref(tr));
    }

    if in_place {
        if threads.as_slice() != list {
            update_catalog(dst, |_, cat| {
                if threads.is_empty() {
                    cat.remove(b"Threads");
                } else {
                    cat.set(b"Threads", threads);
                }
                true
            });
        }
        return;
    }
    if threads.is_empty() {
        return;
    }
    let mut pages: Vec<ObjRef> = Vec::new();
    let mut seen = HashSet::new();
    for (p, _) in &on_pages {
        if seen.insert(p.num) {
            pages.push(*p);
        }
    }
    for p in pages {
        let Some(mut d) = dst.get(p).and_then(|o| o.as_dict().cloned()) else {
            continue;
        };
        let mut b: Vec<Object> = dst
            .get_in(&d, b"B")
            .as_array()
            .map(<[Object]>::to_vec)
            .unwrap_or_default();
        b.extend(
            on_pages
                .iter()
                .filter(|e| e.0 == p)
                .map(|e| Object::Ref(e.1)),
        );
        d.set(b"B", b);
        dst.set(p, d);
    }
    update_catalog(dst, |dst, cat| {
        let mut all: Vec<Object> = dst
            .get_in(cat, b"Threads")
            .as_array()
            .map(<[Object]>::to_vec)
            .unwrap_or_default();
        all.extend(threads);
        cat.set(b"Threads", all);
        true
    });
}

/// The beads of a thread in order, from `/F` along `/N` until it comes round.
fn chain(doc: &Document, thread: &Dict) -> Vec<(ObjRef, Dict)> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut cur = thread.get(b"F").and_then(Object::as_objref);
    while let Some(r) = cur {
        if !seen.insert(r.num) || out.len() >= MAX_BEADS {
            break;
        }
        let Some(d) = doc.get(r).and_then(|o| o.as_dict().cloned()) else {
            break;
        };
        cur = d.get(b"N").and_then(Object::as_objref);
        out.push((r, d));
    }
    out
}
