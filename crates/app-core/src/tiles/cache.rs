//! Rendered tiles under a memory budget, least recently used first out.

use std::collections::{BTreeMap, HashMap};

use super::Key;

#[derive(Debug)]
struct Entry<B> {
    buf: B,
    bytes: usize,
    used: u64,
    /// Drawn from an older revision of the document: shown until replaced.
    stale: bool,
}

#[derive(Debug)]
pub struct TileCache<B> {
    map: HashMap<Key, Entry<B>>,
    /// Per page, how many tiles of each bucket are cached (for fallbacks).
    levels: HashMap<u32, BTreeMap<i16, u32>>,
    bytes: usize,
    budget: usize,
    tick: u64,
}

impl<B> TileCache<B> {
    pub fn new(budget: usize) -> Self {
        TileCache {
            map: HashMap::new(),
            levels: HashMap::new(),
            bytes: 0,
            budget,
            tick: 1,
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn contains(&self, k: &Key) -> bool {
        self.map.contains_key(k)
    }

    /// Cached and drawn from the current document.
    pub fn is_fresh(&self, k: &Key) -> bool {
        self.map.get(k).is_some_and(|e| !e.stale)
    }

    pub fn is_stale(&self, k: &Key) -> bool {
        self.map.get(k).is_some_and(|e| e.stale)
    }

    /// Marks the tiles of `pages` (all when `None`) to be rendered again; they stay shown
    /// until their replacements arrive.
    pub fn mark_stale(&mut self, pages: Option<&[usize]>) {
        for (k, e) in &mut self.map {
            if pages.is_none_or(|p| p.contains(&(k.page as usize))) {
                e.stale = true;
            }
        }
    }

    pub fn get(&self, k: &Key) -> Option<&B> {
        self.map.get(k).map(|e| &e.buf)
    }

    /// Marks `k` as in use this round, so it is not evicted; returns whether it is cached.
    pub fn touch(&mut self, k: &Key) -> bool {
        let tick = self.tick;
        self.map.get_mut(k).map(|e| e.used = tick).is_some()
    }

    /// Starts a new round of [`TileCache::touch`]es.
    pub fn next_round(&mut self) {
        self.tick += 1;
    }

    /// The buckets with cached tiles of `page`.
    pub fn levels(&self, page: u32) -> impl Iterator<Item = i16> + '_ {
        self.levels
            .get(&page)
            .into_iter()
            .flat_map(|m| m.keys().copied())
    }

    pub fn insert(&mut self, k: Key, buf: B, bytes: usize) {
        let tick = self.tick;
        let old = self.map.insert(
            k,
            Entry {
                buf,
                bytes,
                used: tick,
                stale: false,
            },
        );
        match old {
            Some(old) => self.bytes -= old.bytes,
            None => {
                *self
                    .levels
                    .entry(k.page)
                    .or_default()
                    .entry(k.level)
                    .or_default() += 1
            }
        }
        self.bytes += bytes;
    }

    /// Drops the least recently used tiles not touched this round until under budget.
    pub fn evict(&mut self) {
        if self.bytes <= self.budget {
            return;
        }
        let mut old: Vec<(u64, Key)> = self
            .map
            .iter()
            .filter(|(_, e)| e.used < self.tick)
            .map(|(k, e)| (e.used, *k))
            .collect();
        old.sort_unstable();
        // Go a little below the budget so this does not run on every new tile.
        let target = self.budget / 10 * 9;
        for (_, k) in old {
            if self.bytes <= target {
                break;
            }
            self.remove(&k);
        }
    }

    fn remove(&mut self, k: &Key) {
        if let Some(e) = self.map.remove(k) {
            self.bytes -= e.bytes;
            if let Some(levels) = self.levels.get_mut(&k.page)
                && let Some(n) = levels.get_mut(&k.level)
            {
                *n -= 1;
                if *n == 0 {
                    levels.remove(&k.level);
                }
            }
        }
    }
}
