//! A lenient BER reader (signatures in the wild use indefinite lengths and unsorted sets,
//! and hashes must be taken over the bytes exactly as received) and a small DER writer.

use std::borrow::Cow;

use const_oid::ObjectIdentifier;

pub const BOOLEAN: u8 = 0x01;
pub const INTEGER: u8 = 0x02;
pub const BIT_STRING: u8 = 0x03;
pub const OCTET_STRING: u8 = 0x04;
pub const NULL: u8 = 0x05;
pub const OID: u8 = 0x06;
pub const ENUMERATED: u8 = 0x0A;
pub const UTF8_STRING: u8 = 0x0C;
pub const PRINTABLE_STRING: u8 = 0x13;
pub const IA5_STRING: u8 = 0x16;
pub const UTC_TIME: u8 = 0x17;
pub const GENERALIZED_TIME: u8 = 0x18;
pub const BMP_STRING: u8 = 0x1E;
pub const SEQUENCE: u8 = 0x30;
pub const SET: u8 = 0x31;

/// Context-specific constructed tag `[n]`.
pub const fn ctx(n: u8) -> u8 {
    0xA0 | n
}

/// Context-specific primitive tag `[n]`.
pub const fn ctx_prim(n: u8) -> u8 {
    0x80 | n
}

/// Nesting limit for indefinite-length elements.
const MAX_DEPTH: u32 = 64;

/// One element: its tag byte, contents and whole encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tlv<'a> {
    pub tag: u8,
    /// The contents (without the end-of-contents marker of an indefinite length).
    pub value: &'a [u8],
    /// Header, contents and any end-of-contents marker, as received.
    pub raw: &'a [u8],
}

/// Reads one element from the start of `input`, returning it and the rest.
pub fn parse(input: &[u8]) -> Option<(Tlv<'_>, &[u8])> {
    parse_at(input, 0)
}

fn parse_at(input: &[u8], depth: u32) -> Option<(Tlv<'_>, &[u8])> {
    if depth > MAX_DEPTH {
        return None;
    }
    let tag = *input.first()?;
    // Multi-byte tag numbers do not occur in the structures read here.
    if tag & 0x1F == 0x1F {
        return None;
    }
    let first = *input.get(1)?;
    let (len, header) = match first {
        0x80 => {
            if tag & 0x20 == 0 {
                return None;
            }
            // Indefinite: children until an end-of-contents marker.
            let mut rest = &input[2..];
            loop {
                if rest.starts_with(&[0, 0]) {
                    let value_len = input.len() - 2 - rest.len();
                    let value = &input[2..2 + value_len];
                    let total = 2 + value_len + 2;
                    let tlv = Tlv {
                        tag,
                        value,
                        raw: &input[..total],
                    };
                    return Some((tlv, &input[total..]));
                }
                rest = parse_at(rest, depth + 1)?.1;
            }
        }
        n if n < 0x80 => (usize::from(n), 2),
        n => {
            let count = usize::from(n & 0x7F);
            if count > 8 {
                return None;
            }
            let bytes = input.get(2..2 + count)?;
            let len = bytes.iter().try_fold(0usize, |a, &b| {
                a.checked_mul(256)?.checked_add(usize::from(b))
            })?;
            (len, 2 + count)
        }
    };
    let end = header.checked_add(len)?;
    let raw = input.get(..end)?;
    Some((
        Tlv {
            tag,
            value: &raw[header..],
            raw,
        },
        &input[end..],
    ))
}

/// Reads consecutive elements.
#[derive(Clone, Debug)]
pub struct Reader<'a> {
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Reader { rest: b }
    }

    pub fn peek_tag(&self) -> Option<u8> {
        self.rest.first().copied()
    }

    /// The next element, if it has tag `tag`.
    pub fn tagged(&mut self, tag: u8) -> Option<Tlv<'a>> {
        if self.peek_tag() != Some(tag) {
            return None;
        }
        self.next()
    }
}

impl<'a> Iterator for Reader<'a> {
    type Item = Tlv<'a>;

    fn next(&mut self) -> Option<Tlv<'a>> {
        let (t, rest) = parse(self.rest)?;
        self.rest = rest;
        Some(t)
    }
}

impl<'a> Tlv<'a> {
    /// The children of a constructed element.
    pub fn reader(&self) -> Reader<'a> {
        Reader::new(self.value)
    }

    /// The contents of an octet (or bit) string, joining the segments of a constructed one.
    pub fn octets(&self) -> Cow<'a, [u8]> {
        if self.tag & 0x20 == 0 {
            return Cow::Borrowed(self.value);
        }
        let mut out = Vec::new();
        for c in self.reader() {
            out.extend_from_slice(&c.octets());
        }
        Cow::Owned(out)
    }

    pub fn oid(&self) -> Option<ObjectIdentifier> {
        if self.tag != OID {
            return None;
        }
        ObjectIdentifier::from_bytes(self.value).ok()
    }

    /// A non-negative INTEGER as big-endian bytes without leading zeros.
    pub fn uint(&self) -> &'a [u8] {
        let mut v = self.value;
        while v.len() > 1 && v[0] == 0 {
            v = &v[1..];
        }
        v
    }

    pub fn small_int(&self) -> Option<i64> {
        if !matches!(self.tag, INTEGER | ENUMERATED)
            || self.value.is_empty()
            || self.value.len() > 8
        {
            return None;
        }
        let neg = self.value[0] & 0x80 != 0;
        let mut v: i64 = if neg { -1 } else { 0 };
        for &b in self.value {
            v = (v << 8) | i64::from(b);
        }
        Some(v)
    }

    /// A UTCTime or GeneralizedTime as Unix seconds.
    pub fn time(&self) -> Option<i64> {
        parse_time(self.tag, self.value)
    }

    /// A character string as text.
    pub fn text(&self) -> Option<String> {
        match self.tag {
            BMP_STRING => {
                let u: Vec<u16> = self
                    .value
                    .chunks(2)
                    .map(|c| u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)]))
                    .collect();
                Some(String::from_utf16_lossy(&u))
            }
            0x0C | 0x12 | 0x13 | 0x14 | 0x16 | 0x1A => {
                Some(String::from_utf8_lossy(self.value).into_owned())
            }
            _ => None,
        }
    }

    /// The DER bytes with a different tag (e.g. `[0]` IMPLICIT signed attributes hashed
    /// as a SET).
    pub fn retagged(&self, tag: u8) -> Vec<u8> {
        let mut v = self.raw.to_vec();
        v[0] = tag;
        v
    }
}

// Writing.

pub fn tlv(tag: u8, value: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(value.len() + 6);
    out.push(tag);
    let len = value.len();
    if len < 0x80 {
        out.push(len as u8);
    } else {
        let bytes = len.to_be_bytes();
        let skip = bytes.iter().take_while(|&&b| b == 0).count();
        out.push(0x80 | (bytes.len() - skip) as u8);
        out.extend_from_slice(&bytes[skip..]);
    }
    out.extend_from_slice(value);
    out
}

fn concat(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

pub fn seq(parts: &[&[u8]]) -> Vec<u8> {
    tlv(SEQUENCE, &concat(parts))
}

/// A SET OF with its elements in DER order.
pub fn set_of(mut items: Vec<Vec<u8>>) -> Vec<u8> {
    items.sort();
    tlv(SET, &items.concat())
}

pub fn explicit(n: u8, parts: &[&[u8]]) -> Vec<u8> {
    tlv(ctx(n), &concat(parts))
}

pub fn oid(o: &ObjectIdentifier) -> Vec<u8> {
    tlv(OID, o.as_bytes())
}

/// A non-negative INTEGER from big-endian bytes.
pub fn uint(bytes: &[u8]) -> Vec<u8> {
    let mut v = bytes;
    while v.len() > 1 && v[0] == 0 {
        v = &v[1..];
    }
    let mut c = Vec::with_capacity(v.len() + 1);
    if v.is_empty() || v[0] & 0x80 != 0 {
        c.push(0);
    }
    c.extend_from_slice(v);
    tlv(INTEGER, &c)
}

pub fn int(i: u64) -> Vec<u8> {
    uint(&i.to_be_bytes())
}

pub fn octets(b: &[u8]) -> Vec<u8> {
    tlv(OCTET_STRING, b)
}

pub fn bit_string(b: &[u8]) -> Vec<u8> {
    let mut c = vec![0];
    c.extend_from_slice(b);
    tlv(BIT_STRING, &c)
}

pub fn null() -> Vec<u8> {
    vec![NULL, 0]
}

pub fn boolean(b: bool) -> Vec<u8> {
    vec![BOOLEAN, 1, if b { 0xFF } else { 0 }]
}

pub fn utf8(s: &str) -> Vec<u8> {
    tlv(UTF8_STRING, s.as_bytes())
}

/// An AlgorithmIdentifier with optional encoded parameters.
pub fn algorithm(o: &ObjectIdentifier, params: Option<&[u8]>) -> Vec<u8> {
    seq(&[&oid(o), params.unwrap_or_default()])
}

pub fn generalized_time(t: i64) -> Vec<u8> {
    let (y, mo, d, h, mi, s) = civil(t);
    let text = format!("{y:04}{mo:02}{d:02}{h:02}{mi:02}{s:02}Z");
    tlv(GENERALIZED_TIME, text.as_bytes())
}

/// UTCTime for years 1950–2049, else GeneralizedTime (RFC 5280 §4.1.2.5).
pub fn x509_time(t: i64) -> Vec<u8> {
    let (y, mo, d, h, mi, s) = civil(t);
    if (1950..2050).contains(&y) {
        let text = format!("{:02}{mo:02}{d:02}{h:02}{mi:02}{s:02}Z", y % 100);
        tlv(UTC_TIME, text.as_bytes())
    } else {
        generalized_time(t)
    }
}

// Time.

fn parse_time(tag: u8, v: &[u8]) -> Option<i64> {
    let s = std::str::from_utf8(v).ok()?;
    let digits = |s: &str| -> Option<i64> {
        s.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| s.parse().ok())?
    };
    let (year, rest) = match tag {
        UTC_TIME => {
            let y = digits(s.get(..2)?)?;
            (if y < 50 { 2000 + y } else { 1900 + y }, s.get(2..)?)
        }
        GENERALIZED_TIME => (digits(s.get(..4)?)?, s.get(4..)?),
        _ => return None,
    };
    let field = |i: usize| rest.get(i..i + 2).and_then(digits);
    let (mo, d, h) = (field(0)?, field(2)?, field(4)?);
    let mi = field(6).unwrap_or(0);
    let mut i = 8;
    let sec = match field(8) {
        Some(x) => {
            i = 10;
            x
        }
        None => 0,
    };
    // Fractions are dropped.
    let mut tail = &rest[i.min(rest.len())..];
    if let Some(t) = tail.strip_prefix(['.', ',']) {
        tail = t.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    let offset = match tail.as_bytes().first() {
        None | Some(b'Z') => 0,
        Some(&c @ (b'+' | b'-')) => {
            let oh = tail.get(1..3).and_then(digits)?;
            let om = tail.get(3..5).and_then(digits).unwrap_or(0);
            let o = oh * 3600 + om * 60;
            if c == b'+' { o } else { -o }
        }
        _ => return None,
    };
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 24 || mi > 59 || sec > 60 {
        return None;
    }
    Some(days_from_civil(year, mo, d) * 86400 + h * 3600 + mi * 60 + sec - offset)
}

/// Days since 1970-01-01 (H. Hinnant's algorithm).
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Year, month, day, hour, minute, second of Unix time `t` (UTC).
pub fn civil(t: i64) -> (i64, i64, i64, i64, i64, i64) {
    let days = t.div_euclid(86400);
    let secs = t.rem_euclid(86400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d, secs / 3600, secs / 60 % 60, secs % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_lengths() {
        for n in [0, 1, 127, 128, 255, 256, 70_000] {
            let v = vec![7u8; n];
            let e = tlv(OCTET_STRING, &v);
            let (t, rest) = parse(&e).unwrap();
            assert!(rest.is_empty());
            assert_eq!(t.value.len(), n);
        }
    }

    #[test]
    fn indefinite() {
        // SEQUENCE (indefinite) { OCTET STRING (constructed, indefinite) { "ab", "c" } }
        let b = [
            0x30, 0x80, 0x24, 0x80, 0x04, 0x02, b'a', b'b', 0x04, 0x01, b'c', 0, 0, 0, 0, 0xFF,
        ];
        let (t, rest) = parse(&b).unwrap();
        assert_eq!(rest, [0xFF]);
        let inner = t.reader().next().unwrap();
        assert_eq!(&*inner.octets(), b"abc");
    }

    #[test]
    fn times() {
        let t = parse_time(UTC_TIME, b"240305133000Z").unwrap();
        assert_eq!(civil(t), (2024, 3, 5, 13, 30, 0));
        let g = parse_time(GENERALIZED_TIME, b"20240305143000.123+0100").unwrap();
        assert_eq!(g, t);
        let enc = x509_time(t);
        let (tt, _) = parse(&enc).unwrap();
        assert_eq!(tt.time(), Some(t));
        assert_eq!(civil(days_from_civil(2100, 2, 28) * 86400).2, 28);
    }

    #[test]
    fn integers() {
        assert_eq!(int(0), [2, 1, 0]);
        assert_eq!(int(128), [2, 2, 0, 128]);
        let enc = int(65537);
        let (t, _) = parse(&enc).unwrap();
        assert_eq!(t.small_int(), Some(65537));
        assert_eq!(t.uint(), [1, 0, 1]);
    }
}
