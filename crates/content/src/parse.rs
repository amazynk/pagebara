//! Operators with their operands, read from a decoded content stream without allocating
//! per operator.

use memchr::memmem;
use pagebara_syntax::lexer::is_white;
use pagebara_syntax::parser::Parser;
use pagebara_syntax::{Dict, Object};

use crate::lex::{Lexer, Operand, Token};
use crate::ops::Op;

/// Operands beyond this many without an operator are dropped as garbage.
const MAX_OPERANDS: usize = 1 << 16;

/// One operator and its operands, borrowed from the parser until the next call.
#[derive(Clone, Copy, Debug)]
pub struct Instr<'s, 'a> {
    pub op: Op,
    pub keyword: &'a [u8],
    /// Top-level operands. For inline images: the dictionary as key/value pairs.
    pub args: &'s [Operand<'a>],
    /// Elements of array operands.
    pub elements: &'s [Operand<'a>],
    /// Dictionary operands.
    pub dicts: &'s [Dict],
    /// The data of an inline image.
    pub data: &'a [u8],
    /// Byte offset just after the operator (after `EI` for inline images).
    pub end: usize,
}

impl<'s, 'a> Instr<'s, 'a> {
    /// The elements of an array operand (empty for other operands).
    pub fn array(&self, o: &Operand<'a>) -> &'s [Operand<'a>] {
        match *o {
            Operand::Array { start, end } => &self.elements[start as usize..end as usize],
            _ => &[],
        }
    }

    pub fn dict(&self, o: &Operand<'a>) -> Option<&'s Dict> {
        match *o {
            Operand::Dict(i) => self.dicts.get(i as usize),
            _ => None,
        }
    }

    /// The last `N` operands if they are all numbers: extra leading operands are ignored
    /// like other readers do.
    #[inline]
    pub fn numbers<const N: usize>(&self) -> Option<[f32; N]> {
        let tail = self.args.len().checked_sub(N)?;
        let mut out = [0.0; N];
        for (o, a) in out.iter_mut().zip(&self.args[tail..]) {
            *o = a.number()?;
        }
        Some(out)
    }

    /// The last operand as a number.
    #[inline]
    pub fn number(&self) -> Option<f32> {
        self.args.last()?.number()
    }

    /// The last operand as a raw name.
    #[inline]
    pub fn raw_name(&self) -> Option<&'a [u8]> {
        self.args.last()?.raw_name()
    }

    /// An operand converted to an object (for dictionaries and arrays of inline images and
    /// property lists).
    pub fn object(&self, o: &Operand<'a>) -> Object {
        self.object_at_depth(o, 0)
    }

    fn object_at_depth(&self, o: &Operand<'a>, depth: u32) -> Object {
        match *o {
            Operand::Number(n) => {
                if n.fract() == 0.0 && n.abs() < 1e9 {
                    Object::Int(n as i64)
                } else {
                    Object::Real(f64::from(n))
                }
            }
            Operand::Bool(b) => Object::Bool(b),
            Operand::Null => Object::Null,
            Operand::Name(_) => Object::Name(pagebara_syntax::Name::new(&o.name().unwrap())),
            Operand::String { .. } => {
                let mut s = Vec::new();
                o.string_into(&mut s);
                Object::String(pagebara_syntax::PdfString {
                    bytes: s,
                    hex: false,
                })
            }
            Operand::Array { .. } if depth < 32 => Object::Array(
                self.array(o)
                    .iter()
                    .map(|e| self.object_at_depth(e, depth + 1))
                    .collect(),
            ),
            Operand::Array { .. } => Object::Null,
            Operand::Dict(_) => self.dict(o).cloned().map_or(Object::Null, Object::Dict),
        }
    }
}

#[derive(Debug)]
pub struct ContentParser<'a> {
    lex: Lexer<'a>,
    args: Vec<Operand<'a>>,
    elements: Vec<Operand<'a>>,
    dicts: Vec<Dict>,
    marks: Vec<usize>,
}

impl<'a> ContentParser<'a> {
    pub fn new(src: &'a [u8]) -> Self {
        ContentParser {
            lex: Lexer::new(src),
            args: Vec::with_capacity(16),
            elements: Vec::new(),
            dicts: Vec::new(),
            marks: Vec::new(),
        }
    }

    /// Byte offset of the next token.
    pub fn pos(&self) -> usize {
        self.lex.pos
    }

    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<Instr<'_, 'a>> {
        self.args.clear();
        self.elements.clear();
        self.dicts.clear();
        self.marks.clear();
        loop {
            match self.lex.next_token() {
                Token::Eof => return None,
                Token::Keyword(k) => {
                    if matches!(k, b"{" | b"}" | b")" | b">" | b">>") {
                        continue;
                    }
                    self.close_arrays();
                    let op = Op::from_keyword(k);
                    let data = if op == Op::InlineImage {
                        self.inline_image()
                    } else {
                        &[]
                    };
                    return Some(Instr {
                        op,
                        keyword: k,
                        args: &self.args,
                        elements: &self.elements,
                        dicts: &self.dicts,
                        data,
                        end: self.lex.pos,
                    });
                }
                t => self.operand(t),
            }
        }
    }

    fn operand(&mut self, t: Token<'a>) {
        match t {
            Token::Operand(o) => {
                if self.args.len() >= MAX_OPERANDS {
                    self.args.clear();
                    self.marks.clear();
                }
                self.args.push(o);
            }
            Token::ArrayOpen => self.marks.push(self.args.len()),
            Token::ArrayClose => self.close_array(),
            Token::DictOpen => {
                let start = self.lex.pos - 2;
                let mut p = Parser::new(self.lex.src, start);
                let d = match p.object() {
                    Ok(Object::Dict(d)) => d,
                    _ => Dict::new(),
                };
                self.lex.pos = p.pos().max(self.lex.pos);
                let i = self.dicts.len() as u32;
                self.dicts.push(d);
                self.args.push(Operand::Dict(i));
            }
            Token::Keyword(_) | Token::Eof => {}
        }
    }

    fn close_array(&mut self) {
        if let Some(m) = self.marks.pop() {
            let m = m.min(self.args.len());
            let start = self.elements.len() as u32;
            self.elements.extend(self.args.drain(m..));
            let end = self.elements.len() as u32;
            self.args.push(Operand::Array { start, end });
        }
    }

    fn close_arrays(&mut self) {
        while !self.marks.is_empty() {
            self.close_array();
        }
    }

    /// Reads the dictionary of `BI` up to `ID` into the operands and returns the data,
    /// leaving the lexer after `EI`.
    fn inline_image(&mut self) -> &'a [u8] {
        loop {
            match self.lex.next_token() {
                Token::Eof => return &[],
                Token::Keyword(b"ID") => break,
                Token::Keyword(b"EI") => return &[],
                Token::Keyword(_) => {}
                t => self.operand(t),
            }
        }
        self.close_arrays();
        let src = self.lex.src;
        let mut start = self.lex.pos;
        if src.get(start).is_some_and(|&b| is_white(b)) {
            start += 1;
        }
        let info = ImageInfo::from_pairs(&self.args);
        if let Some(len) = info.exact_len() {
            // `ID\r\n` is common although only one white-space byte is allowed.
            for s in [start, start + 1] {
                if let Some(end) = s.checked_add(len)
                    && let Some(after) = ei_after(src, end)
                {
                    self.lex.pos = after;
                    return &src[s..end];
                }
            }
        }
        let (end, after) = find_ei(src, start);
        self.lex.pos = after;
        &src[start.min(end)..end]
    }
}

/// `EI` after optional white space at `at`, returning the offset after it.
fn ei_after(src: &[u8], mut at: usize) -> Option<usize> {
    while src.get(at).is_some_and(|&b| is_white(b)) {
        at += 1;
    }
    (src.get(at..at + 2)? == b"EI" && ends_token(src, at + 2)).then_some(at + 2)
}

fn ends_token(src: &[u8], at: usize) -> bool {
    src.get(at)
        .is_none_or(|&b| !pagebara_syntax::lexer::is_regular(b))
}

/// The data end and resume offset of an inline image whose length is not known: the first
/// `EI` between white space that is followed by plausible content.
fn find_ei(src: &[u8], start: usize) -> (usize, usize) {
    let finder = memmem::Finder::new(b"EI");
    let mut from = start;
    while let Some(p) = finder.find(&src[from.min(src.len())..]) {
        let at = from + p;
        let before = at == start || is_white(src[at - 1]);
        if before && ends_token(src, at + 2) && plausible_content(&src[at + 2..]) {
            let end = if at > start && is_white(src[at - 1]) {
                at - 1
            } else {
                at
            };
            return (end, at + 2);
        }
        from = at + 1;
    }
    (src.len(), src.len())
}

/// Whether bytes look like content stream syntax up to the next operator.
fn plausible_content(rest: &[u8]) -> bool {
    let head = &rest[..rest.len().min(64)];
    let mut lex = Lexer::new(head);
    for _ in 0..16 {
        let before = lex.pos;
        let t = lex.next_token();
        if head[before..lex.pos]
            .iter()
            .any(|&b| !(b.is_ascii_graphic() || is_white(b)))
        {
            return false;
        }
        match t {
            Token::Eof => return true,
            // A token cut off by the window is fine.
            Token::Keyword(_) if lex.pos == head.len() && head.len() < rest.len() => {
                return true;
            }
            Token::Keyword(k) => return Op::from_keyword(k) != Op::Unknown || k == b"EI",
            _ if lex.pos == before => return false,
            _ => {}
        }
    }
    true
}

/// What the dictionary of an inline image says about its size.
#[derive(Debug, Default)]
struct ImageInfo {
    width: Option<f32>,
    height: Option<f32>,
    bpc: Option<f32>,
    comps: Option<u32>,
    mask: bool,
    filtered: bool,
    length: Option<f32>,
}

impl ImageInfo {
    fn from_pairs(args: &[Operand<'_>]) -> Self {
        let mut i = ImageInfo::default();
        // Full key names first: the abbreviations win when both are given.
        let (full, short): (Vec<&[Operand<'_>; 2]>, Vec<_>) = args
            .as_chunks::<2>()
            .0
            .iter()
            .partition(|kv| kv[0].raw_name().is_some_and(|k| k.len() > 3));
        for kv in full.into_iter().chain(short) {
            let Some(k) = kv[0].raw_name() else { continue };
            let v = &kv[1];
            match k {
                b"W" | b"Width" => i.width = v.number(),
                b"H" | b"Height" => i.height = v.number(),
                b"BPC" | b"BitsPerComponent" => i.bpc = v.number(),
                b"IM" | b"ImageMask" => i.mask = *v == Operand::Bool(true),
                b"L" | b"Length" => i.length = v.number(),
                b"F" | b"Filter" => {
                    i.filtered = !matches!(*v, Operand::Null)
                        && !matches!(*v, Operand::Array { start, end } if start == end)
                }
                b"CS" | b"ColorSpace" => {
                    i.comps = match v.raw_name() {
                        Some(b"G" | b"DeviceGray" | b"I" | b"Indexed") => Some(1),
                        Some(b"RGB" | b"DeviceRGB") => Some(3),
                        Some(b"CMYK" | b"DeviceCMYK") => Some(4),
                        _ => None,
                    }
                }
                _ => {}
            }
        }
        i
    }

    fn exact_len(&self) -> Option<usize> {
        if let Some(l) = self.length
            && l >= 0.0
        {
            return Some(l as usize);
        }
        if self.filtered {
            return None;
        }
        let (w, h) = (self.width? as u64, self.height? as u64);
        let (bpc, comps) = if self.mask {
            (1, 1)
        } else {
            (self.bpc? as u64, u64::from(self.comps?))
        };
        let row = w.checked_mul(comps)?.checked_mul(bpc)?.div_ceil(8);
        usize::try_from(row.checked_mul(h)?).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ops(src: &[u8]) -> Vec<(Op, usize)> {
        let mut p = ContentParser::new(src);
        let mut v = Vec::new();
        while let Some(i) = p.next() {
            v.push((i.op, i.args.len()));
        }
        v
    }

    #[test]
    fn operators_and_arrays() {
        let src = b"q 1 0 0 1 10 20 cm [(a) -5 [1 2] (b)] TJ [3] 0 d Q /P <</MCID 3>> BDC EMC";
        let mut p = ContentParser::new(src);
        assert_eq!(p.next().unwrap().op, Op::Save);
        let cm = p.next().unwrap();
        assert_eq!(
            (cm.op, cm.numbers::<6>()),
            (Op::Concat, Some([1., 0., 0., 1., 10., 20.]))
        );
        let tj = p.next().unwrap();
        assert_eq!(tj.op, Op::ShowTextArray);
        let a = tj.array(&tj.args[0]);
        assert_eq!(a.len(), 4);
        assert_eq!(a[1], Operand::Number(-5.0));
        assert_eq!(tj.array(&a[2]).len(), 2);
        let d = p.next().unwrap();
        assert_eq!((d.op, d.args.len()), (Op::Dash, 2));
        assert_eq!(p.next().unwrap().op, Op::Restore);
        let bdc = p.next().unwrap();
        assert_eq!(bdc.dict(&bdc.args[1]).unwrap().int(b"MCID"), Some(3));
        assert_eq!(p.next().unwrap().op, Op::EndMarked);
        assert!(p.next().is_none());
    }

    #[test]
    fn junk_is_skipped() {
        assert_eq!(
            ops(b"1 2 ] } ) >> m [ 1 2 l foo 3 Tz"),
            [
                (Op::MoveTo, 2),
                (Op::LineTo, 1),
                (Op::Unknown, 0),
                (Op::HorizontalScale, 1)
            ]
        );
    }

    #[test]
    fn inline_image_with_known_length() {
        // The data contains `EI` surrounded by spaces; the size from the dictionary wins.
        let src = b"BI /W 4 /H 1 /BPC 8 /CS /G ID\n\x20EI\x20 EI Q";
        let mut p = ContentParser::new(src);
        let i = p.next().unwrap();
        assert_eq!(i.op, Op::InlineImage);
        assert_eq!(i.data, b" EI ");
        assert_eq!(i.args.len(), 8);
        assert_eq!(p.next().unwrap().op, Op::Restore);
    }

    #[test]
    fn inline_image_searching_ei() {
        let src = b"BI /W 2 /H 2 /F /AHx ID 0aEI1 EIx 0f EI q 1 w Q";
        let mut p = ContentParser::new(src);
        let i = p.next().unwrap();
        assert_eq!(i.data, b"0aEI1 EIx 0f");
        assert_eq!(p.next().unwrap().op, Op::Save);
        // Binary bytes after a false `EI` reject it.
        let src = b"BI /F /Fl ID x\x9c EI \x01\x02 EI Q";
        let mut p = ContentParser::new(src);
        assert_eq!(p.next().unwrap().data, b"x\x9c EI \x01\x02");
        assert_eq!(p.next().unwrap().op, Op::Restore);
    }

    #[test]
    fn truncated_inline_image() {
        let mut p = ContentParser::new(b"BI /W 1 ID abc");
        assert_eq!(p.next().unwrap().data, b"abc");
        assert!(p.next().is_none());
        let mut p = ContentParser::new(b"BI /W 1");
        assert_eq!(p.next().unwrap().data, b"");
        assert!(p.next().is_none());
    }
}
