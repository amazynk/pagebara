//! Dropping resource entries no content stream asks for. A category dictionary is only
//! pruned when every stream that may look into it was read.

use std::collections::{HashMap, HashSet};

use pagebara_syntax::{Dict, Document, Object};

use crate::scan::{CATEGORIES, CatKey, ResLoc, Usage};

/// The category dictionaries of a resource dictionary at `loc`.
fn cat_keys(res: &Dict, loc: ResLoc) -> Vec<(CatKey, &'static [u8])> {
    CATEGORIES
        .iter()
        .filter_map(|&cat| {
            let v = res.get(cat)?;
            Some(match v.as_objref() {
                Some(r) => (CatKey::Obj(r.num), cat),
                None => (CatKey::In(loc, cat), cat),
            })
        })
        .collect()
}

/// Removes unused resource entries; returns how many went.
pub(crate) fn prune(doc: &mut Document, u: &Usage) -> usize {
    if u.uncertain {
        return 0;
    }
    // Every holder of resources, and where its resources are.
    let mut blocked: HashSet<CatKey> = HashSet::new();
    let mut prunable: HashMap<CatKey, (ResLoc, &'static [u8])> = HashMap::new();
    for num in doc.object_numbers() {
        let Some(r) = doc.reference(num) else {
            continue;
        };
        let Some(o) = doc.get(r) else { continue };
        let d = match &*o {
            Object::Dict(d) => d,
            Object::Stream(s) => &s.dict,
            _ => continue,
        };
        for key in [&b"Resources"[..], b"DR"] {
            let Some(v) = d.get(key) else { continue };
            let loc = match v.as_objref() {
                Some(r) => ResLoc::Obj(r.num),
                None => ResLoc::In(num),
            };
            let res = doc.resolve(v);
            let Some(res) = res.as_dict() else { continue };
            let scanned = key == b"Resources" && u.holders.contains(&num);
            for (k, cat) in cat_keys(res, loc) {
                if scanned {
                    prunable.insert(k, (loc, cat));
                } else {
                    blocked.insert(k);
                }
            }
        }
    }
    let mut n = 0;
    for (key, (loc, cat)) in prunable {
        if blocked.contains(&key) {
            continue;
        }
        let used = u.names.get(&key);
        let target = match &key {
            CatKey::Obj(num) => doc.reference(*num),
            CatKey::In(..) => None,
        };
        match target {
            Some(r) => {
                let Some(o) = doc.get(r) else { continue };
                let Some(d) = o.as_dict() else { continue };
                let (d, k) = keep_used(d, used);
                if k > 0 {
                    n += k;
                    doc.set(r, d);
                }
            }
            None => {
                // Directly in the resource dictionary: edit that dictionary.
                let (holder, direct) = match loc {
                    ResLoc::Obj(num) => (num, true),
                    ResLoc::In(num) => (num, false),
                };
                let Some(r) = doc.reference(holder) else {
                    continue;
                };
                let Some(o) = doc.get(r) else { continue };
                let mut o = (*o).clone();
                let hd = match &mut o {
                    Object::Dict(d) => d,
                    Object::Stream(s) => &mut s.dict,
                    _ => continue,
                };
                let res = if direct {
                    Some(hd)
                } else {
                    match hd.get_mut(b"Resources") {
                        Some(Object::Dict(rd)) => Some(rd),
                        _ => None,
                    }
                };
                let Some(res) = res else { continue };
                let Some(Object::Dict(cd)) = res.get(cat) else {
                    continue;
                };
                let (cd, k) = keep_used(cd, used);
                if k > 0 {
                    n += k;
                    if cd.is_empty() {
                        res.remove(cat);
                    } else {
                        res.set(cat, cd);
                    }
                    doc.set(r, o);
                }
            }
        }
    }
    n
}

fn keep_used(d: &Dict, used: Option<&HashSet<Vec<u8>>>) -> (Dict, usize) {
    let kept = Dict(
        d.iter()
            .filter(|(k, _)| used.is_some_and(|u| u.contains(k.as_bytes())))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    );
    let removed = d.len() - kept.len();
    (kept, removed)
}
