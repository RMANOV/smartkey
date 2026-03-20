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
