// Session cache LM — recency-weighted session vocabulary for burstiness.
//
// Tracks words typed in the current session with exponential recency decay.
// Unlike personal Markov (bigram/trigram transitions), this captures
// *burstiness* — the tendency to re-use the same words within a session.
// Literature: Church & Gale (1995) "Poisson Mixtures".

use std::collections::HashMap;

/// Recency-weighted session cache language model.
///
/// Words typed in the current session get a burstiness boost: if you typed
/// "kubernetes" 5 times, it's boosted regardless of Markov context.
pub struct SessionCacheLM {
    /// word → (last_seen_position, occurrence_count)
    cache: HashMap<String, (u64, u32)>,
    /// Monotonic position counter (incremented per commit).
    position: u64,
    /// Decay rate: `score = count * exp(-lambda * (position - last_seen))`.
    decay_lambda: f64,
    /// Max entries (LRU eviction when exceeded).
    max_entries: usize,
}

impl SessionCacheLM {
    /// Create a new session cache with default parameters.
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
            position: 0,
            decay_lambda: 0.05,
            max_entries: 200,
        }
    }

    /// Create with custom parameters.
    pub fn with_params(decay_lambda: f64, max_entries: usize) -> Self {
        Self {
            cache: HashMap::new(),
            position: 0,
            decay_lambda,
            max_entries,
        }
    }

    /// Record a word commit — updates position and occurrence count.
    pub fn observe(&mut self, word: &str) {
        self.position += 1;
        let entry = self.cache.entry(word.to_owned()).or_insert((0, 0));
        entry.0 = self.position;
        entry.1 += 1;

        // LRU eviction if over max_entries.
        if self.cache.len() > self.max_entries {
            self.evict_oldest();
        }
    }

    /// Compute the recency-weighted frequency score for a word.
    ///
    /// `score = count * exp(-lambda * (position - last_seen))`
    ///
    /// Returns 0.0 if the word was never seen in this session.
    pub fn score(&self, word: &str) -> f64 {
        if let Some(&(last_seen, count)) = self.cache.get(word) {
            let distance = self.position.saturating_sub(last_seen) as f64;
            count as f64 * (-self.decay_lambda * distance).exp()
        } else {
            0.0
        }
    }

    /// Clear all session state (called on session start/reset).
    pub fn clear(&mut self) {
        self.cache.clear();
        self.position = 0;
    }

    /// Number of distinct words currently cached.
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }

    /// Evict the entry with the smallest `last_seen` position (oldest).
    fn evict_oldest(&mut self) {
        if let Some(oldest_key) = self
            .cache
            .iter()
            .min_by_key(|(_, (pos, _))| *pos)
            .map(|(k, _)| k.clone())
        {
            self.cache.remove(&oldest_key);
        }
    }
}

impl Default for SessionCacheLM {
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

    #[test]
    fn test_session_cache_boosts_repeated_word() {
        let mut cache = SessionCacheLM::new();
        cache.observe("kubernetes");
        cache.observe("kubernetes");
        cache.observe("kubernetes");
        cache.observe("docker"); // different word, once

        let k_score = cache.score("kubernetes");
        let d_score = cache.score("docker");
        assert!(
            k_score > d_score,
            "repeated 'kubernetes' ({k_score}) should score higher than 'docker' ({d_score})"
        );
    }

    #[test]
    fn test_session_cache_decay() {
        let mut cache = SessionCacheLM::with_params(0.1, 200);
        cache.observe("old_word");
        // Observe 20 other words to push "old_word" into the past.
        for i in 0..20 {
            cache.observe(&format!("filler_{i}"));
        }
        cache.observe("new_word");

        let old_score = cache.score("old_word");
        let new_score = cache.score("new_word");
        assert!(
            new_score > old_score,
            "recent 'new_word' ({new_score}) should score higher than decayed 'old_word' ({old_score})"
        );
    }

    #[test]
    fn test_session_cache_eviction() {
        let mut cache = SessionCacheLM::with_params(0.05, 5);
        for i in 0..10 {
            cache.observe(&format!("word_{i}"));
        }
        assert!(
            cache.len() <= 5,
            "cache should respect max_entries=5, got {}",
            cache.len()
        );
    }

    #[test]
    fn test_session_cache_clear() {
        let mut cache = SessionCacheLM::new();
        cache.observe("test");
        assert!(!cache.is_empty());

        cache.clear();
        assert!(cache.is_empty());
        assert_eq!(cache.score("test"), 0.0);
    }

    #[test]
    fn test_session_cache_unseen_word() {
        let cache = SessionCacheLM::new();
        assert_eq!(cache.score("never_seen"), 0.0);
    }

    #[test]
    fn test_session_cache_zero_decay() {
        let mut cache = SessionCacheLM::with_params(0.0, 200);
        cache.observe("word");
        for i in 0..100 {
            cache.observe(&format!("filler_{i}"));
        }
        // With decay_lambda=0, score should be exactly count * 1.0
        let score = cache.score("word");
        assert!(
            (score - 1.0).abs() < 1e-9,
            "with zero decay, score should be count=1.0, got {score}"
        );
    }
}
