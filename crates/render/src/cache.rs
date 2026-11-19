//! A size-bounded map evicting the least recently used entries.

use std::collections::HashMap;
use std::hash::Hash;

use parking_lot::Mutex;

#[derive(Debug)]
struct Entry<V> {
    value: V,
    size: usize,
    used: u64,
}

#[derive(Debug)]
struct State<K, V> {
    map: HashMap<K, Entry<V>>,
    bytes: usize,
    clock: u64,
}

#[derive(Debug)]
pub struct Lru<K, V> {
    state: Mutex<State<K, V>>,
    budget: usize,
}

impl<K: Hash + Eq + Clone, V: Clone> Lru<K, V> {
    pub fn new(budget: usize) -> Self {
        Lru {
            state: Mutex::new(State {
                map: HashMap::new(),
                bytes: 0,
                clock: 0,
            }),
            budget,
        }
    }

    pub fn get(&self, k: &K) -> Option<V> {
        let mut s = self.state.lock();
        s.clock += 1;
        let c = s.clock;
        let e = s.map.get_mut(k)?;
        e.used = c;
        Some(e.value.clone())
    }

    pub fn insert(&self, k: K, value: V, size: usize) {
        let mut s = self.state.lock();
        s.clock += 1;
        let used = s.clock;
        if let Some(old) = s.map.insert(k, Entry { value, size, used }) {
            s.bytes -= old.size;
        }
        s.bytes += size;
        if s.bytes > self.budget {
            // Evict the oldest entries down to three quarters of the budget.
            let mut ages: Vec<(u64, K)> = s.map.iter().map(|(k, e)| (e.used, k.clone())).collect();
            ages.sort_unstable_by_key(|a| a.0);
            let target = self.budget / 4 * 3;
            for (_, k) in ages {
                if s.bytes <= target {
                    break;
                }
                if let Some(e) = s.map.remove(&k) {
                    s.bytes -= e.size;
                }
            }
        }
    }

    /// Updates the size of an entry (values that grow, such as lazily built mip levels).
    pub fn resize(&self, k: &K, size: usize) {
        let mut s = self.state.lock();
        if let Some(e) = s.map.get_mut(k) {
            let old = std::mem::replace(&mut e.size, size);
            s.bytes = s.bytes - old + size;
        }
    }

    pub fn clear(&self) {
        let mut s = self.state.lock();
        s.map.clear();
        s.bytes = 0;
    }
}
