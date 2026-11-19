//! Dropping XObject resources no content stream draws.
//!
//! Every content stream reachable from the pages (contents, forms, annotation appearances,
//! Type 3 glyphs, tiling patterns, soft mask groups) is scanned for `Do` names against the
//! resource dictionary it actually uses (its own, or the one it inherits). An XObject entry
//! no stream names is removed, so a full save after compacting leaves the object out.
//! Dictionaries used by a stream that cannot be read are left alone.

use std::collections::{HashMap, HashSet};

use pagebara_content::{ContentParser, Op};
use pagebara_syntax::{Dict, Document, Object, Stream};

/// Where a resource dictionary lives: an object of its own, or inside the dictionary of
/// the object owning it (page, page tree node, form).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum ResKey {
    Obj(u32),
    Owner(u32),
}

/// Where an `/XObject` dictionary lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum XoKey {
    Obj(u32),
    In(ResKey),
}

#[derive(Default)]
struct Scan {
    used: HashSet<(XoKey, Vec<u8>)>,
    known: HashSet<XoKey>,
    unsafe_keys: HashSet<XoKey>,
    visited: HashSet<(u32, Option<ResKey>)>,
    res_seen: HashSet<ResKey>,
}

type Scope = Option<(ResKey, Dict)>;

fn res_scope(doc: &Document, value: &Object, owner: u32) -> Scope {
    let key = match value {
        Object::Ref(r) => ResKey::Obj(r.num),
        _ => ResKey::Owner(owner),
    };
    Some((key, doc.resolve(value).as_dict()?.clone()))
}

fn xo_key(scope: &Scope) -> Option<XoKey> {
    let (k, d) = scope.as_ref()?;
    match d.get(b"XObject")? {
        Object::Ref(r) => Some(XoKey::Obj(r.num)),
        Object::Dict(_) => Some(XoKey::In(*k)),
        _ => None,
    }
}

impl Scan {
    fn content(&mut self, doc: &Document, data: Option<Vec<u8>>, scope: &Scope) {
        let xk = xo_key(scope);
        if let Some(k) = xk {
            self.known.insert(k);
        }
        let Some(data) = data else {
            if let Some(k) = xk {
                self.unsafe_keys.insert(k);
            }
            return;
        };
        self.resources(doc, scope);
        let mut names: Vec<Vec<u8>> = Vec::new();
        let mut p = ContentParser::new(&data);
        while let Some(i) = p.next() {
            if i.op == Op::XObject
                && let Some(n) = i.args.last().and_then(|a| a.name())
                && !names.iter().any(|x| *x == *n)
            {
                names.push(n.into_owned());
            }
        }
        let Some((_, res)) = scope else { return };
        let xo = doc.get_in(res, b"XObject");
        let Some(xo) = xo.as_dict() else { return };
        for n in names {
            if let Some(k) = xk {
                self.used.insert((k, n.clone()));
            }
            let Some(entry) = xo.get(&n) else { continue };
            let num = entry.as_objref().map_or(0, |r| r.num);
            let o = doc.resolve(entry);
            if let Some(s) = o.as_stream()
                && s.dict.name(b"Subtype") != Some(b"Image")
            {
                self.form(doc, s, num, scope);
            }
        }
    }

    /// A form-like stream: own resources, or inherited ones.
    fn form(&mut self, doc: &Document, s: &Stream, num: u32, inherited: &Scope) {
        let scope = match s.dict.get(b"Resources") {
            Some(v) => res_scope(doc, v, num),
            None => inherited.clone(),
        };
        if num != 0 && !self.visited.insert((num, scope.as_ref().map(|s| s.0))) {
            return;
        }
        let data = doc.decode(s).ok().map(|d| d.data);
        self.content(doc, data, &scope);
    }

    /// Streams the resources themselves hold: Type 3 glyphs, patterns, soft mask groups.
    fn resources(&mut self, doc: &Document, scope: &Scope) {
        let Some((k, res)) = scope else { return };
        if !self.res_seen.insert(*k) {
            return;
        }
        let entries = |cat: &[u8]| -> Vec<Object> {
            doc.get_in(res, cat)
                .as_dict()
                .map(|d| d.iter().map(|(_, v)| v.clone()).collect())
                .unwrap_or_default()
        };
        for f in entries(b"Font") {
            let fo = doc.resolve(&f);
            let Some(fd) = fo.as_dict() else { continue };
            if fd.name(b"Subtype") != Some(b"Type3") {
                continue;
            }
            let num = f.as_objref().map_or(0, |r| r.num);
            let fscope = match fd.get(b"Resources") {
                Some(v) => res_scope(doc, v, num),
                None => scope.clone(),
            };
            let procs = doc.get_in(fd, b"CharProcs");
            for (_, p) in procs.as_dict().map(|d| d.0.clone()).unwrap_or_default() {
                if let Some(s) = doc.resolve(&p).as_stream() {
                    let data = doc.decode(s).ok().map(|d| d.data);
                    self.content(doc, data, &fscope);
                }
            }
        }
        for p in entries(b"Pattern") {
            let num = p.as_objref().map_or(0, |r| r.num);
            if let Some(s) = doc.resolve(&p).as_stream() {
                self.form(doc, s, num, scope);
            }
        }
        for g in entries(b"ExtGState") {
            let go = doc.resolve(&g);
            let Some(gd) = go.as_dict() else { continue };
            let sm = doc.get_in(gd, b"SMask");
            let Some(smd) = sm.as_dict() else { continue };
            if let Some(gref) = smd.get(b"G") {
                let num = gref.as_objref().map_or(0, |r| r.num);
                if let Some(s) = doc.resolve(gref).as_stream() {
                    self.form(doc, s, num, scope);
                }
            }
        }
    }
}

fn inherited(doc: &Document, page: &Dict, own: u32, key: &[u8]) -> Option<(Object, u32)> {
    let mut d = page.clone();
    let mut owner = own;
    for _ in 0..64 {
        if let Some(v) = d.get(key) {
            return Some((v.clone(), owner));
        }
        let parent = d.get(b"Parent")?.as_objref()?;
        owner = parent.num;
        d = doc.get(parent)?.as_dict()?.clone();
    }
    None
}

/// Removes XObject resources that nothing draws; with `only`, just entries naming those
/// objects. Returns how many entries went.
pub(crate) fn prune_xobjects(doc: &mut Document, only: Option<&HashSet<u32>>) -> usize {
    let mut scan = Scan::default();
    let tree = pagebara_pages::PageTree::load(doc);
    for page in tree.pages() {
        let Some(po) = doc.get(page.r) else { continue };
        let Some(pd) = po.as_dict() else { continue };
        let scope = inherited(doc, pd, page.r.num, b"Resources")
            .and_then(|(v, owner)| res_scope(doc, &v, owner));
        let data = Some(pagebara_content::page_contents(doc, pd));
        scan.content(doc, data, &scope);
        for a in crate::mark::annots(doc, pd) {
            let Some(ao) = doc.get(a) else { continue };
            let Some(ad) = ao.as_dict() else { continue };
            let ap = doc.get_in(ad, b"AP");
            let Some(ap) = ap.as_dict() else { continue };
            for (_, v) in ap.iter() {
                let vo = doc.resolve(v);
                let streams: Vec<(Object, u32)> = match &*vo {
                    Object::Stream(_) => vec![(v.clone(), v.as_objref().map_or(0, |r| r.num))],
                    Object::Dict(d) => d
                        .iter()
                        .map(|(_, s)| (s.clone(), s.as_objref().map_or(0, |r| r.num)))
                        .collect(),
                    _ => Vec::new(),
                };
                for (s, num) in streams {
                    if let Some(st) = doc.resolve(&s).as_stream() {
                        scan.form(doc, st, num, &None);
                    }
                }
            }
        }
    }
    let mut removed = 0;
    let mut edits: HashMap<XoKey, Dict> = HashMap::new();
    for &k in &scan.known {
        if scan.unsafe_keys.contains(&k) {
            continue;
        }
        let Some(xo) = xobject_dict(doc, k) else {
            continue;
        };
        let mut nd = xo.clone();
        nd.0.retain(|(name, v)| {
            let used = scan.used.contains(&(k, name.as_bytes().to_vec()));
            let target = only.is_none_or(|o| v.as_objref().is_some_and(|r| o.contains(&r.num)));
            let keep = used || !target;
            if !keep {
                removed += 1;
            }
            keep
        });
        if nd.len() != xo.len() {
            edits.insert(k, nd);
        }
    }
    for (k, nd) in edits {
        write_xobject_dict(doc, k, nd);
    }
    removed
}

fn owner_resources(doc: &Document, num: u32) -> Option<Dict> {
    let r = doc.reference(num)?;
    let o = doc.get(r)?;
    o.as_dict()?.get(b"Resources")?.as_dict().cloned()
}

fn xobject_dict(doc: &Document, k: XoKey) -> Option<Dict> {
    match k {
        XoKey::Obj(n) => doc.get(doc.reference(n)?)?.as_dict().cloned(),
        XoKey::In(ResKey::Obj(n)) => {
            let res = doc.get(doc.reference(n)?)?;
            res.as_dict()?.get(b"XObject")?.as_dict().cloned()
        }
        XoKey::In(ResKey::Owner(n)) => owner_resources(doc, n)?.get(b"XObject")?.as_dict().cloned(),
    }
}

fn write_xobject_dict(doc: &mut Document, k: XoKey, nd: Dict) {
    let set_in = |o: &Object, f: &dyn Fn(&mut Dict)| -> Option<Object> {
        match o {
            Object::Dict(d) => {
                let mut d = d.clone();
                f(&mut d);
                Some(Object::Dict(d))
            }
            Object::Stream(s) => {
                let mut s = s.clone();
                f(&mut s.dict);
                Some(Object::Stream(s))
            }
            _ => None,
        }
    };
    match k {
        XoKey::Obj(n) => {
            if let Some(r) = doc.reference(n) {
                doc.set(r, nd);
            }
        }
        XoKey::In(ResKey::Obj(n)) => {
            let Some(r) = doc.reference(n) else { return };
            let Some(o) = doc.get(r) else { return };
            if let Some(o) = set_in(&o, &|d| d.set(b"XObject", nd.clone())) {
                doc.set(r, o);
            }
        }
        XoKey::In(ResKey::Owner(n)) => {
            let Some(r) = doc.reference(n) else { return };
            let Some(o) = doc.get(r) else { return };
            let edit = |d: &mut Dict| {
                if let Some(Object::Dict(res)) = d.get_mut(b"Resources") {
                    res.set(b"XObject", nd.clone());
                }
            };
            if let Some(o) = set_in(&o, &edit) {
                doc.set(r, o);
            }
        }
    }
}
