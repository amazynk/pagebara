//! Copying pages with what belongs to them beyond the page objects: structure, threads,
//! optional content, named destinations and, for new documents, the outline, metadata,
//! viewer settings, attachments and scripts.

use std::collections::{HashMap, HashSet};

use pagebara_syntax::{Dict, Document, ObjRef, Object, Stream};

use crate::dest::{Target, explicit, named, target};
use crate::edit::update_catalog;
use crate::import::import_pages;
use crate::names::{add_names, read_names};
use crate::outline::{self, Item};
use crate::tree::PageTree;
use crate::xfer::{Place, Xfer};
use crate::{structure, threads};

/// What comes along with the pages.
#[derive(Clone, Debug, Default)]
pub(crate) struct Carry {
    /// `/Info`, `/Lang`, `/ViewerPreferences`, `/PageMode` and `/PageLayout`, where the
    /// target has none.
    pub document: bool,
    pub outline: Outline,
    /// The embedded files and JavaScript name trees.
    pub attachments: bool,
    /// Every page is copied, so structure whose page cannot be told is kept too.
    pub all_pages: bool,
}

#[derive(Clone, Debug, Default)]
pub(crate) enum Outline {
    #[default]
    None,
    /// The items leading to copied pages.
    Pruned,
    /// Those items under one new item, titled from `/Info` or else this.
    Under(String),
}

/// Copies pages `sel` of `src` into `dst` without linking them into its tree; returns
/// the new pages in order.
pub(crate) fn add(
    dst: &mut Document,
    src: &Document,
    tree: &PageTree,
    sel: &[usize],
    carry: &Carry,
) -> Vec<ObjRef> {
    let (pages, map, stub) = import_pages(dst, src, tree, sel);
    let mut x = Xfer::copy(src, map, stub);
    structure::carry(dst, &mut x, carry.all_pages);
    threads::carry(dst, &mut x);
    optional_content(dst, &mut x);
    named_dests(dst, &mut x);
    if carry.attachments {
        for key in [&b"EmbeddedFiles"[..], b"JavaScript"] {
            let entries: Vec<_> = source_names(src, key)
                .into_iter()
                .map(|(k, v)| (k, x.value(dst, &v)))
                .collect();
            add_names(dst, key, entries, &HashSet::new());
        }
    }
    if carry.document {
        document(dst, &mut x);
    }
    let items = match &carry.outline {
        Outline::None => Vec::new(),
        Outline::Pruned => outline::collect(dst, &mut x),
        Outline::Under(fallback) => match pages.first() {
            Some(&first) => {
                let title = info_title(src).unwrap_or_else(|| fallback.as_bytes().to_vec());
                let mut item = Item::new(&title, first);
                item.kids = outline::collect(dst, &mut x);
                vec![item]
            }
            None => Vec::new(),
        },
    };
    outline::append(dst, items);
    finish(dst, &x, stub);
    pages
}

fn source_names(src: &Document, key: &[u8]) -> Vec<(Vec<u8>, Object)> {
    src.catalog()
        .and_then(|c| {
            let c = c.as_dict()?;
            src.get_in(c, b"Names").as_dict()?.get(key).cloned()
        })
        .map(|root| read_names(src, &root))
        .unwrap_or_default()
}

/// Named destinations leading to copied pages, explicit and renamed where taken.
fn named_dests(dst: &mut Document, x: &mut Xfer) {
    let src = x.src;
    let mut all: Vec<(Vec<u8>, Object)> = named(src).into_iter().collect();
    all.sort_by(|a, b| a.0.cmp(&b.0));
    let entries: Vec<_> = all
        .into_iter()
        .filter_map(|(k, v)| match target(src, &v) {
            Target::Page(p) => match x.page(p) {
                Place::Kept(p) => Some((k, explicit(src, &v, p)?)),
                _ => None,
            },
            _ => None,
        })
        .collect();
    if entries.is_empty() {
        return;
    }
    let taken: HashSet<Vec<u8>> = named(dst).into_keys().collect();
    add_names(dst, b"Dests", entries, &taken);
}

fn info_title(src: &Document) -> Option<Vec<u8>> {
    let info = src.get_in(src.trailer(), b"Info");
    let t = src.get_in(info.as_dict()?, b"Title");
    t.as_string().filter(|t| !t.is_empty()).map(<[u8]>::to_vec)
}

/// Document information and viewer settings, where the target has none.
fn document(dst: &mut Document, x: &mut Xfer) {
    let src = x.src;
    let info = src.get_in(src.trailer(), b"Info").as_dict().cloned();
    if let Some(info) = info
        && !dst.trailer().contains(b"Info")
    {
        let v = x.value(dst, &info.into());
        let r = dst.add(v);
        let mut t = dst.trailer().clone();
        t.set(b"Info", r);
        dst.set_trailer(t);
    }
    let Some(cat) = src.catalog().and_then(|c| c.as_dict().cloned()) else {
        return;
    };
    let mut add = Vec::new();
    for key in [
        &b"Lang"[..],
        b"ViewerPreferences",
        b"PageMode",
        b"PageLayout",
    ] {
        if let Some(v) = cat.get(key) {
            add.push((key, x.value(dst, v)));
        }
    }
    update_catalog(dst, |_, c| {
        let mut changed = false;
        for (k, v) in add {
            if !c.contains(k) {
                c.set(k, v);
                changed = true;
            }
        }
        changed
    });
}

/// Optional content groups of the source joined to the target's: the groups united, the
/// default configuration's ON, OFF, locked and radio button lists, order and usage
/// application merged, so every group keeps its initial state.
fn optional_content(dst: &mut Document, x: &mut Xfer) {
    let src = x.src;
    let Some(theirs) = src
        .catalog()
        .and_then(|c| src.get_in(c.as_dict()?, b"OCProperties").as_dict().cloned())
    else {
        return;
    };
    let theirs = x.value(dst, &theirs.into());
    let Some(theirs) = theirs.as_dict() else {
        return;
    };
    let held = dst
        .catalog()
        .and_then(|c| c.as_dict()?.get(b"OCProperties").cloned());
    let Some(held) = held else {
        let theirs = theirs.clone();
        update_catalog(dst, |_, c| {
            c.set(b"OCProperties", theirs);
            true
        });
        return;
    };
    let Some(mut ours) = dst.resolve(&held).as_dict().cloned() else {
        return;
    };
    let arr = |doc: &Document, d: &Dict, k: &[u8]| -> Vec<Object> {
        doc.get_in(d, k)
            .as_array()
            .map(<[Object]>::to_vec)
            .unwrap_or_default()
    };
    let union = |a: &mut Vec<Object>, b: &[Object]| {
        for o in b {
            if !a.contains(o) {
                a.push(o.clone());
            }
        }
    };
    let their_ocgs = arr(dst, theirs, b"OCGs");
    let mut ocgs = arr(dst, &ours, b"OCGs");
    let our_ocgs = ocgs.clone();
    union(&mut ocgs, &their_ocgs);
    ours.set(b"OCGs", ocgs);

    let their_d = dst
        .get_in(theirs, b"D")
        .as_dict()
        .cloned()
        .unwrap_or_default();
    let mut d = dst
        .get_in(&ours, b"D")
        .as_dict()
        .cloned()
        .unwrap_or_default();
    let base_off = |d: &Dict| d.name(b"BaseState") == Some(b"OFF");
    for key in [&b"ON"[..], b"OFF", b"Locked", b"RBGroups", b"AS"] {
        let add = arr(dst, &their_d, key);
        if add.is_empty() {
            continue;
        }
        let mut have = arr(dst, &d, key);
        union(&mut have, &add);
        d.set(key, have);
    }
    // Groups that took their state from the source's base state.
    let (flip, listed) = match (base_off(&their_d), base_off(&d)) {
        (true, false) => (&b"OFF"[..], arr(dst, &their_d, b"ON")),
        (false, true) => (&b"ON"[..], arr(dst, &their_d, b"OFF")),
        _ => (&b""[..], Vec::new()),
    };
    if !flip.is_empty() {
        let mut have = arr(dst, &d, flip);
        let implied: Vec<Object> = their_ocgs
            .iter()
            .filter(|o| !listed.contains(o))
            .cloned()
            .collect();
        union(&mut have, &implied);
        d.set(flip, have);
    }
    let their_order = arr(dst, &their_d, b"Order");
    if !their_order.is_empty() {
        let mut order = match d.get(b"Order") {
            Some(_) => arr(dst, &d, b"Order"),
            None => our_ocgs,
        };
        order.extend(their_order);
        d.set(b"Order", order);
    }
    ours.set(b"D", d);
    let configs = arr(dst, theirs, b"Configs");
    if !configs.is_empty() {
        let mut have = arr(dst, &ours, b"Configs");
        have.extend(configs);
        ours.set(b"Configs", have);
    }
    match held.as_objref() {
        Some(r) => dst.set(r, ours),
        None => update_catalog(dst, |_, c| {
            c.set(b"OCProperties", ours);
            true
        }),
    }
}

/// Makes named go-to actions in the copied objects explicit (names may have been renamed)
/// and removes every reference to what was left behind; then drops the stub.
fn finish(dst: &mut Document, x: &Xfer, stub: ObjRef) {
    let src = x.src;
    let mut names: Option<HashMap<Vec<u8>, Object>> = None;
    for num in dst.object_numbers() {
        if num <= stub.num {
            continue;
        }
        let Some(r) = dst.reference(num) else {
            continue;
        };
        let Some(mut o) = dst.get(r).map(|o| (*o).clone()) else {
            continue;
        };
        if fix(
            &mut o,
            &mut |d: &mut Dict| {
                if d.name(b"S") != Some(b"GoTo") {
                    return false;
                }
                let Some(Target::Named(n)) = d.get(b"D").map(|v| target(src, v)) else {
                    return false;
                };
                let names = names.get_or_insert_with(|| named(src));
                let Some(v) = names.get(&n) else { return false };
                let Target::Page(p) = target(src, v) else {
                    return false;
                };
                match x.page(p) {
                    Place::Kept(p) => match explicit(src, v, p) {
                        Some(e) => {
                            d.set(b"D", e);
                            true
                        }
                        None => false,
                    },
                    _ => false,
                }
            },
            stub,
        ) {
            dst.set(r, o);
        }
    }
    dst.delete(stub);
}

/// Applies `f` to every dictionary within `o` (direct values only) and removes references
/// to `stub`: dictionary entries are dropped, array elements become null.
fn fix(o: &mut Object, f: &mut impl FnMut(&mut Dict) -> bool, stub: ObjRef) -> bool {
    match o {
        Object::Array(a) => {
            let mut changed = false;
            for v in a.iter_mut() {
                if *v == Object::Ref(stub) {
                    *v = Object::Null;
                    changed = true;
                } else {
                    changed |= fix(v, f, stub);
                }
            }
            changed
        }
        Object::Dict(d) | Object::Stream(Stream { dict: d, .. }) => {
            let before = d.len();
            d.0.retain(|(_, v)| *v != Object::Ref(stub));
            let mut changed = d.len() != before;
            changed |= f(d);
            for (_, v) in d.0.iter_mut() {
                changed |= fix(v, f, stub);
            }
            changed
        }
        _ => false,
    }
}
