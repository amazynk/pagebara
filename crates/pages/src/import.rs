//! Copying pages between documents.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use pagebara_syntax::xref::NumMap;
use pagebara_syntax::{Dict, Document, ObjRef, Object};

use crate::dest::{Target, dest_of, explicit, named, target};
use crate::edit::{as_dict_mut, modify_in, update_catalog};
use crate::names::unique;
use crate::tree::{INHERITABLE, PageTree};
use crate::xfer::copy_value;

/// Copies the pages `sel` (indices into `tree`, the tree of `src`) into `dst` as new page
/// objects without a parent. Objects the pages share (fonts, images) are copied once.
/// Links to pages that are not copied are dropped, named links become explicit, and form
/// fields come along with their widgets, renamed where their names are taken.
///
/// Returns the new pages in order, the import map and the stub object that stands for
/// everything left behind; the caller deletes the stub once done copying.
pub(crate) fn import_pages(
    dst: &mut Document,
    src: &Document,
    tree: &PageTree,
    sel: &[usize],
) -> (Vec<ObjRef>, NumMap<ObjRef>, ObjRef) {
    // Everything outside the selection that copied objects may point to maps here, so
    // the copy never pulls in other pages or the source's page tree.
    let stub = dst.add(Object::Null);
    let wanted: HashSet<usize> = sel.iter().copied().collect();
    let mut map: NumMap<ObjRef> = NumMap::default();
    let others = tree
        .pages()
        .iter()
        .enumerate()
        .filter(|(i, _)| !wanted.contains(i));
    for r in others
        .map(|(_, p)| p.r)
        .chain(tree.root())
        .chain(tree.nodes().iter().copied())
    {
        map.insert(r.num, stub);
    }
    if let Some(c) = src.trailer().get(b"Root").and_then(Object::as_objref) {
        map.insert(c.num, stub);
    }

    let mut out = Vec::with_capacity(sel.len());
    let mut done = HashSet::new();
    let mut shared = HashMap::new();
    for &i in sel {
        let page = &tree.pages()[i];
        if !done.insert(i) {
            // The same source page again: a second, independent page object.
            let first = out[sel.iter().position(|&j| j == i).unwrap_or(0)];
            out.push(crate::ops::copy_page(dst, first));
            continue;
        }
        let Some(mut d) = src.get(page.r).and_then(|o| o.as_dict().cloned()) else {
            out.push(empty_page(dst, page.media_box));
            continue;
        };
        d.remove(b"Parent");
        d.remove(b"B");
        // Links on earlier pages may already have pulled this one in.
        let new = match map.get(&page.r.num) {
            Some(&n) => n,
            None => {
                let n = dst.add(Object::Null);
                map.insert(page.r.num, n);
                n
            }
        };
        let mut inherited = Vec::new();
        for (key, v) in INHERITABLE.into_iter().zip(&page.inherited.0) {
            let Some(v) = v.as_ref().filter(|_| !d.contains(key)) else {
                continue;
            };
            let v = match v {
                // Made indirect once, so the pages sharing it keep sharing it.
                Object::Dict(_) if key == b"Resources" => {
                    let k = Arc::as_ptr(&page.inherited) as usize;
                    let r = *shared.entry(k).or_insert_with(|| {
                        let c = copy_value(dst, src, v.clone(), &mut map);
                        dst.add(c)
                    });
                    r.into()
                }
                v => copy_value(dst, src, v.clone(), &mut map),
            };
            inherited.push((key, v));
        }
        let mut d = copy_value(dst, src, d.into(), &mut map);
        if let Some(d) = as_dict_mut(&mut d) {
            for (k, v) in inherited {
                d.set(k, v);
            }
        }
        dst.set(new, d);
        out.push(new);
    }

    restore_parents(dst, src, &mut map, stub);
    let mut dropped = Vec::new();
    drop_foreign_widgets(dst, &out, &map, stub, &mut dropped);
    let mut names = None;
    for &p in &out {
        fix_links(dst, src, p, &mut map, stub, &mut names, &mut dropped);
    }
    // Left behind after all, so the structure tree does not keep them.
    let dropped: HashSet<u32> = dropped.into_iter().map(|r| r.num).collect();
    for r in map.values_mut() {
        if dropped.contains(&r.num) {
            dst.delete(*r);
            *r = stub;
        }
    }
    merge_fields(dst, src, &mut map, stub);
    (out, map, stub)
}

fn empty_page(dst: &mut Document, media_box: crate::Rect) -> ObjRef {
    let mut d = Dict::new();
    d.set(b"Type", Object::name(b"Page"));
    d.set(b"MediaBox", media_box.to_object());
    d.set(b"Resources", Dict::new());
    dst.add(d)
}

/// `Document::import` drops every `/Parent`; this puts back those that are not page tree
/// links (form field hierarchy, pop-up annotations).
fn restore_parents(dst: &mut Document, src: &Document, map: &mut NumMap<ObjRef>, stub: ObjRef) {
    let mut done: HashSet<u32> = HashSet::new();
    loop {
        let todo: Vec<(u32, ObjRef)> = map
            .iter()
            .filter(|(n, r)| **r != stub && !done.contains(n))
            .map(|(&n, &r)| (n, r))
            .collect();
        if todo.is_empty() {
            return;
        }
        for (num, new) in todo {
            done.insert(num);
            let Some(parent) = src
                .reference(num)
                .and_then(|r| src.get(r))
                .and_then(|o| o.as_dict()?.get(b"Parent")?.as_objref())
            else {
                continue;
            };
            if map.get(&parent.num) == Some(&stub) {
                continue;
            }
            let p = dst.import(src, parent, map);
            if let Some(mut o) = dst.get(new).map(|o| (*o).clone())
                && let Some(d) = as_dict_mut(&mut o)
            {
                d.set(b"Parent", p);
                dst.set(new, o);
            }
        }
    }
}

/// Removes from copied fields the widgets of pages that were not copied.
fn drop_foreign_widgets(
    dst: &mut Document,
    pages: &[ObjRef],
    map: &NumMap<ObjRef>,
    stub: ObjRef,
    dropped: &mut Vec<ObjRef>,
) {
    let mut on_pages = HashSet::new();
    for &p in pages {
        if let Some(d) = dst.get(p).and_then(|o| o.as_dict().cloned())
            && let Some(a) = dst.get_in(&d, b"Annots").as_array()
        {
            on_pages.extend(a.iter().filter_map(|o| Some(o.as_objref()?.num)));
        }
    }
    for &r in map.values() {
        if r == stub {
            continue;
        }
        let Some(mut d) = dst.get(r).and_then(|o| o.as_dict().cloned()) else {
            continue;
        };
        if !d.contains(b"Kids") || d.name(b"Type") == Some(b"Pages") {
            continue;
        }
        let changed = modify_in(dst, &mut d, b"Kids", |dst, a| {
            let Object::Array(a) = a else { return false };
            let before = a.len();
            a.retain(|k| {
                let keep = k.as_objref().is_none_or(|k| {
                    on_pages.contains(&k.num)
                        || dst.get(k).is_none_or(|o| {
                            o.as_dict()
                                .is_none_or(|d| d.name(b"Subtype") != Some(b"Widget"))
                        })
                });
                if !keep {
                    dropped.extend(k.as_objref());
                }
                keep
            });
            a.len() != before
        });
        if changed {
            dst.set(r, d);
        }
    }
}

/// Makes the links of copied page `p` point into `dst`, dropping those that lead to pages
/// that were not copied.
fn fix_links(
    dst: &mut Document,
    src: &Document,
    p: ObjRef,
    map: &mut NumMap<ObjRef>,
    stub: ObjRef,
    names: &mut Option<HashMap<Vec<u8>, Object>>,
    dropped: &mut Vec<ObjRef>,
) {
    let Some(mut page) = dst.get(p).and_then(|o| o.as_dict().cloned()) else {
        return;
    };
    let changed = modify_in(dst, &mut page, b"Annots", |dst, a| {
        let Object::Array(a) = a else { return false };
        let before = a.len();
        a.retain(|o| {
            let Some(r) = o.as_objref() else { return true };
            let Some(mut d) = dst.get(r).and_then(|o| o.as_dict().cloned()) else {
                return true;
            };
            if d.name(b"Subtype") != Some(b"Link") {
                return true;
            }
            let keep = match dest_of(dst, &d) {
                Some(Target::Page(t)) => t != stub,
                Some(Target::Named(n)) => 'named: {
                    let names = names.get_or_insert_with(|| named(src));
                    let Some(Target::Page(t)) = names.get(&n).map(|v| target(src, v)) else {
                        break 'named false;
                    };
                    let Some(&t) = map.get(&t.num).filter(|&&t| t != stub) else {
                        break 'named false;
                    };
                    let Some(dest) = explicit(src, &names[&n], t) else {
                        break 'named false;
                    };
                    d.remove(b"A");
                    d.set(b"Dest", dest);
                    dst.set(r, d);
                    true
                }
                _ => true,
            };
            if !keep {
                dropped.push(r);
            }
            keep
        });
        a.len() != before
    });
    if changed {
        dst.set(p, page);
    }
}

/// Adds the copied form fields to `dst`'s interactive form. Top-level fields whose name is
/// taken get a `#2`-style suffix, which renames their descendants with them (a radio
/// group's buttons stay one group). Fields keep their source's default appearance.
fn merge_fields(dst: &mut Document, src: &Document, map: &mut NumMap<ObjRef>, stub: ObjRef) {
    let Some(cat) = src.catalog() else { return };
    let Some(af) = cat
        .as_dict()
        .and_then(|c| src.get_in(c, b"AcroForm").as_dict().cloned())
    else {
        return;
    };
    let mapped = |o: &Object| map.get(&o.as_objref()?.num).copied().filter(|&r| r != stub);
    let roots: Vec<ObjRef> = src
        .get_in(&af, b"Fields")
        .as_array()
        .unwrap_or_default()
        .iter()
        .filter_map(mapped)
        .collect();
    if roots.is_empty() {
        return;
    }
    let co: Vec<Object> = src
        .get_in(&af, b"CO")
        .as_array()
        .unwrap_or_default()
        .iter()
        .filter_map(mapped)
        .map(Object::Ref)
        .collect();

    let form = dst
        .catalog()
        .and_then(|c| dst.get_in(c.as_dict()?, b"AcroForm").as_dict().cloned())
        .unwrap_or_default();
    let mut taken: HashSet<Vec<u8>> = dst
        .get_in(&form, b"Fields")
        .as_array()
        .unwrap_or_default()
        .iter()
        .filter_map(|f| Some(dst.resolve(f).as_dict()?.get(b"T")?.as_string()?.to_vec()))
        .collect();
    let da = af.get(b"DA").map(|o| (*src.resolve(o)).clone());
    let q = af.get(b"Q").map(|o| (*src.resolve(o)).clone());
    let own_da = form.get(b"DA").map(|o| (*dst.resolve(o)).clone());
    let own_q = form.get(b"Q").map(|o| (*dst.resolve(o)).clone());
    for &r in &roots {
        let Some(mut d) = dst.get(r).and_then(|o| o.as_dict().cloned()) else {
            continue;
        };
        let before = d.clone();
        if let Some(t) = d.get(b"T").and_then(Object::as_string) {
            let t = unique(t, |k| taken.contains(k));
            taken.insert(t.clone());
            if d.get(b"T").and_then(Object::as_string) != Some(&t[..]) {
                d.set(b"T", Object::string(&t));
            }
        }
        // Inherited from the form: pinned where the target's form says otherwise.
        for (key, theirs, ours) in [(&b"DA"[..], &da, &own_da), (b"Q", &q, &own_q)] {
            if let Some(v) = theirs
                && ours.is_some()
                && ours.as_ref() != Some(v)
                && !d.contains(key)
            {
                d.set(key, v.clone());
            }
        }
        if d != before {
            dst.set(r, d);
        }
    }

    let dr = af
        .get(b"DR")
        .cloned()
        .map(|dr| copy_value(dst, src, dr, map));
    let dr = dr.and_then(|dr| dst.resolve(&dr).as_dict().cloned());
    let need_appearances = src.get_in(&af, b"NeedAppearances").as_bool() == Some(true);
    let sig_flags = src.get_in(&af, b"SigFlags").as_int().unwrap_or(0);
    update_catalog(dst, |dst, cat| {
        if !cat.contains(b"AcroForm") {
            cat.set(b"AcroForm", Dict::new());
        }
        modify_in(dst, cat, b"AcroForm", |dst, o| {
            let Some(form) = as_dict_mut(o) else {
                return false;
            };
            for (key, add) in [
                (&b"Fields"[..], roots.into_iter().map(Object::Ref).collect()),
                (b"CO", co),
            ] {
                if add.is_empty() {
                    continue;
                }
                if !form.contains(key) {
                    form.set(key, Vec::<Object>::new());
                }
                modify_in(dst, form, key, |_, f| match f {
                    Object::Array(f) => {
                        f.extend(add);
                        true
                    }
                    _ => false,
                });
            }
            if let Some(dr) = dr {
                if !form.contains(b"DR") {
                    form.set(b"DR", Dict::new());
                }
                modify_in(dst, form, b"DR", |dst, own| {
                    let Some(own) = as_dict_mut(own) else {
                        return false;
                    };
                    merge_resources(dst, own, &dr)
                });
            }
            if let Some(da) = da
                && !form.contains(b"DA")
            {
                form.set(b"DA", da);
            }
            if need_appearances {
                form.set(b"NeedAppearances", true);
            }
            if sig_flags != 0 {
                let own = dst.get_in(form, b"SigFlags").as_int().unwrap_or(0);
                form.set(b"SigFlags", own | sig_flags);
            }
            true
        });
        true
    });
}

/// Adds the resources of `theirs` that `own` lacks, by category (`/Font`, ...) and name.
fn merge_resources(dst: &mut Document, own: &mut Dict, theirs: &Dict) -> bool {
    let mut changed = false;
    for (cat, v) in theirs.iter() {
        let cat = cat.as_bytes();
        let Some(add) = dst.resolve(v).as_dict().cloned() else {
            if !own.contains(cat) {
                own.set(cat, v.clone());
                changed = true;
            }
            continue;
        };
        if !own.contains(cat) {
            own.set(cat, add);
            changed = true;
            continue;
        }
        changed |= modify_in(dst, own, cat, |_, o| {
            let Some(o) = as_dict_mut(o) else {
                return false;
            };
            let mut c = false;
            for (name, v) in add.iter() {
                if !o.contains(name.as_bytes()) {
                    o.set(name.as_bytes(), v.clone());
                    c = true;
                }
            }
            c
        });
    }
    changed
}
