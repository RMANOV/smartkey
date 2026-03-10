use aho_corasick::AhoCorasick;

/// Fast prefix-based word filter.
///
/// For single-prefix lookup, uses a simple `str::starts_with` scan.
/// For multi-pattern batch queries (e.g., typo correction with multiple
/// possible prefixes), leverages an [`AhoCorasick`] automaton so that each
/// word is scanned against all prefixes in a single pass.
pub struct PrefixMatcher {
    words: Vec<String>,
}

impl PrefixMatcher {
    /// Build a matcher from a word list.  Words are stored as owned `String`s.
    pub fn build(words: &[&str]) -> Self {
        Self {
            words: words.iter().map(|w| (*w).to_owned()).collect(),
        }
    }

    /// Return every word that starts with `prefix`.
    ///
    /// An empty prefix matches all words.
    pub fn find_by_prefix(&self, prefix: &str) -> Vec<String> {
        self.words
            .iter()
            .filter(|w| w.starts_with(prefix))
            .cloned()
            .collect()
    }

    /// Return every word that starts with **any** of the given `prefixes`.
    ///
    /// Duplicates are avoided: a word appears at most once even if it matches
    /// multiple prefixes.  An empty `prefixes` slice returns an empty `Vec`.
    /// If any prefix is the empty string, all words match.
    pub fn find_by_prefixes(&self, prefixes: &[&str]) -> Vec<String> {
        if prefixes.is_empty() {
            return Vec::new();
        }

        // Fast path: if any prefix is empty, everything matches.
        if prefixes.iter().any(|p| p.is_empty()) {
            return self.words.clone();
        }

        // Build an Aho-Corasick automaton over the prefixes.
        let Ok(ac) = AhoCorasick::new(prefixes) else {
            return self
                .words
                .iter()
                .filter(|w| prefixes.iter().any(|p| w.starts_with(p)))
                .cloned()
                .collect();
        };

        self.words
            .iter()
            .filter(|word| {
                // We only care about matches anchored at position 0.
                ac.find(word.as_str()).is_some_and(|m| m.start() == 0)
            })
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_and_search() {
        let matcher = PrefixMatcher::build(&["apple", "app", "banana", "band", "bat"]);

        let mut result = matcher.find_by_prefix("app");
        result.sort();
        assert_eq!(result, vec!["app", "apple"]);

        let mut result = matcher.find_by_prefix("ban");
        result.sort();
        assert_eq!(result, vec!["banana", "band"]);

        assert_eq!(matcher.find_by_prefix("bat"), vec!["bat"]);
    }

    #[test]
    fn empty_prefix_matches_all() {
        let matcher = PrefixMatcher::build(&["foo", "bar", "baz"]);
        let mut result = matcher.find_by_prefix("");
        result.sort();
        assert_eq!(result, vec!["bar", "baz", "foo"]);
    }

    #[test]
    fn no_match_returns_empty() {
        let matcher = PrefixMatcher::build(&["apple", "banana"]);
        assert!(matcher.find_by_prefix("xyz").is_empty());
        assert!(matcher.find_by_prefixes(&["xyz", "qqq"]).is_empty());
    }

    #[test]
    fn multi_prefix() {
        let matcher = PrefixMatcher::build(&["apple", "app", "banana", "band", "cat", "car"]);

        let mut result = matcher.find_by_prefixes(&["app", "ca"]);
        result.sort();
        assert_eq!(result, vec!["app", "apple", "car", "cat"]);
    }

    #[test]
    fn multi_prefix_no_duplicates() {
        // "app" and "ap" both match "apple" / "app" — each word appears once.
        let matcher = PrefixMatcher::build(&["apple", "app", "apricot"]);
        let mut result = matcher.find_by_prefixes(&["app", "ap"]);
        result.sort();
        assert_eq!(result, vec!["app", "apple", "apricot"]);
    }

    #[test]
    fn multi_prefix_empty_slice() {
        let matcher = PrefixMatcher::build(&["hello"]);
        assert!(matcher.find_by_prefixes(&[]).is_empty());
    }

    #[test]
    fn multi_prefix_with_empty_string() {
        let matcher = PrefixMatcher::build(&["hello", "world"]);
        let mut result = matcher.find_by_prefixes(&["", "he"]);
        result.sort();
        assert_eq!(result, vec!["hello", "world"]);
    }

    #[test]
    fn build_empty_word_list() {
        let matcher = PrefixMatcher::build(&[]);
        assert!(matcher.find_by_prefix("a").is_empty());
        assert!(matcher.find_by_prefixes(&["a"]).is_empty());
    }
}
