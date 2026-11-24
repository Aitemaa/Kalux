//! Byte-budgeted LRU cache.

use std::collections::HashSet;
use std::hash::Hash;

/// Anything with a memory weight in bytes.
pub trait Weighted {
    fn weight(&self) -> usize;
}

impl Weighted for crate::DecodedImage {
    fn weight(&self) -> usize {
        self.byte_size()
    }
}

impl<T: Weighted> Weighted for std::sync::Arc<T> {
    fn weight(&self) -> usize {
        (**self).weight()
    }
}

struct Entry<K, V> {
    key: K,
    value: V,
    weight: usize,
}

/// LRU keyed by `K`, evicting least-recently-used entries until total weight <= budget.
/// An entry heavier than the whole budget is still stored (it becomes the only entry) so the
/// current image is never refused. `pinned` keys are never evicted.
pub struct ByteLru<K, V> {
    budget: usize,
    total_weight: usize,
    /// Ordered from least-recently-used (index 0) to most-recently-used (back).
    entries: Vec<Entry<K, V>>,
    pinned: HashSet<K>,
}

impl<K: Eq + Hash + Clone, V: Weighted> ByteLru<K, V> {
    pub fn new(budget_bytes: usize) -> Self {
        Self {
            budget: budget_bytes,
            total_weight: 0,
            entries: Vec::new(),
            pinned: HashSet::new(),
        }
    }

    /// Lookup and mark as most recently used.
    pub fn get(&mut self, key: &K) -> Option<&V> {
        let idx = self.entries.iter().position(|e| &e.key == key)?;
        self.entries[idx..].rotate_left(1);
        self.entries.last().map(|e| &e.value)
    }

    /// Lookup without changing recency order.
    pub fn peek(&self, key: &K) -> Option<&V> {
        self.entries
            .iter()
            .find(|e| &e.key == key)
            .map(|e| &e.value)
    }

    pub fn contains(&self, key: &K) -> bool {
        self.entries.iter().any(|e| &e.key == key)
    }

    /// Insert/replace, then evict. Returns evicted entries.
    pub fn insert(&mut self, key: K, value: V) -> Vec<(K, V)> {
        let weight = value.weight();
        if let Some(idx) = self.entries.iter().position(|e| e.key == key) {
            let old = self.entries.remove(idx);
            self.total_weight = self.total_weight.saturating_sub(old.weight);
        }
        self.entries.push(Entry { key, value, weight });
        self.total_weight = self.total_weight.saturating_add(weight);

        let mut evicted = Vec::new();
        while self.total_weight > self.budget {
            // Never evict the just-inserted entry (at the back) or any pinned entry.
            let last_idx = self.entries.len() - 1;
            let Some(victim_idx) = self.entries[..last_idx]
                .iter()
                .position(|e| !self.pinned.contains(&e.key))
            else {
                break;
            };
            let victim = self.entries.remove(victim_idx);
            self.total_weight = self.total_weight.saturating_sub(victim.weight);
            evicted.push((victim.key, victim.value));
        }
        evicted
    }

    pub fn remove(&mut self, key: &K) -> Option<V> {
        let idx = self.entries.iter().position(|e| &e.key == key)?;
        let entry = self.entries.remove(idx);
        self.total_weight = self.total_weight.saturating_sub(entry.weight);
        Some(entry.value)
    }

    /// Replace the pinned set (e.g. current + neighbors). Pinned entries are skipped by eviction.
    pub fn set_pinned(&mut self, keys: &[K]) {
        self.pinned.clear();
        self.pinned.extend(keys.iter().cloned());
    }

    pub fn total_weight(&self) -> usize {
        self.total_weight
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Eq)]
    struct Blob(usize);

    impl Weighted for Blob {
        fn weight(&self) -> usize {
            self.0
        }
    }

    #[test]
    fn evicts_lru_order_and_respects_get_recency() {
        let mut lru = ByteLru::new(100);
        assert!(lru.is_empty());
        assert!(lru.insert("a", Blob(40)).is_empty());
        assert!(lru.insert("b", Blob(40)).is_empty());
        assert_eq!(lru.total_weight(), 80);

        // Touch "a" so "b" becomes LRU
        assert_eq!(lru.get(&"a"), Some(&Blob(40)));
        assert_eq!(lru.peek(&"b"), Some(&Blob(40))); // peek must not make "b" MRU

        let evicted = lru.insert("c", Blob(40));
        assert_eq!(evicted, vec![("b", Blob(40))]);
        assert!(!lru.contains(&"b"));
        assert!(lru.contains(&"a"));
        assert!(lru.contains(&"c"));
        assert_eq!(lru.total_weight(), 80);
    }

    #[test]
    fn pinned_keys_are_never_evicted() {
        let mut lru = ByteLru::new(100);
        lru.set_pinned(&["a"]);
        lru.insert("a", Blob(60));
        lru.insert("b", Blob(40));

        // Inserting "c" (60) must evict "b", NOT pinned "a" (even though "a" is older)
        let evicted = lru.insert("c", Blob(60));
        assert_eq!(evicted, vec![("b", Blob(40))]);
        assert!(lru.contains(&"a"));
        assert!(lru.contains(&"c"));
        assert_eq!(lru.total_weight(), 120);

        // Unpin "a"; next insert evicts "a"
        lru.set_pinned(&[]);
        let evicted2 = lru.insert("d", Blob(40));
        assert_eq!(evicted2, vec![("a", Blob(60))]);
        assert_eq!(lru.total_weight(), 100);
    }

    #[test]
    fn oversized_single_entry_is_still_stored() {
        let mut lru = ByteLru::new(100);
        lru.insert("small", Blob(50));
        let evicted = lru.insert("huge", Blob(500));
        assert_eq!(evicted, vec![("small", Blob(50))]);
        assert_eq!(lru.len(), 1);
        assert!(lru.contains(&"huge"));
        assert_eq!(lru.total_weight(), 500);
    }

    #[test]
    fn replace_and_remove_update_weight() {
        let mut lru = ByteLru::new(100);
        lru.insert("k", Blob(30));
        assert_eq!(lru.total_weight(), 30);
        lru.insert("k", Blob(70));
        assert_eq!(lru.len(), 1);
        assert_eq!(lru.total_weight(), 70);
        assert_eq!(lru.remove(&"k"), Some(Blob(70)));
        assert_eq!(lru.total_weight(), 0);
        assert!(lru.is_empty());
    }
}
