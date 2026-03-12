// N-gram trie storage — word dictionary with frequency-ranked prefix search.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

/// A word with its associated frequency count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WordEntry {
    pub word: String,
    pub frequency: u32,
}

/// A fuzzy match result with edit distance information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyMatch {
    pub word: String,
    pub frequency: u32,
    pub edit_distance: u8,
}

/// Internal trie node. Children are keyed by `char` (not byte), which
/// naturally handles multi-byte UTF-8 scripts like Cyrillic.
#[derive(Debug, Default)]
struct TrieNode {
    children: HashMap<char, TrieNode>,
    /// `Some(freq)` when this node marks the end of a stored word.
    frequency: Option<u32>,
    /// Maximum frequency in this subtree (enables top-K pruning).
    ///
    /// **Invariant:** Only maintained for non-decreasing frequency updates.
    /// If `insert()` is called with a *lower* frequency for an existing word,
    /// `max_freq` may remain stale (over-estimate). This is acceptable because
    /// it only weakens pruning (fewer subtrees skipped), never causes incorrect
    /// results. Callers should ensure frequencies are non-decreasing, or accept
    /// slightly degraded pruning performance.
    max_freq: u32,
}

/// Character-level trie that stores words with frequency counts and
/// supports fast prefix search with results ranked by frequency.
#[derive(Debug)]
pub struct NgramTrie {
    root: TrieNode,
    /// Number of distinct words stored.
    count: usize,
}

impl NgramTrie {
    /// Create an empty trie.
    pub fn new() -> Self {
        Self {
            root: TrieNode::default(),
            count: 0,
        }
    }

    /// Insert a word with the given frequency. If the word already exists
    /// its frequency is **replaced** (last-write-wins).
    pub fn insert(&mut self, word: &str, frequency: u32) {
        let mut node = &mut self.root;
        node.max_freq = node.max_freq.max(frequency);
        for ch in word.chars() {
            node = node.children.entry(ch).or_default();
            node.max_freq = node.max_freq.max(frequency);
        }
        if node.frequency.is_none() {
            self.count += 1;
        }
        node.frequency = Some(frequency);
    }

    /// Return all words that start with `prefix`, sorted by frequency
    /// descending, truncated to `limit`. An empty prefix returns the
    /// highest-frequency words across the entire trie.
    pub fn prefix_search(&self, prefix: &str, limit: usize) -> Vec<WordEntry> {
        // Navigate to the node that represents `prefix`.
        let mut node = &self.root;
        for ch in prefix.chars() {
            match node.children.get(&ch) {
                Some(child) => node = child,
                None => return Vec::new(),
            }
        }

        // Min-heap of (frequency, word) — keeps top-K by frequency.
        // Reverse wrapping makes this a min-heap so we can efficiently
        // evict the weakest candidate when a better one appears.
        let mut heap: BinaryHeap<Reverse<(u32, String)>> =
            BinaryHeap::with_capacity(limit + 1);
        let mut current_word = prefix.to_string();
        Self::collect_top_k(node, &mut current_word, limit, &mut heap);

        // into_sorted_vec on BinaryHeap<Reverse<T>> returns ascending Reverse order
        // = descending original order = highest frequency first. Exactly what we want.
        heap.into_sorted_vec()
            .into_iter()
            .map(|Reverse((freq, word))| WordEntry { word, frequency: freq })
            .collect()
    }

    /// Find words whose prefix is within Damerau-Levenshtein distance `max_edits`
    /// of the query. This walks the trie with a fuzzy DFS that maintains an edit
    /// distance row at each node and prunes branches where the minimum distance
    /// already exceeds `max_edits`.
    ///
    /// Results are sorted by edit distance ascending, then frequency descending.
    /// Words that were already found by exact prefix search can be excluded by
    /// the caller.
    ///
    /// Works on `char` (Unicode scalar values), so Cyrillic and other multi-byte
    /// scripts are handled correctly.
    pub fn fuzzy_search(&self, prefix: &str, max_edits: u8, limit: usize) -> Vec<FuzzyMatch> {
        if limit == 0 {
            return Vec::new();
        }

        let prefix_chars: Vec<char> = prefix.chars().collect();
        let prefix_len = prefix_chars.len();

        // Max-heap: (edit_distance, Reverse(frequency), word).
        // The maximum = worst entry (highest distance, lowest frequency).
        // peek() returns the worst → evict when a better entry arrives.
        let mut heap: BinaryHeap<(u8, Reverse<u32>, String)> =
            BinaryHeap::with_capacity(limit + 1);

        // Special case: empty prefix matches every word at distance 0.
        if prefix_len == 0 {
            Self::collect_words_bounded(
                &self.root,
                &mut String::new(),
                0,
                &mut heap,
                limit,
            );
            // into_sorted_vec returns ascending = (dist ASC, freq DESC). Correct order.
            return heap
                .into_sorted_vec()
                .into_iter()
                .map(|(dist, Reverse(freq), word)| FuzzyMatch {
                    word,
                    frequency: freq,
                    edit_distance: dist,
                })
                .collect();
        }

        // Initial row: [0, 1, 2, ..., prefix_len]
        let initial_row: Vec<u8> = (0..=prefix_len)
            .map(|i| i.min(u8::MAX as usize) as u8)
            .collect();

        // DFS: process each child of root to start the walk.
        for (&ch, child) in &self.root.children {
            Self::fuzzy_dfs_bounded(
                child,
                ch,
                &prefix_chars,
                max_edits,
                &initial_row,
                None,
                None,
                &mut String::from(ch),
                &mut heap,
                limit,
            );
        }

        // into_sorted_vec returns ascending = (dist ASC, freq DESC). Correct order.
        heap.into_sorted_vec()
            .into_iter()
            .map(|(dist, Reverse(freq), word)| FuzzyMatch {
                word,
                frequency: freq,
                edit_distance: dist,
            })
            .collect()
    }

    /// Recursive fuzzy DFS helper using Damerau-Levenshtein distance
    /// with bounded collection into a min-heap.
    #[allow(clippy::too_many_arguments)]
    fn fuzzy_dfs_bounded(
        node: &TrieNode,
        ch: char,
        prefix_chars: &[char],
        max_edits: u8,
        prev_row: &[u8],
        prev_prev_row: Option<&[u8]>,
        prev_ch: Option<char>,
        current_word: &mut String,
        heap: &mut BinaryHeap<(u8, Reverse<u32>, String)>,
        limit: usize,
    ) {
        let prefix_len = prefix_chars.len();
        let mut current_row = vec![0u8; prefix_len + 1];

        // current_row[0] = depth in trie = prev_row[0] + 1
        current_row[0] = prev_row[0].saturating_add(1);

        for i in 1..=prefix_len {
            let cost = if prefix_chars[i - 1] == ch { 0u8 } else { 1u8 };

            // Classic Levenshtein: min(delete, insert, replace)
            let delete = current_row[i - 1].saturating_add(1);
            let insert = prev_row[i].saturating_add(1);
            let replace = prev_row[i - 1].saturating_add(cost);
            let mut dist = delete.min(insert).min(replace);

            // Damerau extension: transposition.
            if i >= 2 {
                if let (Some(p_ch), Some(pp_row)) = (prev_ch, prev_prev_row) {
                    if prefix_chars[i - 1] == p_ch && prefix_chars[i - 2] == ch {
                        let transpose = pp_row[i - 2].saturating_add(1);
                        dist = dist.min(transpose);
                    }
                }
            }

            current_row[i] = dist;
        }

        // If prefix distance is within max_edits, collect subtree words
        // into the bounded heap instead of an unbounded Vec.
        let prefix_dist = current_row[prefix_len];
        if prefix_dist <= max_edits {
            Self::collect_words_bounded(node, current_word, prefix_dist, heap, limit);
        }

        // Prune: if the minimum value in current_row exceeds max_edits,
        // no descendant can possibly match.
        let min_dist = *current_row.iter().min().unwrap_or(&u8::MAX);
        if min_dist > max_edits {
            return;
        }

        // Early termination: if heap is full and the worst entry is distance 0,
        // all entries are distance 0 — skip branches that yield higher distance.
        if heap.len() >= limit {
            if let Some(&(worst_dist, _, _)) = heap.peek() {
                if worst_dist == 0 && prefix_dist > 0 {
                    return;
                }
            }
        }

        // Recurse into children.
        let word_len_before = current_word.len();
        for (&next_ch, child) in &node.children {
            current_word.push(next_ch);
            Self::fuzzy_dfs_bounded(
                child,
                next_ch,
                prefix_chars,
                max_edits,
                &current_row,
                Some(prev_row),
                Some(ch),
                current_word,
                heap,
                limit,
            );
            current_word.truncate(word_len_before);
        }
    }

    /// Bounded top-K collection with subtree pruning.
    ///
    /// Uses `max_freq` on each node to skip entire subtrees whose best
    /// possible frequency can't beat the current K-th best in the heap.
    fn collect_top_k(
        node: &TrieNode,
        current_word: &mut String,
        limit: usize,
        heap: &mut BinaryHeap<Reverse<(u32, String)>>,
    ) {
        // Subtree pruning: if heap is full and this subtree's max_freq
        // can't beat the current K-th best, skip entirely.
        if heap.len() >= limit {
            if let Some(&Reverse((min_freq, _))) = heap.peek() {
                if node.max_freq <= min_freq {
                    return;
                }
            }
        }

        if let Some(freq) = node.frequency {
            // Check acceptance BEFORE cloning the string.
            if heap.len() < limit {
                heap.push(Reverse((freq, current_word.clone())));
            } else if heap.peek().map_or(false, |&Reverse((min_freq, _))| freq > min_freq) {
                heap.pop();
                heap.push(Reverse((freq, current_word.clone())));
            }
        }

        let word_len_before = current_word.len();
        for (&ch, child) in &node.children {
            current_word.push(ch);
            Self::collect_top_k(child, current_word, limit, heap);
            current_word.truncate(word_len_before);
        }
    }

    /// Bounded collection for fuzzy matches using a min-heap.
    ///
    /// Entries are ordered by (edit_distance ASC, frequency DESC) — the heap
    /// evicts the worst match (highest distance or lowest frequency) when full.
    fn collect_words_bounded(
        node: &TrieNode,
        current_word: &mut String,
        edit_distance: u8,
        heap: &mut BinaryHeap<(u8, Reverse<u32>, String)>,
        limit: usize,
    ) {
        // Early pruning: if heap is full and this edit_distance is already
        // worse than the worst entry, skip this node and its entire subtree.
        if heap.len() >= limit {
            if let Some(&(worst_dist, _, _)) = heap.peek() {
                if edit_distance > worst_dist {
                    return;
                }
            }
        }

        if let Some(freq) = node.frequency {
            // Check acceptance BEFORE cloning the string.
            let dominated = heap.len() >= limit
                && heap
                    .peek()
                    .map_or(false, |w| (edit_distance, Reverse(freq)) >= (w.0, w.1));
            if !dominated {
                let entry = (edit_distance, Reverse(freq), current_word.clone());
                if heap.len() >= limit {
                    heap.pop();
                }
                heap.push(entry);
            }
        }

        let word_len_before = current_word.len();
        for (&ch, child) in &node.children {
            current_word.push(ch);
            Self::collect_words_bounded(child, current_word, edit_distance, heap, limit);
            current_word.truncate(word_len_before);
        }
    }

    /// Number of distinct words in the trie.
    pub fn len(&self) -> usize {
        self.count
    }

    /// Whether the trie is empty.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

impl Default for NgramTrie {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_lookup() {
        let mut trie = NgramTrie::new();
        trie.insert("hello", 10);
        trie.insert("help", 5);
        trie.insert("hero", 3);

        let results = trie.prefix_search("hel", 10);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].word, "hello");
        assert_eq!(results[0].frequency, 10);
        assert_eq!(results[1].word, "help");
        assert_eq!(results[1].frequency, 5);

        // "hero" must NOT appear under prefix "hel"
        assert!(results.iter().all(|e| e.word != "hero"));
    }

    #[test]
    fn empty_prefix_returns_all() {
        let mut trie = NgramTrie::new();
        trie.insert("alpha", 1);
        trie.insert("beta", 2);
        trie.insert("gamma", 3);

        let results = trie.prefix_search("", 10);
        assert_eq!(results.len(), 3);
        // Sorted by frequency descending.
        assert_eq!(results[0].word, "gamma");
        assert_eq!(results[1].word, "beta");
        assert_eq!(results[2].word, "alpha");
    }

    #[test]
    fn limit_truncates_results() {
        let mut trie = NgramTrie::new();
        for i in 0..20 {
            trie.insert(&format!("word{i}"), i);
        }
        assert_eq!(trie.len(), 20);

        let results = trie.prefix_search("word", 5);
        assert_eq!(results.len(), 5);
        // Top-5 by frequency: word19, word18, word17, word16, word15
        assert_eq!(results[0].frequency, 19);
        assert_eq!(results[4].frequency, 15);
    }

    #[test]
    fn cyrillic_support() {
        let mut trie = NgramTrie::new();
        trie.insert("здравей", 50);
        trie.insert("здраве", 30);
        trie.insert("зима", 10);
        trie.insert("добър", 20);

        let results = trie.prefix_search("здрав", 10);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].word, "здравей");
        assert_eq!(results[0].frequency, 50);
        assert_eq!(results[1].word, "здраве");
        assert_eq!(results[1].frequency, 30);

        // "зима" and "добър" must not match prefix "здрав"
        assert!(results.iter().all(|e| e.word != "зима"));
        assert!(results.iter().all(|e| e.word != "добър"));
    }

    #[test]
    fn insert_overwrites_frequency() {
        let mut trie = NgramTrie::new();
        trie.insert("word", 5);
        trie.insert("word", 99);

        assert_eq!(trie.len(), 1); // still one distinct word
        let results = trie.prefix_search("word", 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].frequency, 99);
    }

    #[test]
    fn prefix_not_found() {
        let mut trie = NgramTrie::new();
        trie.insert("hello", 1);

        let results = trie.prefix_search("xyz", 10);
        assert!(results.is_empty());
    }

    #[test]
    fn is_empty_and_default() {
        let trie = NgramTrie::default();
        assert!(trie.is_empty());
        assert_eq!(trie.len(), 0);
    }

    // ===================================================================
    // Fuzzy search tests
    // ===================================================================

    /// Helper: build a trie with common English words for fuzzy tests.
    fn fuzzy_trie() -> NgramTrie {
        let mut trie = NgramTrie::new();
        trie.insert("function", 100);
        trie.insert("functional", 60);
        trie.insert("funnel", 30);
        trie.insert("funny", 50);
        trie.insert("fusion", 20);
        trie.insert("future", 40);
        trie.insert("hello", 90);
        trie.insert("help", 80);
        trie.insert("hero", 70);
        trie
    }

    #[test]
    fn fuzzy_exact_match_distance_zero() {
        let trie = fuzzy_trie();
        // Exact prefix "func" should find "function" and "functional" at distance 0.
        let results = trie.fuzzy_search("func", 2, 10);
        let exact: Vec<&FuzzyMatch> = results.iter().filter(|m| m.edit_distance == 0).collect();
        assert!(
            exact.iter().any(|m| m.word == "function"),
            "exact prefix should find 'function'"
        );
        assert!(
            exact.iter().any(|m| m.word == "functional"),
            "exact prefix should find 'functional'"
        );
        for m in &exact {
            assert_eq!(m.edit_distance, 0);
        }
    }

    #[test]
    fn fuzzy_substitution() {
        let trie = fuzzy_trie();
        // "function" is a substitution of 'u' → 'u' at pos 1... wait, that's exact.
        // "fanction" — 'u' → 'a' substitution at position 1.
        let results = trie.fuzzy_search("fanc", 1, 10);
        assert!(
            results.iter().any(|m| m.word == "function"),
            "'fanc' (sub u->a) should fuzzy-match 'function': got {:?}",
            results
        );
        let func_match = results.iter().find(|m| m.word == "function").unwrap();
        assert_eq!(func_match.edit_distance, 1);
    }

    #[test]
    fn fuzzy_insertion() {
        let trie = fuzzy_trie();
        // "fnc" is "func" with 'u' deleted — from the query's perspective, the
        // trie word has an extra 'u' inserted. Edit distance should be 1.
        let results = trie.fuzzy_search("fnc", 1, 10);
        assert!(
            results.iter().any(|m| m.word == "function"),
            "'fnc' (deletion) should fuzzy-match 'function': got {:?}",
            results
        );
    }

    #[test]
    fn fuzzy_deletion() {
        let trie = fuzzy_trie();
        // "funcc" has an extra 'c' — the trie prefix "func" is one char shorter.
        // Edit distance of "funcc" vs "func" prefix = 1 (extra c deleted).
        let results = trie.fuzzy_search("funcc", 1, 10);
        assert!(
            results.iter().any(|m| m.word == "function"),
            "'funcc' should fuzzy-match 'function': got {:?}",
            results
        );
    }

    #[test]
    fn fuzzy_transposition() {
        let trie = fuzzy_trie();
        // "fucn" is "func" with 'c' and 'n'... wait, "func" → "fucn" swaps c↔n.
        // Actually "func" chars are f-u-n-c. "fucn" chars are f-u-c-n.
        // That's a transposition of positions 2 and 3 (n↔c). Distance should be 1.
        let results = trie.fuzzy_search("fucn", 1, 10);
        assert!(
            results.iter().any(|m| m.word == "function"),
            "'fucn' (transposition) should fuzzy-match 'function': got {:?}",
            results
        );
    }

    #[test]
    fn fuzzy_misspelled_function() {
        let trie = fuzzy_trie();
        // The motivating example: "fucntion" instead of "function".
        // "fucntion" vs "function": f-u-c-n-t-i-o-n vs f-u-n-c-t-i-o-n
        // Transposition of 'n' and 'c' at positions 2-3 → distance 1.
        let results = trie.fuzzy_search("fucntion", 2, 5);
        assert!(
            results.iter().any(|m| m.word == "function"),
            "'fucntion' should fuzzy-match 'function': got {:?}",
            results
        );
    }

    #[test]
    fn fuzzy_max_edits_respected() {
        let trie = fuzzy_trie();
        // "xyz" is very far from any word in the trie. With max_edits=1 nothing
        // should match.
        let results = trie.fuzzy_search("xyz", 1, 10);
        assert!(
            results.is_empty(),
            "completely unrelated prefix should return nothing at distance 1: got {:?}",
            results
        );
    }

    #[test]
    fn fuzzy_sort_order() {
        let trie = fuzzy_trie();
        // "func" should produce distance-0 matches first (sorted by freq desc),
        // then distance-1 matches, etc.
        let results = trie.fuzzy_search("func", 2, 20);
        // Verify sort: edit_distance ascending, then frequency descending.
        for w in results.windows(2) {
            assert!(
                w[0].edit_distance < w[1].edit_distance
                    || (w[0].edit_distance == w[1].edit_distance
                        && w[0].frequency >= w[1].frequency),
                "sort violated: {:?} vs {:?}",
                w[0],
                w[1]
            );
        }
    }

    #[test]
    fn fuzzy_limit_respected() {
        let trie = fuzzy_trie();
        let results = trie.fuzzy_search("f", 2, 3);
        assert!(results.len() <= 3, "limit should cap results at 3");
    }

    #[test]
    fn fuzzy_limit_zero() {
        let trie = fuzzy_trie();
        let results = trie.fuzzy_search("func", 2, 0);
        assert!(results.is_empty(), "limit 0 should return empty");
    }

    #[test]
    fn fuzzy_cyrillic() {
        let mut trie = NgramTrie::new();
        trie.insert("здравей", 50);
        trie.insert("здраве", 30);
        trie.insert("зима", 10);
        trie.insert("добър", 20);

        // "здарве" is "здраве" with 'р' and 'а' transposed → distance 1.
        let results = trie.fuzzy_search("здарв", 1, 10);
        assert!(
            results
                .iter()
                .any(|m| m.word == "здраве" || m.word == "здравей"),
            "Cyrillic transposition should match: got {:?}",
            results
        );

        // "здрав" exact prefix match — distance 0.
        let results = trie.fuzzy_search("здрав", 0, 10);
        assert!(
            results
                .iter()
                .any(|m| m.word == "здравей" && m.edit_distance == 0),
            "exact Cyrillic prefix should match at distance 0: got {:?}",
            results
        );
    }

    #[test]
    fn fuzzy_empty_prefix() {
        let trie = fuzzy_trie();
        // Empty prefix should match everything at distance 0 (every word starts
        // with the empty string).
        let results = trie.fuzzy_search("", 0, 100);
        assert_eq!(
            results.len(),
            trie.len(),
            "empty prefix at distance 0 should match all words"
        );
        for m in &results {
            assert_eq!(m.edit_distance, 0);
        }
    }

    #[test]
    fn fuzzy_empty_trie() {
        let trie = NgramTrie::new();
        let results = trie.fuzzy_search("hello", 2, 10);
        assert!(results.is_empty());
    }

    #[test]
    fn fuzzy_distance_two() {
        let trie = fuzzy_trie();
        // "fulction" has two edits from "function": n→l (sub) + missing n?
        // Actually let's use "fxnction" — u→x substitution — distance 1.
        // And "fxnctin" — u→x sub + missing 'o' — distance 2.
        let results = trie.fuzzy_search("fxnctin", 2, 10);
        // Should find "function" within distance 2.
        assert!(
            results.iter().any(|m| m.word == "function"),
            "'fxnctin' should match 'function' within distance 2: got {:?}",
            results
        );
    }
}
