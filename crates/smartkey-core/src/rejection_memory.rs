// Rejection memory — session-scoped suppression of repeatedly-rejected ghosts.
//
// When the user rejects the SAME ghost completion for the SAME typed prefix
// `SUPPRESS_THRESHOLD` (K=2) times within a session, `is_suppressed` returns
// true so the ghost is not offered again for the rest of the session.
//
// Unlike `CorrectionMemory`, this is intentionally **NOT persisted**: it lives
// only in the running engine process and dies with it. That process lifetime
// *is* the required session-scope decay — restart the engine and the slate is
// clean. It is deliberately never wired into `PersonalProfile` serialization.
//
// Bounded to `MAX_PREFIXES` distinct prefixes via LRU (by most-recent record),
// so a long session cannot grow it without bound.

use std::collections::HashMap;

/// Number of rejections of the same (prefix, completion) that trips suppression.
const SUPPRESS_THRESHOLD: u32 = 2;

/// Maximum distinct typed prefixes tracked before LRU eviction kicks in.
const MAX_PREFIXES: usize = 256;

/// Rejection counts for a single typed prefix, plus an LRU access stamp.
struct PrefixEntry {
    /// Rejected completion (lowercased) -> number of rejections in this session.
    completions: HashMap<String, u32>,
    /// Monotonic stamp of the last `record` that touched this prefix (LRU).
    access_order: u64,
}

/// Session-scoped memory of ghost completions the user has rejected.
///
/// Keyed by typed prefix (lowercased, trimmed) → rejected completion
/// (lowercased) → count. Purely in-memory; see the module docs for why.
pub struct RejectionMemory {
    entries: HashMap<String, PrefixEntry>,
    max_prefixes: usize,
    suppression_threshold: u32,
    access_counter: u64,
}

impl RejectionMemory {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            max_prefixes: MAX_PREFIXES,
            suppression_threshold: SUPPRESS_THRESHOLD,
            access_counter: 0,
        }
    }

    /// Normalize a prefix/completion the same way on record and query:
    /// trim surrounding whitespace, then lowercase.
    fn norm(value: &str) -> String {
        value.trim().to_lowercase()
    }

    /// Record that `completion` was rejected for the given typed `prefix`.
    ///
    /// No-op when either side normalizes to empty (nothing meaningful to key on).
    pub fn record(&mut self, prefix: &str, completion: &str) {
        let prefix = Self::norm(prefix);
        let completion = Self::norm(completion);
        if prefix.is_empty() || completion.is_empty() {
            return;
        }

        self.access_counter += 1;
        let stamp = self.access_counter;

        let entry = self.entries.entry(prefix).or_insert_with(|| PrefixEntry {
            completions: HashMap::new(),
            access_order: stamp,
        });
        entry.access_order = stamp;
        let count = entry.completions.entry(completion).or_insert(0);
        *count = count.saturating_add(1);

        // Bound the number of distinct prefixes (LRU by most-recent record).
        if self.entries.len() > self.max_prefixes {
            self.evict_lru();
        }
    }

    /// Whether `completion` for `prefix` has been rejected at least `K` times
    /// this session and must therefore no longer be offered.
    ///
    /// Read-only: querying never mutates the LRU order, so the hot ghost path
    /// can call it with a shared borrow.
    pub fn is_suppressed(&self, prefix: &str, completion: &str) -> bool {
        let prefix = Self::norm(prefix);
        let completion = Self::norm(completion);
        if prefix.is_empty() || completion.is_empty() {
            return false;
        }
        self.entries
            .get(&prefix)
            .and_then(|entry| entry.completions.get(&completion))
            .is_some_and(|&count| count >= self.suppression_threshold)
    }

    /// Number of distinct prefixes currently tracked (diagnostics/tests).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is tracked yet.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
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

impl Default for RejectionMemory {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_rejection_not_suppressed_two_suppressed() {
        let mut mem = RejectionMemory::new();
        mem.record("th", "the");
        // 1 rejection — below K=2, still offered.
        assert!(!mem.is_suppressed("th", "the"));
        mem.record("th", "the");
        // 2 rejections — at K=2, now suppressed.
        assert!(mem.is_suppressed("th", "the"));
    }

    #[test]
    fn different_prefix_not_suppressed() {
        let mut mem = RejectionMemory::new();
        mem.record("th", "the");
        mem.record("th", "the");
        assert!(mem.is_suppressed("th", "the"));
        // A different typed prefix shares no rejection history.
        assert!(!mem.is_suppressed("wh", "the"));
    }

    #[test]
    fn different_completion_same_prefix_not_suppressed() {
        let mut mem = RejectionMemory::new();
        mem.record("th", "the");
        mem.record("th", "the");
        assert!(mem.is_suppressed("th", "the"));
        // Same prefix, a different completion, is tracked independently.
        assert!(!mem.is_suppressed("th", "this"));
    }

    #[test]
    fn normalization_is_case_and_whitespace_insensitive() {
        let mut mem = RejectionMemory::new();
        mem.record("  Th ", "The");
        mem.record("th", "the");
        // Recorded under mixed case/whitespace, queried plainly — still matches.
        assert!(mem.is_suppressed("TH", "  THE  "));
    }

    #[test]
    fn empty_prefix_or_completion_is_noop() {
        let mut mem = RejectionMemory::new();
        mem.record("", "the");
        mem.record("th", "   ");
        assert!(mem.is_empty());
        assert!(!mem.is_suppressed("", "the"));
        assert!(!mem.is_suppressed("th", ""));
    }

    #[test]
    fn lru_eviction_keeps_within_capacity() {
        let mut mem = RejectionMemory::new();
        // Insert well past the cap, each a distinct prefix.
        for i in 0..(MAX_PREFIXES + 50) {
            mem.record(&format!("prefix{i}"), "done");
        }
        assert!(
            mem.len() <= MAX_PREFIXES,
            "prefix count {} exceeded cap {}",
            mem.len(),
            MAX_PREFIXES
        );
        // The most-recently recorded prefix must survive eviction.
        let newest = format!("prefix{}", MAX_PREFIXES + 49);
        mem.record(&newest, "done");
        assert!(mem.is_suppressed(&newest, "done"));
    }

    #[test]
    fn fresh_instance_is_session_clean() {
        // A brand-new instance carries no history from any prior "session".
        let mem = RejectionMemory::new();
        assert!(mem.is_empty());
        assert!(!mem.is_suppressed("th", "the"));
        let def = RejectionMemory::default();
        assert!(def.is_empty());
    }
}
