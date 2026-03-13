// Markov chain with Katz backoff scoring.
//
// Provides bigram and trigram language-model probabilities with interpolated
// backoff: score = λ₁·P₃(trigram) + λ₂·P₂(bigram) + λ₃·P₁(unigram).

use std::collections::{HashMap, HashSet};

/// Minimum score returned for completely unseen words so that no candidate
/// is ever assigned a probability of exactly zero.
const UNSEEN_FLOOR: f64 = 1e-6;

/// Interpolated Markov chain language model (up to trigram).
pub struct MarkovChain {
    /// context_word → (followers_map, cached_total)
    bigrams: HashMap<String, (HashMap<String, u32>, u32)>,
    /// w1 → w2 → (followers_map, cached_total)
    trigrams: HashMap<String, HashMap<String, (HashMap<String, u32>, u32)>>,
    /// Distinct words observed (tracked incrementally).
    vocab: HashSet<String>,
    /// Interpolation weights: [trigram, bigram, unigram].
    lambda: [f64; 3],
}

impl MarkovChain {
    /// Create a new, empty model with default lambdas `[0.6, 0.3, 0.1]`.
    pub fn new() -> Self {
        Self {
            bigrams: HashMap::new(),
            trigrams: HashMap::new(),
            vocab: HashSet::new(),
            lambda: [0.6, 0.3, 0.1],
        }
    }

    /// Create a new, empty model with custom interpolation weights.
    pub fn with_lambdas(lambda: [f64; 3]) -> Self {
        Self {
            bigrams: HashMap::new(),
            trigrams: HashMap::new(),
            vocab: HashSet::new(),
            lambda,
        }
    }

    // ------------------------------------------------------------------
    // Training
    // ------------------------------------------------------------------

    /// Record `count` observations of the bigram `context → word`.
    pub fn train_bigram(&mut self, context: &str, word: &str, count: u32) {
        let (followers, total) = self
            .bigrams
            .entry(context.to_owned())
            .or_insert_with(|| (HashMap::new(), 0));
        let entry = followers.entry(word.to_owned()).or_insert(0);
        *entry += count;
        *total += count;
        self.vocab.insert(word.to_owned());
    }

    /// Record `count` observations of the trigram `(w1, w2) → word`.
    pub fn train_trigram(&mut self, w1: &str, w2: &str, word: &str, count: u32) {
        let (followers, total) = self
            .trigrams
            .entry(w1.to_owned())
            .or_default()
            .entry(w2.to_owned())
            .or_insert_with(|| (HashMap::new(), 0));
        let entry = followers.entry(word.to_owned()).or_insert(0);
        *entry += count;
        *total += count;
        self.vocab.insert(word.to_owned());
    }

    // ------------------------------------------------------------------
    // Probabilities
    // ------------------------------------------------------------------

    /// P(word | context) from bigram counts.
    ///
    /// Returns `UNSEEN_FLOOR` when `context` has never been observed.
    pub fn bigram_prob(&self, word: &str, context: &str) -> f64 {
        let Some((followers, total)) = self.bigrams.get(context) else {
            return UNSEEN_FLOOR;
        };
        Self::prob_from_followers(word, followers, *total)
    }

    /// P(word | w1, w2) from trigram counts.
    ///
    /// Returns `UNSEEN_FLOOR` when the `(w1, w2)` context has never been
    /// observed.
    pub fn trigram_prob(&self, word: &str, w1: &str, w2: &str) -> f64 {
        let Some(level2) = self.trigrams.get(w1) else {
            return UNSEEN_FLOOR;
        };
        let Some((followers, total)) = level2.get(w2) else {
            return UNSEEN_FLOOR;
        };
        Self::prob_from_followers(word, followers, *total)
    }

    /// Shared probability computation from a follower frequency map.
    fn prob_from_followers(word: &str, followers: &HashMap<String, u32>, total: u32) -> f64 {
        if total == 0 {
            return UNSEEN_FLOOR;
        }
        let count = followers.get(word).copied().unwrap_or(0);
        if count == 0 {
            return UNSEEN_FLOOR;
        }
        count as f64 / total as f64
    }

    /// Uniform unigram probability: `1 / vocab_size`.
    ///
    /// Falls back to `UNSEEN_FLOOR` when the vocabulary is empty.
    fn unigram_prob(&self) -> f64 {
        if self.vocab.is_empty() {
            return UNSEEN_FLOOR;
        }
        1.0 / self.vocab.len() as f64
    }

    // ------------------------------------------------------------------
    // Scoring
    // ------------------------------------------------------------------

    /// Interpolated backoff score.
    ///
    /// * Both `recent` and `older` present → full trigram interpolation.
    /// * Only `recent` → bigram + unigram (trigram weight redistributed).
    /// * Neither → unigram only.
    ///
    /// * `recent` — the most recent preceding word (bigram context).
    /// * `older`  — the second-to-last word (trigram context, paired with `recent`).
    ///
    /// The score is clamped to at least `UNSEEN_FLOOR`.
    pub fn score_with_backoff(&self, word: &str, recent: Option<&str>, older: Option<&str>) -> f64 {
        let p_uni = self.unigram_prob();

        let score = match (older, recent) {
            (Some(w1), Some(w2)) => {
                let p_tri = self.trigram_prob(word, w1, w2);
                let p_bi = self.bigram_prob(word, w2);
                self.lambda[0] * p_tri + self.lambda[1] * p_bi + self.lambda[2] * p_uni
            }
            (None, Some(ctx)) => {
                // No trigram context — redistribute λ₁ proportionally.
                let p_bi = self.bigram_prob(word, ctx);
                let w_bi = self.lambda[1]
                    + self.lambda[0] * (self.lambda[1] / (self.lambda[1] + self.lambda[2]));
                let w_uni = self.lambda[2]
                    + self.lambda[0] * (self.lambda[2] / (self.lambda[1] + self.lambda[2]));
                w_bi * p_bi + w_uni * p_uni
            }
            _ => p_uni,
        };

        score.max(UNSEEN_FLOOR)
    }

    /// Score every candidate and return them sorted by score (descending).
    pub fn rank_candidates(
        &self,
        candidates: &[String],
        recent: Option<&str>,
        older: Option<&str>,
    ) -> Vec<(String, f64)> {
        let mut scored: Vec<(String, f64)> = candidates
            .iter()
            .map(|w| {
                let s = self.score_with_backoff(w, recent, older);
                (w.clone(), s)
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored
    }

    /// Number of distinct words in the vocabulary.
    pub fn vocab_size(&self) -> usize {
        self.vocab.len()
    }

    /// Raw bigram data access (for serialization).
    pub fn bigrams_raw(&self) -> &HashMap<String, (HashMap<String, u32>, u32)> {
        &self.bigrams
    }

    /// Raw trigram data access (for serialization).
    pub fn trigrams_raw(&self) -> &HashMap<String, HashMap<String, (HashMap<String, u32>, u32)>> {
        &self.trigrams
    }
}

impl Default for MarkovChain {
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

    /// Helper: build a small trained model for reuse across tests.
    fn small_model() -> MarkovChain {
        let mut m = MarkovChain::new();
        // "i" → "love" (3), "i" → "like" (2)
        m.train_bigram("i", "love", 3);
        m.train_bigram("i", "like", 2);
        // "love" → "rust" (4), "love" → "python" (1)
        m.train_bigram("love", "rust", 4);
        m.train_bigram("love", "python", 1);
        // trigram: ("i", "love") → "rust" (3), → "python" (1)
        m.train_trigram("i", "love", "rust", 3);
        m.train_trigram("i", "love", "python", 1);
        m
    }

    #[test]
    fn bigram_probability() {
        let m = small_model();
        // P(love | i) = 3 / (3+2) = 0.6
        let p = m.bigram_prob("love", "i");
        assert!((p - 0.6).abs() < 1e-9, "expected 0.6, got {p}");
        // P(like | i) = 2/5 = 0.4
        let p = m.bigram_prob("like", "i");
        assert!((p - 0.4).abs() < 1e-9, "expected 0.4, got {p}");
    }

    #[test]
    fn trigram_probability() {
        let m = small_model();
        // P(rust | i, love) = 3 / (3+1) = 0.75
        let p = m.trigram_prob("rust", "i", "love");
        assert!((p - 0.75).abs() < 1e-9, "expected 0.75, got {p}");
        // P(python | i, love) = 1/4 = 0.25
        let p = m.trigram_prob("python", "i", "love");
        assert!((p - 0.25).abs() < 1e-9, "expected 0.25, got {p}");
    }

    #[test]
    fn unseen_word_gets_floor_score() {
        let m = small_model();
        // Unseen context → bigram falls back to UNSEEN_FLOOR.
        let p = m.bigram_prob("rust", "nonexistent");
        assert_eq!(p, UNSEEN_FLOOR);
        // Unseen word for a known context → UNSEEN_FLOOR.
        let p = m.bigram_prob("java", "i");
        assert_eq!(p, UNSEEN_FLOOR);
        // score_with_backoff for a totally unseen word must still be > 0.
        let s = m.score_with_backoff("zzzzz", Some("i"), Some("love"));
        assert!(s >= UNSEEN_FLOOR);
    }

    #[test]
    fn katz_backoff_full() {
        let m = small_model();
        // Full trigram backoff: λ₁·P₃ + λ₂·P₂ + λ₃·P₁
        // For "rust" with context ("i", "love"):
        //   P₃ = 0.75, P₂ = bigram_prob("rust", "love") = 4/5 = 0.8
        //   P₁ = 1/vocab_size
        let p3 = 0.75;
        let p2 = 4.0 / 5.0;
        let p1 = 1.0 / m.vocab_size() as f64;
        let expected = 0.6 * p3 + 0.3 * p2 + 0.1 * p1;
        let score = m.score_with_backoff("rust", Some("love"), Some("i"));
        assert!(
            (score - expected).abs() < 1e-9,
            "expected {expected}, got {score}"
        );
    }

    #[test]
    fn katz_backoff_bigram_only() {
        let m = small_model();
        // Only bigram context (prev2 = None).
        // Weights are redistributed: w_bi and w_uni absorb λ₁.
        let p_bi = m.bigram_prob("love", "i"); // 0.6
        let p_uni = 1.0 / m.vocab_size() as f64;
        let w_bi = 0.3 + 0.6 * (0.3 / 0.4);
        let w_uni = 0.1 + 0.6 * (0.1 / 0.4);
        let expected = w_bi * p_bi + w_uni * p_uni;
        let score = m.score_with_backoff("love", Some("i"), None);
        assert!(
            (score - expected).abs() < 1e-9,
            "expected {expected}, got {score}"
        );
    }

    #[test]
    fn rank_candidates_ordering() {
        let m = small_model();
        let candidates: Vec<String> = vec!["python".into(), "rust".into(), "java".into()];
        let ranked = m.rank_candidates(&candidates, Some("love"), Some("i"));
        // "rust" should rank first (highest trigram + bigram support).
        assert_eq!(ranked[0].0, "rust");
        // "python" second (seen but lower counts).
        assert_eq!(ranked[1].0, "python");
        // "java" last (unseen).
        assert_eq!(ranked[2].0, "java");
        // Scores must be strictly decreasing.
        assert!(ranked[0].1 > ranked[1].1);
        assert!(ranked[1].1 > ranked[2].1);
    }

    #[test]
    fn empty_model_does_not_panic() {
        let m = MarkovChain::new();
        let s = m.score_with_backoff("hello", None, None);
        assert_eq!(s, UNSEEN_FLOOR);
        let s = m.score_with_backoff("hello", Some("a"), Some("b"));
        assert_eq!(s, UNSEEN_FLOOR);
        let ranked = m.rank_candidates(&["a".into(), "b".into()], None, None);
        assert_eq!(ranked.len(), 2);
    }

    #[test]
    fn with_lambdas_uses_custom_weights() {
        let mut m = MarkovChain::with_lambdas([0.1, 0.2, 0.7]);
        m.train_bigram("i", "love", 3);
        m.train_bigram("i", "like", 2);
        m.train_trigram("i", "love", "rust", 3);
        m.train_trigram("i", "love", "python", 1);
        m.train_bigram("love", "rust", 4);
        m.train_bigram("love", "python", 1);

        // With custom lambdas [0.1, 0.2, 0.7], unigram weight dominates.
        // Full backoff: 0.1·P₃ + 0.2·P₂ + 0.7·P₁
        let p3 = 3.0 / 4.0; // trigram P(rust | i, love)
        let p2 = 4.0 / 5.0; // bigram P(rust | love)
        let p1 = 1.0 / m.vocab_size() as f64;
        let expected = 0.1 * p3 + 0.2 * p2 + 0.7 * p1;
        let score = m.score_with_backoff("rust", Some("love"), Some("i"));
        assert!(
            (score - expected).abs() < 1e-9,
            "expected {expected}, got {score}"
        );
    }

    #[test]
    fn test_precomputed_total_matches_sum() {
        let m = small_model();
        // Verify bigram totals
        for (ctx, (followers, total)) in &m.bigrams {
            let computed: u32 = followers.values().sum();
            assert_eq!(*total, computed, "bigram total mismatch for context '{ctx}'");
        }
        // Verify trigram totals
        for (w1, level2) in &m.trigrams {
            for (w2, (followers, total)) in level2 {
                let computed: u32 = followers.values().sum();
                assert_eq!(*total, computed, "trigram total mismatch for ({w1}, {w2})");
            }
        }
    }
}
