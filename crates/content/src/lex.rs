//! Zero-copy tokens of content streams. Operands borrow the decoded stream; strings and
//! names are decoded only when an operator uses them.

use std::borrow::Cow;

use pagebara_syntax::lexer::{is_regular, is_white};

/// An operand as written. Arrays refer to the parser's element arena.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Operand<'a> {
    Number(f32),
    Bool(bool),
    Null,
    /// Without the slash; `#xx` escapes are still encoded (see [`name`]).
    Name(&'a [u8]),
    /// The bytes between the delimiters (see [`Operand::string_into`]).
    String {
        raw: &'a [u8],
        hex: bool,
    },
    /// Elements `start..end` of [`crate::parse::Instr::elements`].
    Array {
        start: u32,
        end: u32,
    },
    /// Index of a `<<…>>` dictionary in [`crate::parse::Instr::dicts`].
    Dict(u32),
}

impl<'a> Operand<'a> {
    #[inline]
    pub fn number(&self) -> Option<f32> {
        match *self {
            Operand::Number(n) => Some(n),
            _ => None,
        }
    }

    #[inline]
    pub fn raw_name(&self) -> Option<&'a [u8]> {
        match *self {
            Operand::Name(n) => Some(n),
            _ => None,
        }
    }

    /// The name with escapes decoded.
    pub fn name(&self) -> Option<Cow<'a, [u8]>> {
        self.raw_name().map(name)
    }

    /// Appends the decoded bytes of a string operand to `out`; false for other operands.
    pub fn string_into(&self, out: &mut Vec<u8>) -> bool {
        match *self {
            Operand::String { raw, hex: true } => {
                decode_hex(raw, out);
                true
            }
            Operand::String { raw, hex: false } => {
                decode_literal(raw, out);
                true
            }
            _ => false,
        }
    }
}

/// Decodes `#xx` escapes of a raw name.
pub fn name(raw: &[u8]) -> Cow<'_, [u8]> {
    if !raw.contains(&b'#') {
        return Cow::Borrowed(raw);
    }
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'#'
            && let (Some(h), Some(l)) = (
                raw.get(i + 1).copied().and_then(hex_val),
                raw.get(i + 2).copied().and_then(hex_val),
            )
        {
            out.push(h << 4 | l);
            i += 3;
            continue;
        }
        out.push(raw[i]);
        i += 1;
    }
    Cow::Owned(out)
}

#[inline]
fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

pub fn decode_hex(raw: &[u8], out: &mut Vec<u8>) {
    let mut hi = None;
    for &b in raw {
        let Some(v) = hex_val(b) else { continue };
        match hi.take() {
            None => hi = Some(v),
            Some(h) => out.push(h << 4 | v),
        }
    }
    if let Some(h) = hi {
        out.push(h << 4);
    }
}

pub fn decode_literal(raw: &[u8], out: &mut Vec<u8>) {
    if !raw.iter().any(|&b| b == b'\\' || b == b'\r') {
        out.extend_from_slice(raw);
        return;
    }
    let mut i = 0;
    while let Some(&b) = raw.get(i) {
        i += 1;
        match b {
            b'\r' => {
                if raw.get(i) == Some(&b'\n') {
                    i += 1;
                }
                out.push(b'\n');
            }
            b'\\' => {
                let Some(&e) = raw.get(i) else { break };
                i += 1;
                match e {
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'b' => out.push(8),
                    b'f' => out.push(12),
                    b'0'..=b'7' => {
                        let mut v = u32::from(e - b'0');
                        for _ in 0..2 {
                            match raw.get(i) {
                                Some(&d @ b'0'..=b'7') => {
                                    v = v * 8 + u32::from(d - b'0');
                                    i += 1;
                                }
                                _ => break,
                            }
                        }
                        out.push(v as u8);
                    }
                    b'\r' => {
                        if raw.get(i) == Some(&b'\n') {
                            i += 1;
                        }
                    }
                    b'\n' => {}
                    _ => out.push(e),
                }
            }
            _ => out.push(b),
        }
    }
}

/// A raw token. Strings, names and numbers are already operands.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Token<'a> {
    Operand(Operand<'a>),
    ArrayOpen,
    ArrayClose,
    /// `<<`: the caller parses the dictionary.
    DictOpen,
    Keyword(&'a [u8]),
    Eof,
}

#[derive(Clone, Debug)]
pub struct Lexer<'a> {
    pub src: &'a [u8],
    pub pos: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a [u8]) -> Self {
        Lexer { src, pos: 0 }
    }

    pub fn skip_white(&mut self) {
        while let Some(&b) = self.src.get(self.pos) {
            if is_white(b) {
                self.pos += 1;
            } else if b == b'%' {
                self.pos += memchr::memchr2(b'\r', b'\n', &self.src[self.pos..])
                    .unwrap_or(self.src.len() - self.pos);
            } else {
                break;
            }
        }
    }

    pub fn next_token(&mut self) -> Token<'a> {
        self.skip_white();
        let src = self.src;
        let Some(&b) = src.get(self.pos) else {
            return Token::Eof;
        };
        match b {
            b'0'..=b'9' | b'+' | b'-' | b'.' => Token::Operand(Operand::Number(self.number())),
            b'/' => {
                self.pos += 1;
                Token::Operand(Operand::Name(self.regular_run()))
            }
            b'(' => Token::Operand(self.literal()),
            b'<' if src.get(self.pos + 1) == Some(&b'<') => {
                self.pos += 2;
                Token::DictOpen
            }
            b'<' => {
                let start = self.pos + 1;
                let end = memchr::memchr(b'>', &src[start..]).map_or(src.len(), |e| start + e);
                self.pos = (end + 1).min(src.len());
                Token::Operand(Operand::String {
                    raw: &src[start..end],
                    hex: true,
                })
            }
            b'[' => {
                self.pos += 1;
                Token::ArrayOpen
            }
            b']' => {
                self.pos += 1;
                Token::ArrayClose
            }
            b'>' | b')' | b'{' | b'}' => {
                self.pos += 1;
                self.pos += usize::from(b == b'>' && src.get(self.pos) == Some(&b'>'));
                Token::Keyword(&src[self.pos - 1..self.pos])
            }
            _ => {
                let k = self.regular_run();
                match k {
                    b"true" => Token::Operand(Operand::Bool(true)),
                    b"false" => Token::Operand(Operand::Bool(false)),
                    b"null" => Token::Operand(Operand::Null),
                    _ => Token::Keyword(k),
                }
            }
        }
    }

    #[inline]
    fn regular_run(&mut self) -> &'a [u8] {
        let start = self.pos;
        let len = self.src[start..]
            .iter()
            .position(|&b| !is_regular(b))
            .unwrap_or(self.src.len() - start);
        self.pos += len;
        &self.src[start..self.pos]
    }

    fn literal(&mut self) -> Operand<'a> {
        let src = self.src;
        let start = self.pos + 1;
        let mut i = start;
        let mut depth = 1u32;
        let end = loop {
            match memchr::memchr3(b'(', b')', b'\\', &src[i.min(src.len())..]) {
                None => break src.len(),
                Some(p) => {
                    i += p;
                    match src[i] {
                        b'\\' => i += 2,
                        b'(' => {
                            depth += 1;
                            i += 1;
                        }
                        _ => {
                            depth -= 1;
                            if depth == 0 {
                                break i;
                            }
                            i += 1;
                        }
                    }
                }
            }
        };
        let end = end.min(src.len());
        self.pos = (end + 1).min(src.len());
        Operand::String {
            raw: &src[start..end],
            hex: false,
        }
    }

    /// Lenient like Acrobat: `--5`, `1.2.3`, `4.` and a lone `-` are read.
    fn number(&mut self) -> f32 {
        let run = self.regular_run();
        let mut i = 0;
        let mut neg = false;
        while let Some(&c) = run.get(i) {
            match c {
                b'-' => neg = true,
                b'+' => {}
                _ => break,
            }
            i += 1;
        }
        let mut int: u64 = 0;
        let mut digits = 0u32;
        while let Some(&c @ b'0'..=b'9') = run.get(i) {
            if digits < 19 {
                int = int * 10 + u64::from(c - b'0');
            } else {
                int = int.saturating_mul(10);
            }
            digits += 1;
            i += 1;
        }
        let mut v = int as f64;
        if run.get(i) == Some(&b'.') {
            i += 1;
            let mut frac: u64 = 0;
            let mut scale = 1u64;
            while let Some(&c @ b'0'..=b'9') = run.get(i) {
                if scale < 1_000_000_000_000_000 {
                    frac = frac * 10 + u64::from(c - b'0');
                    scale *= 10;
                }
                i += 1;
            }
            v += frac as f64 / scale as f64;
        }
        let v = v as f32;
        if neg { -v } else { v }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(s: &[u8]) -> Vec<Token<'_>> {
        let mut l = Lexer::new(s);
        let mut v = Vec::new();
        loop {
            match l.next_token() {
                Token::Eof => return v,
                t => v.push(t),
            }
        }
    }

    #[test]
    fn numbers() {
        let n: Vec<f32> = toks(b"0 -12 +7 3.5 -.5 4. --5 1.2.3 - 0.000001")
            .into_iter()
            .map(|t| match t {
                Token::Operand(Operand::Number(n)) => n,
                _ => f32::NAN,
            })
            .collect();
        assert_eq!(
            n,
            [0.0, -12.0, 7.0, 3.5, -0.5, 4.0, -5.0, 1.2, 0.0, 0.000001]
        );
    }

    #[test]
    fn strings_names_keywords() {
        let t = toks(b"(a(b)c\\)d) <41 42> /Na#20me true BT%c\n[]<<");
        let mut s = Vec::new();
        let Token::Operand(o) = t[0] else { panic!() };
        assert!(o.string_into(&mut s));
        assert_eq!(s, b"a(b)c)d");
        s.clear();
        let Token::Operand(o) = t[1] else { panic!() };
        o.string_into(&mut s);
        assert_eq!(s, b"AB");
        let Token::Operand(o) = t[2] else { panic!() };
        assert_eq!(&*o.name().unwrap(), b"Na me");
        assert_eq!(t[3], Token::Operand(Operand::Bool(true)));
        assert_eq!(t[4], Token::Keyword(b"BT"));
        assert_eq!(
            &t[5..],
            [Token::ArrayOpen, Token::ArrayClose, Token::DictOpen]
        );
    }

    #[test]
    fn unterminated() {
        assert_eq!(toks(b"(abc").len(), 1);
        assert_eq!(toks(b"<41").len(), 1);
        assert_eq!(toks(b"(a\\").len(), 1);
    }
}
