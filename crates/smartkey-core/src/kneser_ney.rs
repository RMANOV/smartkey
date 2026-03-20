// Interpolated Kneser-Ney smoothing — industry-standard n-gram LM.
//
// Uses corpus-estimated discount parameters (n1/n2/n3/n4 formula) instead of
// hardcoded values, providing better probability estimates for rare and unseen
// n-grams. Falls back to conservative defaults when corpus is too small.

use std::collections::HashMap;

use crate::markov::{BigramData, TrigramData};

/// Minimum probability floor.
const KN_FLOOR: f64 = 1e-8;

/// Fallback discount values when corpus is too small for reliable estimation.
const FALLBACK_D1: f64 = 0.75;
const FALLBACK_D2: f64 = 0.90;
const FALLBACK_D3_PLUS: f64 = 1.0;

/// Corpus-estimated discount parameters for Modified Kneser-Ney.
///
/// Computed from the n-gram count-of-counts (n1, n2, n3, n4) using the
/// standard Chen & Goodman (1999) formula:
///   Y = n1 / (n1 + 2·n2)
///   D1 = 1 - 2Y·(n2/n1)
///   D2 = 2 - 3Y·(n3/n2)
///   D3+ = 3 - 4Y·(n4/n3)
#[derive(Debug, Clone)]
struct EstimatedDiscounts {
    d1: f64,
    d2: f64,
    d3_plus: f64,
}

impl EstimatedDiscounts {
    /// Estimate discounts from count-of-counts.
    ///
    /// n1 = number of n-grams occurring exactly 1 time, etc.
    /// Falls back to conservative defaults when counts are too small.
    fn estimate(n1: u32, n2: u32, n3: u32, n4: u32) -> Self {
        // Need at least n1 > 0 and n2 > 0 for meaningful estimation.
        if n1 == 0 || n2 == 0 {
            return Self {
                d1: FALLBACK_D1,
                d2: FALLBACK_D2,
                d3_plus: FALLBACK_D3_PLUS,
            };
        }

        let n1f = n1 as f64;
        let n2f = n2 as f64;
        let n3f = n3.max(1) as f64; // avoid div-by-zero
        let n4f = n4.max(1) as f64;

        let y = n1f / (n1f + 2.0 * n2f);

        let d1 = (1.0 - 2.0 * y * (n2f / n1f)).clamp(0.01, 0.99);
        let d2 = (2.0 - 3.0 * y * (n3f / n2f)).clamp(0.01, 1.50);
        let d3_plus = (3.0 - 4.0 * y * (n4f / n3f)).clamp(0.01, 2.00);

        Self { d1, d2, d3_plus }
    }
}

/// Interpolated Kneser-Ney language model scorer.
///
/// Wraps bigram/trigram data and computes KN-smoothed probabilities.
/// Unlike Katz backoff, lower-order models use *continuation counts*
/// (how many unique contexts a word appears in) rather than raw frequency.
pub struct KneserNeyScorer {
    /// context → {word → count}
    bigrams: HashMap<String, HashMap<String, u32>>,
    /// (w1, w2) → {word → count}
    trigrams: HashMap<(String, String), HashMap<String, u32>>,
    /// word → number of unique left contexts (for continuation probability)
    continuation_count: HashMap<String, u32>,
    /// Total number of unique bigram types (for normalization)
    total_bigram_types: u32,
    /// Corpus-estimated discount parameters.
    discounts: EstimatedDiscounts,
}

impl KneserNeyScorer {
    /// Create a new scorer from existing Markov data.
    ///
    /// Takes the raw bigram and trigram maps from `MarkovChain`.
    /// Estimates discount parameters from the corpus n-gram count-of-counts.
    pub fn from_markov_data(bigram_data: &BigramData, trigram_data: &TrigramData) -> Self {
        let mut bigrams: HashMap<String, HashMap<String, u32>> = HashMap::new();
        let mut continuation_count: HashMap<String, u32> = HashMap::new();
        let mut total_bigram_types = 0u32;

        // Count-of-counts for discount estimation (from bigram counts).
        let mut n1 = 0u32; // bigrams occurring exactly 1 time
        let mut n2 = 0u32; // bigrams occurring exactly 2 times
        let mut n3 = 0u32; // bigrams occurring exactly 3 times
        let mut n4 = 0u32; // bigrams occurring exactly 4 times

        for (ctx, (followers, _)) in bigram_data {
            let entry = bigrams.entry(ctx.clone()).or_default();
            for (word, &count) in followers {
                entry.insert(word.clone(), count);
                // Each (ctx, word) pair is a unique bigram type that contributes
                // to word's continuation count.
                *continuation_count.entry(word.clone()).or_insert(0) += 1;
                total_bigram_types += 1;

                // Tally count-of-counts.
                match count {
                    1 => n1 += 1,
                    2 => n2 += 1,
                    3 => n3 += 1,
                    4 => n4 += 1,
                    _ => {}
                }
            }
        }

        let mut trigrams: HashMap<(String, String), HashMap<String, u32>> = HashMap::new();
        for (w1, level2) in trigram_data {
            for (w2, (followers, _)) in level2 {
                let key = (w1.clone(), w2.clone());
                let entry = trigrams.entry(key).or_default();
                for (word, &count) in followers {
                    entry.insert(word.clone(), count);
                }
            }
        }

        let discounts = EstimatedDiscounts::estimate(n1, n2, n3, n4);

        Self {
            bigrams,
            trigrams,
            continuation_count,
            total_bigram_types,
            discounts,
        }
    }

    /// Score a word with Modified Interpolated Kneser-Ney smoothing.
    ///
    /// Uses a two-level interpolation: each KN level (trigram, bigram) has its
    /// own discount-based lambda for internal backoff, and the top-level `score()`
    /// applies additional fixed mixing weights across levels. This deviates from
    /// standard MKN (which uses a single recursive interpolation) to allow tuning
    /// the relative influence of each n-gram order independently.
    ///
    /// * `recent` — most recent preceding word (bigram context)
    /// * `older` — second-to-last word (trigram context)
    pub fn score(&self, word: &str, recent: Option<&str>, older: Option<&str>) -> f64 {
        match (older, recent) {
            (Some(w1), Some(w2)) => {
                let p_tri = self.trigram_kn(word, w1, w2);
                let p_bi = self.bigram_kn(word, w2);
                let p_cont = self.continuation_prob(word);
                // Interpolation: trigram → bigram → continuation
                let score = 0.6 * p_tri + 0.3 * p_bi + 0.1 * p_cont;
                score.max(KN_FLOOR)
            }
            (None, Some(ctx)) => {
                let p_bi = self.bigram_kn(word, ctx);
                let p_cont = self.continuation_prob(word);
                let score = 0.75 * p_bi + 0.25 * p_cont;
                score.max(KN_FLOOR)
            }
            _ => self.continuation_prob(word).max(KN_FLOOR),
        }
    }

    /// KN-smoothed bigram probability P_KN(word | context).
    fn bigram_kn(&self, word: &str, context: &str) -> f64 {
        let followers = match self.bigrams.get(context) {
            Some(f) => f,
            None => return self.continuation_prob(word),
        };

        let total: u32 = followers.values().sum();
        if total == 0 {
            return self.continuation_prob(word);
        }

        let count = followers.get(word).copied().unwrap_or(0);
        let discount = self.discount(count);
        let discounted = ((count as f64) - discount).max(0.0) / total as f64;

        // Interpolation weight: sum of discounts across ALL followers of this context.
        let lambda = self.discount_mass(followers) / total as f64;

        discounted + lambda * self.continuation_prob(word)
    }

    /// KN-smoothed trigram probability P_KN(word | w1, w2).
    fn trigram_kn(&self, word: &str, w1: &str, w2: &str) -> f64 {
        let key = (w1.to_owned(), w2.to_owned());
        let followers = match self.trigrams.get(&key) {
            Some(f) => f,
            None => return self.bigram_kn(word, w2),
        };

        let total: u32 = followers.values().sum();
        if total == 0 {
            return self.bigram_kn(word, w2);
        }

        let count = followers.get(word).copied().unwrap_or(0);
        let discount = self.discount(count);
        let discounted = ((count as f64) - discount).max(0.0) / total as f64;

        let lambda = self.discount_mass(followers) / total as f64;

        discounted + lambda * self.bigram_kn(word, w2)
    }

    /// Continuation probability: how many unique contexts has this word appeared in?
    fn continuation_prob(&self, word: &str) -> f64 {
        if self.total_bigram_types == 0 {
            return KN_FLOOR;
        }
        let cont = self.continuation_count.get(word).copied().unwrap_or(0) as f64;
        (cont / self.total_bigram_types as f64).max(KN_FLOOR)
    }

    /// Total discount mass for a set of followers: sum(D(count_i)) for all followers.
    fn discount_mass(&self, followers: &HashMap<String, u32>) -> f64 {
        followers.values().map(|&c| self.discount(c)).sum()
    }

    /// Corpus-estimated discount: D1, D2, or D3+ depending on count.
    fn discount(&self, count: u32) -> f64 {
        match count {
            0 => 0.0,
            1 => self.discounts.d1,
            2 => self.discounts.d2,
            _ => self.discounts.d3_plus,
        }
    }
}

// ======================================================================
// Tests
// ======================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn test_scorer() -> KneserNeyScorer {
        let mut bigrams: BigramData = HashMap::new();
        let mut followers = HashMap::new();
        followers.insert("love".into(), 3u32);
        followers.insert("like".into(), 2);
        bigrams.insert("i".into(), (followers, 5));

        let mut followers2 = HashMap::new();
        followers2.insert("rust".into(), 4);
        followers2.insert("python".into(), 1);
        bigrams.insert("love".into(), (followers2, 5));

        let mut trigrams: TrigramData = HashMap::new();
        let mut tri_followers = HashMap::new();
        tri_followers.insert("rust".into(), 3u32);
        tri_followers.insert("python".into(), 1);
        let mut level2 = HashMap::new();
        level2.insert("love".into(), (tri_followers, 4));
        trigrams.insert("i".into(), level2);

        KneserNeyScorer::from_markov_data(&bigrams, &trigrams)
    }

    #[test]
    fn test_kn_basic_scoring() {
        let scorer = test_scorer();
        let rust_score = scorer.score("rust", Some("love"), Some("i"));
        let python_score = scorer.score("python", Some("love"), Some("i"));
        assert!(
            rust_score > python_score,
            "'rust' ({rust_score}) should score higher than 'python' ({python_score})"
        );
    }

    #[test]
    fn test_kn_continuation_prob() {
        let scorer = test_scorer();
        let rust_cont = scorer.continuation_prob("rust");
        let unknown_cont = scorer.continuation_prob("java");
        assert!(
            rust_cont > unknown_cont,
            "known word should have higher continuation prob: {rust_cont} vs {unknown_cont}"
        );
    }

    #[test]
    fn test_kn_unseen_context() {
        let scorer = test_scorer();
        let score = scorer.score("rust", Some("unknown_context"), None);
        assert!(
            score >= KN_FLOOR,
            "unseen context should still give non-zero score"
        );
    }

    #[test]
    fn test_kn_no_context() {
        let scorer = test_scorer();
        let score = scorer.score("rust", None, None);
        assert!(score >= KN_FLOOR);
    }

    #[test]
    fn test_kn_bigram_only() {
        let scorer = test_scorer();
        let score = scorer.score("rust", Some("love"), None);
        assert!(
            score > KN_FLOOR,
            "bigram context should boost score above floor"
        );
    }

    #[test]
    fn test_kn_scores_are_bounded() {
        let scorer = test_scorer();
        let score = scorer.score("rust", Some("love"), Some("i"));
        assert!(score >= 0.0, "KN score must be non-negative, got {}", score);
        let cont = scorer.continuation_prob("rust");
        assert!(
            cont >= 0.0,
            "continuation prob must be non-negative, got {}",
            cont
        );
    }

    #[test]
    fn test_discount_estimation() {
        // With known count-of-counts, verify the formula.
        let d = EstimatedDiscounts::estimate(100, 50, 30, 20);
        // Y = 100 / (100 + 100) = 0.5
        // D1 = 1 - 2*0.5*(50/100) = 1 - 0.5 = 0.5
        // D2 = 2 - 3*0.5*(30/50) = 2 - 0.9 = 1.1
        // D3+ = 3 - 4*0.5*(20/30) = 3 - 1.333 = 1.667
        assert!((d.d1 - 0.5).abs() < 1e-6, "D1 should be ~0.5, got {}", d.d1);
        assert!(
            (d.d2 - 1.1).abs() < 1e-6,
            "D2 should be ~1.1, got {}",
            d.d2
        );
        assert!(
            (d.d3_plus - 5.0 / 3.0).abs() < 1e-6,
            "D3+ should be ~1.667, got {}",
            d.d3_plus
        );
    }

    #[test]
    fn test_discount_estimation_small_corpus() {
        // With n1=0, should fall back to defaults.
        let d = EstimatedDiscounts::estimate(0, 0, 0, 0);
        assert!((d.d1 - FALLBACK_D1).abs() < 1e-9);
        assert!((d.d2 - FALLBACK_D2).abs() < 1e-9);
        assert!((d.d3_plus - FALLBACK_D3_PLUS).abs() < 1e-9);
    }
}
