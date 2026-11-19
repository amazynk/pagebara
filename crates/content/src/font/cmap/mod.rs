//! CMaps (§9.7.5, §9.10.3): codespace ranges, code to CID and code to Unicode mappings,
//! parsed from embedded streams and ToUnicode streams, plus the predefined CMaps.

mod data;
pub mod predefined;

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use crate::lex::{Lexer, Operand, Token};
pub use predefined::{Form, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Codespace {
    pub len: u8,
    pub lo: [u8; 4],
    pub hi: [u8; 4],
}

impl Codespace {
    fn new(lo: &[u8], hi: &[u8]) -> Option<Self> {
        if lo.is_empty() || lo.len() > 4 || lo.len() != hi.len() {
            return None;
        }
        let mut c = Codespace {
            len: lo.len() as u8,
            lo: [0; 4],
            hi: [0; 4],
        };
        c.lo[..lo.len()].copy_from_slice(lo);
        c.hi[..hi.len()].copy_from_slice(hi);
        Some(c)
    }

    pub(crate) const fn of(len: u8, lo: [u8; 4], hi: [u8; 4]) -> Self {
        Codespace { len, lo, hi }
    }

    #[inline]
    fn matches(&self, b: &[u8]) -> bool {
        b.len() == usize::from(self.len)
            && b.iter()
                .zip(self.lo.iter().zip(&self.hi))
                .all(|(&b, (&lo, &hi))| lo <= b && b <= hi)
    }
}

/// Codes `lo..=hi` map to consecutive CIDs from `cid`, or all to `cid & !SAME_CID` if
/// [`SAME_CID`] is set (`notdefrange`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CidSpan {
    lo: u32,
    hi: u32,
    cid: u32,
}

const SAME_CID: u32 = 1 << 31;

impl CidSpan {
    fn get(&self, code: u32) -> u32 {
        match self.cid & SAME_CID {
            0 => self.cid.wrapping_add(code - self.lo),
            _ => self.cid & !SAME_CID,
        }
    }
}

/// Codes `lo..=hi` map to the UTF-16 text `units[dst..dst + len]`, its last unit
/// incremented by `code - start`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct UniSpan {
    lo: u32,
    hi: u32,
    start: u32,
    dst: u32,
    len: u16,
}

/// A code range of a mapping, cut to a sub-range when later definitions override parts.
trait Span: Copy {
    fn range(&self) -> (u32, u32);
    fn cut(&self, lo: u32, hi: u32) -> Self;
}

impl Span for CidSpan {
    fn range(&self) -> (u32, u32) {
        (self.lo, self.hi)
    }

    fn cut(&self, lo: u32, hi: u32) -> Self {
        CidSpan {
            lo,
            hi,
            cid: self.get(lo) | self.cid & SAME_CID,
        }
    }
}

impl Span for UniSpan {
    fn range(&self) -> (u32, u32) {
        (self.lo, self.hi)
    }

    fn cut(&self, lo: u32, hi: u32) -> Self {
        UniSpan { lo, hi, ..*self }
    }
}

#[derive(Clone, Debug, Default)]
pub struct CMap {
    pub name: Option<Box<[u8]>>,
    /// `/WMode 1`.
    pub vertical: bool,
    /// A CMap named by `usecmap`.
    pub use_cmap: Option<Box<[u8]>>,
    /// How codes encode Unicode (predefined `Uni…` CMaps).
    pub form: Form,
    /// The character collection of the CIDs (predefined CMaps).
    pub ordering: Ordering,
    codespace: Vec<Codespace>,
    /// Sorted and disjoint after [`CMap::finish`].
    cids: Vec<CidSpan>,
    unicode: Vec<UniSpan>,
    units: Vec<u16>,
    /// `notdefrange` mappings while parsing; they yield to other mappings.
    notdef: Vec<CidSpan>,
    /// Codes are CIDs (Identity-H/V).
    identity: bool,
}

impl CMap {
    pub fn parse(src: &[u8]) -> CMap {
        let mut m = CMap::default();
        let mut lex = Lexer::new(src);
        let mut stack: Vec<Value> = Vec::new();
        let mut array: Option<Vec<Value>> = None;
        loop {
            let v = match lex.next_token() {
                Token::Eof => break,
                Token::Operand(o) => match o {
                    Operand::Number(n) => Value::Num(n),
                    Operand::Name(n) => Value::Name(n.to_vec()),
                    Operand::String { .. } => {
                        let mut s = Vec::new();
                        o.string_into(&mut s);
                        Value::Str(s)
                    }
                    _ => continue,
                },
                Token::ArrayOpen => {
                    array = Some(Vec::new());
                    continue;
                }
                Token::ArrayClose => Value::Array(array.take().unwrap_or_default()),
                Token::DictOpen => continue,
                Token::Keyword(k) => {
                    m.keyword(k, &mut stack);
                    continue;
                }
            };
            match &mut array {
                Some(a) if !matches!(v, Value::Array(_)) => a.push(v),
                _ => stack.push(v),
            }
        }
        m.finish();
        m
    }

    fn keyword(&mut self, k: &[u8], stack: &mut Vec<Value>) {
        match k {
            b"endcodespacerange" => {
                for p in stack.as_chunks::<2>().0 {
                    if let (Value::Str(lo), Value::Str(hi)) = (&p[0], &p[1])
                        && let Some(c) = Codespace::new(lo, hi)
                    {
                        self.codespace.push(c);
                    }
                }
            }
            b"endcidchar" => {
                for p in stack.as_chunks::<2>().0 {
                    if let (Value::Str(c), Value::Num(cid)) = (&p[0], &p[1]) {
                        let c = code(c);
                        self.cids.push(CidSpan {
                            lo: c,
                            hi: c,
                            cid: (*cid as u32).min(!SAME_CID),
                        });
                    }
                }
            }
            b"endcidrange" | b"endnotdefrange" => {
                for p in stack.as_chunks::<3>().0 {
                    if let (Value::Str(lo), Value::Str(hi), Value::Num(cid)) = (&p[0], &p[1], &p[2])
                    {
                        let (lo, hi) = (code(lo), code(hi));
                        if lo <= hi {
                            let cid = (*cid as u32).min(!SAME_CID);
                            match k {
                                b"endnotdefrange" => self.notdef.push(CidSpan {
                                    lo,
                                    hi,
                                    cid: cid | SAME_CID,
                                }),
                                _ => self.cids.push(CidSpan { lo, hi, cid }),
                            }
                        }
                    }
                }
            }
            b"endbfchar" => {
                for p in stack.as_chunks::<2>().0 {
                    let c = match &p[0] {
                        Value::Str(c) => code(c),
                        _ => continue,
                    };
                    match &p[1] {
                        Value::Str(dst) => self.push_unicode(c, c, dst),
                        Value::Name(n) => {
                            let mut s = String::new();
                            if super::names::name_to_unicode(n, &mut s) {
                                let b: Vec<u8> =
                                    s.encode_utf16().flat_map(u16::to_be_bytes).collect();
                                self.push_unicode(c, c, &b);
                            }
                        }
                        _ => {}
                    }
                }
            }
            b"endbfrange" => {
                for p in stack.as_chunks::<3>().0 {
                    let (Value::Str(lo), Value::Str(hi)) = (&p[0], &p[1]) else {
                        continue;
                    };
                    let (lo, hi) = (code(lo), code(hi));
                    if lo > hi {
                        continue;
                    }
                    match &p[2] {
                        Value::Str(dst) => self.push_unicode(lo, hi, dst),
                        Value::Array(a) => {
                            for (i, d) in a.iter().enumerate().take((hi - lo) as usize + 1) {
                                if let Value::Str(d) = d {
                                    let c = lo + i as u32;
                                    self.push_unicode(c, c, d);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            b"usecmap" => {
                if let Some(Value::Name(n)) = stack.last() {
                    self.use_cmap = Some(n.clone().into());
                }
            }
            b"def" => {
                if let [.., Value::Name(k), v] = stack.as_slice() {
                    match (k.as_slice(), v) {
                        (b"WMode", Value::Num(n)) => self.vertical = *n == 1.0,
                        (b"CMapName", Value::Name(n)) => self.name = Some(n.clone().into()),
                        (b"Ordering", Value::Str(o)) => self.ordering = Ordering::from_name(o),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        stack.clear();
    }

    fn push_unicode(&mut self, lo: u32, hi: u32, dst: &[u8]) {
        let dst = &dst[..dst.len().min(512)];
        let start = self.units.len() as u32;
        self.units.extend(
            dst.chunks(2)
                .map(|c| u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)])),
        );
        let len = (self.units.len() as u32 - start) as u16;
        self.unicode.push(UniSpan {
            lo,
            hi,
            start: lo,
            dst: start,
            len,
        });
    }

    fn finish(&mut self) {
        if !self.notdef.is_empty() {
            let mut cids = std::mem::take(&mut self.notdef);
            cids.append(&mut self.cids);
            self.cids = cids;
        }
        disjoint(&mut self.cids);
        disjoint(&mut self.unicode);
    }

    /// Adds the mappings of `parent` (from `usecmap`) underneath this CMap's own.
    pub fn inherit(&mut self, parent: &CMap) {
        self.codespace.extend_from_slice(&parent.codespace);
        self.identity |= parent.identity && self.cids.is_empty();
        let mut cids = parent.cids.clone();
        cids.append(&mut self.cids);
        self.cids = cids;
        let off = self.units.len() as u32;
        self.units.extend_from_slice(&parent.units);
        let mut uni: Vec<UniSpan> = parent
            .unicode
            .iter()
            .map(|s| UniSpan {
                dst: s.dst + off,
                ..*s
            })
            .collect();
        uni.append(&mut self.unicode);
        self.unicode = uni;
        if self.form == Form::Bytes {
            self.form = parent.form;
        }
        if self.ordering == Ordering::Other {
            self.ordering = parent.ordering;
        }
        self.vertical |= parent.vertical;
        self.finish();
    }

    /// A predefined CMap by name (§9.7.5.2), decoded once per process.
    pub fn predefined(name: &[u8]) -> Option<Arc<CMap>> {
        static IDENTITY: OnceLock<[Arc<CMap>; 2]> = OnceLock::new();
        let vertical = match name {
            b"Identity-H" => false,
            b"Identity-V" => true,
            _ => return predefined::load(name),
        };
        let pair = IDENTITY.get_or_init(|| {
            [&b"Identity-H"[..], b"Identity-V"].map(|n| {
                Arc::new(CMap {
                    name: Some(n.into()),
                    vertical: n.ends_with(b"V"),
                    identity: true,
                    codespace: vec![Codespace::of(2, [0; 4], [0xFF, 0xFF, 0, 0])],
                    ..CMap::default()
                })
            })
        });
        Some(Arc::clone(&pair[usize::from(vertical)]))
    }

    pub fn is_identity(&self) -> bool {
        self.identity
    }

    pub fn has_codespace(&self) -> bool {
        !self.codespace.is_empty()
    }

    /// Splits the next code off `bytes` (non-empty), returning it and its length.
    pub fn next_code(&self, bytes: &[u8]) -> (u32, usize) {
        let mut min = 4;
        for n in 1..=bytes.len().min(4) {
            let b = &bytes[..n];
            for c in &self.codespace {
                min = min.min(usize::from(c.len));
                if c.matches(b) {
                    return (code(b), n);
                }
            }
        }
        // No codespace matches: consume the shortest length so 2-byte text stays aligned.
        let n = if self.codespace.is_empty() { 2 } else { min };
        let n = n.min(bytes.len()).max(1);
        (code(&bytes[..n]), n)
    }

    /// The CID of a code; `None` if unmapped.
    pub fn cid(&self, code: u32) -> Option<u32> {
        if self.identity {
            return Some(code);
        }
        lookup(&self.cids, code).map(|s| s.get(code))
    }

    /// Appends the Unicode text of a code; false if unmapped.
    pub fn unicode(&self, code: u32, out: &mut String) -> bool {
        if self.form != Form::Bytes {
            let c = self.form.char(code);
            out.extend(c);
            return c.is_some();
        }
        let Some(s) = lookup(&self.unicode, code) else {
            return false;
        };
        let units = &self.units[s.dst as usize..(s.dst + u32::from(s.len)) as usize];
        let Some((&last, head)) = units.split_last() else {
            return false;
        };
        let last = last.wrapping_add((code - s.start) as u16);
        if head.is_empty()
            && let Some(c) = char::from_u32(u32::from(last))
        {
            out.push(c);
            return true;
        }
        out.extend(
            char::decode_utf16(head.iter().copied().chain([last])).map(|c| c.unwrap_or('\u{FFFD}')),
        );
        true
    }

    pub fn is_empty(&self) -> bool {
        self.unicode.is_empty()
            && self.cids.is_empty()
            && !self.identity
            && self.form == Form::Bytes
    }
}

fn code(b: &[u8]) -> u32 {
    b.iter().take(4).fold(0, |a, &b| a << 8 | u32::from(b))
}

/// Sorts spans given in definition order into disjoint ranges, later definitions winning.
fn disjoint<T: Span>(spans: &mut Vec<T>) {
    let ordered = |s: &[T]| s.windows(2).all(|w| w[0].range().1 < w[1].range().0);
    if ordered(spans) {
        return;
    }
    let mut sorted = spans.clone();
    sorted.sort_by_key(|s| s.range().0);
    if ordered(&sorted) {
        *spans = sorted;
        return;
    }
    // Paint from the last definition back, keeping what is not covered yet. Covered ranges
    // a span touches merge into one, so each is visited once.
    let mut covered: BTreeMap<u32, u32> = BTreeMap::new();
    let mut out = Vec::with_capacity(spans.len());
    for s in spans.iter().rev() {
        let (lo, hi) = s.range();
        let mut over: Vec<(u32, u32)> = covered
            .range(..=hi)
            .rev()
            .take_while(|(_, e)| **e >= lo)
            .map(|(a, b)| (*a, *b))
            .collect();
        over.reverse();
        let mut at = Some(lo);
        for &(a, b) in &over {
            if let Some(c) = at
                && a > c
            {
                out.push(s.cut(c, a - 1));
            }
            at = at.and(b.checked_add(1)).filter(|&n| n <= hi);
            covered.remove(&a);
        }
        if let Some(c) = at {
            out.push(s.cut(c, hi));
        }
        let start = over.first().map_or(lo, |o| o.0.min(lo));
        let end = over.last().map_or(hi, |o| o.1.max(hi));
        covered.insert(start, end);
    }
    out.sort_by_key(|s| s.range().0);
    *spans = out;
}

/// The span holding `code` among sorted disjoint spans.
fn lookup<T: Span>(spans: &[T], code: u32) -> Option<&T> {
    let i = spans.partition_point(|s| s.range().1 < code);
    spans.get(i).filter(|s| s.range().0 <= code)
}

#[derive(Debug)]
enum Value {
    Num(f32),
    Str(Vec<u8>),
    Name(Vec<u8>),
    Array(Vec<Value>),
}

#[cfg(test)]
mod tests {
    use super::*;

    const TO_UNICODE: &[u8] = b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap
/CMapName /Adobe-Identity-UCS def /CMapType 2 def
1 begincodespacerange <0000> <FFFF> endcodespacerange
2 beginbfchar <0003> <0020> <0024> <00660069> endbfchar
2 beginbfrange <0010> <0012> <0041> <0020> <0021> [<00E9> <D83DDE00>] endbfrange
endcmap CMapName currentdict /CMap defineresource pop end end";

    fn uni(m: &CMap, c: u32) -> Option<String> {
        let mut s = String::new();
        m.unicode(c, &mut s).then_some(s)
    }

    #[test]
    fn to_unicode() {
        let m = CMap::parse(TO_UNICODE);
        assert_eq!(m.name.as_deref(), Some(&b"Adobe-Identity-UCS"[..]));
        assert_eq!(uni(&m, 3).as_deref(), Some(" "));
        assert_eq!(uni(&m, 0x24).as_deref(), Some("fi"));
        assert_eq!(uni(&m, 0x11).as_deref(), Some("B"));
        assert_eq!(uni(&m, 0x12).as_deref(), Some("C"));
        assert_eq!(uni(&m, 0x20).as_deref(), Some("é"));
        assert_eq!(uni(&m, 0x21).as_deref(), Some("😀"));
        assert_eq!(uni(&m, 0x13), None);
        assert_eq!(m.next_code(&[0, 3, 0, 4]), (3, 2));
    }

    #[test]
    fn cid_cmap_with_mixed_codespace() {
        let m = CMap::parse(
            b"/WMode 1 def 2 begincodespacerange <00> <80> <8140> <9FFC> endcodespacerange
             1 begincidrange <8140> <817E> 633 endcidrange 1 begincidchar <41> 34 endcidchar",
        );
        assert!(m.vertical);
        assert_eq!(m.next_code(b"A\x81\x41"), (0x41, 1));
        assert_eq!(m.next_code(b"\x81\x41"), (0x8141, 2));
        assert_eq!(m.next_code(b"\xFF\x41"), (0xFF, 1));
        assert_eq!(m.cid(0x41), Some(34));
        assert_eq!(m.cid(0x8142), Some(635));
        assert_eq!(m.cid(0x42), None);
    }

    #[test]
    fn predefined() {
        let m = CMap::predefined(b"Identity-V").unwrap();
        assert!(m.vertical && m.is_identity());
        assert_eq!(m.cid(0x1234), Some(0x1234));
        let m = CMap::predefined(b"90ms-RKSJ-H").unwrap();
        assert_eq!(m.next_code(b"\xA5"), (0xA5, 1));
        assert_eq!(m.next_code(b"\x88\x9F"), (0x889F, 2));
        let m = CMap::predefined(b"UniJIS-UTF16-H").unwrap();
        assert_eq!(uni(&m, 0x3042).as_deref(), Some("あ"));
        assert_eq!(m.next_code(b"\xD8\x3D\xDE\x00"), (0xD83DDE00, 4));
        assert_eq!(uni(&m, 0xD83DDE00).as_deref(), Some("😀"));
        assert!(CMap::predefined(b"Nonsense").is_none());
    }

    #[test]
    fn predefined_tables() {
        let h = CMap::predefined(b"90ms-RKSJ-H").unwrap();
        assert_eq!(h.ordering, Ordering::Japan1);
        assert_eq!(h.cid(0x889F), Some(1125));
        assert_eq!(h.cid(0x8141), Some(634));
        assert_eq!(h.cid(0x41), Some(264));
        // The V CMap overrides a few codes of its `usecmap` parent.
        let v = CMap::predefined(b"90ms-RKSJ-V").unwrap();
        assert!(v.vertical);
        assert_eq!(v.cid(0x8141), Some(7887));
        assert_eq!(v.cid(0x8143), Some(8268));
        assert_eq!(v.cid(0x889F), Some(1125));
        assert_eq!(v.next_code(b"\x88\x9F"), (0x889F, 2));
        // One table serves the Unicode encoding forms.
        let u16 = CMap::predefined(b"UniJIS-UTF16-H").unwrap();
        assert_eq!(u16.cid(0x3042), Some(843));
        assert_eq!(u16.cid(0xD840DC0B), Some(13839));
        let u8 = CMap::predefined(b"UniJIS-UTF8-H").unwrap();
        assert_eq!(u8.cid(0xE38182), Some(843));
        assert_eq!(u8.next_code("あ".as_bytes()), (0xE38182, 3));
        assert_eq!(uni(&u8, 0xE38182).as_deref(), Some("あ"));
        let u32 = CMap::predefined(b"UniJIS-UTF32-V").unwrap();
        assert_eq!(u32.cid(0x2000B), Some(13839));
        assert!(u32.vertical);
        let gb = CMap::predefined(b"GB-EUC-H").unwrap();
        assert_eq!((gb.cid(0xA1A1), gb.cid(0xB0A2)), (Some(96), Some(941)));
        for name in [
            &b"GBK-EUC-H"[..],
            b"GBK2K-V",
            b"ETen-B5-H",
            b"KSCms-UHC-H",
            b"UniKS-UCS2-H",
            b"UniCNS-UTF32-V",
            b"UniGB-UTF8-H",
            b"78-RKSJ-V",
        ] {
            let m = CMap::predefined(name).unwrap();
            assert!(m.has_codespace() && !m.cids.is_empty(), "{name:?}");
        }
        // Shared once decoded.
        assert!(Arc::ptr_eq(&h, &CMap::predefined(b"90ms-RKSJ-H").unwrap()));
    }

    #[test]
    fn collection_unicode() {
        let s = |o: Ordering, cid| {
            let mut s = String::new();
            o.unicode(cid, &mut s).then_some(s)
        };
        assert_eq!(s(Ordering::Japan1, 843).as_deref(), Some("あ"));
        assert_eq!(s(Ordering::Japan1, 0x20E).as_deref(), Some("あ"));
        assert_eq!(s(Ordering::Japan1, 34).as_deref(), Some("A"));
        assert_eq!(s(Ordering::GB1, 940).as_deref(), Some("啊"));
        assert_eq!(s(Ordering::Korea1, 34).as_deref(), Some("A"));
        assert_eq!(s(Ordering::CNS1, 34).as_deref(), Some("A"));
        assert_eq!(s(Ordering::Other, 34), None);
        assert_eq!(s(Ordering::Japan1, 999_999), None);
    }

    #[test]
    fn overlapping_definitions() {
        // Later definitions win; notdef ranges yield to everything.
        let m = CMap::parse(
            b"1 begincodespacerange <00> <FF> endcodespacerange
             1 beginnotdefrange <00> <FF> 1 endnotdefrange
             1 begincidrange <20> <7F> 100 endcidrange
             1 begincidchar <41> 7 endcidchar
             1 begincidrange <40> <42> 50 endcidrange
             1 begincidchar <42> 9 endcidchar",
        );
        let cids: Vec<_> = [0x10, 0x20, 0x3F, 0x40, 0x41, 0x42, 0x43, 0x80]
            .map(|c| m.cid(c).unwrap())
            .into();
        assert_eq!(cids, [1, 100, 131, 50, 51, 9, 135, 1]);
        // Many overrides inside one range: each still wins, the range covers the rest.
        let mut src = b"1 beginbfrange <0000> <00FF> <0041> endbfrange 40 beginbfchar".to_vec();
        for c in 0..40 {
            src.extend(format!(" <{:04X}> <0030>", c * 2).bytes());
        }
        src.extend(b" endbfchar");
        let m2 = CMap::parse(&src);
        assert_eq!(uni(&m2, 78).as_deref(), Some("0"));
        assert_eq!(uni(&m2, 79).as_deref(), Some("\u{90}"));
        assert_eq!(uni(&m2, 0x80).as_deref(), Some("\u{C1}"));
        let mut p = CMap::parse(b"1 begincidrange <41> <41> 5 endcidrange");
        p.inherit(&m);
        assert_eq!(
            (p.cid(0x40), p.cid(0x41), p.cid(0x42)),
            (Some(50), Some(5), Some(9))
        );
    }
}
