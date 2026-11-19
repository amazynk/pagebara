//! Writing changed content back: byte patches on the decoded streams, so untouched
//! operators stay as written; changed page streams and forms become new stream objects
//! (forms drawn elsewhere too are copied for this use), resources gain and lose entries.

use std::collections::{BTreeMap, HashSet};

use pagebara_content::{ContentParser, Op};
use pagebara_syntax::{Dict, Document, ObjRef, Object, Stream};

use crate::Error;
use crate::assemble::Parsed;
use crate::emit;
use crate::model::StreamId;
use crate::scan::StreamKind;

/// Replaces bytes `start..end` of a stream (an insertion when they are equal).
#[derive(Clone, Debug)]
pub(crate) struct Patch {
    pub start: u32,
    pub end: u32,
    pub bytes: Vec<u8>,
}

/// How objects inside forms drawn in several places are edited.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FormEdit {
    /// The page gets its own copy of the form (other uses are unchanged).
    #[default]
    ThisUse,
    /// The form itself changes, everywhere it is drawn.
    AllUses,
}

/// A resource category and name.
pub(crate) type ResName = (&'static [u8], Vec<u8>);

/// Changes to the streams of one parsed page.
#[derive(Debug, Default)]
pub(crate) struct Changes {
    pub patches: BTreeMap<StreamId, Vec<Patch>>,
    /// Resources of streams that gained entries.
    pub res: BTreeMap<StreamId, Dict>,
    /// Resource entries whose uses were removed: dropped when no longer used.
    pub unused: BTreeMap<StreamId, Vec<ResName>>,
    /// Writes every stream with patches (even none) anew.
    pub rewrite: bool,
    pub counter: u32,
}

impl Changes {
    pub fn patch(&mut self, stream: StreamId, start: u32, end: u32, bytes: Vec<u8>) {
        self.patches
            .entry(stream)
            .or_default()
            .push(Patch { start, end, bytes });
    }

    pub fn insert(&mut self, stream: StreamId, at: u32, bytes: Vec<u8>) {
        self.patch(stream, at, at, bytes);
    }

    /// Adds a resource to a stream's resources under a new name, returning the name.
    pub fn add_resource(
        &mut self,
        doc: &Document,
        p: &Parsed,
        stream: StreamId,
        cat: &[u8],
        value: Object,
    ) -> Vec<u8> {
        let res = self
            .res
            .entry(stream)
            .or_insert_with(|| p.scan.streams[stream.0 as usize].res.clone());
        let mut sub = doc.get_in(res, cat).as_dict().cloned().unwrap_or_default();
        // The same object under an existing name is used again.
        if let Some((n, _)) = sub
            .iter()
            .find(|(_, v)| **v == value && v.as_objref().is_some())
        {
            return n.as_bytes().to_vec();
        }
        let name = loop {
            self.counter += 1;
            let n = format!("Ed{}", self.counter);
            if !sub.contains(n.as_bytes()) {
                break n;
            }
        };
        sub.set(name.as_bytes(), value);
        res.set(cat, Object::Dict(sub));
        name.into_bytes()
    }

    pub fn unused(&mut self, stream: StreamId, cat: &'static [u8], name: &[u8]) {
        self.unused
            .entry(stream)
            .or_default()
            .push((cat, name.to_vec()));
    }

    pub fn is_empty(&self) -> bool {
        self.patches.values().all(Vec::is_empty) && self.res.is_empty()
    }
}

/// Bytes with patches applied. Patches must not overlap; insertions at the same offset
/// keep their order.
pub(crate) fn apply(data: &[u8], patches: &[Patch]) -> Result<Vec<u8>, Error> {
    let mut ps: Vec<&Patch> = patches.iter().collect();
    ps.sort_by_key(|p| (p.start, p.end != p.start, p.end));
    let mut out = Vec::with_capacity(data.len() + 256);
    let mut at = 0usize;
    for p in ps {
        let (s, e) = (p.start as usize, p.end as usize);
        if s < at || e > data.len() || s > e {
            return Err(Error::Conflict);
        }
        out.extend_from_slice(&data[at..s]);
        out.extend_from_slice(&p.bytes);
        at = e;
    }
    out.extend_from_slice(&data[at..]);
    Ok(out)
}

/// Writes the changes of a parsed page into the document.
pub(crate) fn commit(
    doc: &mut Document,
    p: &Parsed,
    mut ch: Changes,
    forms: FormEdit,
) -> Result<(), Error> {
    // Forms first, deepest last-read first: a copied form patches the stream drawing it.
    let mut ids: Vec<StreamId> = ch.patches.keys().chain(ch.res.keys()).copied().collect();
    ids.sort();
    ids.dedup();
    let mut done: HashSet<StreamId> = HashSet::new();
    while let Some(&sid) = ids.iter().rev().find(|s| !done.contains(s) && s.0 != 0) {
        done.insert(sid);
        write_form(doc, p, &mut ch, sid, forms)?;
        if let Some((parent, _, _)) = p.scan.streams[sid.0 as usize].drawn_by
            && !ids.contains(&parent)
        {
            ids.push(parent);
            ids.sort();
        }
    }
    if ids.contains(&StreamId(0)) {
        write_page(doc, p, &mut ch)?;
    }
    Ok(())
}

fn new_data(doc: &Document, p: &Parsed, ch: &mut Changes, sid: StreamId) -> Result<Vec<u8>, Error> {
    let rec = &p.scan.streams[sid.0 as usize];
    let patches = ch.patches.remove(&sid).unwrap_or_default();
    let data = apply(&rec.data, &patches)?;
    prune(doc, &data, ch, sid, &rec.res);
    Ok(data)
}

/// Drops resource entries noted as unused that the new content does not name.
fn prune(doc: &Document, data: &[u8], ch: &mut Changes, sid: StreamId, base: &Dict) {
    let Some(unused) = ch.unused.remove(&sid) else {
        return;
    };
    let used = names_used(data);
    for (cat, name) in unused {
        if used.contains(&(cat, name.clone())) {
            continue;
        }
        let res = ch.res.entry(sid).or_insert_with(|| base.clone());
        if let Some(sub) = doc.get_in(res, cat).as_dict()
            && sub.contains(&name)
        {
            let mut sub = sub.clone();
            sub.remove(&name);
            res.set(cat, Object::Dict(sub));
        }
    }
}

/// Resource names a content stream uses, by category.
pub(crate) fn names_used(data: &[u8]) -> HashSet<(&'static [u8], Vec<u8>)> {
    let mut out = HashSet::new();
    let mut p = ContentParser::new(data);
    while let Some(i) = p.next() {
        let cat: &'static [u8] = match i.op {
            Op::XObject => b"XObject",
            Op::Font => {
                if let Some(n) = i.args.first().and_then(|a| a.name()) {
                    out.insert((&b"Font"[..], n.to_vec()));
                }
                continue;
            }
            Op::ExtGState => b"ExtGState",
            Op::Shade => b"Shading",
            Op::FillSpace | Op::StrokeSpace => b"ColorSpace",
            Op::FillColorN | Op::StrokeColorN => b"Pattern",
            Op::BeginMarkedProps => b"Properties",
            _ => continue,
        };
        if let Some(n) = i.args.last().and_then(|a| a.name()) {
            out.insert((cat, n.to_vec()));
        }
    }
    out
}

fn flate_stream(mut d: Dict, data: &[u8]) -> Stream {
    for k in [&b"Filter"[..], b"DecodeParms", b"Length", b"DL"] {
        d.remove(k);
    }
    d.set(b"Filter", Object::name(b"FlateDecode"));
    Stream::new(d, pagebara_filters::encode_flate(data, 6))
}

fn write_form(
    doc: &mut Document,
    p: &Parsed,
    ch: &mut Changes,
    sid: StreamId,
    forms: FormEdit,
) -> Result<(), Error> {
    let rec = &p.scan.streams[sid.0 as usize];
    let StreamKind::Form { object } = rec.kind else {
        return Ok(());
    };
    let data = new_data(doc, p, ch, sid)?;
    let res = ch.res.remove(&sid);
    let orig = object
        .and_then(|r| doc.get(r))
        .and_then(|o| o.as_stream().map(|s| s.dict.clone()))
        .unwrap_or_default();
    let mut d = orig;
    if let Some(res) = res {
        d.set(b"Resources", Object::Dict(res));
    }
    let stream = flate_stream(d, &data);
    let (parent, (s, e), item) = rec
        .drawn_by
        .ok_or(Error::Unsupported("a form without a use"))?;
    let shared = object.is_none_or(|r| form_shared(doc, p, item, r));
    match object {
        Some(r) if !shared || forms == FormEdit::AllUses => doc.set(r, stream),
        _ => {
            let r = doc.add(stream);
            let name = ch.add_resource(doc, p, parent, b"XObject", Object::Ref(r));
            let mut b = b"\n".to_vec();
            emit::named_op(&mut b, &name, b"Do");
            // The use may itself be edited: its rewritten bytes then name the copy.
            let old = &p.scan.streams[parent.0 as usize].data[s as usize..e as usize];
            let edited = ch
                .patches
                .get_mut(&parent)
                .and_then(|ps| ps.iter_mut().find(|q| (q.start, q.end) == (s, e)));
            match edited {
                Some(q) => q.bytes = replace_all(&q.bytes, old, &b),
                None => ch.patch(parent, s, e, b),
            }
            if let Some(old) = object.map(|_| p.scan.items[item].data.clone())
                && let crate::scan::ItemData::Object(crate::model::ObjectData::Form(f)) = old
            {
                ch.unused(parent, b"XObject", f.name.as_bytes());
            }
        }
    }
    Ok(())
}

fn replace_all(hay: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(hay.len());
    let mut i = 0;
    while i < hay.len() {
        if !from.is_empty() && hay[i..].starts_with(from) {
            out.extend_from_slice(to);
            i += from.len();
        } else {
            out.push(hay[i]);
            i += 1;
        }
    }
    out
}

/// Whether a form is drawn anywhere but at this one use: twice on the page, or by
/// another page.
fn form_shared(doc: &Document, p: &Parsed, item: usize, r: ObjRef) -> bool {
    if let crate::scan::ItemData::Object(crate::model::ObjectData::Form(_)) =
        &p.scan.items[item].data
    {
        let uses = p
            .scan
            .items
            .iter()
            .filter(|it| {
                matches!(&it.data, crate::scan::ItemData::Object(crate::model::ObjectData::Form(f)) if f.object == Some(r))
            })
            .count();
        if uses > 1 {
            return true;
        }
    }
    let tree = pagebara_pages::PageTree::load(doc);
    let mut pages = 0;
    for pg in tree.pages() {
        let Some(o) = doc.get(pg.r) else { continue };
        let Some(d) = o.as_dict() else { continue };
        let res = inherited_resources(doc, d);
        let mut seen = HashSet::new();
        if reaches(doc, &res, r, &mut seen, 0) {
            pages += 1;
            if pages > 1 {
                return true;
            }
        }
    }
    false
}

fn reaches(
    doc: &Document,
    res: &Dict,
    target: ObjRef,
    seen: &mut HashSet<ObjRef>,
    depth: u32,
) -> bool {
    if depth > 16 {
        return false;
    }
    let xo = doc.get_in(res, b"XObject");
    let Some(xo) = xo.as_dict() else { return false };
    for (_, v) in xo.iter() {
        let Some(r) = v.as_objref() else { continue };
        if r == target {
            return true;
        }
        if !seen.insert(r) {
            continue;
        }
        if let Some(o) = doc.get(r)
            && let Some(s) = o.as_stream()
            && s.dict.name(b"Subtype") == Some(b"Form")
            && let Some(fr) = doc.get_in(&s.dict, b"Resources").as_dict()
            && reaches(doc, fr, target, seen, depth + 1)
        {
            return true;
        }
    }
    false
}

/// A page's resources, inherited from its ancestors when it has none, resolved.
pub(crate) fn inherited_resources(doc: &Document, page: &Dict) -> Dict {
    let mut d = page.clone();
    for _ in 0..64 {
        if let Some(r) = d.get(b"Resources") {
            return doc.resolve(r).as_dict().cloned().unwrap_or_default();
        }
        let Some(parent) = d.get(b"Parent").and_then(Object::as_objref) else {
            break;
        };
        match doc.get(parent).and_then(|o| o.as_dict().cloned()) {
            Some(pd) => d = pd,
            None => break,
        }
    }
    Dict::new()
}

fn write_page(doc: &mut Document, p: &Parsed, ch: &mut Changes) -> Result<(), Error> {
    let sid = StreamId(0);
    let rec = &p.scan.streams[0];
    let StreamKind::Page { parts } = &rec.kind else {
        return Ok(());
    };
    let listed = ch.patches.contains_key(&sid);
    let patches = ch.patches.remove(&sid).unwrap_or_default();
    let pd = doc
        .get(p.model.page_ref)
        .and_then(|o| o.as_dict().cloned())
        .ok_or(Error::NotAPage)?;
    let mut pd = pd;
    if !patches.is_empty() || (listed && ch.rewrite) {
        // Each patch within one stream: only those streams change.
        let part_of = |at: u32, end: u32| parts.iter().position(|&(_, s, e)| s <= at && end <= e);
        let split = !parts.is_empty()
            && parts.iter().all(|pt| pt.0.is_some())
            && patches.iter().all(|pa| part_of(pa.start, pa.end).is_some());
        let mut contents: Vec<Object> = Vec::new();
        let all = apply(&rec.data, &patches)?;
        prune(doc, &all, ch, sid, &rec.res);
        if split {
            for (k, &(r, s, e)) in parts.iter().enumerate() {
                let mine: Vec<Patch> = patches
                    .iter()
                    .filter(|pa| part_of(pa.start, pa.end) == Some(k))
                    .map(|pa| Patch {
                        start: pa.start - s,
                        end: pa.end - s,
                        bytes: pa.bytes.clone(),
                    })
                    .collect();
                if mine.is_empty() && !ch.rewrite {
                    contents.push(Object::Ref(r.unwrap()));
                } else {
                    let data = apply(&rec.data[s as usize..e as usize], &mine)?;
                    contents.push(Object::Ref(doc.add(flate_stream(Dict::new(), &data))));
                }
            }
        } else {
            contents.push(Object::Ref(doc.add(flate_stream(Dict::new(), &all))));
        }
        let c = if contents.len() == 1 {
            contents.pop().unwrap()
        } else {
            Object::Array(contents)
        };
        pd.set(b"Contents", c);
        pd.remove(b"Thumb");
    }
    if let Some(res) = ch.res.remove(&sid) {
        pd.set(b"Resources", Object::Dict(res));
    }
    doc.set(p.model.page_ref, pd);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patches_apply_in_order() {
        let data = b"0123456789";
        let ps = vec![
            Patch {
                start: 2,
                end: 4,
                bytes: b"ab".to_vec(),
            },
            Patch {
                start: 0,
                end: 0,
                bytes: b"<".to_vec(),
            },
            Patch {
                start: 10,
                end: 10,
                bytes: b">".to_vec(),
            },
            Patch {
                start: 6,
                end: 6,
                bytes: b"x".to_vec(),
            },
            Patch {
                start: 6,
                end: 6,
                bytes: b"y".to_vec(),
            },
        ];
        assert_eq!(apply(data, &ps).unwrap(), b"<01ab45xy6789>");
        let overlap = vec![
            Patch {
                start: 2,
                end: 5,
                bytes: vec![],
            },
            Patch {
                start: 4,
                end: 6,
                bytes: vec![],
            },
        ];
        assert!(apply(data, &overlap).is_err());
    }
}
