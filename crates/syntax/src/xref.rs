//! Cross-reference tables and streams (§7.5.4, §7.5.8), hybrid files and `/Prev` chains.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

use crate::lexer::{Lexer, Token};
use crate::object::{Dict, Object};
use crate::parser::Parser;
use crate::stream::decode_direct;
use crate::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry {
    Free,
    Offset {
        offset: usize,
        generation: u16,
    },
    /// Object `index` of object stream `stream`.
    InStream {
        stream: u32,
        index: u32,
    },
}

/// Object numbers are dense small integers: a multiply is all the hashing they need.
#[derive(Default, Clone, Copy, Debug)]
pub struct NumHasher(u64);

impl Hasher for NumHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 << 8 | u64::from(b)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        }
    }

    fn write_u32(&mut self, n: u32) {
        self.0 = u64::from(n).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
}

pub type NumMap<V> = HashMap<u32, V, BuildHasherDefault<NumHasher>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Table,
    Stream,
    /// Rebuilt by scanning the file.
    Repaired,
}

#[derive(Debug)]
pub struct Xref {
    pub entries: NumMap<Entry>,
    pub trailer: Dict,
    pub kind: Kind,
    /// Offset of the newest section, for `/Prev` of an incremental update.
    pub startxref: usize,
}

/// Bounds hostile `/Prev` chains.
const MAX_SECTIONS: usize = 4096;

/// Keys that describe one xref stream rather than the document.
const SECTION_KEYS: [&[u8]; 9] = [
    b"Prev",
    b"XRefStm",
    b"W",
    b"Index",
    b"Length",
    b"Filter",
    b"DecodeParms",
    b"Type",
    b"DL",
];

/// Reads the xref chain ending at the last `startxref`. `shift` is the offset of `%PDF-`,
/// tried when offsets are off by leading junk.
pub fn load(src: &[u8], shift: usize) -> Result<Xref> {
    let startxref = find_startxref(src).ok_or(Error::Xref("no startxref"))?;
    let mut x = Xref {
        entries: NumMap::default(),
        trailer: Dict::new(),
        kind: Kind::Table,
        startxref,
    };
    let mut todo = vec![startxref];
    let mut seen = Vec::new();
    let mut first = true;
    while let Some(off) = todo.pop() {
        if seen.contains(&off) || seen.len() >= MAX_SECTIONS {
            continue;
        }
        seen.push(off);
        let (kind, data) = section(src, off).or_else(|e| match shift {
            0 => Err(e),
            s => section(src, off + s),
        })?;
        let (mut entries, trailer) = (data.entries, data.dict);
        if let Some(stm) = trailer
            .int(b"XRefStm")
            .and_then(|o| usize::try_from(o).ok())
            && !seen.contains(&stm)
        {
            // Hybrid file: the stream's entries take precedence over the table's free ones.
            seen.push(stm);
            if let Ok((_, s)) = section(src, stm) {
                let table = std::mem::replace(&mut entries, s.entries);
                entries.extend(table);
            }
        }
        for (n, e) in entries {
            x.entries.entry(n).or_insert(e);
        }
        if first {
            x.kind = kind;
            first = false;
        }
        if let Some(prev) = trailer.int(b"Prev").and_then(|p| usize::try_from(p).ok()) {
            todo.push(prev);
        }
        merge_trailer(&mut x.trailer, trailer);
    }
    for k in SECTION_KEYS {
        x.trailer.remove(k);
    }
    Ok(x)
}

/// Fills keys missing from `newer` with those of an older trailer.
pub fn merge_trailer(newer: &mut Dict, older: Dict) {
    if newer.is_empty() {
        *newer = older;
        return;
    }
    for (k, v) in older.0 {
        if !newer.contains(&k.0) && !SECTION_KEYS.contains(&&*k.0) {
            newer.0.push((k, v));
        }
    }
}

pub fn find_startxref(src: &[u8]) -> Option<usize> {
    let tail = src.len().saturating_sub(4096);
    let at = memchr::memmem::rfind(&src[tail..], b"startxref")
        .map(|p| tail + p)
        .or_else(|| memchr::memmem::rfind(src, b"startxref"))?;
    match Lexer::new(src, at + 9).next_token() {
        Token::Int(n) => usize::try_from(n).ok(),
        _ => None,
    }
}

struct SectionData {
    entries: Vec<(u32, Entry)>,
    dict: Dict,
}

fn section(src: &[u8], off: usize) -> Result<(Kind, SectionData)> {
    if off >= src.len() {
        return Err(Error::Xref("xref offset past end of file"));
    }
    let mut lex = Lexer::new(src, off);
    if lex.next_token() == Token::Keyword(b"xref") {
        return table(lex).map(|s| (Kind::Table, s));
    }
    stream_section(src, off).map(|s| (Kind::Stream, s))
}

fn table(mut lex: Lexer<'_>) -> Result<SectionData> {
    let mut entries = Vec::new();
    loop {
        match lex.next_token() {
            Token::Keyword(b"trailer") => break,
            Token::Int(start) => {
                let Token::Int(count) = lex.next_token() else {
                    return Err(Error::Xref("bad xref subsection header"));
                };
                let (Ok(mut start), Ok(count)) = (u32::try_from(start), u32::try_from(count))
                else {
                    return Err(Error::Xref("bad xref subsection header"));
                };
                for i in 0..count {
                    let (Token::Int(off), Token::Int(generation)) =
                        (lex.next_token(), lex.next_token())
                    else {
                        return Err(Error::Xref("bad xref entry"));
                    };
                    let kind = lex.next_token();
                    // Common writer bug: the subsection starts at 1 but begins with object 0.
                    if i == 0 && start == 1 && off == 0 && generation == 65535 {
                        start = 0;
                    }
                    let e = match kind {
                        Token::Keyword(b"n") if off > 0 => Entry::Offset {
                            offset: usize::try_from(off).unwrap_or(usize::MAX),
                            generation: u16::try_from(generation).unwrap_or(0),
                        },
                        Token::Keyword(b"n" | b"f") => Entry::Free,
                        _ => return Err(Error::Xref("bad xref entry type")),
                    };
                    if let Some(n) = start.checked_add(i) {
                        entries.push((n, e));
                    }
                }
            }
            _ => return Err(Error::Xref("expected trailer")),
        }
    }
    let dict = match Parser::new(lex.src(), lex.pos).object()? {
        Object::Dict(d) => d,
        _ => return Err(Error::Xref("trailer is not a dictionary")),
    };
    Ok(SectionData { entries, dict })
}

fn stream_section(src: &[u8], off: usize) -> Result<SectionData> {
    let (_, obj) = Parser::new(src, off).indirect(&mut |_| None)?;
    let Object::Stream(s) = obj else {
        return Err(Error::Xref("xref offset points at no xref stream"));
    };
    if s.dict.name(b"Type").is_some_and(|t| t != b"XRef") {
        return Err(Error::Xref("xref offset points at no xref stream"));
    }
    let data = decode_direct(src, &s)?;
    let entries = stream_entries(&s.dict, &data).ok_or(Error::Xref("bad xref stream"))?;
    Ok(SectionData {
        entries,
        dict: s.dict,
    })
}

pub fn stream_entries(dict: &Dict, data: &[u8]) -> Option<Vec<(u32, Entry)>> {
    let w: Vec<usize> = dict
        .get(b"W")?
        .as_array()?
        .iter()
        .map(|o| {
            o.as_int()
                .and_then(|v| usize::try_from(v).ok())
                .filter(|&v| v <= 8)
        })
        .collect::<Option<_>>()?;
    let [w0, w1, w2] = w[..] else { return None };
    let row = w0 + w1 + w2;
    if row == 0 {
        return None;
    }
    let size = dict.int(b"Size").unwrap_or(0);
    let index: Vec<i64> = match dict.get(b"Index").and_then(Object::as_array) {
        Some(a) => a.iter().filter_map(Object::as_int).collect(),
        None => vec![0, size],
    };
    let field = |b: &[u8]| b.iter().fold(0u64, |v, &x| v << 8 | u64::from(x));
    let mut rows = data.chunks_exact(row);
    let mut out = Vec::new();
    for pair in index.as_chunks::<2>().0 {
        let (Ok(start), Ok(count)) = (u32::try_from(pair[0]), u32::try_from(pair[1])) else {
            continue;
        };
        for i in 0..count {
            let Some(r) = rows.next() else {
                return Some(out);
            };
            let (a, rest) = r.split_at(w0);
            let (b, c) = rest.split_at(w1);
            let t = if w0 == 0 { 1 } else { field(a) };
            let (b, c) = (field(b), field(c));
            let e = match t {
                0 => Entry::Free,
                1 if b > 0 => Entry::Offset {
                    offset: usize::try_from(b).unwrap_or(usize::MAX),
                    generation: u16::try_from(c).unwrap_or(0),
                },
                2 => Entry::InStream {
                    stream: u32::try_from(b).unwrap_or(u32::MAX),
                    index: u32::try_from(c).unwrap_or(u32::MAX),
                },
                // Unknown types are references to the null object.
                _ => Entry::Free,
            };
            if let Some(n) = start.checked_add(i) {
                out.push((n, e));
            }
        }
    }
    Some(out)
}
