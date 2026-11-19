//! Stable widget slots for a changing set of items: an item keeps its slot while it stays
//! wanted, so the UI does not rebuild (or re-upload) what is already on screen.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotChange<K> {
    Assign(usize, K),
    Clear(usize),
}

#[derive(Debug)]
pub struct SlotPool<K> {
    slots: Vec<Option<K>>,
    index: HashMap<K, usize>,
    /// Free slots, the lowest last.
    free: Vec<usize>,
}

impl<K> Default for SlotPool<K> {
    fn default() -> Self {
        SlotPool {
            slots: Vec::new(),
            index: HashMap::new(),
            free: Vec::new(),
        }
    }
}

impl<K: Copy + Eq + Hash> SlotPool<K> {
    /// Number of slots, used or free.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    pub fn slot_of(&self, k: &K) -> Option<usize> {
        self.index.get(k).copied()
    }

    /// Frees the slots of items no longer wanted and gives new items a slot. A slot past
    /// the current [`SlotPool::len`] in an `Assign` is a new one.
    pub fn sync(&mut self, wanted: impl IntoIterator<Item = K>) -> Vec<SlotChange<K>> {
        let wanted: Vec<K> = wanted.into_iter().collect();
        let keep: HashSet<&K> = wanted.iter().collect();
        let mut changes = Vec::new();
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if let Some(k) = slot
                && !keep.contains(k)
            {
                self.index.remove(k);
                *slot = None;
                self.free.push(i);
                changes.push(SlotChange::Clear(i));
            }
        }
        self.free.sort_unstable_by(|a, b| b.cmp(a));
        for k in wanted {
            if self.index.contains_key(&k) {
                continue;
            }
            let i = self.free.pop().unwrap_or_else(|| {
                self.slots.push(None);
                self.slots.len() - 1
            });
            self.slots[i] = Some(k);
            self.index.insert(k, i);
            // A cleared slot that is assigned again needs only the assignment.
            changes.retain(|c| *c != SlotChange::Clear(i));
            changes.push(SlotChange::Assign(i, k));
        }
        changes
    }

    pub fn clear(&mut self) {
        *self = SlotPool::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_keep_their_slots() {
        let mut p = SlotPool::default();
        assert_eq!(
            p.sync([1, 2, 3]),
            [
                SlotChange::Assign(0, 1),
                SlotChange::Assign(1, 2),
                SlotChange::Assign(2, 3)
            ]
        );
        assert_eq!(p.sync([2, 3, 4]), [SlotChange::Assign(0, 4)]);
        assert_eq!(p.slot_of(&3), Some(2));
        assert_eq!(p.sync([4]), [SlotChange::Clear(1), SlotChange::Clear(2)]);
        assert_eq!(p.sync([4, 5]), [SlotChange::Assign(1, 5)]);
        assert_eq!(p.len(), 3);
    }
}
