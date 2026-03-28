// Markov chain with Katz backoff scoring.
//
// Provides bigram and trigram language-model probabilities with interpolated
// backoff: score = λ₁·P₃(trigram) + λ₂·P₂(bigram) + λ₃·P₁(unigram).

use std::collections::{HashMap, HashSet};

/// Minimum score returned for completely unseen words so that no candidate
/// is ever assigned a probability of exactly zero.
const UNSEEN_FLOOR: f64 = 1e-6;

/// Bigram data: context_word → (followers_map, cached_total).
pub type BigramData = HashMap<String, (HashMap<String, u32>, u32)>;

/// Trigram data: w1 → w2 → (followers_map, cached_total).
pub type TrigramData = HashMap<String, HashMap<String, (HashMap<String, u32>, u32)>>;

/// Higher-order n-gram data: context tuple → (followers_map, cached_total).
/// Used for 4-gram through 7-gram (personal learning from user typing).
pub type HigherNgramData = HashMap<Vec<String>, (HashMap<String, u32>, u32)>;

/// Maximum n-gram order supported (7-gram = 6 context words + 1 predicted word).
pub const MAX_NGRAM_ORDER: usize = 7;

/// Interpolated Markov chain language model with extended ∞-gram backoff.
///
/// Stores bigrams and trigrams (from corpus) plus higher-order n-grams
/// (from personal learning). Scoring uses longest-suffix-match backoff
/// inspired by Infini-gram (Liu et al., ICML 2024): start from the
/// longest available context and back off to shorter only when needed.
pub struct MarkovChain {
    /// context_word → (followers_map, cached_total)
    bigrams: BigramData,
    /// w1 → w2 → (followers_map, cached_total)
    trigrams: TrigramData,
    /// 4-gram through 7-gram contexts (personal learning only).
    higher_ngrams: HigherNgramData,
    /// Distinct words observed (tracked incrementally).
    vocab: HashSet<String>,
    /// Interpolation weights: [trigram, bigram, unigram].
    lambda: [f64; 3],
    /// Auto-tuned weights from corpus statistics (None = use fixed lambda).
    adaptive_lambda: Option<[f64; 3]>,
    /// Per-word unigram frequency counts for frequency-weighted backoff.
    /// Populated via `train_unigram()` during corpus loading.
    unigram_counts: HashMap<String, u32>,
    /// Sum of all unigram counts (cached for fast normalisation).
    total_unigram_count: u64,
}

impl MarkovChain {
    /// Create a new, empty model with default lambdas `[0.6, 0.3, 0.1]`.
    pub fn new() -> Self {
        Self {
            bigrams: HashMap::new(),
            trigrams: HashMap::new(),
            higher_ngrams: HashMap::new(),
            vocab: HashSet::new(),
            lambda: [0.6, 0.3, 0.1],
            adaptive_lambda: None,
            unigram_counts: HashMap::new(),
            total_unigram_count: 0,
        }
    }

    /// Create a new, empty model with custom interpolation weights.
    pub fn with_lambdas(lambda: [f64; 3]) -> Self {
        Self {
            bigrams: HashMap::new(),
            trigrams: HashMap::new(),
            higher_ngrams: HashMap::new(),
            vocab: HashSet::new(),
            lambda,
            adaptive_lambda: None,
            unigram_counts: HashMap::new(),
            total_unigram_count: 0,
        }
    }

    // ------------------------------------------------------------------
    // Training
    // ------------------------------------------------------------------

    /// Record unigram frequency for a word (from corpus loading).
    ///
    /// This enables frequency-weighted unigram backoff: `P₁(w) = count(w) / total`
    /// instead of the uniform `1 / vocab_size`. Words like "the" get much higher
    /// unigram probability than "xylophone", dramatically improving prediction
    /// quality when backoff to unigram is needed (short context, rare bigrams).
    pub fn train_unigram(&mut self, word: &str, count: u32) {
        let entry = self.unigram_counts.entry(word.to_owned()).or_insert(0);
        let old = *entry;
        *entry = entry.saturating_add(count);
        self.total_unigram_count += (*entry - old) as u64;
        self.vocab.insert(word.to_owned());
    }

    /// Record `count` observations of the bigram `context → word`.
    pub fn train_bigram(&mut self, context: &str, word: &str, count: u32) {
        let (followers, total) = self
            .bigrams
            .entry(context.to_owned())
            .or_insert_with(|| (HashMap::new(), 0));
        let entry = followers.entry(word.to_owned()).or_insert(0);
        *entry = entry.saturating_add(count);
        *total = total.saturating_add(count);
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
        *entry = entry.saturating_add(count);
        *total = total.saturating_add(count);
        self.vocab.insert(word.to_owned());
    }

    /// Record `count` observations of a higher-order n-gram (4-gram through 7-gram).
    ///
    /// `context` is the preceding words (e.g. for a 5-gram: 4 context words).
    /// Contexts longer than `MAX_NGRAM_ORDER - 1` are silently truncated.
    pub fn train_ngram(&mut self, context: &[&str], word: &str, count: u32) {
        if context.len() < 3 {
            // Use dedicated bigram/trigram storage for orders ≤3.
            return;
        }
        let ctx_len = context.len().min(MAX_NGRAM_ORDER - 1);
        let key: Vec<String> = context[context.len() - ctx_len..]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (followers, total) = self
            .higher_ngrams
            .entry(key)
            .or_insert_with(|| (HashMap::new(), 0));
        let entry = followers.entry(word.to_owned()).or_insert(0);
        *entry = entry.saturating_add(count);
        *total = total.saturating_add(count);
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

    /// Frequency-weighted unigram probability: `count(word) / total_count`.
    ///
    /// When unigram counts are available (populated via `train_unigram()`),
    /// returns the corpus-based frequency. Falls back to uniform `1/vocab_size`
    /// when no unigram counts have been loaded (backward compatibility).
    fn unigram_prob(&self, word: &str) -> f64 {
        if self.total_unigram_count > 0 {
            let count = self.unigram_counts.get(word).copied().unwrap_or(0) as f64;
            if count > 0.0 {
                return count / self.total_unigram_count as f64;
            }
            return UNSEEN_FLOOR;
        }
        // Fallback: uniform distribution when no unigram data loaded.
        if self.vocab.is_empty() {
            return UNSEEN_FLOOR;
        }
        1.0 / self.vocab.len() as f64
    }

    /// P(word | context) from higher-order n-gram counts (4-gram+).
    ///
    /// Returns `None` when the context has never been observed.
    fn higher_ngram_prob(&self, word: &str, context: &[&str]) -> Option<f64> {
        if context.len() < 3 {
            return None;
        }
        let key: Vec<String> = context.iter().map(|s| s.to_string()).collect();
        let (followers, total) = self.higher_ngrams.get(&key)?;
        if *total == 0 {
            return None;
        }
        let count = followers.get(word).copied().unwrap_or(0);
        if count == 0 {
            return None;
        }
        Some(count as f64 / *total as f64)
    }

    // ------------------------------------------------------------------
    // Scoring
    // ------------------------------------------------------------------

    /// Extended ∞-gram backoff score (Infini-gram inspired).
    ///
    /// Starts from the longest available context and backs off to shorter
    /// contexts when no match is found. Higher-order matches receive a
    /// confidence boost because they capture more specific context.
    ///
    /// Falls back to the classic `score_with_backoff()` for trigram and below.
    pub fn score_extended_backoff(&self, word: &str, context: &[&str]) -> f64 {
        // Try higher-order n-grams from longest to shortest (7-gram → 4-gram).
        let max_ctx = context.len().min(MAX_NGRAM_ORDER - 1);
        for n in (3..=max_ctx).rev() {
            let start = context.len() - n;
            let ctx_slice = &context[start..];
            if let Some(prob) = self.higher_ngram_prob(word, ctx_slice) {
                // Higher-order match found — blend with lower-order for smoothing.
                // Confidence bonus: longer context = more specific = higher weight.
                let confidence = 0.7 + 0.05 * (n as f64 - 3.0); // 0.7 for 4-gram, 0.9 for 7-gram
                let lower = self.score_trigram_and_below(word, context);
                return (confidence * prob + (1.0 - confidence) * lower).max(UNSEEN_FLOOR);
            }
        }
        // No higher-order match — fall through to trigram/bigram/unigram.
        self.score_trigram_and_below(word, context)
    }

    /// Score using trigram, bigram, and unigram levels only.
    ///
    /// Extracts prev1/prev2 from the context slice and delegates to
    /// `score_with_backoff()`.
    fn score_trigram_and_below(&self, word: &str, context: &[&str]) -> f64 {
        let prev1 = context.last().copied();
        let prev2 = if context.len() >= 2 {
            Some(context[context.len() - 2])
        } else {
            None
        };
        self.score_with_backoff(word, prev1, prev2)
    }

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
        let p_uni = self.unigram_prob(word);
        let lam = self.effective_lambda();

        let score = match (older, recent) {
            (Some(w1), Some(w2)) => {
                let p_tri = self.trigram_prob(word, w1, w2);
                let p_bi = self.bigram_prob(word, w2);
                lam[0] * p_tri + lam[1] * p_bi + lam[2] * p_uni
            }
            (None, Some(ctx)) => {
                // No trigram context — redistribute λ₁ proportionally.
                let p_bi = self.bigram_prob(word, ctx);
                let w_bi = lam[1] + lam[0] * (lam[1] / (lam[1] + lam[2]));
                let w_uni = lam[2] + lam[0] * (lam[2] / (lam[1] + lam[2]));
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
    pub fn bigrams_raw(&self) -> &BigramData {
        &self.bigrams
    }

    /// Raw trigram data access (for serialization).
    pub fn trigrams_raw(&self) -> &TrigramData {
        &self.trigrams
    }

    /// Raw unigram counts (for collocation PMI computation).
    pub fn unigram_counts_raw(&self) -> &HashMap<String, u32> {
        &self.unigram_counts
    }

    /// Total unigram count (for collocation PMI computation).
    pub fn total_unigram_count(&self) -> u64 {
        self.total_unigram_count
    }

    /// Raw higher-order n-gram data access (for serialization).
    pub fn higher_ngrams_raw(&self) -> &HigherNgramData {
        &self.higher_ngrams
    }

    // ------------------------------------------------------------------
    // Adaptive lambda (Information Gain inspired, Phase 2)
    // ------------------------------------------------------------------

    /// Compute adaptive interpolation weights from corpus statistics.
    ///
    /// Inspired by Olifant/Memory-based LMs (van den Bosch, 2025): instead
    /// of fixed λ=[0.6, 0.3, 0.1], compute weights proportional to the
    /// empirical coverage and discrimination power at each n-gram level.
    ///
    /// Call after corpus loading is complete. The computed weights are used
    /// by `score_with_backoff()` when `adaptive_lambda` is `Some`.
    pub fn compute_adaptive_lambda(&mut self) {
        let bigram_contexts = self.bigrams.len() as f64;
        let trigram_contexts: usize = self.trigrams.values().map(|v| v.len()).sum();
        let trigram_contexts = trigram_contexts as f64;
        let vocab = self.vocab.len().max(1) as f64;

        if bigram_contexts < 10.0 || vocab < 10.0 {
            // Too little data for meaningful adaptation — keep defaults.
            return;
        }

        // Coverage ratio: what fraction of vocab appears as a context?
        // Higher coverage = more useful that level is for discrimination.
        let bigram_coverage = (bigram_contexts / vocab).min(1.0);
        let trigram_coverage = (trigram_contexts / (bigram_contexts.max(1.0))).min(1.0);

        // Compute discrimination power via average fan-out.
        // Lower average fan-out = more discriminative (sharper distributions).
        let bigram_avg_fanout = if bigram_contexts > 0.0 {
            self.bigrams
                .values()
                .map(|(f, _)| f.len() as f64)
                .sum::<f64>()
                / bigram_contexts
        } else {
            vocab
        };
        let trigram_avg_fanout = if trigram_contexts > 0.0 {
            self.trigrams
                .values()
                .flat_map(|v| v.values())
                .map(|(f, _)| f.len() as f64)
                .sum::<f64>()
                / trigram_contexts
        } else {
            vocab
        };

        // Discrimination score: inverse of normalized fan-out.
        // Smaller fan-out → sharper distribution → higher weight.
        let tri_disc = 1.0 / (1.0 + trigram_avg_fanout / vocab);
        let bi_disc = 1.0 / (1.0 + bigram_avg_fanout / vocab);
        let uni_disc = 0.1; // unigram baseline (low discrimination)

        // Raw weights = coverage × discrimination.
        let w_tri = trigram_coverage * tri_disc;
        let w_bi = bigram_coverage * bi_disc;
        let w_uni = uni_disc;

        let sum = w_tri + w_bi + w_uni;
        if sum > 0.0 {
            self.adaptive_lambda = Some([w_tri / sum, w_bi / sum, w_uni / sum]);
        }
    }

    /// Get the effective interpolation weights (adaptive if computed, else fixed).
    pub fn effective_lambda(&self) -> [f64; 3] {
        self.adaptive_lambda.unwrap_or(self.lambda)
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
    fn frequency_weighted_unigram() {
        let mut m = MarkovChain::new();
        // Train unigrams with realistic frequencies.
        m.train_unigram("the", 1000);
        m.train_unigram("a", 500);
        m.train_unigram("xylophone", 1);
        // Total = 1501
        let p_the = m.unigram_prob("the");
        let p_xylo = m.unigram_prob("xylophone");
        // "the" should have ~666x higher probability than "xylophone".
        assert!(
            p_the > p_xylo * 100.0,
            "'the' ({p_the}) should be much more probable than 'xylophone' ({p_xylo})"
        );
        // Exact values.
        assert!(
            (p_the - 1000.0 / 1501.0).abs() < 1e-9,
            "expected {}, got {p_the}",
            1000.0 / 1501.0
        );
        // Unseen word gets UNSEEN_FLOOR.
        assert_eq!(m.unigram_prob("zzzzz"), UNSEEN_FLOOR);
    }

    #[test]
    fn frequency_weighted_backoff_uses_unigram_counts() {
        let mut m = MarkovChain::new();
        m.train_unigram("hello", 100);
        m.train_unigram("world", 10);
        // No bigrams/trigrams — score_with_backoff falls to unigram only.
        let s_hello = m.score_with_backoff("hello", None, None);
        let s_world = m.score_with_backoff("world", None, None);
        assert!(
            s_hello > s_world,
            "higher-frequency 'hello' ({s_hello}) should score above 'world' ({s_world})"
        );
        // Ratio should reflect corpus frequency.
        let ratio = s_hello / s_world;
        assert!(
            (ratio - 10.0).abs() < 1e-6,
            "ratio should be ~10, got {ratio}"
        );
    }

    #[test]
    fn test_precomputed_total_matches_sum() {
        let m = small_model();
        // Verify bigram totals
        for (ctx, (followers, total)) in &m.bigrams {
            let computed: u32 = followers.values().sum();
            assert_eq!(
                *total, computed,
                "bigram total mismatch for context '{ctx}'"
            );
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
