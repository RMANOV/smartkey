// Ensemble scorer — combines corpus frequency, Markov context, and CVM personal boost.

use crate::cvm::CvmCounter;
use crate::markov::MarkovChain;
use crate::ngram::NgramTrie;

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
}

impl SmartKeyEngine {
    /// Create a new engine with default weights `(0.4, 0.4, 0.2)` and a
    /// CVM counter sized `(500, 5000)`.
    pub fn new() -> Self {
        Self {
            trie: NgramTrie::new(),
            markov: MarkovChain::new(),
            personal: CvmCounter::new(500, 5000),
            alpha: 0.4,
            beta: 0.4,
            gamma: 0.2,
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

        // Step 1: candidate generation via trie prefix search.
        let candidates = self.trie.prefix_search(prefix, limit * 3);
        if candidates.is_empty() {
            return Vec::new();
        }

        // Determine the max corpus frequency among candidates for normalisation.
        let max_freq = candidates
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
        let max_personal = candidates
            .iter()
            .map(|c| self.personal.frequency_score(&c.word))
            .fold(0.0_f64, f64::max)
            .max(1.0); // avoid division by zero

        // Step 2–3: score each candidate.
        let mut scored: Vec<Prediction> = candidates
            .iter()
            .map(|entry| {
                let corpus_score = entry.frequency as f64 / max_freq;
                let markov_score = self.markov.score_with_backoff(&entry.word, prev1, prev2);
                let personal_score = self.personal.frequency_score(&entry.word) / max_personal;

                let score = self.alpha * corpus_score
                    + self.beta * markov_score
                    + self.gamma * personal_score;

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
        // CVM is probabilistic — run multiple trials and check that the
        // personal boost works in the majority of them.
        let mut boosted = 0;
        let trials = 50;

        for _ in 0..trials {
            let mut engine = SmartKeyEngine::new();
            engine.load_word("apple", 50);
            engine.load_word("apply", 50);

            // Learn "apply" so CVM retains it.
            for _ in 0..100 {
                engine.learn("apply");
            }

            let preds = engine.predict("app", &[], 5);
            if preds.len() >= 2 {
                let apply_pos = preds.iter().position(|p| p.word == "apply");
                let apple_pos = preds.iter().position(|p| p.word == "apple");
                if let (Some(ap), Some(al)) = (apply_pos, apple_pos) {
                    if ap < al {
                        boosted += 1;
                    }
                }
            }
        }

        // Personal boost should win in at least 50% of trials (67% expected).
        assert!(
            boosted >= trials / 2,
            "personal boost should rank 'apply' first in >=50% of trials, \
             but only won {boosted}/{trials}"
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
        let preds = engine.predict("zzz", &[], 5);
        assert!(preds.is_empty());
    }

    #[test]
    fn test_limit_zero() {
        let engine = test_engine();
        let preds = engine.predict("hel", &[], 0);
        assert!(preds.is_empty());
    }
}
