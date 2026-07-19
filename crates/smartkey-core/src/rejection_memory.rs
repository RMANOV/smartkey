// Session-scoped suppression for repeatedly rejected ghost completions.
//
// The state is intentionally process-local and is never part of the personal
// profile. Restarting the input engine therefore starts a clean session.

use std::collections::HashMap;

const SUPPRESS_THRESHOLD: u32 = 2;
const MAX_PREFIXES: usize = 256;

struct PrefixEntry {
    completions: HashMap<String, u32>,
    access_order: u64,
}

/// Counts rejected completions independently for each normalized typed prefix.
pub struct RejectionMemory {
    entries: HashMap<String, PrefixEntry>,
    access_counter: u64,
}

impl RejectionMemory {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            access_counter: 0,
        }
    }

    fn normalize(value: &str) -> String {
        value.trim().to_lowercase()
    }

    /// Record one user rejection. Empty keys are ignored.
    pub fn record(&mut self, prefix: &str, completion: &str) {
        let prefix = Self::normalize(prefix);
        let completion = Self::normalize(completion);
        if prefix.is_empty() || completion.is_empty() {
            return;
        }

        self.access_counter = self.access_counter.saturating_add(1);
        let stamp = self.access_counter;
        let entry = self.entries.entry(prefix).or_insert_with(|| PrefixEntry {
            completions: HashMap::new(),
            access_order: stamp,
        });
        entry.access_order = stamp;
        let count = entry.completions.entry(completion).or_insert(0);
        *count = count.saturating_add(1);

        if self.entries.len() > MAX_PREFIXES {
            self.evict_lru();
        }
    }

    /// True after the same normalized pair has been rejected K=2 times.
    pub fn is_suppressed(&self, prefix: &str, completion: &str) -> bool {
        let prefix = Self::normalize(prefix);
        let completion = Self::normalize(completion);
        if prefix.is_empty() || completion.is_empty() {
            return false;
        }
        self.entries
            .get(&prefix)
            .and_then(|entry| entry.completions.get(&completion))
            .is_some_and(|count| *count >= SUPPRESS_THRESHOLD)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn evict_lru(&mut self) {
        if let Some(oldest) = self
            .entries
            .iter()
            .min_by_key(|(_, entry)| entry.access_order)
            .map(|(prefix, _)| prefix.clone())
        {
            self.entries.remove(&oldest);
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
    fn exactly_two_rejections_suppress() {
        let mut memory = RejectionMemory::new();
        memory.record("th", "the");
        assert!(!memory.is_suppressed("th", "the"));
        memory.record("th", "the");
        assert!(memory.is_suppressed("th", "the"));
    }

    #[test]
    fn prefix_and_completion_are_independent() {
        let mut memory = RejectionMemory::new();
        memory.record("th", "the");
        memory.record("th", "the");
        assert!(!memory.is_suppressed("wh", "the"));
        assert!(!memory.is_suppressed("th", "this"));
    }

    #[test]
    fn normalization_is_case_and_whitespace_insensitive() {
        let mut memory = RejectionMemory::new();
        memory.record("  Th ", "The");
        memory.record("th", "the");
        assert!(memory.is_suppressed("TH", "  THE  "));
    }

    #[test]
    fn empty_keys_are_ignored() {
        let mut memory = RejectionMemory::new();
        memory.record("", "the");
        memory.record("th", "  ");
        assert!(memory.is_empty());
    }

    #[test]
    fn distinct_prefixes_are_lru_bounded() {
        let mut memory = RejectionMemory::new();
        for i in 0..(MAX_PREFIXES + 50) {
            memory.record(&format!("prefix{i}"), "done");
        }
        assert_eq!(memory.len(), MAX_PREFIXES);
        let newest = format!("prefix{}", MAX_PREFIXES + 49);
        memory.record(&newest, "done");
        assert!(memory.is_suppressed(&newest, "done"));
        assert!(!memory.entries.contains_key("prefix0"));
    }

    #[test]
    fn fresh_instance_has_no_prior_session_state() {
        let memory = RejectionMemory::new();
        assert!(memory.is_empty());
        assert!(!memory.is_suppressed("th", "the"));
    }
}
