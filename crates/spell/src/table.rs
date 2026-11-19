//! The dictionary's words: one string arena, interned flag sets and an open-addressing
//! hash table. Homonyms (the same word with other flags) are separate entries.

use std::collections::HashMap;

use crate::aff::{Flag, Flags};

#[derive(Clone, Copy, Debug)]
struct Entry {
    start: u32,
    len: u32,
    flags: u32,
}

#[derive(Debug, Default)]
pub(crate) struct WordTable {
    text: String,
    entries: Vec<Entry>,
    flag_sets: Vec<Flags>,
    interned: HashMap<Flags, u32>,
    /// Entry index + 1; 0 is empty.
    slots: Vec<u32>,
}

fn hash(s: &[u8]) -> u64 {
    let mut h = s.len() as u64;
    for chunk in s.chunks(8) {
        let mut b = [0u8; 8];
        b[..chunk.len()].copy_from_slice(chunk);
        h = (h.rotate_left(5) ^ u64::from_le_bytes(b)).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
    h ^ (h >> 29)
}

impl WordTable {
    pub(crate) fn with_capacity(words: usize, bytes: usize) -> Self {
        let mut t = WordTable {
            text: String::with_capacity(bytes),
            entries: Vec::with_capacity(words),
            ..WordTable::default()
        };
        t.intern(Flags::default());
        t
    }

    fn intern(&mut self, flags: Flags) -> u32 {
        if let Some(&i) = self.interned.get(&flags) {
            return i;
        }
        let i = self.flag_sets.len() as u32;
        self.flag_sets.push(flags.clone());
        self.interned.insert(flags, i);
        i
    }

    /// Adds a word; [`WordTable::finish`] makes it findable.
    pub(crate) fn push(&mut self, word: &str, flags: Flags) {
        let flags = self.intern(flags);
        self.entries.push(Entry {
            start: self.text.len() as u32,
            len: word.len() as u32,
            flags,
        });
        self.text.push_str(word);
    }

    pub(crate) fn finish(&mut self) {
        self.interned = HashMap::new();
        self.flag_sets.shrink_to_fit();
        self.text.shrink_to_fit();
        self.entries.shrink_to_fit();
        let cap = (self.entries.len() * 2).next_power_of_two().max(16);
        let mask = cap - 1;
        let mut slots = vec![0u32; cap];
        for (i, e) in self.entries.iter().enumerate() {
            let w = &self.text.as_bytes()[e.start as usize..(e.start + e.len) as usize];
            let mut p = hash(w) as usize & mask;
            while slots[p] != 0 {
                p = (p + 1) & mask;
            }
            slots[p] = i as u32 + 1;
        }
        self.slots = slots;
    }

    fn word(&self, e: &Entry) -> &str {
        &self.text[e.start as usize..(e.start + e.len) as usize]
    }

    /// The flag sets of `word`'s homonyms.
    pub(crate) fn get<'a>(&'a self, word: &'a str) -> Homonyms<'a> {
        let mask = self.slots.len().wrapping_sub(1);
        Homonyms {
            table: self,
            word,
            pos: if self.slots.is_empty() {
                usize::MAX
            } else {
                hash(word.as_bytes()) as usize & mask
            },
        }
    }

    pub(crate) fn contains(&self, word: &str) -> bool {
        self.get(word).next().is_some()
    }

    /// Every word with its flags.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&str, &[Flag])> {
        self.entries
            .iter()
            .map(|e| (self.word(e), &*self.flag_sets[e.flags as usize]))
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Homonyms<'a> {
    table: &'a WordTable,
    word: &'a str,
    pos: usize,
}

impl<'a> Iterator for Homonyms<'a> {
    type Item = &'a [Flag];

    fn next(&mut self) -> Option<&'a [Flag]> {
        let t = self.table;
        if self.pos == usize::MAX {
            return None;
        }
        let mask = t.slots.len() - 1;
        loop {
            let s = t.slots[self.pos];
            if s == 0 {
                self.pos = usize::MAX;
                return None;
            }
            self.pos = (self.pos + 1) & mask;
            let e = &t.entries[s as usize - 1];
            if t.word(e) == self.word {
                return Some(&t.flag_sets[e.flags as usize]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn homonyms() {
        let mut t = WordTable::with_capacity(4, 16);
        t.push("ab", Box::new([1]));
        t.push("cd", Box::new([]));
        t.push("ab", Box::new([2, 3]));
        t.finish();
        let ab: Vec<_> = t.get("ab").collect();
        assert_eq!(ab, [&[1][..], &[2, 3][..]]);
        assert!(t.contains("cd") && !t.contains("a") && !t.contains("abc"));
        assert_eq!(t.len(), 3);
    }
}
