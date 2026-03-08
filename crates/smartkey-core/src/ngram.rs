// N-gram trie storage — word dictionary with frequency-ranked prefix search.

use std::collections::HashMap;

/// A word with its associated frequency count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WordEntry {
    pub word: String,
    pub frequency: u32,
}

/// Internal trie node. Children are keyed by `char` (not byte), which
/// naturally handles multi-byte UTF-8 scripts like Cyrillic.
#[derive(Debug, Default)]
struct TrieNode {
    children: HashMap<char, TrieNode>,
    /// `Some(freq)` when this node marks the end of a stored word.
    frequency: Option<u32>,
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
        for ch in word.chars() {
            node = node.children.entry(ch).or_default();
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

        // DFS to collect every word below this node.
        let mut results = Vec::new();
        let mut stack: Vec<(&TrieNode, String)> = vec![(node, prefix.to_string())];

        while let Some((cur, cur_word)) = stack.pop() {
            if let Some(freq) = cur.frequency {
                results.push(WordEntry {
                    word: cur_word.clone(),
                    frequency: freq,
                });
            }
            for (&ch, child) in &cur.children {
                let mut next_word = cur_word.clone();
                next_word.push(ch);
                stack.push((child, next_word));
            }
        }

        // Sort by frequency descending; break ties alphabetically.
        results.sort_by(|a, b| b.frequency.cmp(&a.frequency).then(a.word.cmp(&b.word)));
        results.truncate(limit);
        results
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
}
