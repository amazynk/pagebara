//! Tokens of PDF syntax (§7.2). Lenient: malformed numbers, strings and names produce the
//! closest reasonable token instead of an error, as other readers do.

use crate::object::{Name, PdfString};

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Class {
    Regular,
    White,
    Delim,
}

const fn classes() -> [Class; 256] {
    let mut t = [Class::Regular; 256];
    let white = [0u8, 9, 10, 12, 13, 32];
    let mut i = 0;
    while i < white.len() {
        t[white[i] as usize] = Class::White;
        i += 1;
    }
    let delim = *b"()<>[]{}/%";
    let mut i = 0;
    while i < delim.len() {
        t[delim[i] as usize] = Class::Delim;
        i += 1;
    }
    t
}

static CLASS: [Class; 256] = classes();

#[inline]
pub fn is_white(b: u8) -> bool {
    CLASS[b as usize] == Class::White
}

#[inline]
pub fn is_regular(b: u8) -> bool {
    CLASS[b as usize] == Class::Regular
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

#[derive(Clone, Debug, PartialEq)]
pub enum Token<'a> {
    Int(i64),
    Real(f64),
    Name(Name),
    String(PdfString),
    ArrayOpen,
    ArrayClose,
    DictOpen,
    DictClose,
    /// Any other run of regular characters: `true`, `obj`, `R`, operators, `{`, `}`.
    Keyword(&'a [u8]),
    Eof,
}

#[derive(Clone, Debug)]
pub struct Lexer<'a> {
    src: &'a [u8],
    pub pos: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a [u8], pos: usize) -> Self {
        Lexer {
            src,
            pos: pos.min(src.len()),
        }
    }

    pub fn src(&self) -> &'a [u8] {
        self.src
    }

    pub fn rest(&self) -> &'a [u8] {
        &self.src[self.pos..]
    }

    /// Skips whitespace and comments.
    pub fn skip_white(&mut self) {
        while let Some(&b) = self.src.get(self.pos) {
            if is_white(b) {
                self.pos += 1;
            } else if b == b'%' {
                while let Some(&c) = self.src.get(self.pos) {
                    if c == b'\r' || c == b'\n' {
                        break;
                    }
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    pub fn peek(&mut self) -> Token<'a> {
        let save = self.pos;
        let t = self.next_token();
        self.pos = save;
        t
    }

    pub fn next_token(&mut self) -> Token<'a> {
        self.skip_white();
        let Some(&b) = self.src.get(self.pos) else {
            return Token::Eof;
        };
        match b {
            b'[' => {
                self.pos += 1;
                Token::ArrayOpen
            }
            b']' => {
                self.pos += 1;
                Token::ArrayClose
            }
            b'<' if self.src.get(self.pos + 1) == Some(&b'<') => {
                self.pos += 2;
                Token::DictOpen
            }
            b'>' if self.src.get(self.pos + 1) == Some(&b'>') => {
                self.pos += 2;
                Token::DictClose
            }
            b'<' => Token::String(self.hex_string()),
            b'(' => Token::String(self.literal_string()),
            b'/' => Token::Name(self.name()),
            b'{' | b'}' | b'>' | b')' => {
                self.pos += 1;
                Token::Keyword(&self.src[self.pos - 1..self.pos])
            }
            b'0'..=b'9' | b'+' | b'-' | b'.' => self.number(),
            _ => Token::Keyword(self.regular_run()),
        }
    }

    fn regular_run(&mut self) -> &'a [u8] {
        let start = self.pos;
        while self.src.get(self.pos).is_some_and(|&b| is_regular(b)) {
            self.pos += 1;
        }
        &self.src[start..self.pos]
    }

    /// Numbers such as `--5`, `1.2.3`, `4.` and `-` are accepted the way Acrobat reads them.
    fn number(&mut self) -> Token<'a> {
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
        let digits = &run[i..];
        let int_end = digits
            .iter()
            .position(|b| !b.is_ascii_digit())
            .unwrap_or(digits.len());
        if int_end == digits.len() {
            if int_end == 0 {
                // Only signs, or not a number at all (e.g. `+abc`): PDF readers treat as 0.
                return Token::Int(0);
            }
            let mut v: i64 = 0;
            for &d in digits {
                match v
                    .checked_mul(10)
                    .and_then(|v| v.checked_add(i64::from(d - b'0')))
                {
                    Some(n) => v = n,
                    None => return Token::Real(parse_real(digits, neg)),
                }
            }
            return Token::Int(if neg { -v } else { v });
        }
        if digits[int_end] != b'.' {
            // `12abc`: take the leading integer.
            return match parse_int(&digits[..int_end]) {
                Some(v) => Token::Int(if neg { -v } else { v }),
                None => Token::Real(parse_real(&digits[..int_end], neg)),
            };
        }
        let frac_end = digits[int_end + 1..]
            .iter()
            .position(|b| !b.is_ascii_digit())
            .map_or(digits.len(), |p| int_end + 1 + p);
        Token::Real(parse_real(&digits[..frac_end], neg))
    }

    fn name(&mut self) -> Name {
        self.pos += 1;
        let raw = self.regular_run();
        if !raw.contains(&b'#') {
            return Name::new(raw);
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
        Name(out.into())
    }

    fn hex_string(&mut self) -> PdfString {
        self.pos += 1;
        let mut out = Vec::new();
        let mut hi = None;
        while let Some(&b) = self.src.get(self.pos) {
            self.pos += 1;
            if b == b'>' {
                break;
            }
            // Whitespace and junk are skipped.
            let Some(v) = hex_val(b) else { continue };
            match hi.take() {
                None => hi = Some(v),
                Some(h) => out.push(h << 4 | v),
            }
        }
        if let Some(h) = hi {
            out.push(h << 4);
        }
        PdfString {
            bytes: out,
            hex: true,
        }
    }

    fn literal_string(&mut self) -> PdfString {
        self.pos += 1;
        let src = self.src;
        let mut out = Vec::new();
        let mut depth = 1u32;
        while let Some(&b) = src.get(self.pos) {
            self.pos += 1;
            match b {
                b'(' => {
                    depth += 1;
                    out.push(b);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    out.push(b);
                }
                b'\r' => {
                    // Bare EOLs read as LF.
                    if src.get(self.pos) == Some(&b'\n') {
                        self.pos += 1;
                    }
                    out.push(b'\n');
                }
                b'\\' => {
                    let Some(&e) = src.get(self.pos) else { break };
                    self.pos += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'0'..=b'7' => {
                            let mut v = u32::from(e - b'0');
                            for _ in 0..2 {
                                match src.get(self.pos) {
                                    Some(&d @ b'0'..=b'7') => {
                                        v = v * 8 + u32::from(d - b'0');
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push(v as u8);
                        }
                        // Line continuation.
                        b'\r' => {
                            if src.get(self.pos) == Some(&b'\n') {
                                self.pos += 1;
                            }
                        }
                        b'\n' => {}
                        // `\(`, `\)`, `\\`, and unknown escapes: the character itself.
                        _ => out.push(e),
                    }
                }
                _ => out.push(b),
            }
        }
        PdfString {
            bytes: out,
            hex: false,
        }
    }
}

fn parse_int(d: &[u8]) -> Option<i64> {
    d.iter().try_fold(0i64, |v, &c| {
        v.checked_mul(10)?.checked_add(i64::from(c - b'0'))
    })
}

fn parse_real(d: &[u8], neg: bool) -> f64 {
    // `d` holds only digits and at most one dot, which std parses (`4.` and `.5` included).
    let s = std::str::from_utf8(d).unwrap_or("0");
    let v = if s == "." {
        0.0
    } else {
        s.parse().unwrap_or(0.0)
    };
    if neg { -v } else { v }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(s: &[u8]) -> Vec<Token<'_>> {
        let mut l = Lexer::new(s, 0);
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
        use Token::*;
        assert_eq!(
            toks(b"0 -12 +7 3.5 -.5 4. --5 1.2.3 - 99999999999999999999"),
            [
                Int(0),
                Int(-12),
                Int(7),
                Real(3.5),
                Real(-0.5),
                Real(4.0),
                Int(-5),
                Real(1.2),
                Int(0),
                Real(1e20),
            ]
        );
    }

    #[test]
    fn names_strings_and_comments() {
        let t = toks(b"/A#20B /#zz % comment\n(a(b)c\\)\\101\\\n\r\nd) <48 6 5 7>");
        assert_eq!(t[0], Token::Name(Name::new(b"A B")));
        assert_eq!(t[1], Token::Name(Name::new(b"#zz")));
        let Token::String(s) = &t[2] else { panic!() };
        assert_eq!(s.bytes, b"a(b)c)A\nd");
        let Token::String(h) = &t[3] else { panic!() };
        assert_eq!((h.bytes.as_slice(), h.hex), (&b"Hep"[..], true));
    }

    #[test]
    fn delimiters_and_keywords() {
        use Token::*;
        assert_eq!(
            toks(b"<</K[1 0 R]>>obj{true}"),
            [
                DictOpen,
                Name(crate::Name::new(b"K")),
                ArrayOpen,
                Int(1),
                Int(0),
                Keyword(b"R"),
                ArrayClose,
                DictClose,
                Keyword(b"obj"),
                Keyword(b"{"),
                Keyword(b"true"),
                Keyword(b"}"),
            ]
        );
    }
}
