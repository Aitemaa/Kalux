//! Byte-budgeted LRU cache.

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

/// LRU keyed by `K`, evicting least-recently-used entries until total weight <= budget.
/// An entry heavier than the whole budget is still stored (it becomes the only entry) so the
/// current image is never refused. `pinned` keys are never evicted.
pub struct ByteLru<K, V> {
    _budget: usize,
    _p: std::marker::PhantomData<(K, V)>,
}

impl<K: Eq + Hash + Clone, V: Weighted> ByteLru<K, V> {
    pub fn new(budget_bytes: usize) -> Self {
        let _ = budget_bytes;
        todo!("kalux-core")
    }
    /// Lookup and mark as most recently used.
    pub fn get(&mut self, key: &K) -> Option<&V> {
        let _ = key;
        todo!("kalux-core")
    }
    pub fn contains(&self, key: &K) -> bool {
        let _ = key;
        todo!("kalux-core")
    }
    /// Insert/replace, then evict. Returns evicted entries.
    pub fn insert(&mut self, key: K, value: V) -> Vec<(K, V)> {
        let _ = (key, value);
        todo!("kalux-core")
    }
    pub fn remove(&mut self, key: &K) -> Option<V> {
        let _ = key;
        todo!("kalux-core")
    }
    /// Replace the pinned set (e.g. current + neighbors). Pinned entries are skipped by eviction.
    pub fn set_pinned(&mut self, keys: &[K]) {
        let _ = keys;
        todo!("kalux-core")
    }
    pub fn total_weight(&self) -> usize {
        todo!("kalux-core")
    }
    pub fn len(&self) -> usize {
        todo!("kalux-core")
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
