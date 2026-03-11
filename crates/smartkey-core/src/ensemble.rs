// Ensemble scorer — combines corpus frequency, Markov context, and CVM personal boost.

use std::collections::{HashMap, HashSet};

use crate::cvm::{CvmCounter, CvmSnapshot};
use crate::input::InputConfig;
use crate::markov::MarkovChain;
use crate::ngram::{NgramTrie, WordEntry};

/// A single prediction with its ensemble score and UI confidence.
#[derive(Debug, Clone)]
pub struct Prediction {
    pub word: String,
    pub score: f64,
    /// Normalised confidence in `[0.0, 1.0]` — the top candidate is ~1.0.
    pub confidence: f64,
}

/// Unified prediction engine that blends three scoring signals:
///
/// * **Corpus frequency** (α) — how common the word is in the language model trie.
/// * **Markov context** (β) — bigram/trigram probability given recent words.
/// * **Personal boost** (γ) — CVM-tracked frequency of words *this* user types.
///
/// The final score is `α·corpus + β·markov + γ·personal`.
pub struct SmartKeyEngine {
    trie: NgramTrie,
    markov: MarkovChain,
    personal: CvmCounter,
    alpha: f64,
    beta: f64,
    gamma: f64,
    fuzzy_max_edits: u8,
    fuzzy_discounts: [f64; 3],
}

impl SmartKeyEngine {
    /// Create a new engine with default weights `(0.4, 0.4, 0.2)` and a
    /// CVM counter sized `(500, 5000)`.
    pub fn new() -> Self {
        Self::from_config(&InputConfig::default())
    }

    /// Create a new engine from an `InputConfig`, respecting all tuning parameters.
    pub fn from_config(config: &InputConfig) -> Self {
        Self {
            trie: NgramTrie::new(),
            markov: MarkovChain::with_lambdas(config.markov_lambdas),
            personal: CvmCounter::new(config.cvm_initial_size, config.cvm_max_size)
                .with_decay_lambda(config.cvm_decay_lambda),
            alpha: config.weights.0,
            beta: config.weights.1,
            gamma: config.weights.2,
            fuzzy_max_edits: config.fuzzy_max_edits,
            fuzzy_discounts: config.fuzzy_discounts,
        }
    }

    /// Insert a word with its corpus frequency into the trie.
    pub fn load_word(&mut self, word: &str, frequency: u32) {
        self.trie.insert(word, frequency);
    }

    /// Train a bigram transition `context → word` with the given count.
    pub fn load_bigram(&mut self, context: &str, word: &str, count: u32) {
        self.markov.train_bigram(context, word, count);
    }

    /// Train a trigram transition `(w1, w2) → word` with the given count.
    pub fn load_trigram(&mut self, w1: &str, w2: &str, word: &str, count: u32) {
        self.markov.train_trigram(w1, w2, word, count);
    }

    /// Feed a word into the personal CVM layer (call when the user types a word).
    pub fn learn(&mut self, word: &str) {
        self.personal.process(word);
    }

    /// Export the personal CVM layer as a portable snapshot.
    pub fn export_personal(&self) -> CvmSnapshot {
        self.personal.to_snapshot()
    }

    /// Replace the personal CVM layer from a snapshot (import / load).
    pub fn import_personal(&mut self, snapshot: &CvmSnapshot) {
        self.personal = CvmCounter::from_snapshot(snapshot);
    }

    /// Produce up to `limit` predictions for the given prefix and context.
    ///
    /// * `prefix` — the characters typed so far for the current word.
    /// * `context` — preceding words, most recent last (e.g. `&["i", "love"]`).
    /// * `limit` — maximum number of predictions to return.
    ///
    /// The pipeline:
    /// 1. Trie prefix search to gather `limit * 3` candidates.
    /// 2. Score each candidate on three axes (corpus, markov, personal).
    /// 3. Blend with `α·corpus + β·markov + γ·personal`.
    /// 4. Sort descending, truncate to `limit`, normalise confidence.
    pub fn predict(&self, prefix: &str, context: &[&str], limit: usize) -> Vec<Prediction> {
        if limit == 0 {
            return Vec::new();
        }

        let candidate_pool = limit * 3;

        // Step 1: candidate generation via trie prefix search.
        let candidates = self.trie.prefix_search(prefix, candidate_pool);

        // Step 1b: fuzzy fallback — if exact prefix returned fewer than `limit`
        // candidates, fill the gap with fuzzy matches (edit distance ≤ 2).
        let (fuzzy_candidates, discount_map) = if candidates.len() < limit && !prefix.is_empty() {
            let fuzzy_slots = candidate_pool.saturating_sub(candidates.len());
            let fuzzy_matches = self
                .trie
                .fuzzy_search(prefix, self.fuzzy_max_edits, fuzzy_slots);

            // Build a set of words already found by exact prefix search to
            // avoid scoring the same word twice.
            let exact_words: HashSet<&str> =
                candidates.iter().map(|c| c.word.as_str()).collect();

            let mut discount_map: HashMap<String, f64> = HashMap::new();
            let mut merged: Vec<WordEntry> = Vec::new();
            for fm in &fuzzy_matches {
                if exact_words.contains(fm.word.as_str()) {
                    continue;
                }
                let discount = self.fuzzy_discounts[fm.edit_distance.min(2) as usize];
                discount_map.insert(fm.word.clone(), discount);
                merged.push(WordEntry {
                    word: fm.word.clone(),
                    frequency: fm.frequency,
                });
            }
            (merged, discount_map)
        } else {
            (Vec::new(), HashMap::new())
        };

        // If both exact and fuzzy are empty, nothing to do.
        if candidates.is_empty() && fuzzy_candidates.is_empty() {
            return Vec::new();
        }

        // Combine all candidates for scoring.
        let all_candidates: Vec<&WordEntry> =
            candidates.iter().chain(fuzzy_candidates.iter()).collect();

        // Determine the max corpus frequency among all candidates for normalisation.
        let max_freq = all_candidates
            .iter()
            .map(|c| c.frequency)
            .max()
            .unwrap_or(1)
            .max(1) as f64;

        // Derive Markov context pointers: prev1 = last word, prev2 = second-to-last.
        let prev1 = context.last().copied();
        let prev2 = if context.len() >= 2 {
            Some(context[context.len() - 2])
        } else {
            None
        };

        // Determine max personal score among candidates for normalisation.
        let max_personal = all_candidates
            .iter()
            .map(|c| self.personal.frequency_score(&c.word))
            .fold(0.0_f64, f64::max)
            .max(1.0); // avoid division by zero

        // Determine max Markov score among candidates for normalisation.
        let max_markov = all_candidates
            .iter()
            .map(|c| self.markov.score_with_backoff(&c.word, prev1, prev2))
            .fold(0.0_f64, f64::max)
            .max(1e-6);

        // Step 2-3: score each candidate.
        let mut scored: Vec<Prediction> = all_candidates
            .iter()
            .map(|entry| {
                let corpus_score = entry.frequency as f64 / max_freq;
                let markov_score =
                    self.markov.score_with_backoff(&entry.word, prev1, prev2) / max_markov;
                let personal_score = self.personal.frequency_score(&entry.word) / max_personal;

                let mut score = self.alpha * corpus_score
                    + self.beta * markov_score
                    + self.gamma * personal_score;

                // Apply fuzzy discount if this came from fuzzy matching.
                if let Some(&discount) = discount_map.get(&entry.word) {
                    score *= discount;
                }

                Prediction {
                    word: entry.word.clone(),
                    score,
                    confidence: 0.0, // filled in after sorting
                }
            })
            .collect();

        // Step 4: sort descending by score, truncate.
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(limit);

        // Step 5: normalise confidence so the top candidate is ~1.0.
        if let Some(top_score) = scored.first().map(|p| p.score) {
            if top_score > 0.0 {
                for p in &mut scored {
                    p.confidence = p.score / top_score;
                }
            }
        }

        scored
    }
}

impl Default for SmartKeyEngine {
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

    /// Helper: build a small engine with a handful of words and bigrams.
    fn test_engine() -> SmartKeyEngine {
        let mut engine = SmartKeyEngine::new();
        engine.load_word("hello", 100);
        engine.load_word("help", 80);
        engine.load_word("hero", 60);
        engine.load_word("helicopter", 20);
        engine.load_word("world", 90);

        // Bigrams: "i" → "hello" (5), "i" → "help" (2)
        engine.load_bigram("i", "hello", 5);
        engine.load_bigram("i", "help", 2);

        // Trigram: ("say", "i") → "hello" (3)
        engine.load_trigram("say", "i", "hello", 3);

        engine
    }

    #[test]
    fn test_predict_basic() {
        let engine = test_engine();

        // Predict words starting with "hel" given context ["i"].
        let preds = engine.predict("hel", &["i"], 5);
        assert!(!preds.is_empty(), "should produce predictions");

        // "hello" should rank higher than "help" — it has higher corpus freq
        // AND stronger bigram support from context "i".
        let hello_pos = preds.iter().position(|p| p.word == "hello");
        let help_pos = preds.iter().position(|p| p.word == "help");
        assert!(
            hello_pos.is_some() && help_pos.is_some(),
            "both 'hello' and 'help' should appear"
        );
        assert!(
            hello_pos.unwrap() < help_pos.unwrap(),
            "'hello' should rank before 'help'"
        );

        // Top candidate should have confidence ~1.0.
        assert!(
            (preds[0].confidence - 1.0).abs() < 1e-9,
            "top prediction confidence should be ~1.0, got {}",
            preds[0].confidence
        );

        // All confidences should be in [0, 1].
        for p in &preds {
            assert!(
                p.confidence >= 0.0 && p.confidence <= 1.0,
                "confidence out of range: {}",
                p.confidence
            );
        }

        // Scores should be in descending order.
        for w in preds.windows(2) {
            assert!(
                w[0].score >= w[1].score,
                "predictions not sorted: {} >= {} failed",
                w[0].score,
                w[1].score
            );
        }
    }

    #[test]
    fn test_personal_boost() {
        use std::time::{SystemTime, UNIX_EPOCH};

        let mut engine = SmartKeyEngine::new();
        engine.load_word("apple", 50);
        engine.load_word("apply", 50);

        // Import a deterministic CVM snapshot with "apply" guaranteed in buffer.
        // This avoids flakiness from probabilistic CVM eviction.
        let now_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let snap = CvmSnapshot {
            version: 1,
            saved_at_unix: now_unix,
            capacity: 500,
            max_capacity: 5000,
            round: 0,
            decay_lambda: 0.001,
            words: vec!["apply".into()],
            age_secs: [("apply".into(), 0.0)].into(),
        };
        engine.import_personal(&snap);

        let preds = engine.predict("app", &[], 5);
        assert!(preds.len() >= 2);
        let apply_pos = preds.iter().position(|p| p.word == "apply").unwrap();
        let apple_pos = preds.iter().position(|p| p.word == "apple").unwrap();
        assert!(
            apply_pos < apple_pos,
            "personal boost should rank 'apply' before 'apple'"
        );
    }

    #[test]
    fn test_empty_context() {
        let engine = test_engine();

        // Predict with no context at all.
        let preds = engine.predict("hel", &[], 3);
        assert!(
            !preds.is_empty(),
            "should produce predictions even without context"
        );

        // Without Markov context, ordering should follow corpus frequency:
        // hello (100) > help (80) > helicopter (20).
        assert_eq!(preds[0].word, "hello");
        if preds.len() > 1 {
            assert_eq!(preds[1].word, "help");
        }
    }

    #[test]
    fn test_empty_prefix() {
        let engine = test_engine();
        // Empty prefix should return top words from the whole trie.
        let preds = engine.predict("", &[], 3);
        assert_eq!(preds.len(), 3);
        // Top by corpus frequency: hello(100), world(90), help(80).
        assert_eq!(preds[0].word, "hello");
        assert_eq!(preds[1].word, "world");
        assert_eq!(preds[2].word, "help");
    }

    #[test]
    fn test_unknown_prefix_returns_empty() {
        let engine = test_engine();
        // "zzz" is too far from any word — even fuzzy can't help.
        let preds = engine.predict("zzz", &[], 5);
        assert!(preds.is_empty());
    }

    #[test]
    fn test_limit_zero() {
        let engine = test_engine();
        let preds = engine.predict("hel", &[], 0);
        assert!(preds.is_empty());
    }

    // ==================================================================
    // Fuzzy fallback integration tests
    // ==================================================================

    #[test]
    fn test_fuzzy_fallback_misspelled_prefix() {
        let mut engine = SmartKeyEngine::new();
        engine.load_word("function", 100);
        engine.load_word("functional", 60);
        engine.load_word("funnel", 30);

        // "fucn" — transposition of n and c — exact prefix search finds nothing.
        // Fuzzy fallback should kick in and return "function" / "functional".
        let preds = engine.predict("fucn", &[], 5);
        assert!(
            !preds.is_empty(),
            "misspelled prefix should produce predictions via fuzzy fallback"
        );
        assert!(
            preds.iter().any(|p| p.word == "function"),
            "'function' should appear for misspelled prefix 'fucn': got {:?}",
            preds.iter().map(|p| &p.word).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_fuzzy_fallback_substitution() {
        let mut engine = SmartKeyEngine::new();
        engine.load_word("hello", 100);
        engine.load_word("help", 80);

        // "hallo" — substitution of 'e' → 'a'. Exact prefix "hallo" finds nothing.
        let preds = engine.predict("hallo", &[], 5);
        assert!(
            preds.iter().any(|p| p.word == "hello"),
            "'hello' should appear for 'hallo': got {:?}",
            preds.iter().map(|p| &p.word).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_exact_matches_rank_above_fuzzy() {
        let mut engine = SmartKeyEngine::new();
        engine.load_word("help", 80);
        engine.load_word("hello", 100);
        engine.load_word("held", 40);
        // "helm" — only "hel" words match exact prefix. But "help" is also
        // fuzzy-matchable from "helm" (distance 1: m→p substitution).
        // Let's use a different setup: words starting with "hel" (exact match)
        // plus words reachable only by fuzzy.
        engine.load_word("hero", 90);

        // "hel" matches hello, help, held exactly. "hero" is not a prefix match.
        let preds = engine.predict("hel", &[], 5);
        // All three exact matches should appear.
        assert!(preds.iter().any(|p| p.word == "hello"));
        assert!(preds.iter().any(|p| p.word == "help"));
        assert!(preds.iter().any(|p| p.word == "held"));
        // "hero" could appear via fuzzy (distance 1: l→o), but since we already
        // have 3 exact matches and limit=5, fuzzy also kicks in.
        // The exact matches should rank higher than fuzzy.
        let hello_pos = preds.iter().position(|p| p.word == "hello").unwrap();
        if let Some(hero_pos) = preds.iter().position(|p| p.word == "hero") {
            assert!(
                hello_pos < hero_pos,
                "exact match 'hello' should rank above fuzzy 'hero'"
            );
        }
    }

    #[test]
    fn test_export_import_personal() {
        let mut engine = SmartKeyEngine::new();
        for _ in 0..50 {
            engine.learn("persisted");
        }
        let snap = engine.export_personal();
        assert!(snap.words.contains(&"persisted".to_string()) || snap.words.is_empty());

        let mut engine2 = SmartKeyEngine::new();
        engine2.import_personal(&snap);
        let snap2 = engine2.export_personal();
        assert_eq!(snap.words.len(), snap2.words.len());
    }

    #[test]
    fn test_fuzzy_discount_applied() {
        let mut engine = SmartKeyEngine::new();
        // Two words with same frequency. One matches exactly, one via fuzzy.
        engine.load_word("test", 100);
        engine.load_word("best", 100);

        // "tes" — exact prefix matches "test" only. "best" is distance 1.
        let preds = engine.predict("tes", &[], 5);
        let test_pred = preds.iter().find(|p| p.word == "test");
        let best_pred = preds.iter().find(|p| p.word == "best");
        assert!(test_pred.is_some(), "'test' should appear");
        if let (Some(tp), Some(bp)) = (test_pred, best_pred) {
            assert!(
                tp.score > bp.score,
                "exact match 'test' ({}) should score higher than fuzzy 'best' ({})",
                tp.score,
                bp.score
            );
        }
    }

    #[test]
    fn test_markov_normalization_affects_ranking() {
        // Two candidates with equal corpus frequency but different Markov support.
        // After normalization, the one with stronger bigram context should rank higher.
        let mut engine = SmartKeyEngine::new();
        engine.load_word("world", 50);
        engine.load_word("worry", 50);

        // Strong bigram: "hello" → "world" (10)
        engine.load_bigram("hello", "world", 10);
        // No bigram support for "worry" from "hello".

        let preds = engine.predict("wor", &["hello"], 5);
        let world_pos = preds.iter().position(|p| p.word == "world");
        let worry_pos = preds.iter().position(|p| p.word == "worry");
        assert!(
            world_pos.is_some() && worry_pos.is_some(),
            "both 'world' and 'worry' should appear"
        );
        assert!(
            world_pos.unwrap() < worry_pos.unwrap(),
            "'world' should rank above 'worry' due to Markov context from 'hello'"
        );
    }

    #[test]
    fn test_from_config_respects_fuzzy_discount() {
        let config = InputConfig {
            fuzzy_discounts: [1.0, 0.1, 0.05],
            ..InputConfig::default()
        };

        let mut engine = SmartKeyEngine::from_config(&config);
        engine.load_word("test", 100);
        engine.load_word("best", 100); // distance 1 from "tes" prefix

        // "tes" matches "test" exactly; "best" only via fuzzy (distance 1).
        let preds = engine.predict("tes", &[], 5);
        let test_pred = preds.iter().find(|p| p.word == "test");
        let best_pred = preds.iter().find(|p| p.word == "best");
        assert!(test_pred.is_some(), "'test' should appear");
        if let (Some(tp), Some(bp)) = (test_pred, best_pred) {
            // With discount 0.1 for distance-1, the gap should be very large.
            assert!(
                tp.score > bp.score * 5.0,
                "custom fuzzy discount 0.1 should heavily penalise fuzzy match: \
                 test={}, best={}",
                tp.score,
                bp.score
            );
        }
    }
}
