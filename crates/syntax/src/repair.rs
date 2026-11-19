//! Rebuilds the cross-reference by scanning for `num generation obj` when the file's own is broken.

use crate::lexer::{is_regular, is_white};
use crate::object::{Dict, Object};
use crate::objstm::ObjStm;
use crate::parser::Parser;
use crate::stream::decode_direct;
use crate::xref::{Entry, Kind, NumMap, Xref, merge_trailer};

pub fn repair(src: &[u8]) -> Xref {
    let mut entries = NumMap::default();
    for at in memchr::memmem::find_iter(src, b"obj") {
        if src.get(at + 3).is_some_and(|&b| is_regular(b)) {
            continue;
        }
        if let Some((num, generation, start)) = header_before(src, at) {
            // Later definitions are newer revisions.
            entries.insert(
                num,
                Entry::Offset {
                    offset: start,
                    generation,
                },
            );
        }
    }

    let mut trailers: Vec<(usize, Dict)> = memchr::memmem::find_iter(src, b"trailer")
        .filter_map(|at| match Parser::new(src, at + 7).object() {
            Ok(Object::Dict(d)) => Some((at, d)),
            _ => None,
        })
        .collect();

    let mut catalog = None;
    let mut objstms = Vec::new();
    let mut offsets: Vec<(u32, usize)> = entries
        .iter()
        .filter_map(|(&n, e)| match *e {
            Entry::Offset { offset, .. } => Some((n, offset)),
            _ => None,
        })
        .collect();
    offsets.sort_unstable_by_key(|e| e.1);
    for (num, off) in offsets {
        let Ok((id, obj)) = Parser::new(src, off).indirect(&mut |_| None) else {
            continue;
        };
        let Some(dict) = obj.as_dict() else { continue };
        match dict.name(b"Type") {
            Some(b"Catalog") => catalog = Some(id),
            Some(b"XRef") => trailers.push((off, dict.clone())),
            Some(b"ObjStm") => {
                if let Object::Stream(s) = &obj
                    && let Ok(data) = decode_direct(src, s)
                    && let Some(stm) = ObjStm::new(data, &s.dict)
                {
                    objstms.push((num, stm));
                }
            }
            _ => {}
        }
    }
    // Objects found directly win over copies in object streams; among those, later wins.
    for (stream, stm) in &objstms {
        for (num, index) in stm.numbers() {
            if !matches!(entries.get(&num), Some(Entry::Offset { .. })) {
                entries.insert(
                    num,
                    Entry::InStream {
                        stream: *stream,
                        index,
                    },
                );
            }
        }
    }

    trailers.sort_by_key(|t| t.0);
    let mut trailer = Dict::new();
    for (_, t) in trailers.into_iter().rev() {
        merge_trailer(&mut trailer, t);
    }
    let root_ok = |t: &Dict| {
        t.get(b"Root")
            .and_then(Object::as_objref)
            .is_some_and(|r| entries.contains_key(&r.num))
    };
    if !root_ok(&trailer)
        && let Some(c) = catalog
    {
        trailer.set(b"Root", c);
    }
    for k in [
        &b"Prev"[..],
        b"XRefStm",
        b"W",
        b"Index",
        b"Length",
        b"Filter",
        b"DecodeParms",
        b"Type",
    ] {
        trailer.remove(k);
    }
    let size = entries.keys().max().map_or(0, |&m| i64::from(m) + 1);
    trailer.set(b"Size", size);
    Xref {
        entries,
        trailer,
        kind: Kind::Repaired,
        startxref: 0,
    }
}

/// For `obj` at `at`: the `num generation` before it and where `num` starts.
fn header_before(src: &[u8], at: usize) -> Option<(u32, u16, usize)> {
    let mut i = at;
    let white = |i: &mut usize| {
        let s = *i;
        while *i > 0 && is_white(src[*i - 1]) {
            *i -= 1;
        }
        *i < s
    };
    let digits = |i: &mut usize| {
        let e = *i;
        while *i > 0 && src[*i - 1].is_ascii_digit() {
            *i -= 1;
        }
        (*i < e && e - *i <= 10).then(|| parse(&src[*i..e]))
    };
    if !white(&mut i) {
        return None;
    }
    let generation = digits(&mut i)??;
    if !white(&mut i) {
        return None;
    }
    let num = digits(&mut i)??;
    if i > 0 && is_regular(src[i - 1]) {
        return None;
    }
    Some((u32::try_from(num).ok()?, u16::try_from(generation).ok()?, i))
}

fn parse(d: &[u8]) -> Option<u64> {
    d.iter().try_fold(0u64, |v, &c| {
        v.checked_mul(10)?.checked_add(u64::from(c - b'0'))
    })
}
