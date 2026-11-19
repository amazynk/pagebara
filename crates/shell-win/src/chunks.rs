//! The text Windows Search reads through `IFilter`, as numbered chunks: the title, then
//! one chunk per page. `GetChunk` steps through them and `GetText` hands out each chunk's
//! UTF-16 text in pieces as big as the indexer's buffer.

use pagebara_syntax::Document;

/// Text indexed per document at most (UTF-8 bytes, before conversion).
pub const MAX_TEXT: usize = 64 << 20;

/// Which property a chunk's text belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prop {
    /// The document's contents (`PKEY_Search_Contents`).
    Contents,
    /// `PKEY_Title`.
    Title,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chunk {
    pub prop: Prop,
    pub text: Vec<u16>,
    /// The chunk ends a page (`CHUNK_EOP`), else a paragraph-free break (`CHUNK_EOS`).
    pub end_of_page: bool,
}

/// The chunks of `doc`, for the properties wanted. Pages without text get none.
pub fn document_chunks(doc: &Document, contents: bool, title: bool) -> Vec<Chunk> {
    let mut out = Vec::new();
    if title && let Some(t) = crate::pdf::title(doc) {
        out.push(Chunk {
            prop: Prop::Title,
            text: t.encode_utf16().collect(),
            end_of_page: false,
        });
    }
    if contents {
        for t in crate::pdf::page_texts(doc, MAX_TEXT) {
            let t = t.trim();
            if !t.is_empty() {
                out.push(Chunk {
                    prop: Prop::Contents,
                    text: t.encode_utf16().collect(),
                    end_of_page: true,
                });
            }
        }
    }
    out
}

/// What `GetText` gives back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Read {
    /// `n` units copied; `last` when the chunk has no more (`FILTER_S_LAST_TEXT`).
    Copied { n: usize, last: bool },
    /// Everything of this chunk was handed out (`FILTER_E_NO_MORE_TEXT`).
    NoMore,
    /// `GetChunk` has not been called, or ran out (`FILTER_E_NO_TEXT`).
    NoChunk,
}

/// Walks the chunks for `GetChunk` and `GetText`.
#[derive(Debug, Default)]
pub struct Chunker {
    chunks: Vec<Chunk>,
    /// Index of the current chunk plus one (0: before the first).
    pos: usize,
    /// Units of the current chunk already handed out.
    offset: usize,
}

impl Chunker {
    pub fn new(chunks: Vec<Chunk>) -> Chunker {
        Chunker {
            chunks,
            pos: 0,
            offset: 0,
        }
    }

    /// Moves to the next chunk: its 1-based ID, or `None` at the end.
    pub fn next_chunk(&mut self) -> Option<(u32, &Chunk)> {
        if self.pos > self.chunks.len() {
            return None;
        }
        self.pos += 1;
        self.offset = 0;
        let c = self.chunks.get(self.pos - 1)?;
        Some((self.pos as u32, c))
    }

    /// Copies the current chunk's next units into `out`.
    pub fn read(&mut self, out: &mut [u16]) -> Read {
        let Some(c) = self.pos.checked_sub(1).and_then(|i| self.chunks.get(i)) else {
            return Read::NoChunk;
        };
        let rest = &c.text[self.offset.min(c.text.len())..];
        if rest.is_empty() {
            return Read::NoMore;
        }
        let n = rest.len().min(out.len());
        out[..n].copy_from_slice(&rest[..n]);
        self.offset += n;
        Read::Copied {
            n,
            last: self.offset >= c.text.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(s: &str) -> Chunk {
        Chunk {
            prop: Prop::Contents,
            text: s.encode_utf16().collect(),
            end_of_page: true,
        }
    }

    #[test]
    fn walks_chunks_in_pieces() {
        let mut c = Chunker::new(vec![chunk("hello"), chunk("ab")]);
        let mut buf = [0u16; 3];
        assert_eq!(c.read(&mut buf), Read::NoChunk);
        assert_eq!(c.next_chunk().map(|(id, _)| id), Some(1));
        assert_eq!(c.read(&mut buf), Read::Copied { n: 3, last: false });
        assert_eq!(String::from_utf16_lossy(&buf), "hel");
        assert_eq!(c.read(&mut buf), Read::Copied { n: 2, last: true });
        assert_eq!(String::from_utf16_lossy(&buf[..2]), "lo");
        assert_eq!(c.read(&mut buf), Read::NoMore);
        assert_eq!(c.next_chunk().map(|(id, _)| id), Some(2));
        assert_eq!(c.read(&mut buf), Read::Copied { n: 2, last: true });
        assert!(c.next_chunk().is_none());
        assert_eq!(c.read(&mut buf), Read::NoChunk);
        assert!(c.next_chunk().is_none());
    }

    #[test]
    fn empty_document() {
        let mut c = Chunker::new(Vec::new());
        assert!(c.next_chunk().is_none());
        assert_eq!(c.read(&mut [0; 4]), Read::NoChunk);
    }
}
