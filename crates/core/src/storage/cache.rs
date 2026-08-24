//! In-memory payload cache in front of the SQLite blob store.
//!
//! Pasting always goes through [`Storage::content`]; keeping recently
//! stored/read payloads hot turns the common select-and-paste round trip
//! into a pure memory read. A plain byte-budgeted LRU: no TTL clock to
//! reason about, entries age out by recency, and anything bigger than the
//! whole budget is never cached.
//!
//! Not internally synchronised — [`super::Storage`] owns one alongside its
//! connection and only touches it while holding the connection lock.

use cliphistory_proto::Content;
use std::collections::{HashMap, VecDeque};

/// Byte-bounded LRU over full payloads keyed by entry id.
pub(crate) struct ContentCache {
    budget: usize,
    bytes: usize,
    /// Recency order, coldest first.
    order: VecDeque<i64>,
    map: HashMap<i64, Content>,
}

impl ContentCache {
    /// `budget = 0` disables caching entirely (every op becomes a no-op).
    pub(crate) fn new(budget: usize) -> Self {
        Self {
            budget,
            bytes: 0,
            order: VecDeque::new(),
            map: HashMap::new(),
        }
    }

    pub(crate) fn disabled(&self) -> bool {
        self.budget == 0
    }

    /// Clone the payload for `id`, marking it most-recently-used.
    pub(crate) fn get(&mut self, id: i64) -> Option<Content> {
        if !self.map.contains_key(&id) {
            return None;
        }
        self.touch(id);
        self.map.get(&id).cloned()
    }

    /// Insert or refresh `content`, evicting coldest entries until the
    /// budget holds. Payloads larger than the budget are refused outright.
    pub(crate) fn put(&mut self, id: i64, content: Content) {
        if self.disabled() {
            return;
        }
        let len = content.bytes().len();
        if len > self.budget {
            return;
        }
        if let Some(old) = self.map.insert(id, content) {
            self.bytes -= old.bytes().len();
        }
        self.bytes += len;
        self.touch(id);
        self.evict_over_budget();
    }

    /// Drop every cached copy of the given ids (deletes, prunes, clears).
    pub(crate) fn evict_ids(&mut self, ids: &[i64]) {
        for id in ids {
            if let Some(old) = self.map.remove(id) {
                self.bytes -= old.bytes().len();
                self.order.retain(|&k| k != *id);
            }
        }
    }

    /// Drop everything (used by tests; bulk invalidation goes through
    /// [`evict_ids`](Self::evict_ids)).
    #[cfg(test)]
    pub(crate) fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
        self.bytes = 0;
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.map.len()
    }

    #[cfg(test)]
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }

    /// Move `id` to the most-recently-used end of `order`.
    ///
    /// `Vec::retain`-style rebuilds are fine at clipboard scale: budgets
    /// bound this structure to a handful of entries.
    fn touch(&mut self, id: i64) {
        self.order.retain(|&k| k != id);
        self.order.push_back(id);
    }

    fn evict_over_budget(&mut self) {
        while self.bytes > self.budget {
            let Some(coldest) = self.order.pop_front() else {
                break;
            };
            if let Some(old) = self.map.remove(&coldest) {
                self.bytes -= old.bytes().len();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Content {
        Content::Text { text: s.into() }
    }

    #[test]
    fn hit_and_miss() {
        let mut c = ContentCache::new(100);
        assert_eq!(c.get(1), None);
        c.put(1, text("hello"));
        assert_eq!(c.get(1), Some(text("hello")));
    }

    #[test]
    fn zero_budget_disables_everything() {
        let mut c = ContentCache::new(0);
        c.put(1, text("x"));
        assert!(c.disabled());
        assert_eq!(c.get(1), None);
        assert_eq!(c.len(), 0);
    }

    #[test]
    fn oversized_payloads_are_never_cached() {
        let mut c = ContentCache::new(8);
        c.put(1, text("0123456789"));
        assert_eq!(c.get(1), None);
        // But exactly-fitting ones are.
        c.put(2, text("01234567"));
        assert_eq!(c.get(2), Some(text("01234567")));
    }

    #[test]
    fn eviction_follows_recency() {
        let mut c = ContentCache::new(16);
        c.put(1, text("aaaa")); // 4B
        c.put(2, text("bbbb")); // 4B
        c.put(3, text("cccc")); // 4B
        assert_eq!(c.get(1), Some(text("aaaa"))); // touching 1 beats 3
        c.put(4, text("dddddddd")); // 8B → total 20 > 16 → evict coldest (2)
        assert_eq!(c.get(2), None);
        assert_eq!(c.get(1), Some(text("aaaa")));
        assert_eq!(c.get(3), Some(text("cccc")));
        assert_eq!(c.bytes(), 4 + 4 + 8);
    }

    #[test]
    fn refresh_moves_to_hot_end_without_double_counting() {
        let mut c = ContentCache::new(12);
        c.put(1, text("aaa"));
        c.put(2, text("bbb"));
        c.put(1, text("cc")); // update in place: 3→2 bytes
        assert_eq!(c.bytes(), 2 + 3);
        c.put(3, text("dddddddddd")); // 10B forces eviction of coldest ("bbb")
        assert_eq!(c.get(3), Some(text("dddddddddd")));
        assert_eq!(c.get(1), Some(text("cc"))); // refreshed entry stays hot
        assert_eq!(c.get(2), None);
        assert_eq!(c.len(), 2); // 2B + 10B fit the budget exactly
    }

    #[test]
    fn evict_ids_drops_bytes_exactly() {
        let mut c = ContentCache::new(100);
        c.put(1, text("one"));
        c.put(2, text("two"));
        c.put(3, text("three"));
        let before = c.bytes();
        c.evict_ids(&[2, 99]);
        assert_eq!(c.get(2), None);
        assert_eq!(c.get(1), Some(text("one")));
        assert_eq!(c.bytes(), before - 3);
    }
}
