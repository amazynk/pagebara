//! Objects from tokens (§7.3), indirect objects and stream extents (§7.3.8).

use crate::lexer::{Lexer, Token};
use crate::object::{Dict, ObjRef, Object, Stream, StreamData};
use crate::{Error, Result};

/// Containers nested deeper than this are cut off, so hostile files cannot exhaust the stack.
const MAX_DEPTH: u32 = 256;

#[derive(Debug)]
pub struct Parser<'a> {
    pub lex: Lexer<'a>,
}

impl<'a> Parser<'a> {
    pub fn new(src: &'a [u8], pos: usize) -> Self {
        Parser {
            lex: Lexer::new(src, pos),
        }
    }

    pub fn pos(&self) -> usize {
        self.lex.pos
    }

    /// One direct object. Fails only when no object starts here.
    pub fn object(&mut self) -> Result<Object> {
        let at = self.lex.pos;
        let t = self.lex.next_token();
        self.value(t, 0).ok_or(Error::Syntax {
            offset: at,
            what: "expected an object",
        })
    }

    fn value(&mut self, t: Token<'a>, depth: u32) -> Option<Object> {
        Some(match t {
            Token::Int(a) => self.maybe_ref(a),
            Token::Real(r) => Object::Real(r),
            Token::Name(n) => Object::Name(n),
            Token::String(s) => Object::String(s),
            Token::ArrayOpen => Object::Array(self.array(depth + 1)),
            Token::DictOpen => Object::Dict(self.dict(depth + 1)),
            Token::Keyword(b"true") => Object::Bool(true),
            Token::Keyword(b"false") => Object::Bool(false),
            Token::Keyword(b"null") => Object::Null,
            _ => return None,
        })
    }

    fn maybe_ref(&mut self, a: i64) -> Object {
        let save = self.lex.pos;
        if let Ok(num) = u32::try_from(a)
            && let Token::Int(g) = self.lex.next_token()
            && let Ok(generation) = u16::try_from(g)
            && self.lex.next_token() == Token::Keyword(b"R")
        {
            return Object::Ref(ObjRef { num, generation });
        }
        self.lex.pos = save;
        Object::Int(a)
    }

    fn array(&mut self, depth: u32) -> Vec<Object> {
        let mut v = Vec::new();
        loop {
            let save = self.lex.pos;
            match self.lex.next_token() {
                Token::ArrayClose | Token::Eof => return v,
                // Garbage closing a dictionary or ending the object: the array ends here.
                Token::DictClose | Token::Keyword(b"endobj" | b"stream" | b"endstream") => {
                    self.lex.pos = save;
                    return v;
                }
                _ if depth > MAX_DEPTH => {}
                t => {
                    if let Some(o) = self.value(t, depth) {
                        v.push(o);
                    }
                }
            }
        }
    }

    fn dict(&mut self, depth: u32) -> Dict {
        let mut d = Dict::new();
        loop {
            let save = self.lex.pos;
            match self.lex.next_token() {
                Token::DictClose | Token::Eof => return d,
                Token::ArrayClose | Token::Keyword(b"endobj" | b"stream" | b"endstream") => {
                    self.lex.pos = save;
                    return d;
                }
                Token::Name(k) => {
                    let save = self.lex.pos;
                    let t = self.lex.next_token();
                    let v = match t {
                        // `/Key >>`: a missing value.
                        Token::DictClose | Token::Eof => {
                            self.lex.pos = save;
                            Object::Null
                        }
                        _ if depth > MAX_DEPTH => Object::Null,
                        t => self.value(t, depth).unwrap_or_default(),
                    };
                    // Later duplicates win, as in most readers.
                    if !v.is_null() {
                        d.set(&k.0, v);
                    }
                }
                // A non-name key is skipped.
                _ => {}
            }
        }
    }

    /// `num generation obj`. The position is left after `obj`.
    pub fn indirect_header(&mut self) -> Option<ObjRef> {
        let Token::Int(n) = self.lex.next_token() else {
            return None;
        };
        let Token::Int(g) = self.lex.next_token() else {
            return None;
        };
        if self.lex.next_token() != Token::Keyword(b"obj") {
            return None;
        }
        Some(ObjRef {
            num: u32::try_from(n).ok()?,
            generation: u16::try_from(g).ok()?,
        })
    }

    /// An indirect object `num generation obj … endobj` at the current position. `length`
    /// resolves an indirect `/Length`.
    pub fn indirect(
        &mut self,
        length: &mut dyn FnMut(ObjRef) -> Option<i64>,
    ) -> Result<(ObjRef, Object)> {
        let at = self.lex.pos;
        let id = self.indirect_header().ok_or(Error::Syntax {
            offset: at,
            what: "expected `num generation obj`",
        })?;
        let obj = match self.lex.peek() {
            // `1 0 obj endobj`: an empty object is null.
            Token::Keyword(b"endobj") => Object::Null,
            _ => self.object()?,
        };
        let obj = match obj {
            Object::Dict(dict) if self.lex.peek() == Token::Keyword(b"stream") => {
                self.lex.next_token();
                let data = self.stream_extent(&dict, length);
                Object::Stream(Stream {
                    dict,
                    data: StreamData::Source(data),
                })
            }
            o => o,
        };
        Ok((id, obj))
    }

    /// The data range of a stream whose `stream` keyword was just read. `/Length` is trusted
    /// only if `endstream` follows it; otherwise the data runs to the next `endstream`.
    fn stream_extent(
        &mut self,
        dict: &Dict,
        length: &mut dyn FnMut(ObjRef) -> Option<i64>,
    ) -> std::ops::Range<usize> {
        let src = self.lex.src();
        let mut start = self.lex.pos;
        // Spaces before the EOL are tolerated.
        while src.get(start) == Some(&b' ') {
            start += 1;
        }
        match src.get(start) {
            Some(b'\r') if src.get(start + 1) == Some(&b'\n') => start += 2,
            Some(b'\r' | b'\n') => start += 1,
            _ => {}
        }
        let declared = match dict.get(b"Length") {
            Some(Object::Ref(r)) => length(*r),
            Some(o) => o.as_int(),
            None => None,
        };
        if let Some(len) = declared.and_then(|l| usize::try_from(l).ok())
            && let Some(end) = start.checked_add(len)
            && end <= src.len()
            && ends_stream(&src[end..])
        {
            self.lex.pos = end;
            return start..end;
        }
        let end = match memchr::memmem::find(&src[start..], b"endstream") {
            Some(p) => start + p,
            None => memchr::memmem::find(&src[start..], b"endobj").map_or(src.len(), |p| start + p),
        };
        self.lex.pos = end;
        let mut data_end = end;
        if src[..data_end].ends_with(b"\r\n") {
            data_end -= 2;
        } else if src[..data_end].ends_with(b"\n") || src[..data_end].ends_with(b"\r") {
            data_end -= 1;
        }
        start..data_end.max(start)
    }
}

fn ends_stream(after: &[u8]) -> bool {
    let i = after
        .iter()
        .position(|&b| !crate::lexer::is_white(b))
        .unwrap_or(after.len());
    let rest = &after[i..];
    rest.starts_with(b"endstream") || rest.starts_with(b"endobj")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &[u8]) -> Object {
        Parser::new(s, 0).object().unwrap()
    }

    #[test]
    fn refs_and_numbers() {
        let o = parse(b"[1 0 R 2 3 -1 0 R 4 0]");
        let a = o.as_array().unwrap();
        assert_eq!(a[0], Object::Ref(ObjRef::new(1, 0)));
        assert_eq!(a[1..4], [Object::Int(2), Object::Int(3), Object::Int(-1)]);
        assert_eq!(a[4..], [Object::Int(0), Object::Int(4), Object::Int(0)]);
    }

    #[test]
    fn lenient_dicts() {
        let o = parse(b"<< /A 1 /B /C 2 3 /D >>");
        let d = o.as_dict().unwrap();
        assert_eq!(d.int(b"A"), Some(1));
        assert_eq!(d.name(b"B"), Some(&b"C"[..]));
        // Stray values are skipped; a key without value is absent.
        assert_eq!(d.len(), 2);
        // Unterminated nesting.
        let o = parse(b"<< /K [ 1 2 << /X 3");
        assert_eq!(
            o.as_dict()
                .unwrap()
                .get(b"K")
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn deep_nesting_is_cut() {
        let mut s = vec![b'['; 100_000];
        s.extend(vec![b']'; 100_000]);
        assert!(parse(&s).as_array().is_some());
    }

    #[test]
    fn stream_with_wrong_length() {
        let src = b"4 0 obj << /Length 99 >> stream\r\nabcdef\r\nendstream endobj";
        let (id, o) = Parser::new(src, 0).indirect(&mut |_| None).unwrap();
        assert_eq!(id, ObjRef::new(4, 0));
        let Object::Stream(s) = o else { panic!() };
        let StreamData::Source(r) = s.data else {
            panic!()
        };
        assert_eq!(&src[r], b"abcdef");
    }

    #[test]
    fn stream_with_indirect_length() {
        let src = b"4 0 obj<</Length 5 0 R>>stream\nab\nendstream\nendobj";
        let (_, o) = Parser::new(src, 0)
            .indirect(&mut |r| (r == ObjRef::new(5, 0)).then_some(3))
            .unwrap();
        let StreamData::Source(r) = o.as_stream().unwrap().data.clone() else {
            panic!()
        };
        assert_eq!(&src[r], b"ab\n");
    }
}
