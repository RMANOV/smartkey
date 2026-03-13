// Interpolated Kneser-Ney smoothing — industry-standard n-gram LM.
//
// Experimental (behind `use_kneser_ney` flag). Uses continuation counts
// for lower-order models instead of raw frequency, providing better
// probability estimates for rare and unseen n-grams than Katz backoff.

use std::collections::{HashMap, HashSet};

/// Discount parameters for Modified Kneser-Ney.
const D1: f64 = 0.75;
const D2: f64 = 1.0;
const D3_PLUS: f64 = 1.5;

/// Minimum probability floor.
const KN_FLOOR: f64 = 1e-8;

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
    /// Vocabulary set.
    vocab: HashSet<String>,
}

impl KneserNeyScorer {
    /// Create a new scorer from existing Markov data.
    ///
    /// Takes the raw bigram and trigram maps from `MarkovChain`.
    pub fn from_markov_data(
        bigram_data: &HashMap<String, (HashMap<String, u32>, u32)>,
        trigram_data: &HashMap<String, HashMap<String, (HashMap<String, u32>, u32)>>,
    ) -> Self {
        let mut bigrams: HashMap<String, HashMap<String, u32>> = HashMap::new();
        let mut vocab = HashSet::new();
        let mut continuation_count: HashMap<String, u32> = HashMap::new();
        let mut total_bigram_types = 0u32;

        for (ctx, (followers, _)) in bigram_data {
            vocab.insert(ctx.clone());
            let entry = bigrams.entry(ctx.clone()).or_default();
            for (word, &count) in followers {
                vocab.insert(word.clone());
                entry.insert(word.clone(), count);
                // Each (ctx, word) pair is a unique bigram type that contributes
                // to word's continuation count.
                *continuation_count.entry(word.clone()).or_insert(0) += 1;
                total_bigram_types += 1;
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

        Self {
            bigrams,
            trigrams,
            continuation_count,
            total_bigram_types,
            vocab,
        }
    }

    /// Score a word with Interpolated Kneser-Ney smoothing.
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

        // Interpolation weight: proportional to total discount mass.
        let n_types = followers.len() as f64;
        let lambda = (discount * n_types) / total as f64;

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

        let n_types = followers.len() as f64;
        let lambda = (discount * n_types) / total as f64;

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

    /// Modified KN discount: D1, D2, or D3+ depending on count.
    fn discount(&self, count: u32) -> f64 {
        match count {
            0 => 0.0,
            1 => D1,
            2 => D2,
            _ => D3_PLUS,
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
        let mut bigrams: HashMap<String, (HashMap<String, u32>, u32)> = HashMap::new();
        let mut followers = HashMap::new();
        followers.insert("love".into(), 3u32);
        followers.insert("like".into(), 2);
        bigrams.insert("i".into(), (followers, 5));

        let mut followers2 = HashMap::new();
        followers2.insert("rust".into(), 4);
        followers2.insert("python".into(), 1);
        bigrams.insert("love".into(), (followers2, 5));

        let mut trigrams: HashMap<String, HashMap<String, (HashMap<String, u32>, u32)>> =
            HashMap::new();
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
        // "rust" appears in 2 unique bigram contexts (i→rust doesn't exist directly,
        // but love→rust does), so continuation count should be >= 1
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
        assert!(score >= KN_FLOOR, "unseen context should still give non-zero score");
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
        assert!(score > KN_FLOOR, "bigram context should boost score above floor");
    }
}
