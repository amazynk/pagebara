//! Keeping the rest of the document consistent when pages disappear.

use std::collections::HashSet;

use pagebara_syntax::{Dict, Document, ObjRef, Object};

use crate::dest::{Target, dest_of, prune_named, target};
use crate::edit::{as_dict_mut, dict_of, modify_in, update_catalog};
use crate::xfer::Xfer;
use crate::{structure, threads};

const MAX_DEPTH: u32 = 64;

/// Removes everything that would point at the pages in `gone`: named destinations,
/// outline destinations, the open action, links on `kept` pages, form fields whose
/// widgets were on gone pages, structure elements and article thread beads. Then deletes
/// the gone pages and their annotations. `before` is the document before the edit.
pub(crate) fn forget_pages(
    doc: &mut Document,
    before: &Document,
    gone: &HashSet<u32>,
    kept: &[ObjRef],
) {
    let removed = prune_named(
        doc,
        &|doc, v| matches!(target(doc, v), Target::Page(r) if gone.contains(&r.num)),
    );
    let dead = |doc: &Document, d: &Dict| match dest_of(doc, d) {
        Some(Target::Page(r)) => gone.contains(&r.num),
        Some(Target::Named(n)) => removed.contains(&n),
        _ => false,
    };

    update_catalog(doc, |doc, cat| {
        let stale = match &*doc.get_in(cat, b"OpenAction") {
            Object::Dict(a) => dead(doc, a),
            Object::Array(_) => {
                let mut d = Dict::new();
                d.set(b"Dest", cat.get(b"OpenAction").cloned().unwrap_or_default());
                dead(doc, &d)
            }
            _ => false,
        };
        if stale {
            cat.remove(b"OpenAction");
        }
        stale
    });

    let mut kept_annots = HashSet::new();
    let mut dropped_links = Vec::new();
    for &p in kept {
        let Some(mut d) = doc.get(p).and_then(|o| o.as_dict().cloned()) else {
            continue;
        };
        let changed = modify_in(doc, &mut d, b"Annots", |doc, a| {
            let Object::Array(a) = a else { return false };
            let before = a.len();
            a.retain(|o| {
                let v = doc.resolve(o);
                let drop = v
                    .as_dict()
                    .is_some_and(|d| d.name(b"Subtype") == Some(b"Link") && dead(doc, d));
                if drop {
                    dropped_links.extend(o.as_objref());
                }
                !drop
            });
            a.len() != before
        });
        if let Some(a) = doc.get_in(&d, b"Annots").as_array() {
            kept_annots.extend(a.iter().filter_map(|o| Some(o.as_objref()?.num)));
        }
        if changed {
            doc.set(p, d);
        }
    }

    let mut deleted = Vec::new();
    let mut gone_annots = HashSet::new();
    for &num in gone {
        let Some(r) = doc.reference(num) else {
            continue;
        };
        deleted.push(r);
        if let Some(d) = doc.get(r).and_then(|o| o.as_dict().cloned())
            && let Some(a) = doc.get_in(&d, b"Annots").as_array()
        {
            gone_annots.extend(a.iter().filter_map(|o| o.as_objref()));
        }
        doc.delete(r);
    }
    let gone_nums: HashSet<u32> = gone_annots.iter().map(|r| r.num).collect();
    for r in prune_fields(doc, &gone_nums) {
        doc.delete(r);
        deleted.push(r);
    }
    for r in gone_annots.into_iter().chain(dropped_links) {
        if !kept_annots.contains(&r.num) {
            doc.delete(r);
            deleted.push(r);
        }
    }

    let gone_keys: HashSet<i64> = deleted
        .iter()
        .filter_map(|&r| before.get(r))
        .filter_map(|o| {
            let d = o.as_dict()?;
            d.int(b"StructParents").or_else(|| d.int(b"StructParent"))
        })
        .collect();
    let kept_nums = kept.iter().map(|r| r.num).collect();
    let mut x = Xfer::in_place(before, kept_nums, gone.clone());
    structure::prune(doc, &mut x, &gone_keys);
    threads::carry(doc, &mut x);
    fix_outlines(doc, &dead);
}

fn fix_outlines(doc: &mut Document, dead: &impl Fn(&Document, &Dict) -> bool) {
    let Some(cat) = doc.catalog() else { return };
    let Some(outlines) = cat
        .as_dict()
        .and_then(|c| dict_of(doc, c.get(b"Outlines")?))
    else {
        return;
    };
    let mut seen = HashSet::new();
    let mut stack: Vec<ObjRef> = outlines
        .get(b"First")
        .and_then(Object::as_objref)
        .into_iter()
        .collect();
    while let Some(r) = stack.pop() {
        if !seen.insert(r.num) {
            continue;
        }
        let Some(mut d) = doc.get(r).and_then(|o| o.as_dict().cloned()) else {
            continue;
        };
        stack.extend(
            ["Next", "First"]
                .iter()
                .filter_map(|k| d.get(k.as_bytes())?.as_objref()),
        );
        let mut changed = false;
        if dead(doc, &d) {
            if d.remove(b"Dest").is_none() {
                d.remove(b"A");
            }
            changed = true;
        }
        if d.get(b"SE")
            .and_then(Object::as_objref)
            .is_some_and(|se| doc.get(se).is_none())
        {
            d.remove(b"SE");
            changed = true;
        }
        if changed {
            doc.set(r, d);
        }
    }
}

/// Drops widgets in `gone` from the form's field trees, and fields left without widgets.
/// Returns the removed field objects.
fn prune_fields(doc: &mut Document, gone: &HashSet<u32>) -> Vec<ObjRef> {
    let mut removed = Vec::new();
    if gone.is_empty() {
        return removed;
    }
    update_catalog(doc, |doc, cat| {
        modify_in(doc, cat, b"AcroForm", |doc, af| {
            let Some(af) = as_dict_mut(af) else {
                return false;
            };
            let mut seen = HashSet::new();
            let fields = modify_in(doc, af, b"Fields", |doc, a| {
                retain_fields(doc, a, gone, &mut seen, 0, &mut removed)
            });
            let dropped: HashSet<u32> = removed.iter().map(|r| r.num).collect();
            let co = modify_in(doc, af, b"CO", |_, a| {
                let Object::Array(a) = a else { return false };
                let before = a.len();
                a.retain(|o| {
                    o.as_objref()
                        .is_none_or(|r| !gone.contains(&r.num) && !dropped.contains(&r.num))
                });
                a.len() != before
            });
            fields || co
        })
    });
    removed
}

fn retain_fields(
    doc: &mut Document,
    a: &mut Object,
    gone: &HashSet<u32>,
    seen: &mut HashSet<u32>,
    depth: u32,
    removed: &mut Vec<ObjRef>,
) -> bool {
    let Object::Array(a) = a else { return false };
    let before = a.len();
    let mut kept = Vec::with_capacity(a.len());
    for o in a.drain(..) {
        if o.as_objref()
            .is_none_or(|r| keep_field(doc, r, gone, seen, depth, removed))
        {
            kept.push(o);
        }
    }
    *a = kept;
    a.len() != before
}

fn keep_field(
    doc: &mut Document,
    r: ObjRef,
    gone: &HashSet<u32>,
    seen: &mut HashSet<u32>,
    depth: u32,
    removed: &mut Vec<ObjRef>,
) -> bool {
    if gone.contains(&r.num) {
        return false;
    }
    if depth > MAX_DEPTH || !seen.insert(r.num) {
        return true;
    }
    let Some(mut d) = doc.get(r).and_then(|o| o.as_dict().cloned()) else {
        return true;
    };
    if doc
        .get_in(&d, b"Kids")
        .as_array()
        .is_none_or(<[Object]>::is_empty)
    {
        return true;
    }
    let mut empty = false;
    let changed = modify_in(doc, &mut d, b"Kids", |doc, a| {
        let c = retain_fields(doc, a, gone, seen, depth + 1, removed);
        empty = a.as_array().is_some_and(<[Object]>::is_empty);
        c
    });
    if empty {
        removed.push(r);
        return false;
    }
    if changed {
        doc.set(r, d);
    }
    true
}
