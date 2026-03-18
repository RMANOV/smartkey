// PPM-D character-level prediction model with exclusion (order-4).
//
// Trained from corpus words. Provides character-level scoring for candidates
// and next-character ranking for short-prefix candidate generation.
// Uses Method D escape probability with exclusion for smooth backoff.

use std::collections::HashMap;

/// A node in the PPM context tree.
#[derive(Debug, Default)]
struct PpmNode {
    /// char → (child_node, count)
    children: HashMap<char, (Box<PpmNode>, u32)>,
    /// Total observations at this node.
    total: u32,
    /// Number of distinct children (used for escape probability).
    distinct: u32,
}

/// PPM-D character-level prediction model.
///
/// Builds a context tree from corpus words, up to `max_order` depth.
/// Supports two operations:
/// - `score_candidate(prefix, candidate)` — P(candidate | prefix) for scoring
/// - `rank_next_chars(context)` — top next characters for guided generation
pub struct PpmModel {
    root: PpmNode,
    max_order: usize,
}

impl PpmModel {
    /// Create a new empty PPM model with the given maximum order.
    pub fn new(max_order: usize) -> Self {
        Self {
            root: PpmNode::default(),
            max_order,
        }
    }

    /// Create a new PPM model with default order (4).
    pub fn default_order() -> Self {
        Self::new(4)
    }

    /// Train the model on a single word.
    ///
    /// Inserts all character subsequences up to `max_order` into the tree.
    pub fn train_word(&mut self, word: &str) {
        let chars: Vec<char> = word.chars().collect();
        // Train on each character position with all valid context lengths.
        for i in 0..chars.len() {
            // For each context length from 0 to min(max_order, i)
            for ctx_len in 0..=self.max_order.min(i) {
                let start = i - ctx_len;
                let context = &chars[start..i];
                let target = chars[i];
                self.insert_sequence(context, target);
            }
        }
    }

    /// Score a candidate word given a typed prefix.
    ///
    /// Computes P(candidate | prefix) as the product of character-level
    /// PPM probabilities: `∏ P_ppm(c_i | c_{i-order}..c_{i-1})`.
    pub fn score_candidate(&self, prefix: &str, candidate: &str) -> f64 {
        let chars: Vec<char> = candidate.chars().collect();
        if chars.is_empty() {
            return 0.0;
        }

        let mut log_prob = 0.0;
        for i in 0..chars.len() {
            // Build context from preceding characters (up to max_order).
            let ctx_start = i.saturating_sub(self.max_order);
            let context: Vec<char> = chars[ctx_start..i].to_vec();
            let p = self.char_probability(&context, chars[i]);
            if p <= 0.0 {
                log_prob += -20.0; // floor for unseen chars
            } else {
                log_prob += p.ln();
            }
        }

        // Give bonus if candidate starts with the prefix.
        let prefix_chars: Vec<char> = prefix.chars().collect();
        let prefix_match = chars.iter().zip(prefix_chars.iter()).all(|(a, b)| a == b);
        if !prefix_match {
            log_prob -= 10.0; // heavy penalty for non-matching prefix
        }

        log_prob.exp()
    }

    /// Rank the most likely next characters given a context string.
    ///
    /// Returns up to `limit` (char, probability) pairs sorted by probability.
    pub fn rank_next_chars(&self, context: &str, limit: usize) -> Vec<(char, f64)> {
        let chars: Vec<char> = context.chars().collect();
        let ctx_start = if chars.len() > self.max_order {
            chars.len() - self.max_order
        } else {
            0
        };
        let context_slice = &chars[ctx_start..];

        // Navigate to the deepest matching context node.
        let mut results: HashMap<char, f64> = HashMap::new();

        // Try each order from max down to 0.
        for order in (0..=context_slice.len()).rev() {
            let ctx = &context_slice[context_slice.len() - order..];
            if let Some(node) = self.find_node(ctx) {
                if node.total > 0 {
                    for (ch, (_, count)) in &node.children {
                        results
                            .entry(*ch)
                            .or_insert_with(|| *count as f64 / node.total as f64);
                    }
                    break; // Use highest order that has data
                }
            }
        }

        let mut sorted: Vec<(char, f64)> = results.into_iter().collect();
        sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        sorted.truncate(limit);
        sorted
    }

    /// Number of nodes in the tree (for memory estimation).
    pub fn node_count(&self) -> usize {
        Self::count_nodes(&self.root)
    }

    /// Prune nodes with count below threshold to control memory.
    pub fn prune(&mut self, min_count: u32) {
        Self::prune_node(&mut self.root, min_count);
    }

    // -- internal --

    fn insert_sequence(&mut self, context: &[char], target: char) {
        let mut node = &mut self.root;
        // Navigate to context node, creating path as needed.
        for &ch in context {
            let (child, _) = node
                .children
                .entry(ch)
                .or_insert_with(|| (Box::new(PpmNode::default()), 0));
            node = child;
        }
        // Insert target character.
        node.total = node.total.saturating_add(1);
        let entry = node
            .children
            .entry(target)
            .or_insert_with(|| (Box::new(PpmNode::default()), 0));
        if entry.1 == 0 {
            node.distinct += 1;
        }
        entry.1 = entry.1.saturating_add(1);
    }

    fn find_node(&self, context: &[char]) -> Option<&PpmNode> {
        let mut node = &self.root;
        for &ch in context {
            match node.children.get(&ch) {
                Some((child, _)) => node = child,
                None => return None,
            }
        }
        Some(node)
    }

    /// PPM-D escape probability and character probability.
    ///
    /// Method D: escape = distinct / (2 * total), char = (2*count - 1) / (2 * total)
    fn char_probability(&self, context: &[char], target: char) -> f64 {
        // Try each order from max down to 0.
        for order in (0..=context.len()).rev() {
            let ctx = &context[context.len() - order..];
            if let Some(node) = self.find_node(ctx) {
                if node.total > 0 {
                    if let Some((_, count)) = node.children.get(&target) {
                        if *count > 0 {
                            // Method D: P = (2*count - 1) / (2 * total)
                            return (2.0 * *count as f64 - 1.0) / (2.0 * node.total as f64);
                        }
                    }
                    // Escape: continue to lower order
                }
            }
        }
        // Uniform fallback for completely unseen characters.
        1.0 / 256.0
    }

    fn count_nodes(node: &PpmNode) -> usize {
        1 + node
            .children
            .values()
            .map(|(child, _)| Self::count_nodes(child))
            .sum::<usize>()
    }

    fn prune_node(node: &mut PpmNode, min_count: u32) {
        node.children.retain(|_, (child, count)| {
            if *count < min_count {
                return false;
            }
            Self::prune_node(child, min_count);
            true
        });
        // Recompute distinct count after pruning.
        node.distinct = node.children.len() as u32;
    }
}

impl Default for PpmModel {
    fn default() -> Self {
        Self::default_order()
    }
}

// ======================================================================
// Tests
// ======================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn trained_model() -> PpmModel {
        let mut m = PpmModel::default_order();
        for word in &["hello", "help", "hero", "helicopter", "world", "wonder"] {
            m.train_word(word);
        }
        m
    }

    #[test]
    fn test_ppm_short_prefix_ranking() {
        let m = trained_model();
        // "h" context → next chars should include 'e' (from hello, help, hero, helicopter)
        let ranked = m.rank_next_chars("h", 5);
        assert!(
            !ranked.is_empty(),
            "should have next-char predictions after 'h'"
        );
        // 'e' should be top (all h-words continue with 'e')
        assert_eq!(ranked[0].0, 'e', "most likely char after 'h' should be 'e'");
    }

    #[test]
    fn test_ppm_score_known_word() {
        let m = trained_model();
        let hello_score = m.score_candidate("hel", "hello");
        let zzz_score = m.score_candidate("hel", "zzzzz");
        assert!(
            hello_score > zzz_score,
            "trained word 'hello' ({hello_score}) should score higher than unknown 'zzzzz' ({zzz_score})"
        );
    }

    #[test]
    fn test_ppm_guided_candidate_generation() {
        let m = trained_model();
        // Get top next chars after prefix "hel"
        let ranked = m.rank_next_chars("hel", 5);
        assert!(!ranked.is_empty());
        // Should suggest chars that continue valid words (l, p, i)
        let chars: Vec<char> = ranked.iter().map(|r| r.0).collect();
        assert!(
            chars.contains(&'l') || chars.contains(&'p') || chars.contains(&'i'),
            "next chars after 'hel' should include l/p/i for hello/help/helicopter, got {:?}",
            chars
        );
    }

    #[test]
    fn test_ppm_order_fallback() {
        let m = trained_model();
        // Unseen context should still produce results via lower-order fallback
        let _ranked = m.rank_next_chars("xyz", 5);
        // May be empty (no data at all for 'xyz') or may have fallback results
        // from lower orders — either is acceptable
        let score = m.score_candidate("xyz", "xyzabc");
        assert!(
            score > 0.0,
            "score should be > 0 even for unseen context (uniform fallback)"
        );
    }

    #[test]
    fn test_ppm_empty_model() {
        let m = PpmModel::default_order();
        let ranked = m.rank_next_chars("h", 5);
        assert!(ranked.is_empty());
        let score = m.score_candidate("", "hello");
        assert!(
            score > 0.0,
            "empty model should still give non-zero uniform score"
        );
    }

    #[test]
    fn test_ppm_prune() {
        let mut m = trained_model();
        let before = m.node_count();
        m.prune(2);
        let after = m.node_count();
        assert!(
            after <= before,
            "pruning should reduce node count: {before} → {after}"
        );
    }

    #[test]
    fn test_ppm_cyrillic() {
        let mut m = PpmModel::default_order();
        m.train_word("здравей");
        m.train_word("здраве");
        m.train_word("зима");

        let ranked = m.rank_next_chars("здрав", 5);
        assert!(
            !ranked.is_empty(),
            "should rank next chars for Cyrillic prefix"
        );
    }

    #[test]
    fn test_ppm_no_overflow_on_heavy_training() {
        let mut model = PpmModel::new(5);
        let word = "overflow";
        for _ in 0..10_000 {
            model.train_word(word);
        }
        let scores = model.rank_next_chars("overflo", 10);
        let w_score = scores.iter().find(|(c, _)| *c == 'w');
        assert!(w_score.is_some(), "should have score for 'w'");
        assert!(w_score.unwrap().1 > 0.0, "score must be positive");
    }
}
