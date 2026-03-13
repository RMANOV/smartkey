// Prediction cache — LRU-based cache for ensemble predict() results.
//
// Avoids redundant scoring when the prefix and context haven't changed.
// Invalidated on CVM learn, online Markov training, or weight updates.

use std::collections::hash_map::DefaultHasher;
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};

use crate::ensemble::Prediction;

/// Maximum number of cached entries.
const DEFAULT_CAPACITY: usize = 64;

/// Key for a cache entry: (prefix, context_hash).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    prefix: String,
    context_hash: u64,
}

/// A single cache entry storing predictions for a given key.
#[derive(Debug, Clone)]
struct CacheEntry {
    key: CacheKey,
    predictions: Vec<Prediction>,
}

/// VecDeque-based LRU prediction cache.
///
/// On hit, the entry is moved to the back (most recently used).
/// On miss + insert, the oldest entry (front) is evicted if at capacity.
///
/// Thread safety: wrapped in `Mutex` inside `SmartKeyEngine` (required by
/// PyO3's Send+Sync bounds). TSF/IMK/IBus are single-threaded, but the
/// Mutex satisfies Rust's thread-safety requirements for the Python binding.
#[derive(Debug)]
pub struct PredictionCache {
    entries: VecDeque<CacheEntry>,
    capacity: usize,
}

impl PredictionCache {
    /// Create a new cache with the default capacity (64).
    pub fn new() -> Self {
        Self {
            entries: VecDeque::with_capacity(DEFAULT_CAPACITY),
            capacity: DEFAULT_CAPACITY,
        }
    }

    /// Create a new cache with the given capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// Look up cached predictions for the given prefix and context.
    ///
    /// Returns `Some(predictions)` on hit (and promotes the entry to MRU).
    /// Returns `None` on miss.
    pub fn get(&mut self, prefix: &str, context: &[&str]) -> Option<Vec<Prediction>> {
        let key = Self::make_key(prefix, context);

        // Linear scan is fine for capacity ≤ 128.
        if let Some(idx) = self.entries.iter().position(|e| e.key == key) {
            // Move to back (MRU).
            let entry = self.entries.remove(idx).unwrap();
            let predictions = entry.predictions.clone();
            self.entries.push_back(entry);
            Some(predictions)
        } else {
            None
        }
    }

    /// Store predictions for the given prefix and context.
    pub fn put(&mut self, prefix: &str, context: &[&str], predictions: Vec<Prediction>) {
        let key = Self::make_key(prefix, context);

        // Remove existing entry for this key (if any).
        if let Some(idx) = self.entries.iter().position(|e| e.key == key) {
            self.entries.remove(idx);
        }

        // Evict LRU if at capacity.
        if self.entries.len() >= self.capacity {
            self.entries.pop_front();
        }

        self.entries.push_back(CacheEntry { key, predictions });
    }

    /// Invalidate all cached entries.
    ///
    /// Called when the underlying model changes (CVM learn, online Markov
    /// training, weight updates).
    pub fn invalidate(&mut self) {
        self.entries.clear();
    }

    /// Number of cached entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Build a cache key from prefix and context slice.
    fn make_key(prefix: &str, context: &[&str]) -> CacheKey {
        let mut hasher = DefaultHasher::new();
        context.hash(&mut hasher);
        CacheKey {
            prefix: prefix.to_string(),
            context_hash: hasher.finish(),
        }
    }
}

impl Default for PredictionCache {
    fn default() -> Self {
        Self::new()
    }
}

// ======================================================================
// Tests
// ======================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn pred(word: &str, score: f64) -> Prediction {
        Prediction {
            word: word.to_string(),
            score,
            confidence: 1.0,
        }
    }

    #[test]
    fn test_cache_hit_returns_same_results() {
        let mut cache = PredictionCache::new();
        let preds = vec![pred("hello", 0.9), pred("help", 0.7)];
        cache.put("hel", &["i"], preds.clone());

        let hit = cache.get("hel", &["i"]);
        assert!(hit.is_some());
        let hit = hit.unwrap();
        assert_eq!(hit.len(), 2);
        assert_eq!(hit[0].word, "hello");
        assert_eq!(hit[1].word, "help");
    }

    #[test]
    fn test_cache_miss_on_different_prefix() {
        let mut cache = PredictionCache::new();
        cache.put("hel", &["i"], vec![pred("hello", 0.9)]);

        assert!(cache.get("wor", &["i"]).is_none());
    }

    #[test]
    fn test_cache_miss_on_different_context() {
        let mut cache = PredictionCache::new();
        cache.put("hel", &["i"], vec![pred("hello", 0.9)]);

        assert!(cache.get("hel", &["you"]).is_none());
    }

    #[test]
    fn test_cache_invalidated_on_commit() {
        let mut cache = PredictionCache::new();
        cache.put("hel", &["i"], vec![pred("hello", 0.9)]);
        assert!(!cache.is_empty());

        cache.invalidate();
        assert!(cache.is_empty());
        assert!(cache.get("hel", &["i"]).is_none());
    }

    #[test]
    fn test_lru_eviction() {
        let mut cache = PredictionCache::with_capacity(2);
        cache.put("a", &[], vec![pred("apple", 0.9)]);
        cache.put("b", &[], vec![pred("banana", 0.8)]);
        // Cache full: [a, b]

        // Access "a" to make it MRU: [b, a]
        cache.get("a", &[]);

        // Insert "c" — should evict "b" (LRU): [a, c]
        cache.put("c", &[], vec![pred("cherry", 0.7)]);

        assert!(cache.get("a", &[]).is_some());
        assert!(cache.get("b", &[]).is_none()); // evicted
        assert!(cache.get("c", &[]).is_some());
    }
}
