//! Writing an unprotected document from scratch with objects renumbered densely: plain
//! (optionally with object streams and a cross-reference stream) or linearized for fast
//! web view (ISO 32000-2 Annex F: first page first, page offset and shared object hint
//! tables).

use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::sync::Arc;

use pagebara_syntax::{Dict, Document, ObjRef, Object, PdfString, Stream, write_object};
use rayon::prelude::*;

/// How [`write`] lays out the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct WriteOptions {
    /// Objects other than streams in object streams, with a cross-reference stream.
    pub object_streams: bool,
    /// Flate level for object and cross-reference streams (and unfiltered streams).
    pub level: u8,
    /// First page first, with hint tables (no object streams then).
    pub linearize: bool,
    /// The lowest header version to write.
    pub min_version: (u8, u8),
}

const OBJSTM_MAX: usize = 200;

/// `o` with references renumbered through `map`; references to objects that are gone
/// become null.
fn renumber(o: &Object, map: &HashMap<u32, u32>) -> Object {
    match o {
        Object::Ref(r) => match map.get(&r.num) {
            Some(&n) => Object::Ref(ObjRef::new(n, 0)),
            None => Object::Null,
        },
        Object::Array(a) => Object::Array(a.iter().map(|e| renumber(e, map)).collect()),
        Object::Dict(d) => Object::Dict(renumber_dict(d, map)),
        Object::Stream(s) => Object::Stream(Stream {
            dict: renumber_dict(&s.dict, map),
            data: s.data.clone(),
        }),
        o => o.clone(),
    }
}

fn renumber_dict(d: &Dict, map: &HashMap<u32, u32>) -> Dict {
    Dict(
        d.iter()
            .map(|(k, v)| (k.clone(), renumber(v, map)))
            .collect(),
    )
}

/// `num 0 obj … endobj`.
fn indirect(doc: &Document, num: u32, o: &Object, level: u8) -> Vec<u8> {
    let mut out = Vec::new();
    let _ = writeln!(out, "{num} 0 obj");
    match o {
        Object::Stream(s) => {
            let raw = doc.raw(s);
            let unfiltered = matches!(s.dict.get(b"Filter"), None | Some(Object::Null))
                && s.dict.name(b"Type") != Some(b"Metadata")
                && raw.len() >= 64;
            let packed = unfiltered.then(|| pagebara_filters::encode_flate(raw, level));
            let mut d = s.dict.clone();
            let data: &[u8] = match &packed {
                Some(p) if p.len() < raw.len() => {
                    d.remove(b"DecodeParms");
                    d.set(b"Filter", Object::name(b"FlateDecode"));
                    p
                }
                _ => raw,
            };
            d.set(b"Length", data.len() as i64);
            write_object(&mut out, &Object::Dict(d));
            out.extend_from_slice(b"\nstream\n");
            out.extend_from_slice(data);
            out.extend_from_slice(b"\nendstream");
        }
        o => write_object(&mut out, o),
    }
    out.extend_from_slice(b"\nendobj\n");
    out
}

fn header(out: &mut Vec<u8>, v: (u8, u8)) {
    let _ = writeln!(out, "%PDF-{}.{}", v.0, v.1);
    out.extend_from_slice(b"%\xe2\xe3\xcf\xd3\n");
}

/// The trailer for the new numbering, with a file identifier.
fn trailer(doc: &Document, map: &HashMap<u32, u32>) -> Dict {
    let mut t = Dict::new();
    for k in [&b"Root"[..], b"Info"] {
        if let Some(v) = doc.trailer().get(k) {
            let v = renumber(v, map);
            if !v.is_null() {
                t.set(k, v);
            }
        }
    }
    let id = doc
        .trailer()
        .get(b"ID")
        .and_then(Object::as_array)
        .filter(|a| a.len() == 2 && a.iter().all(|s| s.as_string().is_some()))
        .map(<[Object]>::to_vec);
    let id = id.unwrap_or_else(|| {
        use sha2::Digest;
        let h = sha2::Sha256::digest(doc.source());
        let s = Object::String(PdfString {
            bytes: h[..16].to_vec(),
            hex: true,
        });
        vec![s.clone(), s]
    });
    t.set(b"ID", id);
    t
}

/// Live objects in ascending order, without the old file structure.
fn live(doc: &Document) -> Vec<(u32, Arc<Object>)> {
    let old = doc.old_structure();
    doc.object_numbers()
        .into_iter()
        .filter(|&n| n > 0 && !old.contains(&n))
        .filter_map(|n| Some((n, doc.get(doc.reference(n)?)?)))
        .collect()
}

fn version(doc: &Document, min: (u8, u8)) -> (u8, u8) {
    doc.version().max(min)
}

/// Writes `doc`, which must not be protected.
pub fn write(doc: &Document, opts: &WriteOptions) -> Vec<u8> {
    if opts.linearize {
        return linearized(doc, opts);
    }
    let objs = live(doc);
    let map: HashMap<u32, u32> = objs
        .iter()
        .enumerate()
        .map(|(i, (n, _))| (*n, i as u32 + 1))
        .collect();
    let mut v = version(doc, opts.min_version);
    if opts.object_streams {
        v = v.max((1, 5));
    }
    let mut out = Vec::with_capacity(doc.source().len());
    header(&mut out, v);
    let packable = |o: &Object| match o {
        Object::Stream(_) => false,
        Object::Dict(d) => !d.contains(b"ByteRange"),
        _ => true,
    };
    let renumbered: Vec<(u32, Object)> = objs
        .par_iter()
        .map(|(n, o)| (map[n], renumber(o, &map)))
        .collect();
    let (packed, direct): (Vec<_>, Vec<_>) = renumbered
        .into_iter()
        .partition(|(_, o)| opts.object_streams && packable(o));
    let bodies: Vec<(u32, Vec<u8>)> = direct
        .par_iter()
        .map(|(n, o)| (*n, indirect(doc, *n, o, opts.level)))
        .collect();
    let mut rows: Vec<(u32, Row)> = Vec::with_capacity(objs.len() + 1);
    for (n, b) in bodies {
        rows.push((n, Row::At(out.len())));
        out.extend_from_slice(&b);
    }
    let mut next = objs.len() as u32 + 1;
    let groups: Vec<&[(u32, Object)]> = packed.chunks(OBJSTM_MAX).collect();
    let streams: Vec<(usize, Vec<u8>)> = groups
        .par_iter()
        .map(|g| object_stream(g, opts.level))
        .collect();
    for (g, data) in groups.iter().zip(streams) {
        let snum = next;
        next += 1;
        for (i, (n, _)) in g.iter().enumerate() {
            rows.push((*n, Row::In(snum, i as u32)));
        }
        rows.push((snum, Row::At(out.len())));
        let first = data.0;
        let mut d = Dict::new();
        d.set(b"Type", Object::name(b"ObjStm"));
        d.set(b"N", g.len() as i64);
        d.set(b"First", first as i64);
        d.set(b"Filter", Object::name(b"FlateDecode"));
        d.set(b"Length", data.1.len() as i64);
        let _ = writeln!(out, "{snum} 0 obj");
        write_object(&mut out, &Object::Dict(d));
        out.extend_from_slice(b"\nstream\n");
        out.extend_from_slice(&data.1);
        out.extend_from_slice(b"\nendstream\nendobj\n");
    }
    rows.sort_unstable_by_key(|r| r.0);
    let mut t = trailer(doc, &map);
    let xref_at = out.len();
    if opts.object_streams {
        rows.push((next, Row::At(xref_at)));
        t.set(b"Size", i64::from(next) + 1);
        xref_stream(&mut out, next, &rows, t, opts.level);
    } else {
        t.set(b"Size", i64::from(next));
        xref_table(&mut out, 0, &rows, true);
        out.extend_from_slice(b"trailer\n");
        write_object(&mut out, &Object::Dict(t));
        out.push(b'\n');
    }
    let _ = writeln!(out, "startxref\n{xref_at}\n%%EOF");
    out
}

#[derive(Clone, Copy)]
enum Row {
    At(usize),
    In(u32, u32),
}

/// Offset of the first object and the compressed data of an object stream.
#[allow(clippy::type_complexity)]
fn object_stream(group: &[(u32, Object)], level: u8) -> (usize, Vec<u8>) {
    let mut head = Vec::new();
    let mut body = Vec::new();
    for (num, o) in group {
        let _ = write!(head, "{num} {} ", body.len());
        write_object(&mut body, o);
        body.push(b'\n');
    }
    let first = head.len();
    head.extend_from_slice(&body);
    (first, pagebara_filters::encode_flate(&head, level))
}

/// A table section starting at object `start` for consecutive sorted rows; object 0 is
/// written as the head of the free list when `with_zero`.
fn xref_table(out: &mut Vec<u8>, start: u32, rows: &[(u32, Row)], with_zero: bool) {
    out.extend_from_slice(b"xref\n");
    let first = if with_zero { 0 } else { start };
    let count = rows.len() + usize::from(with_zero);
    let _ = writeln!(out, "{first} {count}");
    if with_zero {
        out.extend_from_slice(b"0000000000 65535 f\r\n");
    }
    for (_, r) in rows {
        if let Row::At(off) = r {
            let _ = write!(out, "{off:010} 00000 n\r\n");
        }
    }
}

fn xref_stream(out: &mut Vec<u8>, num: u32, rows: &[(u32, Row)], mut dict: Dict, level: u8) {
    let max = rows
        .iter()
        .map(|r| match r.1 {
            Row::At(o) => o,
            Row::In(s, _) => s as usize,
        })
        .max()
        .unwrap_or(0);
    let w1 = (usize::BITS - max.leading_zeros()).div_ceil(8).max(1) as usize;
    let width = 1 + w1 + 2;
    let mut data = Vec::with_capacity((rows.len() + 1) * (width + 1));
    let mut prev = vec![0u8; width];
    let mut row = vec![0u8; width];
    let mut put = |t: u8, f2: usize, f3: u16, data: &mut Vec<u8>| {
        row[0] = t;
        row[1..1 + w1].copy_from_slice(&f2.to_be_bytes()[size_of::<usize>() - w1..]);
        row[1 + w1..].copy_from_slice(&f3.to_be_bytes());
        data.push(2);
        data.extend(row.iter().zip(&prev).map(|(a, b)| a.wrapping_sub(*b)));
        std::mem::swap(&mut row, &mut prev);
    };
    put(0, 0, 65535, &mut data);
    for (_, r) in rows {
        match *r {
            Row::At(o) => put(1, o, 0, &mut data),
            Row::In(s, i) => put(2, s as usize, i as u16, &mut data),
        }
    }
    dict.set(b"Type", Object::name(b"XRef"));
    dict.set(
        b"W",
        vec![Object::Int(1), Object::Int(w1 as i64), Object::Int(2)],
    );
    let mut parms = Dict::new();
    parms.set(b"Predictor", 12);
    parms.set(b"Columns", width as i64);
    dict.set(b"Filter", Object::name(b"FlateDecode"));
    dict.set(b"DecodeParms", parms);
    let data = pagebara_filters::encode_flate(&data, level);
    dict.set(b"Length", data.len() as i64);
    let _ = writeln!(out, "{num} 0 obj");
    write_object(out, &Object::Dict(dict));
    out.extend_from_slice(b"\nstream\n");
    out.extend_from_slice(&data);
    out.extend_from_slice(b"\nendstream\nendobj\n");
}

// Linearization.

/// Objects reachable from `start` without passing through `stop` objects or `/Parent`.
fn reach(
    doc: &Document,
    start: u32,
    stop: &HashSet<u32>,
    out: &mut Vec<u32>,
    seen: &mut HashSet<u32>,
) {
    let mut stack = vec![start];
    while let Some(n) = stack.pop() {
        if !seen.insert(n) {
            continue;
        }
        out.push(n);
        let Some(o) = doc.reference(n).and_then(|r| doc.get(r)) else {
            continue;
        };
        let mut refs = Vec::new();
        collect_refs(&o, &mut refs);
        for r in refs.into_iter().rev() {
            if !stop.contains(&r) && !seen.contains(&r) {
                stack.push(r);
            }
        }
    }
}

fn collect_refs(o: &Object, out: &mut Vec<u32>) {
    match o {
        Object::Ref(r) => out.push(r.num),
        Object::Array(a) => a.iter().for_each(|e| collect_refs(e, out)),
        Object::Dict(d) => d
            .iter()
            .filter(|(k, _)| k.as_bytes() != b"Parent")
            .for_each(|(_, v)| collect_refs(v, out)),
        Object::Stream(s) => collect_refs(&Object::Dict(s.dict.clone()), out),
        _ => {}
    }
}

/// Bits needed for values up to `max`.
fn bits(max: u64) -> u32 {
    64 - max.leading_zeros()
}

#[derive(Default)]
struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl BitWriter {
    fn put(&mut self, v: u64, bits: u32) {
        for i in (0..bits).rev() {
            self.acc = self.acc << 1 | (v >> i & 1);
            self.n += 1;
            if self.n == 8 {
                self.out.push(self.acc as u8);
                self.acc = 0;
                self.n = 0;
            }
        }
    }

    fn flush(&mut self) {
        if self.n > 0 {
            self.out.push((self.acc << (8 - self.n)) as u8);
            self.acc = 0;
            self.n = 0;
        }
    }
}

struct PageInfo {
    /// Objects in the page's section (page object first).
    objects: Vec<u32>,
    /// Shared objects the page uses, by their index in the shared object table.
    shared: Vec<u32>,
}

fn linearized(doc: &Document, opts: &WriteOptions) -> Vec<u8> {
    let objs = live(doc);
    let by_num: HashMap<u32, &Object> = objs.iter().map(|(n, o)| (*n, &**o)).collect();
    let pages: Vec<u32> = pagebara_pages::PageTree::load(doc)
        .refs()
        .iter()
        .map(|r| r.num)
        .filter(|n| by_num.contains_key(n))
        .collect();
    let catalog = doc
        .trailer()
        .get(b"Root")
        .and_then(Object::as_objref)
        .map(|r| r.num);
    let plain = WriteOptions {
        linearize: false,
        ..*opts
    };
    let (Some(catalog), false) = (catalog, pages.is_empty()) else {
        return write(doc, &plain);
    };
    let Some(cat) = by_num.get(&catalog).and_then(|o| o.as_dict()) else {
        return write(doc, &plain);
    };
    let mut stop: HashSet<u32> = pages.iter().copied().collect();
    stop.insert(catalog);
    let mut placed: HashSet<u32> = HashSet::from([catalog]);
    let reach_from = |o: &Object, placed: &mut HashSet<u32>| -> Vec<u32> {
        let mut refs = Vec::new();
        collect_refs(o, &mut refs);
        let mut out = Vec::new();
        let mut seen = placed.clone();
        for r in refs {
            if !stop.contains(&r) {
                reach(doc, r, &stop, &mut out, &mut seen);
            }
        }
        out.retain(|n| by_num.contains_key(n));
        placed.extend(&out);
        out
    };
    // Part 4: the catalog and what opening the document needs.
    let mut part4 = vec![catalog];
    for k in [
        &b"ViewerPreferences"[..],
        b"PageMode",
        b"Threads",
        b"OpenAction",
        b"AcroForm",
    ] {
        if let Some(v) = cat.get(k) {
            part4.extend(reach_from(v, &mut placed));
        }
    }
    // Outlines go with the first page when the document opens showing them, else first in
    // part 9; either way they get a hint table.
    let outlines: Vec<u32> = cat
        .get(b"Outlines")
        .map(|o| reach_from(o, &mut placed))
        .unwrap_or_default();
    let outlines_first = cat.name(b"PageMode") == Some(b"UseOutlines");
    // What each page reaches, and how many pages reach each object.
    let mut reached: Vec<Vec<u32>> = Vec::with_capacity(pages.len());
    let mut users: HashMap<u32, u32> = HashMap::new();
    for &p in &pages {
        let mut out = Vec::new();
        let mut seen = placed.clone();
        let mut stop_p = stop.clone();
        stop_p.remove(&p);
        reach(doc, p, &stop_p, &mut out, &mut seen);
        out.retain(|n| by_num.contains_key(n));
        for &n in &out {
            *users.entry(n).or_insert(0) += 1;
        }
        reached.push(out);
    }
    // Part 6: the first page, the shared objects it uses, then the outlines.
    let first_private: Vec<u32> = reached[0]
        .iter()
        .copied()
        .filter(|n| users[n] == 1)
        .collect();
    let first_shared: Vec<u32> = reached[0]
        .iter()
        .copied()
        .filter(|n| users[n] > 1)
        .collect();
    placed.extend(&first_private);
    placed.extend(&first_shared);
    let mut sections: Vec<Vec<u32>> = vec![first_private.clone()];
    for r in &reached[1..] {
        let own: Vec<u32> = r
            .iter()
            .copied()
            .filter(|n| users[n] == 1 && placed.insert(*n))
            .collect();
        sections.push(own);
    }
    let mut shared_rest = Vec::new();
    for r in &reached[1..] {
        for &n in r {
            if placed.insert(n) {
                shared_rest.push(n);
            }
        }
    }
    let mut other: Vec<u32> = objs
        .iter()
        .map(|(n, _)| *n)
        .filter(|n| !placed.contains(n))
        .collect();
    let before_outlines: Vec<u32> = first_private.iter().chain(&first_shared).copied().collect();
    let mut part6 = before_outlines.clone();
    if outlines_first {
        part6.extend(&outlines);
    } else {
        other.splice(0..0, outlines.iter().copied());
    }

    // Numbering: the main section (parts 7–9) from 1, then the first-page section.
    let mut main_order: Vec<u32> = Vec::new();
    for s in &sections[1..] {
        main_order.extend(s);
    }
    main_order.extend(&shared_rest);
    main_order.extend(&other);
    let k = main_order.len() as u32;
    let lin_num = k + 1;
    let mut map: HashMap<u32, u32> = HashMap::new();
    for (i, n) in main_order.iter().enumerate() {
        map.insert(*n, i as u32 + 1);
    }
    for (i, n) in part4.iter().chain(&part6).enumerate() {
        map.insert(*n, lin_num + 1 + i as u32);
    }
    let hint_num = lin_num + 1 + (part4.len() + part6.len()) as u32;
    let size = hint_num + 1;
    let bodies: HashMap<u32, Vec<u8>> = objs
        .par_iter()
        .filter(|(n, _)| map.contains_key(n))
        .map(|(n, o)| (*n, indirect(doc, map[n], &renumber(o, &map), opts.level)))
        .collect();
    let len_of = |n: &u32| bodies[n].len() as u64;

    // The shared object table lists the first page's objects, then the shared section.
    let shared_index: HashMap<u32, u32> = part6
        .iter()
        .chain(&shared_rest)
        .enumerate()
        .map(|(i, n)| (*n, i as u32))
        .collect();
    let infos: Vec<PageInfo> = sections
        .iter()
        .enumerate()
        .map(|(i, s)| PageInfo {
            objects: if i == 0 { part6.clone() } else { s.clone() },
            shared: if i == 0 {
                Vec::new()
            } else {
                reached[i]
                    .iter()
                    .filter(|n| !s.contains(n))
                    .filter_map(|n| shared_index.get(n).copied())
                    .collect()
            },
        })
        .collect();

    // Header, linearization dictionary and first-page cross-reference have fixed lengths.
    let mut head = Vec::new();
    header(&mut head, version(doc, opts.min_version));
    let lin_at = head.len();
    let lin_len = lin_dict(lin_num, 0, 0, 0, 0, 0, 0, 0).len();
    let fx_at = lin_at + lin_len;
    let mut t = trailer(doc, &map);
    t.set(b"Size", i64::from(size));
    let fx_len = first_xref(lin_num, &vec![0; (size - lin_num) as usize], &t, 0).len();
    let cat_at = fx_at + fx_len;
    let part4_len: u64 = part4.iter().map(len_of).sum();
    let hint_at = cat_at + part4_len as usize;
    // Offsets in hint tables are those of the file without the hint stream.
    let mut page_at = Vec::with_capacity(infos.len());
    let mut at = hint_at as u64;
    for p in &infos {
        page_at.push(at);
        at += p.objects.iter().map(len_of).sum::<u64>();
    }
    let shared_rest_at = at;
    let outlines_at = if outlines_first {
        page_at[0] + before_outlines.iter().map(len_of).sum::<u64>()
    } else {
        at + shared_rest.iter().map(len_of).sum::<u64>()
    };
    let nobj: Vec<u64> = infos.iter().map(|p| p.objects.len() as u64).collect();
    let plen: Vec<u64> = infos
        .iter()
        .map(|p| p.objects.iter().map(len_of).sum())
        .collect();
    let nshared: Vec<u64> = infos.iter().map(|p| p.shared.len() as u64).collect();
    let min = |v: &[u64]| v.iter().copied().min().unwrap_or(0);
    let max = |v: &[u64]| v.iter().copied().max().unwrap_or(0);
    let (lo_n, lo_l) = (min(&nobj), min(&plen));
    let total_shared = (part6.len() + shared_rest.len()) as u64;
    let b_n = bits(max(&nobj) - lo_n);
    let b_l = bits(max(&plen) - lo_l);
    let b_ns = bits(max(&nshared));
    let b_id = bits(total_shared.saturating_sub(1));
    let mut w = BitWriter::default();
    // Page offset hint table; a page's content is given as the whole page, as qpdf does.
    w.put(lo_n, 32);
    w.put(page_at[0], 32);
    w.put(u64::from(b_n), 16);
    w.put(lo_l, 32);
    w.put(u64::from(b_l), 16);
    w.put(0, 32);
    w.put(0, 16);
    w.put(lo_l, 32);
    w.put(u64::from(b_l), 16);
    w.put(u64::from(b_ns), 16);
    w.put(u64::from(b_id), 16);
    w.put(0, 16);
    w.put(4, 16);
    for v in &nobj {
        w.put(v - lo_n, b_n);
    }
    w.flush();
    for v in &plen {
        w.put(v - lo_l, b_l);
    }
    w.flush();
    for v in &nshared {
        w.put(*v, b_ns);
    }
    w.flush();
    for p in &infos {
        for &id in &p.shared {
            w.put(u64::from(id), b_id);
        }
    }
    w.flush();
    // Numerators and content offsets take no bits.
    for v in &plen {
        w.put(v - lo_l, b_l);
    }
    w.flush();
    // Shared object hint table: one group per object.
    let shared_table_at = w.out.len();
    let group_lens: Vec<u64> = part6.iter().chain(&shared_rest).map(len_of).collect();
    let lo_g = min(&group_lens);
    let b_g = bits(max(&group_lens) - lo_g);
    let first_shared_num = shared_rest.first().map_or(0, |n| map[n]);
    w.put(u64::from(first_shared_num), 32);
    w.put(
        if shared_rest.is_empty() {
            0
        } else {
            shared_rest_at
        },
        32,
    );
    w.put(part6.len() as u64, 32);
    w.put(total_shared, 32);
    w.put(0, 16);
    w.put(lo_g, 32);
    w.put(u64::from(b_g), 16);
    for v in &group_lens {
        w.put(v - lo_g, b_g);
    }
    w.flush();
    for _ in &group_lens {
        w.put(0, 1);
    }
    w.flush();
    let mut hd = Dict::new();
    hd.set(b"S", shared_table_at as i64);
    if let Some(first) = outlines.first() {
        hd.set(b"O", w.out.len() as i64);
        w.put(u64::from(map[first]), 32);
        w.put(outlines_at, 32);
        w.put(outlines.len() as u64, 32);
        w.put(outlines.iter().map(len_of).sum(), 32);
    }
    let hint = indirect(
        doc,
        hint_num,
        &Object::Stream(Stream::new(hd, w.out)),
        opts.level,
    );
    let hint_len = hint.len();

    // The file.
    let mut out = head;
    let mut offsets: HashMap<u32, usize> = HashMap::new();
    let mut body = Vec::new();
    for &n in &part4 {
        offsets.insert(map[&n], cat_at + body.len());
        body.extend_from_slice(&bodies[&n]);
    }
    let hint_off = cat_at + body.len();
    body.extend_from_slice(&hint);
    for &n in part6.iter().chain(&main_order) {
        offsets.insert(map[&n], cat_at + body.len());
        body.extend_from_slice(&bodies[&n]);
    }
    offsets.insert(hint_num, hint_off);
    let end_first = hint_off + hint_len + part6.iter().map(len_of).sum::<u64>() as usize;
    let main_xref_at = cat_at + body.len();
    let mut tail = Vec::new();
    let main_rows: Vec<(u32, Row)> = (1..=k).map(|n| (n, Row::At(offsets[&n]))).collect();
    xref_table(&mut tail, 0, &main_rows, true);
    let mut mt = Dict::new();
    mt.set(b"Size", i64::from(k) + 1);
    tail.extend_from_slice(b"trailer\n");
    write_object(&mut tail, &Object::Dict(mt));
    let _ = writeln!(tail, "\nstartxref\n{fx_at}\n%%EOF");
    let file_len = main_xref_at + tail.len();
    // T: the white-space before the first entry of the main table.
    let t_at = main_xref_at + b"xref\n".len() + format!("0 {}", k + 1).len();
    out.extend_from_slice(&lin_dict(
        lin_num,
        file_len,
        hint_off,
        hint_len,
        map[&pages[0]],
        end_first,
        pages.len(),
        t_at,
    ));
    let first_offsets: Vec<usize> = (lin_num..size)
        .map(|n| if n == lin_num { lin_at } else { offsets[&n] })
        .collect();
    out.extend_from_slice(&first_xref(lin_num, &first_offsets, &t, main_xref_at));
    debug_assert_eq!(out.len(), cat_at);
    out.extend_from_slice(&body);
    out.extend_from_slice(&tail);
    out
}

#[allow(clippy::too_many_arguments)]
fn lin_dict(
    num: u32,
    l: usize,
    h_at: usize,
    h_len: usize,
    o: u32,
    e: usize,
    n: usize,
    t: usize,
) -> Vec<u8> {
    let mut out = Vec::new();
    let _ = write!(
        out,
        "{num} 0 obj\n<</Linearized 1/L {l:010}/H[{h_at:010} {h_len:010}]/O {o:010}/E {e:010}/N {n:010}/T {t:010}>>\nendobj\n"
    );
    out
}

fn first_xref(first: u32, offsets: &[usize], t: &Dict, prev: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let _ = writeln!(out, "xref\n{first} {}", offsets.len());
    for off in offsets {
        let _ = write!(out, "{off:010} 00000 n\r\n");
    }
    out.extend_from_slice(b"trailer\n");
    let mut t = t.clone();
    t.set(b"Prev", Object::Int(0));
    let mut d = Vec::new();
    write_object(&mut d, &Object::Dict(t));
    // Prev padded to a fixed width so the section's length does not depend on it.
    let s = String::from_utf8_lossy(&d).replace("/Prev 0", &format!("/Prev {prev:010}"));
    out.extend_from_slice(s.as_bytes());
    out.extend_from_slice(b"\nstartxref\n0\n%%EOF\n");
    out
}
