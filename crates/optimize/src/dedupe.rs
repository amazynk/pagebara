//! Merging identical objects. Objects are compared by content with references read through
//! the merges found so far, repeated until nothing more merges, so two identical fonts
//! whose descriptors and programs are separate copies end up as one.

use std::collections::HashMap;
use std::io::Write as _;
use std::sync::Arc;

use pagebara_syntax::{Dict, Document, ObjRef, Object};
use rayon::prelude::*;
use sha2::{Digest, Sha256};

/// Objects that stand for a place in a tree or on a page, where two equal copies still
/// mean two different things.
fn distinct(o: &Object) -> bool {
    let d = match o {
        Object::Dict(d) => d,
        Object::Stream(s) => &s.dict,
        _ => return false,
    };
    if d.contains(b"Parent") || d.contains(b"P") || d.contains(b"StructParent") {
        return true;
    }
    // Annotations without `/Type`, and form fields.
    if (d.contains(b"Subtype") && d.contains(b"Rect")) || d.contains(b"FT") || d.contains(b"T") {
        return true;
    }
    matches!(
        d.name(b"Type"),
        Some(
            b"Page"
                | b"Pages"
                | b"Catalog"
                | b"Annot"
                | b"StructElem"
                | b"StructTreeRoot"
                | b"Sig"
                | b"Outlines"
                | b"OCG"
                | b"MCR"
                | b"OBJR"
                | b"XRef"
                | b"ObjStm"
        )
    ) || d.contains(b"ByteRange")
}

fn ser(o: &Object, rep: &HashMap<u32, u32>, out: &mut Vec<u8>) {
    match o {
        Object::Ref(r) => {
            let n = rep.get(&r.num).copied().unwrap_or(r.num);
            let _ = write!(out, "{n} R");
        }
        Object::Array(a) => {
            out.push(b'[');
            for e in a {
                ser(e, rep, out);
                out.push(b' ');
            }
            out.push(b']');
        }
        Object::Dict(d) => ser_dict(d, rep, out),
        o => pagebara_syntax::write_object(out, o),
    }
}

fn ser_dict(d: &Dict, rep: &HashMap<u32, u32>, out: &mut Vec<u8>) {
    out.extend_from_slice(b"<<");
    for (k, v) in d.iter() {
        if k.as_bytes() == b"Length" {
            continue;
        }
        out.push(b'/');
        out.extend_from_slice(k.as_bytes());
        out.push(b' ');
        ser(v, rep, out);
    }
    out.extend_from_slice(b">>");
}

/// Merges identical objects; returns how many copies went.
pub(crate) fn merge(doc: &mut Document) -> usize {
    let objs: Vec<(u32, Arc<Object>)> = doc
        .object_numbers()
        .into_iter()
        .filter_map(|n| Some((n, doc.get(doc.reference(n)?)?)))
        .filter(|(_, o)| !distinct(o))
        .collect();
    let d: &Document = doc;
    // Stream data is hashed once.
    let data_hash: HashMap<u32, [u8; 32]> = objs
        .par_iter()
        .filter_map(|(n, o)| {
            let s = o.as_stream()?;
            Some((*n, Sha256::digest(d.raw(s)).into()))
        })
        .collect();
    let mut rep: HashMap<u32, u32> = HashMap::new();
    for _ in 0..12 {
        let keys: Vec<(u32, [u8; 32])> = objs
            .par_iter()
            .map(|(n, o)| {
                let mut buf = Vec::new();
                match &**o {
                    Object::Stream(s) => {
                        buf.extend_from_slice(b"S");
                        ser_dict(&s.dict, &rep, &mut buf);
                        buf.extend_from_slice(&data_hash[n]);
                    }
                    o => ser(o, &rep, &mut buf),
                }
                (*n, Sha256::digest(&buf).into())
            })
            .collect();
        let mut first: HashMap<[u8; 32], u32> = HashMap::with_capacity(keys.len());
        let mut next = HashMap::new();
        for (n, k) in keys {
            let r = *first.entry(k).or_insert(n);
            if r != n {
                next.insert(n, r);
            }
        }
        let changed = next.len() != rep.len();
        rep = next;
        if !changed {
            break;
        }
    }
    if rep.is_empty() {
        return 0;
    }
    let gen_of: HashMap<u32, ObjRef> = rep
        .values()
        .filter_map(|&r| Some((r, doc.reference(r)?)))
        .collect();
    let all: Vec<u32> = doc.object_numbers();
    for n in all {
        if rep.contains_key(&n) {
            continue;
        }
        let Some(r) = doc.reference(n) else { continue };
        let Some(o) = doc.get(r) else { continue };
        let mut o2 = (*o).clone();
        if remap(&mut o2, &rep, &gen_of) {
            doc.set(r, o2);
        }
    }
    let mut t = doc.trailer().clone();
    let mut changed = false;
    for (_, v) in t.0.iter_mut() {
        changed |= remap(v, &rep, &gen_of);
    }
    if changed {
        doc.set_trailer(t);
    }
    let n = rep.len();
    for &dup in rep.keys() {
        if let Some(r) = doc.reference(dup) {
            doc.delete(r);
        }
    }
    n
}

/// Points references to merged copies at the kept object; true if anything changed.
fn remap(o: &mut Object, rep: &HashMap<u32, u32>, refs: &HashMap<u32, ObjRef>) -> bool {
    match o {
        Object::Ref(r) => match rep.get(&r.num).and_then(|k| refs.get(k)) {
            Some(&k) => {
                *r = k;
                true
            }
            None => false,
        },
        Object::Array(a) => a.iter_mut().fold(false, |c, e| remap(e, rep, refs) | c),
        Object::Dict(d) => {
            d.0.iter_mut()
                .fold(false, |c, (_, v)| remap(v, rep, refs) | c)
        }
        Object::Stream(s) => s
            .dict
            .0
            .iter_mut()
            .fold(false, |c, (_, v)| remap(v, rep, refs) | c),
        _ => false,
    }
}
