//! Font subsetting in place: the outlines of glyphs no text shows are removed from
//! embedded TrueType programs. Glyph ids stay, so codes, `/CIDToGIDMap` and widths need
//! no change.

use std::collections::{BTreeSet, HashSet};

use pagebara_font::subset::blank::blank;
use pagebara_syntax::{Dict, Document, ObjRef, Object, Stream};
use rayon::prelude::*;

use crate::scan::Usage;

/// TrueType programs (`/FontFile2`) of font descriptors.
fn truetype_programs(doc: &Document) -> HashSet<u32> {
    let mut out = HashSet::new();
    for num in doc.object_numbers() {
        let Some(r) = doc.reference(num) else {
            continue;
        };
        let Some(o) = doc.get(r) else { continue };
        let Some(d) = o.as_dict() else { continue };
        if d.name(b"Type") == Some(b"FontDescriptor")
            && let Some(p) = d.get(b"FontFile2").and_then(Object::as_objref)
        {
            out.insert(p.num);
        }
    }
    out
}

/// Subsets the programs `u` saw used; returns fonts subset and glyphs removed.
pub(crate) fn subset(doc: &mut Document, u: &Usage) -> (usize, usize) {
    let tt = truetype_programs(doc);
    let jobs: Vec<(ObjRef, &BTreeSet<u16>)> = u
        .glyphs
        .iter()
        .filter(|(p, _)| tt.contains(p) && !u.keep_programs.contains(p))
        .filter_map(|(p, g)| Some((doc.reference(*p)?, g)))
        .collect();
    let d: &Document = doc;
    let done: Vec<(ObjRef, Stream, usize)> = jobs
        .par_iter()
        .filter_map(|(r, used)| {
            let o = d.get(*r)?;
            let s = o.as_stream()?;
            let data = d.decode(s).ok()?;
            if data.damaged {
                return None;
            }
            let n = match pagebara_font::Font::parse(data.data.clone())? {
                pagebara_font::Font::Sfnt(f) => f.num_glyphs(),
                _ => return None,
            };
            let drop: BTreeSet<u16> = (1..n).filter(|g| !used.contains(g)).collect();
            if drop.is_empty() {
                return None;
            }
            let b = blank(&data.data, &drop)?;
            if b.glyphs.is_empty() || b.data.len() >= data.data.len() {
                return None;
            }
            let mut dict = Dict::new();
            for (k, v) in s.dict.iter() {
                if !matches!(
                    k.as_bytes(),
                    b"Filter" | b"DecodeParms" | b"Length" | b"Length1" | b"DL"
                ) {
                    dict.set(k.as_bytes(), v.clone());
                }
            }
            dict.set(b"Length1", b.data.len() as i64);
            dict.set(b"Filter", Object::name(b"FlateDecode"));
            let stream = Stream::new(dict, pagebara_filters::encode_flate(&b.data, 10));
            Some((*r, stream, b.glyphs.len()))
        })
        .collect();
    let fonts = done.len();
    let mut glyphs = 0;
    for (r, s, g) in done {
        glyphs += g;
        doc.set(r, s);
    }
    (fonts, glyphs)
}
