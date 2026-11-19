//! The logical structure tree (ISO 32000-2 §14.7): pruned to the pages that remain, or
//! copied with pages into another document, together with its parent tree.

use std::collections::{BTreeMap, HashSet};

use pagebara_syntax::{Dict, Document, ObjRef, Object};

use crate::edit::{as_dict_mut, update_catalog};
use crate::names::{names_tree, nums_tree, read_names, read_nums, unique};
use crate::xfer::{Place, Xfer};

/// Deeper trees are given up on: the structure is dropped rather than half kept.
const MAX_DEPTH: u32 = 256;

#[derive(Debug)]
enum Kid {
    Mcid(i64),
    /// A marked-content reference, as written and resolved.
    Mcr(Object, Dict),
    /// An object reference, as written and resolved.
    Objr(Object, Dict),
    Elem(usize),
}

#[derive(Debug)]
struct Elem {
    src: Option<ObjRef>,
    dict: Dict,
    kids: Vec<Kid>,
    /// The `/K` was an array.
    k_array: bool,
    /// `/Pg` as it should read in the source: `None` drops it.
    pg: Option<ObjRef>,
}

struct Pruner<'x, 'a> {
    x: &'x Xfer<'a>,
    dst: &'x Document,
    keep_unknown: bool,
    elems: Vec<Elem>,
    seen: HashSet<u32>,
    visited: Vec<ObjRef>,
    too_deep: bool,
}

impl Pruner<'_, '_> {
    fn page_ok(&self, pg: Option<ObjRef>) -> bool {
        match pg.map(|r| self.x.page(r)) {
            Some(Place::Kept(_)) => true,
            Some(Place::Gone) => false,
            Some(Place::Unknown) | None => self.keep_unknown,
        }
    }

    fn present(&self, o: Option<&Object>) -> bool {
        o.and_then(Object::as_objref)
            .is_some_and(|r| self.x.object(self.dst, r).is_some())
    }

    /// Prunes element `o` (a reference or a dictionary); `None` drops it.
    fn visit(&mut self, o: &Object, parent_pg: Option<ObjRef>, depth: u32) -> Option<usize> {
        if depth > MAX_DEPTH {
            self.too_deep = true;
            return None;
        }
        let src = self.x.src;
        let r = o.as_objref();
        if let Some(r) = r {
            if !self.seen.insert(r.num) {
                return None;
            }
            self.visited.push(r);
        }
        let d = src.resolve(o).as_dict()?.clone();
        let own_pg = d.get(b"Pg").and_then(Object::as_objref);
        let pg = own_pg.or(parent_pg);
        let k = d.get(b"K").map(|k| (k.clone(), (*src.resolve(k)).clone()));
        let (items, k_array) = match k {
            None => (Vec::new(), false),
            Some((_, Object::Array(a))) => (a, true),
            Some((k, _)) => (vec![k], false),
        };
        let mut kids = Vec::with_capacity(items.len());
        for item in &items {
            let v = src.resolve(item);
            match &*v {
                Object::Int(n) => {
                    if self.page_ok(pg) {
                        kids.push(Kid::Mcid(*n));
                    }
                }
                Object::Dict(kd) if kd.name(b"Type") == Some(b"MCR") => {
                    let mpg = kd.get(b"Pg").and_then(Object::as_objref).or(pg);
                    let stm_ok = !kd.contains(b"Stm") || self.present(kd.get(b"Stm"));
                    if self.page_ok(mpg) && stm_ok {
                        kids.push(Kid::Mcr(item.clone(), kd.clone()));
                    }
                }
                Object::Dict(kd) if kd.name(b"Type") == Some(b"OBJR") => {
                    let opg = kd.get(b"Pg").and_then(Object::as_objref).or(pg);
                    let gone = opg.is_some_and(|p| self.x.page(p) == Place::Gone);
                    if !gone && self.present(kd.get(b"Obj")) {
                        kids.push(Kid::Objr(item.clone(), kd.clone()));
                    }
                }
                Object::Dict(_) => {
                    if let Some(i) = self.visit(item, pg, depth + 1) {
                        kids.push(Kid::Elem(i));
                    }
                }
                _ => {}
            }
        }
        if self.too_deep {
            return None;
        }
        if kids.is_empty() && !(items.is_empty() && self.page_ok(pg)) {
            return None;
        }
        let pg = match own_pg.map(|p| (p, self.x.page(p))) {
            Some((_, Place::Gone)) => None,
            Some((p, _)) => Some(p),
            // Content that relied on an ancestor's page gets it written out.
            None if !self.x.is_in_place() => parent_pg,
            None => None,
        };
        self.elems.push(Elem {
            src: r,
            dict: d,
            kids,
            k_array,
            pg,
        });
        Some(self.elems.len() - 1)
    }
}

/// Root keys handled here; others are copied when the target has none.
const ROOT_KEYS: [&[u8]; 9] = [
    b"Type",
    b"K",
    b"ParentTree",
    b"ParentTreeNextKey",
    b"IDTree",
    b"RoleMap",
    b"ClassMap",
    b"Namespaces",
    b"Parent",
];

/// Prunes the structure tree of `doc` (in place: `x.src` is its snapshot from before the
/// pages went) so it refers to no removed page or object. `gone_keys` are the parent tree
/// keys of removed objects.
pub(crate) fn prune(doc: &mut Document, x: &mut Xfer, gone_keys: &HashSet<i64>) {
    let src = x.src;
    let Some((root_ref, root)) = struct_root(src) else {
        return;
    };
    let Some(root_ref) = root_ref else {
        // A direct root has no identity to keep; start again from its elements.
        return drop_structure(doc);
    };
    let mut p = Pruner {
        x: &*x,
        dst: &*doc,
        keep_unknown: true,
        elems: Vec::new(),
        seen: HashSet::new(),
        visited: Vec::new(),
        too_deep: false,
    };
    let tops = top_level(src, &root)
        .iter()
        .filter_map(|o| p.visit(o, None, 0))
        .collect::<Vec<_>>();
    if p.too_deep {
        return drop_structure(doc);
    }
    let Pruner { elems, visited, .. } = p;
    if tops.is_empty() {
        return drop_structure(doc);
    }
    let kept: HashSet<u32> = elems.iter().filter_map(|e| Some(e.src?.num)).collect();
    let visited: HashSet<u32> = visited.iter().map(|r| r.num).collect();
    for &num in visited.difference(&kept) {
        if let Some(r) = src.reference(num) {
            doc.delete(r);
        }
    }
    let targets = write_elems(doc, x, &elems, root_ref);
    let elem_of = |o: &Object| -> Option<Object> {
        match o.as_objref() {
            Some(r) if visited.contains(&r.num) => kept.contains(&r.num).then(|| o.clone()),
            _ => Some(o.clone()),
        }
    };

    let mut new_root = root.clone();
    let k: Vec<Object> = tops.iter().map(|&i| targets[i].into()).collect();
    let old_k: Vec<Object> = top_level(src, &root);
    if k != old_k {
        new_root.set(b"K", k);
    }
    if let Some(pt) = root.get(b"ParentTree") {
        let entries = read_nums(src, pt);
        let mut out = BTreeMap::new();
        for (key, v) in &entries {
            if gone_keys.contains(key) {
                continue;
            }
            match &*src.resolve(v) {
                Object::Array(a) => {
                    let a: Vec<Object> = a
                        .iter()
                        .map(|o| elem_of(o).unwrap_or(Object::Null))
                        .collect();
                    if a.iter().all(Object::is_null) && !a.is_empty() {
                        continue;
                    }
                    match v.as_objref() {
                        Some(r) => {
                            if src.get(r).as_deref() != Some(&Object::Array(a.clone())) {
                                doc.set(r, a);
                            }
                            out.insert(*key, v.clone());
                        }
                        None => {
                            out.insert(*key, a.into());
                        }
                    }
                }
                _ => {
                    if let Some(v) = elem_of(v) {
                        out.insert(*key, v);
                    }
                }
            }
        }
        if out != entries {
            set_tree(doc, &mut new_root, b"ParentTree", nums_tree(&out));
        }
    }
    if let Some(ids) = root.get(b"IDTree") {
        let entries = read_names(src, ids);
        let before = entries.len();
        let left: Vec<_> = entries
            .into_iter()
            .filter(|(_, v)| elem_of(v).is_some())
            .collect();
        if left.len() != before {
            set_tree(doc, &mut new_root, b"IDTree", names_tree(left));
        }
    }
    if new_root != root {
        doc.set(root_ref, new_root);
    }
}

/// Copies the structure of the pages `x` carried into `dst`, adding it to `dst`'s tree.
/// `keep_unknown` keeps content whose page cannot be told (when every page was copied).
pub(crate) fn carry(dst: &mut Document, x: &mut Xfer, keep_unknown: bool) {
    let src = x.src;
    let Some((_, root)) = struct_root(src) else {
        return strip_keys(dst, x);
    };
    let mut p = Pruner {
        x: &*x,
        dst: &*dst,
        keep_unknown,
        elems: Vec::new(),
        seen: HashSet::new(),
        visited: Vec::new(),
        too_deep: false,
    };
    let tops = top_level(src, &root)
        .iter()
        .filter_map(|o| p.visit(o, None, 0))
        .collect::<Vec<_>>();
    if p.too_deep || tops.is_empty() {
        return strip_keys(dst, x);
    }
    let Pruner { elems, visited, .. } = p;

    // The target's root, created if needed, and its next parent tree key.
    let (dst_root_ref, mut dst_root) = match struct_root(dst) {
        Some((Some(r), d)) => (r, d),
        other => {
            let mut d = other.map(|o| o.1).unwrap_or_default();
            d.set(b"Type", Object::name(b"StructTreeRoot"));
            (dst.add(Object::Null), d)
        }
    };
    let mut entries = dst_root
        .get(b"ParentTree")
        .map(|pt| read_nums(dst, pt))
        .unwrap_or_default();
    let mut next = entries
        .last_key_value()
        .map_or(0, |(k, _)| k.saturating_add(1))
        .max(dst_root.int(b"ParentTreeNextKey").unwrap_or(0));

    // Elements first take their places, so values referring to them follow.
    let kept: HashSet<u32> = elems.iter().filter_map(|e| Some(e.src?.num)).collect();
    let mut ids = dst_root
        .get(b"IDTree")
        .map(|t| read_names(dst, t))
        .unwrap_or_default();
    let mut taken: HashSet<Vec<u8>> = ids.iter().map(|e| e.0.clone()).collect();
    let mut elems = elems;
    for e in &mut elems {
        if let Some(id) = src
            .resolve(e.dict.get(b"ID").unwrap_or(&Object::Null))
            .as_string()
        {
            let id = unique(id, |k| taken.contains(k));
            taken.insert(id.clone());
            e.dict.set(b"ID", Object::string(&id));
        }
    }
    for r in visited.iter().filter(|r| !kept.contains(&r.num)) {
        x.leave(*r);
    }
    let targets = write_elems(dst, x, &elems, dst_root_ref);
    for (e, t) in elems.iter().zip(&targets) {
        if let Some(id) = e.dict.get(b"ID").and_then(Object::as_string) {
            ids.push((id.to_vec(), (*t).into()));
        }
    }

    // Parent tree entries follow the copied objects, under new keys.
    let src_entries = root
        .get(b"ParentTree")
        .map(|pt| read_nums(src, pt))
        .unwrap_or_default();
    let mut copied = x.copied();
    copied.sort_by_key(|c| c.1.num);
    for (_, r) in copied {
        let Some(mut o) = dst.get(r).map(|o| (*o).clone()) else {
            continue;
        };
        let Some(d) = as_dict_mut(&mut o) else {
            continue;
        };
        let Some(key) = [&b"StructParents"[..], b"StructParent"]
            .into_iter()
            .find(|k| d.get(k).and_then(Object::as_int).is_some())
        else {
            continue;
        };
        let old = d.int(key).unwrap_or_default();
        let v = src_entries.get(&old).map(|v| match &*src.resolve(v) {
            Object::Array(a) => Object::Array(
                a.iter()
                    .map(|o| match o.as_objref() {
                        Some(e) => x.object(dst, e).map_or(Object::Null, Object::Ref),
                        None => Object::Null,
                    })
                    .collect(),
            ),
            o => o
                .as_objref()
                .or(v.as_objref())
                .and_then(|e| x.object(dst, e))
                .map_or(Object::Null, Object::Ref),
        });
        match v.filter(|v| !v.is_null()) {
            Some(v) => {
                d.set(key, next);
                entries.insert(next, v);
                next += 1;
            }
            None => {
                d.remove(key);
            }
        }
        dst.set(r, o);
    }

    let mut k = match dst_root.get(b"K").map(|k| (*dst.resolve(k)).clone()) {
        Some(Object::Array(a)) => a,
        Some(Object::Null) | None => Vec::new(),
        Some(_) => vec![dst_root.get(b"K").cloned().unwrap_or_default()],
    };
    k.extend(tops.iter().map(|&i| Object::Ref(targets[i])));
    dst_root.set(b"K", k);
    set_tree(dst, &mut dst_root, b"ParentTree", nums_tree(&entries));
    dst_root.set(b"ParentTreeNextKey", next);
    if !ids.is_empty() {
        set_tree(dst, &mut dst_root, b"IDTree", names_tree(ids));
    }
    for key in [&b"RoleMap"[..], b"ClassMap"] {
        let Some(m) = src
            .resolve(root.get(key).unwrap_or(&Object::Null))
            .as_dict()
            .cloned()
        else {
            continue;
        };
        let mut have = dst
            .resolve(dst_root.get(key).unwrap_or(&Object::Null))
            .as_dict()
            .cloned()
            .unwrap_or_default();
        for (name, v) in m.iter() {
            if !have.contains(name.as_bytes()) {
                let v = x.value(dst, v);
                have.set(name.as_bytes(), v);
            }
        }
        dst_root.set(key, have);
    }
    if let Some(ns) = src
        .resolve(root.get(b"Namespaces").unwrap_or(&Object::Null))
        .as_array()
    {
        let mut have = dst
            .resolve(dst_root.get(b"Namespaces").unwrap_or(&Object::Null))
            .as_array()
            .map(<[Object]>::to_vec)
            .unwrap_or_default();
        for n in ns {
            let n = x.value(dst, n);
            if !have.contains(&n) {
                have.push(n);
            }
        }
        dst_root.set(b"Namespaces", have);
    }
    for (key, v) in root.iter() {
        if !ROOT_KEYS.contains(&key.as_bytes()) && !dst_root.contains(key.as_bytes()) {
            let v = x.value(dst, v);
            dst_root.set(key.as_bytes(), v);
        }
    }
    dst.set(dst_root_ref, dst_root);
    let mark = src
        .catalog()
        .and_then(|c| c.as_dict()?.get(b"MarkInfo").cloned());
    let mark = mark.map(|m| x.value(dst, &m));
    update_catalog(dst, |_, cat| {
        cat.set(b"StructTreeRoot", dst_root_ref);
        if !cat.contains(b"MarkInfo") {
            let mut d = Dict::new();
            d.set(b"Marked", true);
            cat.set(b"MarkInfo", mark.unwrap_or(d.into()));
        }
        true
    });
}

/// Writes the kept elements to their targets, returning them by element index.
fn write_elems(dst: &mut Document, x: &mut Xfer, elems: &[Elem], root: ObjRef) -> Vec<ObjRef> {
    let targets: Vec<ObjRef> = elems.iter().map(|e| x.target(dst, e.src)).collect();
    let mut parent = vec![root; elems.len()];
    for (i, e) in elems.iter().enumerate() {
        for k in &e.kids {
            if let Kid::Elem(j) = k {
                parent[*j] = targets[i];
            }
        }
    }
    let in_place = x.is_in_place();
    let page = |x: &Xfer, r: Option<&Object>| match r.and_then(Object::as_objref).map(|r| x.page(r))
    {
        Some(Place::Kept(p)) => Some(Object::Ref(p)),
        _ => None,
    };
    for (i, e) in elems.iter().enumerate() {
        let kids: Vec<Object> = e
            .kids
            .iter()
            .map(|k| match k {
                Kid::Mcid(n) => Object::Int(*n),
                Kid::Elem(j) => targets[*j].into(),
                Kid::Mcr(o, _) | Kid::Objr(o, _) if in_place => o.clone(),
                Kid::Mcr(_, d) => {
                    let mut m = Dict::new();
                    m.set(b"Type", Object::name(b"MCR"));
                    if let Some(p) = page(x, d.get(b"Pg")) {
                        m.set(b"Pg", p);
                    }
                    for key in [&b"Stm"[..], b"StmOwn"] {
                        if let Some(s) = d.get(key).and_then(Object::as_objref)
                            && let Some(s) = x.object(dst, s)
                        {
                            m.set(key, s);
                        }
                    }
                    m.set(b"MCID", d.get(b"MCID").cloned().unwrap_or_default());
                    m.into()
                }
                Kid::Objr(_, d) => {
                    let mut m = Dict::new();
                    m.set(b"Type", Object::name(b"OBJR"));
                    if let Some(p) = page(x, d.get(b"Pg")) {
                        m.set(b"Pg", p);
                    }
                    if let Some(o) = d.get(b"Obj").and_then(Object::as_objref)
                        && let Some(o) = x.object(dst, o)
                    {
                        m.set(b"Obj", o);
                    }
                    m.into()
                }
            })
            .collect();
        let k: Option<Object> = match kids.len() {
            0 => None,
            1 if !e.k_array => kids.into_iter().next(),
            _ => Some(kids.into()),
        };
        let mut d = Dict::new();
        for (key, v) in e.dict.iter() {
            let v = match key.as_bytes() {
                b"P" => parent[i].into(),
                b"K" => match &k {
                    Some(k) => k.clone(),
                    None => continue,
                },
                b"Pg" => match e.pg {
                    Some(p) if in_place => p.into(),
                    Some(p) => match page(x, Some(&Object::Ref(p))) {
                        Some(p) => p,
                        None => continue,
                    },
                    None => continue,
                },
                b"Ref" => {
                    let refs = x.src.resolve(v);
                    let refs: Vec<Object> = refs
                        .as_array()
                        .unwrap_or(std::slice::from_ref(v))
                        .iter()
                        .filter_map(|o| Some(x.object(dst, o.as_objref()?)?.into()))
                        .collect();
                    if refs.is_empty() {
                        continue;
                    }
                    refs.into()
                }
                _ => x.value(dst, v),
            };
            d.set(key.as_bytes(), v);
        }
        if !d.contains(b"P") {
            d.set(b"P", parent[i]);
        }
        if !d.contains(b"Pg")
            && !in_place
            && let Some(p) = page(x, e.pg.map(Object::Ref).as_ref())
        {
            d.set(b"Pg", p);
        }
        if let Some(k) = k.filter(|_| !d.contains(b"K")) {
            d.set(b"K", k);
        }
        let unchanged = in_place
            && e.src
                .and_then(|r| x.src.get(r))
                .is_some_and(|o| o.as_dict() == Some(&d));
        if !unchanged {
            dst.set(targets[i], d);
        }
    }
    targets
}

/// The structure tree root: its reference, if indirect, and dictionary.
fn struct_root(doc: &Document) -> Option<(Option<ObjRef>, Dict)> {
    let cat = doc.catalog()?;
    let o = cat.as_dict()?.get(b"StructTreeRoot")?.clone();
    let d = doc.resolve(&o).as_dict()?.clone();
    Some((o.as_objref(), d))
}

fn top_level(doc: &Document, root: &Dict) -> Vec<Object> {
    match root.get(b"K") {
        None => Vec::new(),
        Some(k) => match &*doc.resolve(k) {
            Object::Array(a) => a.clone(),
            Object::Null => Vec::new(),
            _ => vec![k.clone()],
        },
    }
}

/// Stores a tree under `key` of `holder`, in the object already holding it if any.
fn set_tree(doc: &mut Document, holder: &mut Dict, key: &[u8], tree: Dict) {
    match holder.get(key).and_then(Object::as_objref) {
        Some(r) => doc.set(r, tree),
        None => {
            let r = doc.add(tree);
            holder.set(key, r);
        }
    }
}

/// Removes the structure tree from the catalog, for when it cannot be kept consistent.
fn drop_structure(doc: &mut Document) {
    update_catalog(doc, |_, cat| {
        let a = cat.remove(b"StructTreeRoot").is_some();
        let b = cat.remove(b"MarkInfo").is_some();
        a || b
    });
}

/// Without a structure to join, copied objects lose their parent tree keys, which would
/// otherwise point into the target's own tree.
fn strip_keys(dst: &mut Document, x: &Xfer) {
    let mut copied = x.copied();
    copied.sort_by_key(|c| c.1.num);
    for (_, r) in copied {
        let Some(mut o) = dst.get(r).map(|o| (*o).clone()) else {
            continue;
        };
        let Some(d) = as_dict_mut(&mut o) else {
            continue;
        };
        let a = d.remove(b"StructParents").is_some();
        let b = d.remove(b"StructParent").is_some();
        if a || b {
            dst.set(r, o);
        }
    }
}
