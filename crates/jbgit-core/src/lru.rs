//! Tiny bounded LRU cache for commit details / diffs (capacity ~256, so the
//! O(n) recency bookkeeping is cheaper than a linked map).

use std::collections::VecDeque;
use std::hash::Hash;

use rustc_hash::FxHashMap;

#[derive(Debug, Clone)]
pub struct LruCache<K, V> {
    capacity: usize,
    map: FxHashMap<K, V>,
    order: VecDeque<K>,
}

impl<K: Clone + Eq + Hash, V> LruCache<K, V> {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            map: FxHashMap::default(),
            order: VecDeque::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn contains(&self, key: &K) -> bool {
        self.map.contains_key(key)
    }

    /// Looks up `key` without changing recency.
    pub fn peek(&self, key: &K) -> Option<&V> {
        self.map.get(key)
    }

    /// Looks up `key` and marks it most recently used.
    pub fn get(&mut self, key: &K) -> Option<&V> {
        if self.map.contains_key(key) {
            self.touch(key);
        }
        self.map.get(key)
    }

    pub fn insert(&mut self, key: K, value: V) {
        if self.map.insert(key.clone(), value).is_some() {
            self.touch(&key);
            return;
        }
        self.order.push_back(key);
        while self.map.len() > self.capacity {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            } else {
                break;
            }
        }
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
    }

    fn touch(&mut self, key: &K) {
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            let k = self.order.remove(pos).expect("position valid");
            self.order.push_back(k);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicts_least_recently_used() {
        let mut c = LruCache::new(2);
        c.insert(1, "a");
        c.insert(2, "b");
        assert_eq!(c.get(&1), Some(&"a"));
        c.insert(3, "c");
        assert!(c.contains(&1) && c.contains(&3) && !c.contains(&2));
        assert_eq!(c.len(), 2);
    }
}
