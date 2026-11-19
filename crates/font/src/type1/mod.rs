//! Type 1 fonts (PFA, PFB and PDF `FontFile` streams): eexec and charstring decryption,
//! the font dictionary, Subrs and CharStrings.

pub(crate) mod blank;
pub(crate) mod charstring;

use std::collections::HashMap;
use std::ops::Range;

use crate::data::BaseEncoding;
use crate::outline::{Matrix, NullSink, OutlineSink, Pen};

const EEXEC_KEY: u16 = 55665;
const CHARSTRING_KEY: u16 = 4330;

/// A Type 1 font. Glyph ids are CharStrings order with `.notdef` moved to 0.
#[derive(Debug)]
pub struct Type1 {
    name: Option<String>,
    matrix: Matrix,
    bbox: [f32; 4],
    encoding: Option<Box<[Option<u16>; 256]>>,
    /// Decrypted charstring and subroutine bytes.
    bytes: Vec<u8>,
    glyphs: Vec<(Box<str>, Range<usize>)>,
    subrs: Vec<Range<usize>>,
    names: HashMap<Box<[u8]>, u16>,
}

fn decrypt(data: &[u8], key: u16, skip: usize) -> Vec<u8> {
    let mut r = key;
    let mut out = Vec::with_capacity(data.len());
    for &c in data {
        out.push(c ^ (r >> 8) as u8);
        r = (u16::from(c).wrapping_add(r))
            .wrapping_mul(52845)
            .wrapping_add(22719);
    }
    out.drain(..skip.min(out.len()));
    out
}

/// Joins PFB segments; other data is returned as is.
fn unwrap_pfb(data: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    if data.first() != Some(&0x80) {
        return data.into();
    }
    let mut out = Vec::with_capacity(data.len());
    let mut p = 0;
    while let Some(&[0x80, kind]) = data.get(p..p + 2) {
        if kind == 3 {
            break;
        }
        let Some(len) = data.get(p + 2..p + 6) else {
            break;
        };
        let len = u32::from_le_bytes([len[0], len[1], len[2], len[3]]) as usize;
        let s = p + 6;
        let e = s.saturating_add(len).min(data.len());
        out.extend_from_slice(&data[s..e]);
        p = e;
    }
    out.into()
}

fn is_hex(b: u8) -> bool {
    b.is_ascii_hexdigit()
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Splits a font program into its clear text and decrypted private part.
fn split(data: &[u8]) -> Option<(&[u8], Vec<u8>)> {
    let pos = find(data, b"eexec")?;
    let clear = &data[..pos];
    let mut p = pos + 5;
    while data.get(p).is_some_and(|b| b" \t\r\n".contains(b)) {
        p += 1;
    }
    let rest = data.get(p..)?;
    let hex = rest.len() >= 4 && rest[..4].iter().all(|&b| is_hex(b));
    let enc: Vec<u8> = if hex {
        let digits = rest
            .iter()
            .filter(|b| !b.is_ascii_whitespace())
            .take_while(|&&b| is_hex(b));
        let mut out = Vec::with_capacity(rest.len() / 2);
        let mut hi: Option<u8> = None;
        for &d in digits {
            let v = (d as char).to_digit(16).unwrap_or(0) as u8;
            match hi.take() {
                Some(h) => out.push(h << 4 | v),
                None => hi = Some(v),
            }
        }
        out
    } else {
        rest.to_vec()
    };
    Some((clear, decrypt(&enc, EEXEC_KEY, 4)))
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Tok<'a> {
    Name(&'a [u8]),
    Op(&'a [u8]),
    Num(f64),
    Open,
    Close,
    BraceOpen,
    BraceClose,
    Str,
}

struct Lexer<'a> {
    d: &'a [u8],
    pos: usize,
    /// Where the last token read starts.
    start: usize,
}

fn delim(b: u8) -> bool {
    b.is_ascii_whitespace() || b"()<>[]{}/%".contains(&b)
}

impl<'a> Lexer<'a> {
    fn next(&mut self) -> Option<Tok<'a>> {
        let d = self.d;
        loop {
            let b = *d.get(self.pos)?;
            if b.is_ascii_whitespace() || b == 0 {
                self.pos += 1;
            } else if b == b'%' {
                while d.get(self.pos).is_some_and(|&c| c != b'\n' && c != b'\r') {
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
        self.start = self.pos;
        let b = d[self.pos];
        self.pos += 1;
        Some(match b {
            b'[' => Tok::Open,
            b']' => Tok::Close,
            b'{' => Tok::BraceOpen,
            b'}' => Tok::BraceClose,
            b'(' => {
                let mut depth = 1;
                while let Some(&c) = d.get(self.pos) {
                    self.pos += 1;
                    match c {
                        b'\\' => self.pos += 1,
                        b'(' => depth += 1,
                        b')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                Tok::Str
            }
            b'<' => {
                while d.get(self.pos).is_some_and(|&c| c != b'>') {
                    self.pos += 1;
                }
                self.pos += 1;
                Tok::Str
            }
            b'/' => {
                let s = self.pos;
                while d.get(self.pos).is_some_and(|&c| !delim(c)) {
                    self.pos += 1;
                }
                Tok::Name(&d[s..self.pos])
            }
            _ => {
                let s = self.pos - 1;
                while d.get(self.pos).is_some_and(|&c| !delim(c)) {
                    self.pos += 1;
                }
                let w = &d[s..self.pos];
                match std::str::from_utf8(w)
                    .ok()
                    .and_then(|s| s.parse::<f64>().ok())
                {
                    Some(v) => Tok::Num(v),
                    None => Tok::Op(w),
                }
            }
        })
    }

    /// Reads `n` binary bytes after the single space following an `RD` token.
    fn binary(&mut self, n: usize) -> Option<Range<usize>> {
        let s = self.pos + 1;
        let e = s.checked_add(n)?;
        if e > self.d.len() {
            return None;
        }
        self.pos = e;
        Some(s..e)
    }
}

fn numbers(lx: &mut Lexer, n: usize) -> Vec<f64> {
    let mut v = Vec::new();
    while let Some(t) = lx.next() {
        match t {
            Tok::Num(x) if v.len() < n => v.push(x),
            Tok::Open | Tok::BraceOpen => {}
            _ => break,
        }
    }
    v
}

/// A charstring or subroutine in the private part: where its entry starts (the glyph
/// name or the subroutine's length), its name or index, its `RD` token and its bytes.
struct Entry {
    head: usize,
    name: Range<usize>,
    index: usize,
    rd: Range<usize>,
    bin: Range<usize>,
}

/// The charstrings and subroutines of a decrypted private part.
struct Private {
    len_iv: i64,
    subrs: Vec<Entry>,
    glyphs: Vec<Entry>,
}

impl Private {
    fn scan(p: &[u8], from: usize) -> Private {
        let mut out = Private {
            len_iv: 4,
            subrs: Vec::new(),
            glyphs: Vec::new(),
        };
        let mut lx = Lexer {
            d: p,
            pos: from,
            start: from,
        };
        while let Some(t) = lx.next() {
            match t {
                Tok::Name(b"lenIV") => {
                    if let Some(Tok::Num(v)) = lx.next() {
                        out.len_iv = v as i64;
                    }
                }
                Tok::Name(b"Subrs") if out.subrs.is_empty() => {
                    // dup <i> <n> RD <binary> NP
                    let Some(Tok::Num(count)) = lx.next() else {
                        continue;
                    };
                    let count = count.clamp(0.0, 65536.0) as usize;
                    let mut toks: [Option<(Tok, usize)>; 3] = [None; 3];
                    while out.subrs.len() < count {
                        let save = lx.pos;
                        let Some(t) = lx.next() else { break };
                        if let Tok::Op(_) = t
                            && let [
                                Some((Tok::Op(b"dup"), _)),
                                Some((Tok::Num(i), _)),
                                Some((Tok::Num(n), head)),
                            ] = toks
                        {
                            let rd = lx.start..lx.pos;
                            let Some(bin) = lx.binary(n.max(0.0) as usize) else {
                                break;
                            };
                            out.subrs.push(Entry {
                                head,
                                name: 0..0,
                                index: i.max(0.0) as usize,
                                rd,
                                bin,
                            });
                            toks = [None; 3];
                            continue;
                        }
                        if let Tok::Name(_) = t {
                            lx.pos = save;
                            break;
                        }
                        toks = [toks[1], toks[2], Some((t, lx.start))];
                    }
                }
                Tok::Name(b"CharStrings") => {
                    // /<name> <n> RD <binary> ND
                    let mut toks: [Option<(Tok, usize)>; 2] = [None; 2];
                    while let Some(t) = lx.next() {
                        if let Tok::Op(op) = t
                            && let [Some((Tok::Name(name), head)), Some((Tok::Num(n), _))] = toks
                            && op != b"def"
                        {
                            let rd = lx.start..lx.pos;
                            let Some(bin) = lx.binary(n.max(0.0) as usize) else {
                                break;
                            };
                            let name = head + 1..head + 1 + name.len();
                            out.glyphs.push(Entry {
                                head,
                                name,
                                index: 0,
                                rd,
                                bin,
                            });
                            toks = [None; 2];
                            continue;
                        }
                        if matches!(t, Tok::Op(b"end")) && !out.glyphs.is_empty() {
                            break;
                        }
                        toks = [toks[1], Some((t, lx.start))];
                    }
                }
                _ => {}
            }
        }
        out
    }
}

impl Type1 {
    pub fn parse(data: &[u8]) -> Option<Type1> {
        let data = unwrap_pfb(data);
        let (clear, private) = split(&data)?;
        let mut f = Type1 {
            name: None,
            matrix: Matrix::scale(0.001),
            bbox: [0.0; 4],
            encoding: None,
            bytes: Vec::new(),
            glyphs: Vec::new(),
            subrs: Vec::new(),
            names: HashMap::new(),
        };
        let mut enc_names: Vec<(u8, Box<[u8]>)> = Vec::new();
        let mut standard = false;
        let mut lx = Lexer {
            d: clear,
            pos: 0,
            start: 0,
        };
        while let Some(t) = lx.next() {
            match t {
                Tok::Name(b"FontName") => {
                    if let Some(Tok::Name(n)) = lx.next() {
                        f.name = Some(String::from_utf8_lossy(n).into_owned());
                    }
                }
                Tok::Name(b"FontMatrix") => {
                    if let Ok(m) = <[f64; 6]>::try_from(numbers(&mut lx, 6)) {
                        f.matrix = Matrix(m);
                    }
                }
                Tok::Name(b"FontBBox") => {
                    if let Ok(b) = <[f64; 4]>::try_from(numbers(&mut lx, 4)) {
                        f.bbox = b.map(|v| v as f32);
                    }
                }
                Tok::Name(b"Encoding") => match lx.next() {
                    Some(Tok::Op(b"StandardEncoding")) => standard = true,
                    Some(Tok::Open) => {
                        let mut code = 0u32;
                        while let Some(t) = lx.next() {
                            match t {
                                Tok::Name(n) => {
                                    if let Ok(c) = u8::try_from(code) {
                                        enc_names.push((c, n.into()));
                                    }
                                    code += 1;
                                }
                                Tok::Close => break,
                                _ => {}
                            }
                        }
                    }
                    _ => {
                        // `dup <code> /<name> put` entries up to `def`.
                        let mut prev = [None, None];
                        while let Some(t) = lx.next() {
                            match t {
                                Tok::Op(b"def") | Tok::Op(b"readonly") => break,
                                Tok::Op(b"put") => {
                                    if let [Some(Tok::Num(c)), Some(Tok::Name(n))] = prev
                                        && (0.0..256.0).contains(&c)
                                    {
                                        enc_names.push((c as u8, n.into()));
                                    }
                                }
                                _ => {}
                            }
                            prev = [prev[1], Some(t)];
                        }
                    }
                },
                _ => {}
            }
        }
        f.parse_private(&private)?;
        let mut enc = Box::new([None; 256]);
        if standard {
            for c in 0..=255u8 {
                if let Some(n) = BaseEncoding::Standard.glyph_name(c) {
                    enc[usize::from(c)] = f.glyph_by_name(n.as_bytes());
                }
            }
        }
        for (c, n) in enc_names {
            enc[usize::from(c)] = f.glyph_by_name(&n);
        }
        f.encoding = Some(enc);
        Some(f)
    }

    fn parse_private(&mut self, p: &[u8]) -> Option<()> {
        let Private {
            len_iv,
            subrs,
            glyphs,
        } = Private::scan(p, 0);
        let subrs: Vec<(usize, Range<usize>)> =
            subrs.into_iter().map(|e| (e.index, e.bin)).collect();
        let mut glyphs: Vec<(Box<str>, Range<usize>)> = glyphs
            .into_iter()
            .map(|e| (String::from_utf8_lossy(&p[e.name]).into(), e.bin))
            .collect();
        if glyphs.is_empty() {
            return None;
        }
        let skip = usize::try_from(len_iv).ok();
        let mut dec = |r: Range<usize>| -> Range<usize> {
            let s = self.bytes.len();
            let src = &p[r];
            match skip {
                Some(k) => self.bytes.extend(decrypt(src, CHARSTRING_KEY, k)),
                None => self.bytes.extend_from_slice(src),
            }
            s..self.bytes.len()
        };
        let max = subrs
            .iter()
            .map(|s| s.0 + 1)
            .max()
            .unwrap_or(0)
            .min(1 << 16);
        let mut sr = vec![0..0; max];
        for (i, r) in subrs {
            if i < max {
                sr[i] = dec(r);
            }
        }
        if let Some(i) = glyphs.iter().position(|g| &*g.0 == ".notdef") {
            glyphs[..=i].rotate_right(1);
        }
        let glyphs: Vec<_> = glyphs
            .into_iter()
            .take(0xFFFF)
            .map(|(n, r)| (n, dec(r)))
            .collect();
        self.subrs = sr;
        for (i, g) in glyphs.iter().enumerate().rev() {
            self.names.insert(g.0.as_bytes().into(), i as u16);
        }
        self.glyphs = glyphs;
        Some(())
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn font_matrix(&self) -> Matrix {
        self.matrix
    }

    pub fn bbox(&self) -> [f32; 4] {
        self.bbox
    }

    pub fn num_glyphs(&self) -> u16 {
        self.glyphs.len() as u16
    }

    pub fn glyph_name(&self, gid: u16) -> Option<&str> {
        self.glyphs.get(usize::from(gid)).map(|g| &*g.0)
    }

    pub fn glyph_by_name(&self, name: &[u8]) -> Option<u16> {
        self.names.get(name).copied()
    }

    /// The glyph of `code` in the font's `/Encoding`.
    pub fn encoding_glyph(&self, code: u8) -> Option<u16> {
        self.encoding.as_ref()?[usize::from(code)]
    }

    fn run(
        &self,
        gid: u16,
        pen: &mut Pen,
        origin: (f32, f32),
        depth: u8,
    ) -> Option<charstring::Result> {
        let cs = self
            .bytes
            .get(self.glyphs.get(usize::from(gid))?.1.clone())?;
        let subrs = |i: usize| self.bytes.get(self.subrs.get(i)?.clone());
        let r = charstring::run(cs, &subrs, pen, origin)?;
        if let Some(s) = r.seac
            && depth == 0
        {
            let std = BaseEncoding::Standard;
            let glyph = |c: u8| {
                std.glyph_name(c)
                    .and_then(|n| self.glyph_by_name(n.as_bytes()))
            };
            if let Some(b) = glyph(s.base) {
                self.run(b, pen, origin, 1)?;
            }
            if let Some(a) = glyph(s.accent) {
                self.run(
                    a,
                    pen,
                    (origin.0 + s.adx + r.sbx - s.asb, origin.1 + s.ady),
                    1,
                )?;
            }
        }
        Some(r)
    }

    pub fn outline(&self, gid: u16, sink: &mut dyn OutlineSink) -> bool {
        let mut pen = Pen::new(sink, None);
        self.run(gid, &mut pen, (0.0, 0.0), 0).is_some()
    }

    /// The `hsbw`/`sbw` advance width.
    pub fn advance(&self, gid: u16) -> Option<f32> {
        let mut sink = NullSink;
        let mut pen = Pen::new(&mut sink, None);
        Some(self.run(gid, &mut pen, (0.0, 0.0), 1)?.width)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eexec_roundtrip() {
        // Encrypt with the inverse of `decrypt`.
        let plain = b"abcdhello world";
        let mut r = EEXEC_KEY;
        let enc: Vec<u8> = plain
            .iter()
            .map(|&p| {
                let c = p ^ (r >> 8) as u8;
                r = (u16::from(c).wrapping_add(r))
                    .wrapping_mul(52845)
                    .wrapping_add(22719);
                c
            })
            .collect();
        assert_eq!(decrypt(&enc, EEXEC_KEY, 4), b"hello world");
    }

    fn encrypt(plain: &[u8], key: u16) -> Vec<u8> {
        let mut r = key;
        [0u8, 1, 2, 3]
            .iter()
            .chain(plain)
            .map(|&p| {
                let c = p ^ (r >> 8) as u8;
                r = (u16::from(c).wrapping_add(r))
                    .wrapping_mul(52845)
                    .wrapping_add(22719);
                c
            })
            .collect()
    }

    fn num(v: i32) -> Vec<u8> {
        match v {
            -107..=107 => vec![(v + 139) as u8],
            108..=1131 => vec![((v - 108) / 256 + 247) as u8, ((v - 108) % 256) as u8],
            -1131..=-108 => vec![((-v - 108) / 256 + 251) as u8, ((-v - 108) % 256) as u8],
            _ => [vec![255], v.to_be_bytes().to_vec()].concat(),
        }
    }

    fn cs(ops: &[&[i32]]) -> Vec<u8> {
        // Numbers, then an operator code (negative: escaped `12 x`).
        let mut out = Vec::new();
        for op in ops {
            let (&code, args) = op.split_last().unwrap();
            for &a in args {
                out.extend(num(a));
            }
            if code < 0 {
                out.extend([12, (-code) as u8]);
            } else {
                out.push(code as u8);
            }
        }
        out
    }

    /// A PFA font with a hex eexec section, one subroutine and flex in glyph `A`.
    fn font() -> Vec<u8> {
        const CALLOTHER: i32 = -16;
        let mut a: Vec<&[i32]> = vec![&[0, 500, 13], &[100, 0, 21], &[0, 1, CALLOTHER]];
        let moves: [[i32; 2]; 7] = [
            [0, 0],
            [10, 10],
            [10, 0],
            [10, -10],
            [10, -10],
            [10, 0],
            [10, 10],
        ];
        let flex: Vec<Vec<i32>> = moves.iter().map(|m| vec![m[0], m[1], 21]).collect();
        for f in &flex {
            a.push(f);
            a.push(&[0, 2, CALLOTHER]);
        }
        a.extend([
            &[50, 160, 0, 3, 0, CALLOTHER][..],
            &[-17],
            &[-17],
            &[-33],
            &[0, 10],
            &[14],
        ]);
        let subr0 = cs(&[&[0, 100, 5], &[9], &[11]]);
        let notdef = cs(&[&[0, 250, 13], &[14]]);
        let a = cs(&a);
        let mut private = b"dup /Private 8 dict dup begin\n/RD{string currentfile exch readstring pop}executeonly def\n/lenIV 4 def\n/Subrs 1 array\n".to_vec();
        for (head, body, tail) in [
            ("dup 0 ", &subr0, " NP\n"),
            (
                "ND\n2 index /CharStrings 2 dict dup begin\n/.notdef ",
                &notdef,
                " ND\n",
            ),
            ("/A ", &a, " ND\n"),
        ] {
            let enc = encrypt(body, CHARSTRING_KEY);
            private.extend(format!("{head}{} RD ", enc.len()).bytes());
            private.extend(enc);
            private.extend(tail.bytes());
        }
        private.extend(b"end\nend\nmark currentfile closefile\n");
        let enc = encrypt(&private, EEXEC_KEY);
        let mut f = b"%!PS-AdobeFont-1.0: Test\n/FontName /Test def\n/FontMatrix [0.001 0 0 0.001 0 0] readonly def\n/Encoding 256 array\n0 1 255 {1 index exch /.notdef put} for\ndup 65 /A put\nreadonly def\ncurrentfile eexec\n".to_vec();
        for (i, b) in enc.iter().enumerate() {
            f.extend(format!("{b:02x}").bytes());
            if i % 32 == 31 {
                f.push(b'\n');
            }
        }
        f.extend(b"\n0000000000\ncleartomark\n");
        f
    }

    #[test]
    fn pfa_with_flex_and_subrs() {
        use crate::outline::{Path, Verb};
        let f = Type1::parse(&font()).unwrap();
        assert_eq!(f.name(), Some("Test"));
        assert_eq!(f.num_glyphs(), 2);
        assert_eq!(f.glyph_name(0), Some(".notdef"));
        assert_eq!(f.encoding_glyph(65), Some(1));
        assert_eq!(f.encoding_glyph(66), None);
        assert_eq!(f.advance(1), Some(500.0));
        let mut p = Path::new();
        assert!(f.outline(1, &mut p));
        assert_eq!(
            p.verbs,
            [
                Verb::Move,
                Verb::Cubic,
                Verb::Cubic,
                Verb::Line,
                Verb::Close
            ]
        );
        assert_eq!(
            p.points,
            [
                (100.0, 0.0),
                (110.0, 10.0),
                (120.0, 10.0),
                (130.0, 0.0),
                (140.0, -10.0),
                (150.0, -10.0),
                (160.0, 0.0),
                (160.0, 100.0)
            ]
        );
    }
}
