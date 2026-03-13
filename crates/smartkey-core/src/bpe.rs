// BPE-Light OOV fallback — subword tokenization for out-of-vocabulary words.
//
// Strictly an OOV fallback: only triggers when BOTH exact and fuzzy search
// return zero candidates. Decomposes unknown words into subword tokens and
// suggests completions by combining known subwords.

use std::collections::HashMap;

/// A single BPE merge rule: pair (a, b) → merged.
#[derive(Debug, Clone)]
pub struct MergeRule {
    pub a: String,
    pub b: String,
    pub merged: String,
}

/// BPE tokenizer for subword-based OOV handling.
pub struct BpeTokenizer {
    /// Ordered list of merge rules (applied in priority order).
    merges: Vec<MergeRule>,
    /// Reverse lookup: merged → (a, b) for decomposition.
    merge_map: HashMap<String, (String, String)>,
    /// Subword token frequencies from corpus training.
    token_freqs: HashMap<String, u32>,
}

impl BpeTokenizer {
    /// Create a new tokenizer from merge rules and token frequencies.
    pub fn new(merges: Vec<MergeRule>, token_freqs: HashMap<String, u32>) -> Self {
        let merge_map = merges
            .iter()
            .map(|r| (r.merged.clone(), (r.a.clone(), r.b.clone())))
            .collect();
        Self {
            merges,
            merge_map,
            token_freqs,
        }
    }

    /// Create an empty tokenizer (no merges).
    pub fn empty() -> Self {
        Self {
            merges: Vec::new(),
            merge_map: HashMap::new(),
            token_freqs: HashMap::new(),
        }
    }

    /// Whether this tokenizer has any merge rules loaded.
    pub fn is_loaded(&self) -> bool {
        !self.merges.is_empty()
    }

    /// Tokenize a word into subword units using BPE merge rules.
    ///
    /// Starts with individual characters, then greedily applies merges
    /// in priority order.
    pub fn tokenize(&self, word: &str) -> Vec<String> {
        // Start with individual characters.
        let mut tokens: Vec<String> = word.chars().map(|c| c.to_string()).collect();

        if tokens.is_empty() {
            return tokens;
        }

        // Apply merges in priority order.
        for rule in &self.merges {
            let mut i = 0;
            while i + 1 < tokens.len() {
                if tokens[i] == rule.a && tokens[i + 1] == rule.b {
                    tokens[i] = rule.merged.clone();
                    tokens.remove(i + 1);
                    // Don't increment i — check if the merged token can merge again.
                } else {
                    i += 1;
                }
            }
        }

        tokens
    }

    /// Compute a subword-level score for a word.
    ///
    /// Score = geometric mean of token frequencies (avoids bias toward short words).
    pub fn subword_score(&self, word: &str) -> f64 {
        let tokens = self.tokenize(word);
        if tokens.is_empty() {
            return 0.0;
        }

        let mut log_sum = 0.0;
        for token in &tokens {
            let freq = self.token_freqs.get(token).copied().unwrap_or(1) as f64;
            log_sum += freq.ln();
        }

        (log_sum / tokens.len() as f64).exp()
    }

    /// Suggest completions for a prefix using subword decomposition.
    ///
    /// Strategy:
    /// 1. Tokenize the prefix.
    /// 2. Find all tokens that could follow the last token.
    /// 3. Build completion candidates from token combinations.
    pub fn suggest_completions(&self, prefix: &str, limit: usize) -> Vec<(String, f64)> {
        if !self.is_loaded() || prefix.is_empty() {
            return Vec::new();
        }

        let prefix_tokens = self.tokenize(prefix);
        let last_token = match prefix_tokens.last() {
            Some(t) => t.clone(),
            None => return Vec::new(),
        };

        // Find tokens that commonly follow the last token in merge rules.
        let mut candidates: Vec<(String, f64)> = Vec::new();

        for rule in &self.merges {
            if rule.a == last_token {
                // Build a completion: prefix + rule.b
                let completion = format!("{}{}", prefix, rule.b);
                let score = self.subword_score(&completion);
                candidates.push((completion, score));
            }
        }

        // Also try extending with high-frequency tokens.
        let mut freq_tokens: Vec<(&String, &u32)> = self.token_freqs.iter().collect();
        freq_tokens.sort_by(|a, b| b.1.cmp(a.1));

        for (token, _) in freq_tokens.iter().take(20) {
            let completion = format!("{}{}", prefix, token);
            if !candidates.iter().any(|(c, _)| c == &completion) {
                let score = self.subword_score(&completion);
                candidates.push((completion, score));
            }
        }

        // Sort by score descending, truncate.
        candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        candidates.truncate(limit);
        candidates
    }

    /// Number of merge rules.
    pub fn merge_count(&self) -> usize {
        self.merges.len()
    }
}

/// Parse BPE merge rules from a list of (a, b, merged) tuples.
///
/// Used when loading from corpus JSON's optional `bpe_merges` field.
pub fn parse_merge_rules(raw: &[(String, String, String)]) -> Vec<MergeRule> {
    raw.iter()
        .map(|(a, b, m)| MergeRule {
            a: a.clone(),
            b: b.clone(),
            merged: m.clone(),
        })
        .collect()
}

// ======================================================================
// Tests
// ======================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn test_tokenizer() -> BpeTokenizer {
        let merges = vec![
            MergeRule { a: "с".into(), b: "а".into(), merged: "са".into() },
            MergeRule { a: "м".into(), b: "о".into(), merged: "мо".into() },
            MergeRule { a: "л".into(), b: "е".into(), merged: "ле".into() },
            MergeRule { a: "са".into(), b: "мо".into(), merged: "само".into() },
            MergeRule { a: "ле".into(), b: "т".into(), merged: "лет".into() },
            MergeRule { a: "само".into(), b: "лет".into(), merged: "самолет".into() },
        ];
        let mut freqs = HashMap::new();
        freqs.insert("само".into(), 100);
        freqs.insert("лет".into(), 50);
        freqs.insert("самолет".into(), 30);
        freqs.insert("са".into(), 200);
        freqs.insert("мо".into(), 150);
        freqs.insert("ле".into(), 80);

        BpeTokenizer::new(merges, freqs)
    }

    #[test]
    fn test_bpe_tokenize_compound() {
        let t = test_tokenizer();
        let tokens = t.tokenize("самолет");
        // Should merge all the way to "самолет" as a single token.
        assert!(
            tokens.len() <= 2,
            "compound word should be tokenized into few subwords, got {:?}",
            tokens
        );
    }

    #[test]
    fn test_bpe_tokenize_unknown() {
        let t = test_tokenizer();
        let tokens = t.tokenize("xyz");
        // Unknown characters stay as individual tokens.
        assert_eq!(tokens, vec!["x", "y", "z"]);
    }

    #[test]
    fn test_bpe_subword_score() {
        let t = test_tokenizer();
        let known_score = t.subword_score("само");
        let unknown_score = t.subword_score("xyz");
        assert!(
            known_score > unknown_score,
            "known subword should score higher: {known_score} vs {unknown_score}"
        );
    }

    #[test]
    fn test_bpe_oov_suggestions() {
        let t = test_tokenizer();
        let suggestions = t.suggest_completions("само", 5);
        assert!(
            !suggestions.is_empty(),
            "should suggest completions for known subword prefix"
        );
    }

    #[test]
    fn test_bpe_not_triggered_for_empty() {
        let t = test_tokenizer();
        let suggestions = t.suggest_completions("", 5);
        assert!(suggestions.is_empty());
    }

    #[test]
    fn test_bpe_empty_tokenizer() {
        let t = BpeTokenizer::empty();
        assert!(!t.is_loaded());
        let suggestions = t.suggest_completions("test", 5);
        assert!(suggestions.is_empty());
    }

    #[test]
    fn test_parse_merge_rules() {
        let raw = vec![
            ("a".into(), "b".into(), "ab".into()),
            ("ab".into(), "c".into(), "abc".into()),
        ];
        let rules = parse_merge_rules(&raw);
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].merged, "ab");
        assert_eq!(rules[1].merged, "abc");
    }
}
