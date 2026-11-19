//! The predefined CMaps (§9.7.5.2) and the CID to Unicode tables of the Adobe character
//! collections (§9.10.2), from Adobe's cmap-resources. Tables are DEFLATE-compressed range
//! lists (see `tools/gen_cmaps.py`), decoded on first use and kept for the process.

use std::sync::{Arc, OnceLock};

use super::data::{CMAPS, ENTRIES, ENTRY_COUNT, TABLES, UNICODE, UNICODE_COUNT};
use super::{CMap, CidSpan, Codespace};

/// How the codes of a CMap encode Unicode, for the Unicode-keyed predefined CMaps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Form {
    /// Codes are not Unicode.
    #[default]
    Bytes,
    Ucs2,
    Utf16,
    Utf8,
    Utf32,
}

impl Form {
    /// The character a code stands for.
    pub fn char(self, code: u32) -> Option<char> {
        match self {
            Form::Bytes => None,
            Form::Ucs2 | Form::Utf32 => char::from_u32(code),
            Form::Utf16 if code > 0xFFFF => char::decode_utf16([(code >> 16) as u16, code as u16])
                .next()?
                .ok(),
            Form::Utf16 => char::from_u32(code),
            Form::Utf8 => {
                let b = code.to_be_bytes();
                let skip = b.iter().take(3).take_while(|&&b| b == 0).count();
                std::str::from_utf8(&b[skip..]).ok()?.chars().next()
            }
        }
    }

    /// The code of a character.
    fn code(self, c: u32) -> u32 {
        let Some(ch) = char::from_u32(c) else {
            return c;
        };
        match self {
            Form::Utf16 => {
                let mut u = [0; 2];
                match ch.encode_utf16(&mut u) {
                    [a, b] => u32::from(*a) << 16 | u32::from(*b),
                    _ => c,
                }
            }
            Form::Utf8 => {
                let mut b = [0; 4];
                ch.encode_utf8(&mut b)
                    .bytes()
                    .fold(0, |a, b| a << 8 | u32::from(b))
            }
            _ => c,
        }
    }

    /// The last character from `c` on whose codes are consecutive.
    fn block_end(self, c: u32) -> u32 {
        match self {
            Form::Utf8 if c >= 0x80 => c | 0x3F,
            Form::Utf8 => 0x7F,
            Form::Utf16 if c >= 0x1_0000 => c | 0x3FF,
            Form::Utf16 => 0xFFFF,
            _ => u32::MAX,
        }
    }
}

/// A character collection (`/CIDSystemInfo` `/Ordering` of registry Adobe).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Ordering {
    Japan1,
    GB1,
    CNS1,
    Korea1,
    KR,
    #[default]
    Other,
}

impl Ordering {
    pub fn from_name(n: &[u8]) -> Self {
        match n {
            b"Japan1" => Ordering::Japan1,
            b"GB1" => Ordering::GB1,
            b"CNS1" => Ordering::CNS1,
            b"Korea1" => Ordering::Korea1,
            b"KR" => Ordering::KR,
            _ => Ordering::Other,
        }
    }

    /// Appends the Unicode text of a CID of the collection; false if unknown.
    pub fn unicode(self, cid: u32, out: &mut String) -> bool {
        static CACHE: [OnceLock<CidUnicode>; UNICODE_COUNT] =
            [const { OnceLock::new() }; UNICODE_COUNT];
        let Some(i) = UNICODE.iter().position(|(o, _)| *o == self) else {
            return false;
        };
        CACHE[i]
            .get_or_init(|| CidUnicode::decode(UNICODE[i].1))
            .get(cid, out)
    }
}

/// A predefined CMap of the index, generated.
#[derive(Debug)]
pub(super) struct Entry {
    pub name: &'static str,
    /// The entry named by `usecmap`, or `u16::MAX`.
    pub parent: u16,
    pub vertical: bool,
    pub form: Form,
    pub ordering: Ordering,
    pub codespace: &'static [Codespace],
    pub table: u16,
}

/// Nested `usecmap` beyond this is ignored.
const MAX_DEPTH: u32 = 4;

/// A predefined CMap by name, decoded once per process.
pub fn load(name: &[u8]) -> Option<Arc<CMap>> {
    let i = ENTRIES
        .binary_search_by(|e| e.name.as_bytes().cmp(name))
        .ok()?;
    Some(entry(i, 0))
}

fn entry(i: usize, depth: u32) -> Arc<CMap> {
    static CACHE: [OnceLock<Arc<CMap>>; ENTRY_COUNT] = [const { OnceLock::new() }; ENTRY_COUNT];
    Arc::clone(CACHE[i].get_or_init(|| Arc::new(build(i, depth))))
}

fn build(i: usize, depth: u32) -> CMap {
    let e = &ENTRIES[i];
    let mut m = CMap {
        name: Some(e.name.as_bytes().into()),
        vertical: e.vertical,
        form: e.form,
        ordering: e.ordering,
        codespace: e.codespace.to_vec(),
        ..CMap::default()
    };
    let mut r = Reader::table(e.table);
    let (mut end, mut next) = (0u32, 0u32);
    for _ in 0..r.count() {
        let lo = end.wrapping_add(r.varint());
        let hi = lo.wrapping_add(r.varint());
        let cid = next.wrapping_add_signed(r.zigzag());
        (end, next) = (hi.wrapping_add(1), cid.wrapping_add(hi - lo + 1));
        push_chars(&mut m.cids, e.form, lo, hi, cid);
    }
    let parent = usize::from(e.parent);
    if parent < ENTRY_COUNT && parent != i && depth < MAX_DEPTH {
        m.inherit(&entry(parent, depth + 1));
    }
    m
}

/// Adds the spans of the codes of characters `lo..=hi` mapped to consecutive CIDs.
fn push_chars(out: &mut Vec<CidSpan>, form: Form, mut lo: u32, hi: u32, mut cid: u32) {
    while lo <= hi {
        let end = form.block_end(lo).min(hi);
        out.push(CidSpan {
            lo: form.code(lo),
            hi: form.code(end),
            cid,
        });
        cid = cid.wrapping_add(end - lo + 1);
        let Some(next) = end.checked_add(1) else {
            break;
        };
        lo = next;
    }
}

/// CID to Unicode of a character collection.
#[derive(Debug, Default)]
struct CidUnicode {
    /// CIDs `lo..=hi` map to consecutive characters from the third.
    spans: Vec<(u32, u32, char)>,
    /// CIDs mapped to several characters, sorted.
    sequences: Vec<(u32, Box<str>)>,
}

impl CidUnicode {
    fn decode(table: u16) -> Self {
        let mut u = CidUnicode::default();
        let mut r = Reader::table(table);
        let (mut end, mut next) = (0u32, 0u32);
        for _ in 0..r.count() {
            let lo = end.wrapping_add(r.varint());
            let hi = lo.wrapping_add(r.varint());
            end = hi.wrapping_add(1);
            match r.varint() {
                1 => {
                    let c = next.wrapping_add_signed(r.zigzag());
                    next = c.wrapping_add(hi - lo + 1);
                    if let Some(c) = char::from_u32(c) {
                        u.spans.push((lo, hi, c));
                    }
                }
                k => {
                    let s = (0..k)
                        .map(|_| char::from_u32(r.varint()).unwrap_or('\u{FFFD}'))
                        .collect();
                    u.sequences.push((lo, s));
                }
            }
        }
        u
    }

    fn get(&self, cid: u32, out: &mut String) -> bool {
        if let Ok(i) = self.sequences.binary_search_by_key(&cid, |s| s.0) {
            out.push_str(&self.sequences[i].1);
            return true;
        }
        let i = self.spans.partition_point(|s| s.1 < cid);
        match self.spans.get(i) {
            Some(&(lo, _, c)) if lo <= cid => {
                match char::from_u32(u32::from(c) + (cid - lo)) {
                    Some(c) => out.push(c),
                    None => return false,
                }
                true
            }
            _ => false,
        }
    }
}

/// Reads a decompressed table.
struct Reader {
    data: Vec<u8>,
    pos: usize,
}

impl Reader {
    fn table(i: u16) -> Self {
        let (a, b) = TABLES[usize::from(i)];
        let data =
            pagebara_flate::raw_to_vec(&CMAPS[a as usize..b as usize], 1 << 24).unwrap_or_default();
        Reader { data, pos: 0 }
    }

    /// The next varint; 0 past the end.
    fn varint(&mut self) -> u32 {
        let mut v = 0u32;
        for shift in (0..35).step_by(7) {
            let Some(&b) = self.data.get(self.pos) else {
                return v;
            };
            self.pos += 1;
            v |= u32::from(b & 0x7F) << shift;
            if b < 0x80 {
                break;
            }
        }
        v
    }

    /// A record count, bounded by the data left.
    fn count(&mut self) -> u32 {
        let n = self.varint();
        n.min(self.data.len() as u32)
    }

    fn zigzag(&mut self) -> i32 {
        let v = self.varint();
        (v >> 1) as i32 ^ -((v & 1) as i32)
    }
}
