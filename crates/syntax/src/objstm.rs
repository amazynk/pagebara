//! Object streams (§7.5.7).

use crate::lexer::{Lexer, Token};
use crate::object::{Dict, Object};
use crate::parser::Parser;

#[derive(Debug)]
pub struct ObjStm {
    data: Vec<u8>,
    first: usize,
    /// Object number and offset after `/First` of each object.
    index: Vec<(u32, usize)>,
}

impl ObjStm {
    /// From the decoded data. A header cut short keeps the pairs that parsed.
    pub fn new(data: Vec<u8>, dict: &Dict) -> Option<Self> {
        let first = usize::try_from(dict.int(b"First")?).ok()?.min(data.len());
        let n = usize::try_from(dict.int(b"N").unwrap_or(0)).unwrap_or(0);
        let mut lex = Lexer::new(&data[..first], 0);
        let mut index = Vec::with_capacity(n.min(first / 4));
        while index.len() < n {
            let (Token::Int(num), Token::Int(off)) = (lex.next_token(), lex.next_token()) else {
                break;
            };
            let (Ok(num), Ok(off)) = (u32::try_from(num), usize::try_from(off)) else {
                break;
            };
            index.push((num, off));
        }
        Some(ObjStm { data, first, index })
    }

    pub fn numbers(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        self.index
            .iter()
            .enumerate()
            .map(|(i, &(n, _))| (n, i as u32))
    }

    /// Object `num`, expected at `index`; found by number when the index is wrong.
    pub fn object(&self, index: u32, num: u32) -> Option<Object> {
        let &(_, off) = self
            .index
            .get(index as usize)
            .filter(|e| e.0 == num)
            .or_else(|| self.index.iter().find(|e| e.0 == num))?;
        let at = self.first.checked_add(off)?;
        if at >= self.data.len() {
            return None;
        }
        Parser::new(&self.data, at).object().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_with_wrong_index() {
        let data = b"10 0 11 4 [1] <</A 2>>".to_vec();
        let mut d = Dict::new();
        d.set(b"N", 2);
        d.set(b"First", 10);
        let s = ObjStm::new(data, &d).unwrap();
        assert_eq!(s.object(0, 10), Some(Object::Array(vec![Object::Int(1)])));
        assert_eq!(
            s.object(0, 11).unwrap().as_dict().unwrap().int(b"A"),
            Some(2)
        );
        assert_eq!(s.object(5, 12), None);
    }
}
