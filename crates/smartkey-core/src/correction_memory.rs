// Correction memory — tracks when the user repeatedly corrects a prediction.
//
// When the same (context_hash, predicted_prefix) → actual mapping occurs
// ≥ suppression_threshold times, the prediction is suppressed and replaced.
// Uses LRU eviction at max_entries to bound memory.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A single correction entry: how many times (context, predicted) was corrected to actual.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorrectionEntry {
    /// Hash of the 2-word context preceding the correction.
    pub context_hash: u64,
    /// The prefix that was predicted (and rejected).
    pub predicted_prefix: String,
    /// What the user actually typed.
    pub actual: String,
    /// Number of times this correction occurred.
    pub count: u32,
    /// Monotonic access counter for LRU eviction.
    #[serde(default)]
    access_order: u64,
}

/// Snapshot for serialization (PersonalProfile v4).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CorrectionSnapshot {
    pub entries: Vec<CorrectionEntry>,
}

/// Tracks repeated corrections to suppress bad predictions.
pub struct CorrectionMemory {
    entries: HashMap<(u64, String), CorrectionEntry>,
    max_entries: usize,
    suppression_threshold: u32,
    access_counter: u64,
}

impl CorrectionMemory {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            max_entries: 500,
            suppression_threshold: 3,
            access_counter: 0,
        }
    }

    /// Record that (context, predicted) was corrected to actual.
    pub fn record(&mut self, context_hash: u64, predicted_prefix: &str, actual: &str) {
        self.access_counter += 1;
        let key = (context_hash, predicted_prefix.to_string());

        let entry = self.entries.entry(key).or_insert_with(|| CorrectionEntry {
            context_hash,
            predicted_prefix: predicted_prefix.to_string(),
            actual: actual.to_string(),
            count: 0,
            access_order: self.access_counter,
        });
        entry.count += 1;
        entry.actual = actual.to_string();
        entry.access_order = self.access_counter;

        // LRU eviction if over capacity.
        if self.entries.len() > self.max_entries {
            self.evict_lru();
        }
    }

    /// Check if a prediction should be overridden for the given context.
    /// Returns Some(replacement) if the predicted prefix has been corrected
    /// at least `suppression_threshold` times in this context.
    pub fn check(&mut self, context_hash: u64, predicted_prefix: &str) -> Option<String> {
        let key = (context_hash, predicted_prefix.to_string());
        if let Some(entry) = self.entries.get_mut(&key) {
            if entry.count >= self.suppression_threshold {
                self.access_counter += 1;
                entry.access_order = self.access_counter;
                return Some(entry.actual.clone());
            }
        }
        None
    }

    /// Compute context hash from the last 2 words.
    pub fn context_hash(prev1: Option<&str>, prev2: Option<&str>) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        prev2.unwrap_or("").hash(&mut hasher);
        prev1.unwrap_or("").hash(&mut hasher);
        hasher.finish()
    }

    pub fn to_snapshot(&self) -> CorrectionSnapshot {
        CorrectionSnapshot {
            entries: self.entries.values().cloned().collect(),
        }
    }

    pub fn from_snapshot(snapshot: &CorrectionSnapshot) -> Self {
        let mut mem = Self::new();
        for entry in &snapshot.entries {
            let key = (entry.context_hash, entry.predicted_prefix.clone());
            mem.entries.insert(key, entry.clone());
        }
        mem
    }

    fn evict_lru(&mut self) {
        if let Some(oldest_key) = self
            .entries
            .iter()
            .min_by_key(|(_, e)| e.access_order)
            .map(|(k, _)| k.clone())
        {
            self.entries.remove(&oldest_key);
        }
    }
}

impl Default for CorrectionMemory {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_check_below_threshold() {
        let mut mem = CorrectionMemory::new();
        let hash = CorrectionMemory::context_hash(Some("the"), Some("quick"));
        mem.record(hash, "fox", "cat");
        mem.record(hash, "fox", "cat");
        // Only 2 corrections — threshold is 3, should not suppress yet
        assert!(mem.check(hash, "fox").is_none());
    }

    #[test]
    fn record_and_check_at_threshold() {
        let mut mem = CorrectionMemory::new();
        let hash = CorrectionMemory::context_hash(Some("the"), Some("quick"));
        mem.record(hash, "fox", "cat");
        mem.record(hash, "fox", "cat");
        mem.record(hash, "fox", "cat");
        // 3 corrections — should now suppress
        let result = mem.check(hash, "fox");
        assert_eq!(result, Some("cat".to_string()));
    }

    #[test]
    fn context_hash_is_deterministic() {
        let h1 = CorrectionMemory::context_hash(Some("hello"), Some("world"));
        let h2 = CorrectionMemory::context_hash(Some("hello"), Some("world"));
        assert_eq!(h1, h2);
    }

    #[test]
    fn context_hash_differs_for_different_context() {
        let h1 = CorrectionMemory::context_hash(Some("hello"), Some("world"));
        let h2 = CorrectionMemory::context_hash(Some("foo"), Some("bar"));
        assert_ne!(h1, h2);
    }

    #[test]
    fn context_hash_none_context() {
        let h1 = CorrectionMemory::context_hash(None, None);
        let h2 = CorrectionMemory::context_hash(None, None);
        assert_eq!(h1, h2);
    }

    #[test]
    fn different_predicted_prefixes_tracked_independently() {
        let mut mem = CorrectionMemory::new();
        let hash = CorrectionMemory::context_hash(Some("the"), None);
        for _ in 0..3 {
            mem.record(hash, "wrong1", "right");
        }
        // "wrong2" has zero corrections — should not suppress
        assert!(mem.check(hash, "wrong2").is_none());
        // "wrong1" has 3 corrections — should suppress
        assert!(mem.check(hash, "wrong1").is_some());
    }

    #[test]
    fn actual_updates_on_new_correction() {
        let mut mem = CorrectionMemory::new();
        let hash = CorrectionMemory::context_hash(Some("a"), Some("b"));
        mem.record(hash, "pred", "first");
        mem.record(hash, "pred", "first");
        // Update "actual" to something different on the 3rd correction
        mem.record(hash, "pred", "second");
        let result = mem.check(hash, "pred");
        assert_eq!(result, Some("second".to_string()));
    }

    #[test]
    fn snapshot_round_trip() {
        let mut mem = CorrectionMemory::new();
        let hash = CorrectionMemory::context_hash(Some("x"), Some("y"));
        for _ in 0..3 {
            mem.record(hash, "bad", "good");
        }
        let snapshot = mem.to_snapshot();
        assert_eq!(snapshot.entries.len(), 1);
        let restored = CorrectionMemory::from_snapshot(&snapshot);
        let snap2 = restored.to_snapshot();
        assert_eq!(snap2.entries.len(), 1);
        assert_eq!(snap2.entries[0].count, 3);
    }

    #[test]
    fn lru_eviction_keeps_within_capacity() {
        // Use a small capacity by manually filling
        let mut mem = CorrectionMemory::new();
        // Fill to just above max_entries (500)
        for i in 0..502u64 {
            let hash = i;
            mem.record(hash, "pred", "actual");
        }
        // After eviction, entries should be <= max_entries
        assert!(mem.entries.len() <= 500);
    }
}
