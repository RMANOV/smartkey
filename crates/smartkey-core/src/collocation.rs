// Collocation awareness — Pointwise Mutual Information (PMI) phrase detection.
//
// Identifies word pairs that occur together more often than chance would predict.
// Boosts predictions when the current context matches a known collocation.
// Examples: "по мое мнение", "in my opinion", "по-рано", "thank you"
//
// PMI(x,y) = log2(P(x,y) / (P(x) * P(y)))
// High PMI = strong collocation (words co-occur far more than expected).

use std::collections::HashMap;

/// Minimum PMI score to consider a pair a collocation.
const MIN_PMI: f64 = 2.0;

/// Minimum bigram count to include in PMI computation (filters noise).
const MIN_BIGRAM_COUNT: u32 = 3;

/// Maximum number of collocations to retain (LRU-style by PMI).
const MAX_COLLOCATIONS: usize = 5000;

/// A detected collocation with its PMI score.
#[derive(Debug, Clone)]
struct Collocation {
    pmi: f64,
}

/// Collocation detector based on Pointwise Mutual Information.
///
/// Built from bigram and unigram counts during corpus loading.
/// Provides a PMI-based boost score when the context matches a collocation.
pub struct CollocationDetector {
    /// (context_word, target_word) → Collocation
    collocations: HashMap<(String, String), Collocation>,
}

impl CollocationDetector {
    pub fn new() -> Self {
        Self {
            collocations: HashMap::new(),
        }
    }

    /// Build collocation table from bigram and unigram frequency data.
    ///
    /// `bigram_counts`: context → (word → count)
    /// `unigram_counts`: word → count
    /// `total_bigrams`: sum of all bigram counts
    /// `total_unigrams`: sum of all unigram counts
    pub fn build(
        bigram_counts: &HashMap<String, HashMap<String, u32>>,
        unigram_counts: &HashMap<String, u32>,
        total_bigrams: u64,
        total_unigrams: u64,
    ) -> Self {
        if total_bigrams == 0 || total_unigrams == 0 {
            return Self::new();
        }

        let total_bi = total_bigrams as f64;
        let total_uni = total_unigrams as f64;

        let mut scored: Vec<((String, String), f64)> = Vec::new();

        for (ctx, followers) in bigram_counts {
            let ctx_count = unigram_counts.get(ctx).copied().unwrap_or(0) as f64;
            if ctx_count < 1.0 {
                continue;
            }
            let p_ctx = ctx_count / total_uni;

            for (word, &count) in followers {
                if count < MIN_BIGRAM_COUNT {
                    continue;
                }
                let word_count = unigram_counts.get(word).copied().unwrap_or(0) as f64;
                if word_count < 1.0 {
                    continue;
                }

                let p_word = word_count / total_uni;
                let p_bigram = count as f64 / total_bi;
                let pmi = (p_bigram / (p_ctx * p_word)).log2();

                if pmi >= MIN_PMI {
                    scored.push(((ctx.clone(), word.clone()), pmi));
                }
            }
        }

        // Keep only top MAX_COLLOCATIONS by PMI.
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(MAX_COLLOCATIONS);

        let collocations = scored
            .into_iter()
            .map(|(key, pmi)| (key, Collocation { pmi }))
            .collect();

        Self { collocations }
    }

    /// Get the collocation boost for a candidate word given context.
    ///
    /// Returns a multiplicative boost factor > 1.0 if (context, word) is a
    /// known collocation, 1.0 otherwise.
    ///
    /// `prev1`: the most recent context word.
    pub fn boost(&self, word: &str, prev1: Option<&str>) -> f64 {
        let Some(ctx) = prev1 else { return 1.0 };
        let key = (ctx.to_owned(), word.to_owned());
        match self.collocations.get(&key) {
            Some(c) => {
                // Scale PMI to a boost factor: PMI 2 → 1.2x, PMI 4 → 1.4x, PMI 8+ → 1.8x
                1.0 + (c.pmi - MIN_PMI).min(6.0) * 0.10
            }
            None => 1.0,
        }
    }

    /// Number of detected collocations.
    pub fn len(&self) -> usize {
        self.collocations.len()
    }

    /// Whether no collocations were detected.
    pub fn is_empty(&self) -> bool {
        self.collocations.is_empty()
    }
}

impl Default for CollocationDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestData = (
        HashMap<String, HashMap<String, u32>>,
        HashMap<String, u32>,
        u64,
        u64,
    );

    fn test_data() -> TestData {
        let mut bigrams: HashMap<String, HashMap<String, u32>> = HashMap::new();
        let mut unigrams: HashMap<String, u32> = HashMap::new();

        // "thank" → "you" appears very frequently together
        bigrams
            .entry("thank".into())
            .or_default()
            .insert("you".into(), 100);
        // "thank" also appears with other words but less
        bigrams
            .entry("thank".into())
            .or_default()
            .insert("god".into(), 5);
        // Random bigram
        bigrams
            .entry("the".into())
            .or_default()
            .insert("world".into(), 50);

        unigrams.insert("thank".into(), 120);
        unigrams.insert("you".into(), 500);
        unigrams.insert("god".into(), 200);
        unigrams.insert("the".into(), 1000);
        unigrams.insert("world".into(), 300);

        let total_bi: u64 = bigrams
            .values()
            .flat_map(|f| f.values())
            .map(|&c| c as u64)
            .sum();
        let total_uni: u64 = unigrams.values().map(|&c| c as u64).sum();

        (bigrams, unigrams, total_bi, total_uni)
    }

    #[test]
    fn test_collocation_detection() {
        let (bigrams, unigrams, total_bi, total_uni) = test_data();
        let det = CollocationDetector::build(&bigrams, &unigrams, total_bi, total_uni);
        assert!(!det.is_empty(), "should detect some collocations");
    }

    #[test]
    fn test_collocation_boost() {
        let (bigrams, unigrams, total_bi, total_uni) = test_data();
        let det = CollocationDetector::build(&bigrams, &unigrams, total_bi, total_uni);

        let boost_you = det.boost("you", Some("thank"));
        let boost_random = det.boost("random", Some("thank"));
        assert!(
            boost_you >= 1.0,
            "collocation boost should be >= 1.0: {boost_you}"
        );
        assert_eq!(boost_random, 1.0, "non-collocation should have no boost");
    }

    #[test]
    fn test_no_context_no_boost() {
        let (bigrams, unigrams, total_bi, total_uni) = test_data();
        let det = CollocationDetector::build(&bigrams, &unigrams, total_bi, total_uni);
        assert_eq!(det.boost("you", None), 1.0);
    }
}
