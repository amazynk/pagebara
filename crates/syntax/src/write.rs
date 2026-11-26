//! Serialisation and saving: incremental updates (§7.5.6) and full rewrites.

use std::collections::HashSet;
use std::io::Write as _;
use std::sync::Arc;

use rayon::prelude::*;

use crate::document::Document;
use crate::lexer::is_regular;
use crate::object::{Dict, ObjRef, Object, PdfString, Stream, StreamData};
use crate::xref::{Kind, NumMap};
use crate::{Error, Result};

/// Appends `o` in PDF syntax.
pub fn write_object(out: &mut Vec<u8>, o: &Object) {
    match o {
        Object::Null => out.extend_from_slice(b"null"),
        Object::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Object::Int(i) => {
            let _ = write!(out, "{i}");
        }
        Object::Real(r) => write_real(out, *r),
        Object::String(s) => write_string(out, s),
        Object::Name(n) => write_name(out, n.as_bytes()),
        Object::Array(a) => {
            out.push(b'[');
            for (i, o) in a.iter().enumerate() {
                if i > 0 {
                    out.push(b' ');
                }
                write_object(out, o);
            }
            out.push(b']');
        }
        Object::Dict(d) => write_dict(out, d),
        Object::Stream(_) => {
            // Streams are only valid as indirect objects; see `write_indirect`.
            out.extend_from_slice(b"null");
        }
        Object::Ref(r) => {
            let _ = write!(out, "{} {} R", r.num, r.generation);
        }
    }
}

fn write_dict(out: &mut Vec<u8>, d: &Dict) {
    out.extend_from_slice(b"<<");
    for (k, v) in d.iter() {
        write_name(out, k.as_bytes());
        out.push(b' ');
        write_object(out, v);
    }
    out.extend_from_slice(b">>");
}

/// PDF has no exponent notation; Rust's `Display` never uses one for floats.
fn write_real(out: &mut Vec<u8>, r: f64) {
    let r = if r.is_finite() { r } else { 0.0 };
    let _ = write!(out, "{r}");
}

fn write_name(out: &mut Vec<u8>, n: &[u8]) {
    out.push(b'/');
    for &b in n {
        if is_regular(b) && b != b'#' && (0x21..0x7f).contains(&b) {
            out.push(b);
        } else {
            let _ = write!(out, "#{b:02X}");
        }
    }
}

fn write_string(out: &mut Vec<u8>, s: &PdfString) {
    if s.hex {
        out.push(b'<');
        for b in &s.bytes {
            let _ = write!(out, "{b:02X}");
        }
        out.push(b'>');
        return;
    }
    out.push(b'(');
    for &b in &s.bytes {
        match b {
            b'(' | b')' | b'\\' => out.extend_from_slice(&[b'\\', b]),
            // A bare CR would read back as LF.
            b'\r' => out.extend_from_slice(b"\\r"),
            _ => out.push(b),
        }
    }
    out.push(b')');
}

/// `num generation obj … endobj`, with a stream's `/Length` set to its data.
fn write_indirect(out: &mut Vec<u8>, doc: &Document, num: u32, generation: u16, o: &Object) {
    let _ = writeln!(out, "{num} {generation} obj");
    let encrypted;
    let o = match doc.save_security() {
        Some((sec, enc_num)) if enc_num != Some(num) => {
            let mut c = o.clone();
            if let Object::Stream(s) = &mut c
                && let StreamData::Source(_) = s.data
            {
                s.data = StreamData::Owned(doc.raw(s).into());
            }
            sec.encrypt(ObjRef::new(num, generation), &mut c);
            encrypted = c;
            &encrypted
        }
        _ => o,
    };
    match o {
        Object::Stream(s) => {
            let data = doc.raw(s);
            let mut d = s.dict.clone();
            d.set(b"Length", data.len() as i64);
            write_dict(out, &d);
            out.extend_from_slice(b"\nstream\n");
            out.extend_from_slice(data);
            out.extend_from_slice(b"\nendstream");
        }
        o => write_object(out, o),
    }
    out.extend_from_slice(b"\nendobj\n");
}

enum Row {
    Free(u16),
    At(usize, u16),
    /// Object `index` of object stream `stream`.
    InStream(u32, u32),
}

/// How [`Document::save_full_with`] writes a file. The default is the plain layout of
/// [`Document::save_full`]; [`SaveOptions::compact`] turns everything on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SaveOptions {
    /// Pack objects other than streams into Flate-compressed object streams (PDF 1.5).
    /// Implies `xref_stream`.
    pub object_streams: bool,
    /// Flate-compress streams stored without any filter (XMP metadata stays readable).
    pub compress_streams: bool,
    /// A compressed cross-reference stream instead of a table (PDF 1.5).
    pub xref_stream: bool,
}

impl SaveOptions {
    /// The smallest output.
    pub const fn compact() -> Self {
        SaveOptions {
            object_streams: true,
            compress_streams: true,
            xref_stream: true,
        }
    }
}

/// Objects per object stream: more compress a little better, but a reader decodes the whole
/// stream to reach any one of them.
const OBJSTM_MAX: usize = 100;
/// Streams shorter than this are not worth compressing.
const COMPRESS_MIN: usize = 32;
const FLATE_LEVEL: u8 = 6;

impl Document {
    /// The original bytes followed by an update holding the edits. Signatures over the
    /// original stay valid. Repaired files need [`Document::save_full`].
    pub fn save_incremental(&self) -> Result<Vec<u8>> {
        if self.base_kind() == Kind::Repaired || self.security_changed() {
            return Err(Error::NeedsFullSave);
        }
        let mut out = self.source().to_vec();
        if !out.ends_with(b"\n") {
            out.push(b'\n');
        }
        let mut rows = Vec::new();
        for (num, o) in self.changes() {
            match o {
                Some((generation, o)) => {
                    rows.push((num, Row::At(out.len(), generation)));
                    write_indirect(&mut out, self, num, generation, &o);
                }
                None => {
                    let generation = self
                        .base_reference(num)
                        .map_or(0, |r| r.generation)
                        .saturating_add(1);
                    rows.push((num, Row::Free(generation)));
                }
            }
        }
        let mut trailer = self.trailer().clone();
        trailer.set(b"Prev", self.base_startxref() as i64);
        let xref_at = out.len();
        if self.base_kind() == Kind::Stream {
            let num = self.next_num();
            rows.push((num, Row::At(xref_at, 0)));
            trailer.set(b"Size", i64::from(num) + 1);
            write_xref_stream(&mut out, num, &rows, trailer);
        } else {
            let size = self
                .trailer()
                .int(b"Size")
                .unwrap_or(0)
                .max(i64::from(self.next_num()));
            trailer.set(b"Size", size);
            write_xref_table(&mut out, &rows, false);
            out.extend_from_slice(b"trailer\n");
            write_dict(&mut out, &trailer);
            out.push(b'\n');
        }
        let _ = writeln!(out, "startxref\n{xref_at}\n%%EOF");
        Ok(out)
    }

    /// A fresh file with every live object written out directly and a single xref table.
    pub fn save_full(&self) -> Result<Vec<u8>> {
        self.save_full_with(&SaveOptions::default())
    }

    /// A fresh file holding every live object, laid out as `opts` asks. Protection is kept
    /// (or changed, see [`Document::set_password`]): object streams are encrypted as a
    /// whole, the cross-reference stream and the `/Encrypt` dictionary never.
    pub fn save_full_with(&self, opts: &SaveOptions) -> Result<Vec<u8>> {
        if self.root_is_object_zero() {
            let mut d = self.clone();
            // With no catalog to move, the file is written as it is.
            if d.move_root_off_zero() {
                return d.save_full_with(opts);
            }
        }
        let xref_stream = opts.xref_stream || opts.object_streams;
        let mut version = self.version();
        if xref_stream {
            version = version.max((1, 5));
        }
        let mut out = Vec::with_capacity(self.source().len());
        let _ = writeln!(out, "%PDF-{}.{}", version.0, version.1);
        out.extend_from_slice(b"%\xe2\xe3\xcf\xd3\n");

        let enc_num = self.save_security().and_then(|s| s.1);
        self.load_all();
        let objects = self.objects();
        let old = self.old_among(&objects);
        let mut packed = Vec::new();
        let mut direct = Vec::new();
        for (r, o) in objects {
            let num = r.num;
            if num == 0 || self.stale_encrypt_num() == Some(num) || old.contains(&num) {
                continue;
            }
            if opts.object_streams && r.generation == 0 && enc_num != Some(num) && packable(&o) {
                packed.push((num, o));
            } else {
                direct.push((num, r.generation, o));
            }
        }
        if opts.compress_streams {
            direct = direct
                .into_par_iter()
                .map(|(num, g, o)| (num, g, self.compressed(o)))
                .collect();
        }

        let mut rows = vec![(0, Row::Free(65535))];
        for (num, generation, o) in &direct {
            rows.push((*num, Row::At(out.len(), *generation)));
            write_indirect(&mut out, self, *num, *generation, o);
        }
        let mut next = self
            .next_num()
            .max(rows.iter().map(|r| r.0 + 1).max().unwrap_or(1))
            .max(packed.last().map_or(1, |p| p.0 + 1));
        let groups: Vec<_> = packed.chunks(OBJSTM_MAX).collect();
        let streams: Vec<_> = groups.par_iter().map(|g| object_stream(g)).collect();
        for (group, stm) in groups.into_iter().zip(streams) {
            let snum = next;
            next += 1;
            for (index, (num, _)) in group.iter().enumerate() {
                rows.push((*num, Row::InStream(snum, index as u32)));
            }
            rows.push((snum, Row::At(out.len(), 0)));
            write_indirect(&mut out, self, snum, 0, &stm);
        }
        rows.sort_unstable_by_key(|r| r.0);

        let mut trailer = self.trailer().clone();
        for k in [&b"Prev"[..], b"XRefStm"] {
            trailer.remove(k);
        }
        let xref_at = out.len();
        if xref_stream {
            rows.push((next, Row::At(xref_at, 0)));
            trailer.set(b"Size", i64::from(next) + 1);
            write_xref_stream(&mut out, next, &rows, trailer);
            let _ = writeln!(out, "startxref\n{xref_at}\n%%EOF");
        } else {
            let size = rows.last().map_or(1, |r| r.0 + 1);
            trailer.set(b"Size", i64::from(size));
            write_xref_table(&mut out, &rows, true);
            out.extend_from_slice(b"trailer\n");
            write_dict(&mut out, &trailer);
            let _ = writeln!(out, "\nstartxref\n{xref_at}\n%%EOF");
        }
        Ok(out)
    }

    /// `o`, Flate-compressed if it is a stream without filters and that makes it smaller.
    fn compressed(&self, o: Arc<Object>) -> Arc<Object> {
        let Object::Stream(s) = &*o else { return o };
        let unfiltered = match s.dict.get(b"Filter") {
            None | Some(Object::Null) => true,
            Some(Object::Array(a)) => a.is_empty(),
            _ => false,
        };
        let raw = self.raw(s);
        if !unfiltered || s.dict.name(b"Type") == Some(b"Metadata") || raw.len() < COMPRESS_MIN {
            return o;
        }
        let data = pagebara_filters::encode_flate(raw, FLATE_LEVEL);
        if data.len() >= raw.len() {
            return o;
        }
        let mut dict = s.dict.clone();
        // Parameters of no filter would apply to the new one.
        dict.remove(b"DecodeParms");
        dict.set(b"Filter", Object::name(b"FlateDecode"));
        Arc::new(Object::Stream(Stream::new(dict, data)))
    }
}

impl Document {
    /// The numbers of the objects a full save leaves out: cross-reference and object
    /// streams, which it replaces, and the linearization dictionary, which no longer
    /// describes the file. An object that looks like one of these but that the trailer or
    /// another object refers to is not one: in a damaged file the root of the page tree
    /// can carry `/Type /ObjStm`, and leaving it out leaves the file without its pages.
    pub fn old_structure(&self) -> HashSet<u32> {
        self.old_among(&self.objects())
    }

    /// Every object the file has, in ascending order of number.
    fn objects(&self) -> Vec<(ObjRef, Arc<Object>)> {
        self.object_numbers()
            .into_iter()
            .filter_map(|n| {
                let r = self.reference(n)?;
                Some((r, self.get(r)?))
            })
            .collect()
    }

    /// The first object number from `from` on that no reference in the file names.
    pub(crate) fn unnamed_num(&self, from: u32) -> Option<u32> {
        let mut named: NumMap<()> = NumMap::default();
        let mut note = |n: u32| {
            if n >= from {
                named.insert(n, ());
            }
        };
        for (_, o) in self.objects() {
            each_reference(&o, &mut note);
        }
        for (_, v) in self.trailer().iter() {
            each_reference(v, &mut note);
        }
        let mut num = from;
        while named.contains_key(&num) {
            num = num.checked_add(1)?;
        }
        Some(num)
    }

    /// [`Document::old_structure`], given [`Document::objects`].
    fn old_among(&self, objects: &[(ObjRef, Arc<Object>)]) -> HashSet<u32> {
        let mut old: HashSet<u32> = objects
            .iter()
            .filter(|(_, o)| is_old_structure(o))
            .map(|(r, _)| r.num)
            .collect();
        if old.is_empty() {
            return old;
        }
        // Asked for every reference in the file, so with the quick hash for numbers.
        let looks_old: NumMap<()> = old.iter().map(|&n| (n, ())).collect();
        let mut referred = objects
            .par_iter()
            .filter(|(r, _)| !looks_old.contains_key(&r.num))
            .fold(Vec::new, |mut found, (_, o)| {
                references_among(o, &looks_old, &mut found);
                found
            })
            .reduce(Vec::new, |mut a, mut b| {
                a.append(&mut b);
                a
            });
        for (_, v) in self.trailer().iter() {
            references_among(v, &looks_old, &mut referred);
        }
        // One that stays may refer to another.
        while let Some(n) = referred.pop() {
            if old.remove(&n)
                && let Some(o) = self.reference(n).and_then(|r| self.get(r))
            {
                references_among(&o, &looks_old, &mut referred);
            }
        }
        old
    }
}

/// Adds the numbers `o` refers to that `among` marks.
fn references_among(o: &Object, among: &NumMap<()>, found: &mut Vec<u32>) {
    each_reference(o, &mut |n| {
        if among.contains_key(&n) {
            found.push(n);
        }
    });
}

/// Calls `f` with the number of every object `o` refers to.
fn each_reference(o: &Object, f: &mut impl FnMut(u32)) {
    match o {
        Object::Ref(r) => f(r.num),
        Object::Array(a) => a.iter().for_each(|e| each_reference(e, f)),
        Object::Dict(d) => d.iter().for_each(|(_, v)| each_reference(v, f)),
        Object::Stream(s) => s.dict.iter().for_each(|(_, v)| each_reference(v, f)),
        _ => {}
    }
}

/// What a cross-reference or object stream, or a linearization dictionary, looks like.
fn is_old_structure(o: &Object) -> bool {
    match o {
        Object::Stream(s) => s
            .dict
            .name(b"Type")
            .is_some_and(|t| t == b"XRef" || t == b"ObjStm"),
        Object::Dict(d) => d.contains(b"Linearized"),
        _ => false,
    }
}

/// Whether an object may go into an object stream (§7.5.7): not a stream, and not a
/// signature, whose `/Contents` must stay unencrypted.
fn packable(o: &Object) -> bool {
    match o {
        Object::Stream(_) => false,
        Object::Dict(d) => !d.contains(b"ByteRange"),
        _ => true,
    }
}

/// An object stream holding `group`.
fn object_stream(group: &[(u32, Arc<Object>)]) -> Object {
    let mut head = Vec::new();
    let mut body = Vec::new();
    for (num, o) in group {
        let _ = write!(head, "{num} {} ", body.len());
        write_object(&mut body, o);
        body.push(b'\n');
    }
    let first = head.len();
    head.extend_from_slice(&body);
    let mut d = Dict::new();
    d.set(b"Type", Object::name(b"ObjStm"));
    d.set(b"N", group.len() as i64);
    d.set(b"First", first as i64);
    d.set(b"Filter", Object::name(b"FlateDecode"));
    Object::Stream(Stream::new(
        d,
        pagebara_filters::encode_flate(&head, FLATE_LEVEL),
    ))
}

/// Free entries a dense table writes to bridge a gap before starting a new subsection.
const MAX_GAP: u32 = 1024;

/// Sorted rows as subsections. With `dense`, small gaps become free entries.
fn write_xref_table(out: &mut Vec<u8>, rows: &[(u32, Row)], dense: bool) {
    out.extend_from_slice(b"xref\n");
    let mut i = 0;
    while i < rows.len() {
        let start = rows[i].0;
        let gap = if dense { MAX_GAP } else { 0 };
        let mut j = i + 1;
        while j < rows.len()
            && rows[j]
                .0
                .checked_sub(rows[j - 1].0)
                .is_some_and(|d| (1..=gap + 1).contains(&d))
        {
            j += 1;
        }
        if dense {
            let end = rows[j - 1].0 + 1;
            let _ = writeln!(out, "{start} {}", end - start);
            let mut k = i;
            for n in start..end {
                if rows.get(k).is_some_and(|r| r.0 == n) {
                    write_row(out, &rows[k].1);
                    k += 1;
                } else {
                    write_row(out, &Row::Free(0));
                }
            }
        } else {
            let _ = writeln!(out, "{start} {}", j - i);
            for r in &rows[i..j] {
                write_row(out, &r.1);
            }
        }
        i = j;
    }
}

fn write_row(out: &mut Vec<u8>, r: &Row) {
    let _ = match *r {
        Row::Free(generation) => writeln!(out, "0000000000 {generation:05} f\r"),
        Row::At(off, generation) => writeln!(out, "{off:010} {generation:05} n\r"),
        // Tables are only written for files without object streams.
        Row::InStream(..) => writeln!(out, "0000000000 00000 f\r"),
    };
}

/// A Flate-compressed xref stream object `num` holding `rows` (sorted except the last, which
/// is the stream itself and has the highest number). Never encrypted (§7.6.1).
fn write_xref_stream(out: &mut Vec<u8>, num: u32, rows: &[(u32, Row)], mut dict: Dict) {
    let max_f2 = rows
        .iter()
        .map(|r| match r.1 {
            Row::At(o, _) => o,
            Row::InStream(s, _) => s as usize,
            Row::Free(_) => 0,
        })
        .max()
        .unwrap_or(0);
    let w1 = (usize::BITS - max_f2.leading_zeros()).div_ceil(8).max(1) as usize;
    let width = 1 + w1 + 2;
    let mut data = Vec::with_capacity(rows.len() * (width + 1));
    let mut index = Vec::new();
    let mut prev = vec![0u8; width];
    let mut row = vec![0u8; width];
    for (k, (n, r)) in rows.iter().enumerate() {
        if k == 0 || *n != rows[k - 1].0 + 1 {
            index.push(Object::Int(i64::from(*n)));
            index.push(Object::Int(0));
        }
        if let Some(Object::Int(c)) = index.last_mut() {
            *c += 1;
        }
        let (t, f2, f3) = match *r {
            Row::Free(generation) => (0u8, 0usize, generation),
            Row::At(off, generation) => (1, off, generation),
            Row::InStream(s, i) => (2, s as usize, i as u16),
        };
        row[0] = t;
        row[1..1 + w1].copy_from_slice(&f2.to_be_bytes()[size_of::<usize>() - w1..]);
        row[1 + w1..].copy_from_slice(&f3.to_be_bytes());
        // PNG Up predictor: neighbouring rows differ in few bytes.
        data.push(2);
        data.extend(row.iter().zip(&prev).map(|(a, b)| a.wrapping_sub(*b)));
        std::mem::swap(&mut row, &mut prev);
    }
    dict.set(b"Type", Object::name(b"XRef"));
    dict.set(
        b"W",
        vec![Object::Int(1), Object::Int(w1 as i64), Object::Int(2)],
    );
    dict.set(b"Index", index);
    dict.remove(b"XRefStm");
    let mut parms = Dict::new();
    parms.set(b"Predictor", 12);
    parms.set(b"Columns", width as i64);
    dict.set(b"Filter", Object::name(b"FlateDecode"));
    dict.set(b"DecodeParms", parms);
    let data = pagebara_filters::encode_flate(&data, FLATE_LEVEL);
    dict.set(b"Length", data.len() as i64);
    let _ = writeln!(out, "{num} 0 obj");
    write_dict(out, &dict);
    out.extend_from_slice(b"\nstream\n");
    out.extend_from_slice(&data);
    out.extend_from_slice(b"\nendstream\nendobj\n");
}
