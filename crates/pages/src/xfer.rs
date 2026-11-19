//! Carrying document-level structures (structure tree, threads, outlines) from a source
//! document into a target: another document that received copies of some pages, or the
//! same document after pages were removed from it.

use std::collections::HashSet;

use pagebara_syntax::xref::NumMap;
use pagebara_syntax::{Dict, Document, ObjRef, Object};

/// What became of a page of the source.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Place {
    Kept(ObjRef),
    Gone,
    /// Not a page of the source's tree.
    Unknown,
}

#[derive(Debug)]
pub(crate) struct Xfer<'a> {
    pub src: &'a Document,
    mode: Mode,
}

#[derive(Debug)]
enum Mode {
    /// The source is a snapshot of the target taken before pages were removed.
    InPlace {
        kept: HashSet<u32>,
        gone: HashSet<u32>,
    },
    /// The target received copies; `map` is the import map, where `stub` marks what was
    /// left behind.
    Copy { map: NumMap<ObjRef>, stub: ObjRef },
}

impl<'a> Xfer<'a> {
    pub fn in_place(src: &'a Document, kept: HashSet<u32>, gone: HashSet<u32>) -> Self {
        Xfer {
            src,
            mode: Mode::InPlace { kept, gone },
        }
    }

    pub fn copy(src: &'a Document, map: NumMap<ObjRef>, stub: ObjRef) -> Self {
        Xfer {
            src,
            mode: Mode::Copy { map, stub },
        }
    }

    pub fn is_in_place(&self) -> bool {
        matches!(self.mode, Mode::InPlace { .. })
    }

    pub fn page(&self, r: ObjRef) -> Place {
        match &self.mode {
            Mode::InPlace { kept, gone } => {
                if kept.contains(&r.num) {
                    Place::Kept(r)
                } else if gone.contains(&r.num) {
                    Place::Gone
                } else {
                    Place::Unknown
                }
            }
            Mode::Copy { map, stub } => match map.get(&r.num) {
                Some(s) if s == stub => Place::Gone,
                Some(&n) => Place::Kept(n),
                None => Place::Unknown,
            },
        }
    }

    /// Where object `r` of the source is in the target, if it is there.
    pub fn object(&self, dst: &Document, r: ObjRef) -> Option<ObjRef> {
        match &self.mode {
            Mode::InPlace { gone, .. } => {
                (!gone.contains(&r.num) && dst.get(r).is_some()).then_some(r)
            }
            Mode::Copy { map, stub } => map.get(&r.num).filter(|n| *n != stub).copied(),
        }
    }

    /// A value of the source as it reads in the target.
    pub fn value(&mut self, dst: &mut Document, o: &Object) -> Object {
        match &mut self.mode {
            Mode::InPlace { .. } => o.clone(),
            Mode::Copy { map, .. } => copy_value(dst, self.src, o.clone(), map),
        }
    }

    /// The target object for source object `r` (a new one for direct objects): the same
    /// object in place, a fresh one for copies.
    pub fn target(&mut self, dst: &mut Document, r: Option<ObjRef>) -> ObjRef {
        match (&mut self.mode, r) {
            (Mode::InPlace { .. }, Some(r)) => r,
            (Mode::Copy { map, stub }, Some(r)) => match map.get(&r.num) {
                Some(n) if n != stub => *n,
                _ => {
                    let n = dst.add(Object::Null);
                    map.insert(r.num, n);
                    n
                }
            },
            (_, None) => dst.add(Object::Null),
        }
    }

    /// Marks source object `r` as left behind, so copied values do not pull it in.
    pub fn leave(&mut self, r: ObjRef) {
        if let Mode::Copy { map, stub } = &mut self.mode {
            map.entry(r.num).or_insert(*stub);
        }
    }

    /// Source and target of every copied object.
    pub fn copied(&self) -> Vec<(u32, ObjRef)> {
        match &self.mode {
            Mode::InPlace { .. } => Vec::new(),
            Mode::Copy { map, stub } => map
                .iter()
                .filter(|(_, r)| *r != stub)
                .map(|(&n, &r)| (n, r))
                .collect(),
        }
    }
}

/// Copies a direct value, importing the objects it refers to.
pub(crate) fn copy_value(
    dst: &mut Document,
    src: &Document,
    o: Object,
    map: &mut NumMap<ObjRef>,
) -> Object {
    match o {
        Object::Ref(r) => dst.import(src, r, map).into(),
        Object::Array(a) => a
            .into_iter()
            .map(|o| copy_value(dst, src, o, map))
            .collect::<Vec<_>>()
            .into(),
        Object::Dict(d) => Dict(
            d.0.into_iter()
                .map(|(k, v)| (k, copy_value(dst, src, v, map)))
                .collect(),
        )
        .into(),
        o => o,
    }
}
