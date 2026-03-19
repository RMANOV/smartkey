//! Smart caps engine — detects capitalization regime from user input
//! and applies it to lowercase predictions from the trie.

use std::collections::HashSet;

/// Detected capitalization regime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapsRegime {
    /// Normal lowercase or sentence-start capitalization.
    Normal,
    /// All characters uppercase: "HTTP", "NASA".
    AllCaps,
    /// camelCase: "getElementById".
    CamelCase,
    /// PascalCase: "MyClass".
    PascalCase,
}

/// Engine for capitalization detection and application.
pub struct CapsEngine {
    proper_nouns: HashSet<String>,
}

impl CapsEngine {
    pub fn new() -> Self {
        Self {
            proper_nouns: HashSet::new(),
        }
    }

    /// Determine if word requires capitalization based on the previous token.
    pub fn requires_capitalization(prev_token: &str) -> bool {
        let trimmed = prev_token.trim();
        if trimmed.is_empty() {
            return true; // Start of input
        }
        let last_char = trimmed.chars().last().unwrap_or(' ');
        matches!(last_char, '.' | '!' | '?' | '\n')
    }

    /// Detect caps regime from the typed prefix.
    ///
    /// Returns `Normal` for empty strings, all-lowercase, or single uppercase
    /// followed by lowercase (sentence-start pattern). Requires 2+ uppercase
    /// chars to detect `AllCaps`.
    pub fn detect_regime(prefix: &str) -> CapsRegime {
        if prefix.is_empty() {
            return CapsRegime::Normal;
        }
        let chars: Vec<char> = prefix.chars().collect();

        // Count alphabetic chars and check their case.
        let alpha_chars: Vec<char> = chars
            .iter()
            .copied()
            .filter(|c| c.is_alphabetic())
            .collect();
        if alpha_chars.len() >= 2 && alpha_chars.iter().all(|c| c.is_uppercase()) {
            return CapsRegime::AllCaps;
        }

        let first = chars[0];
        if first.is_uppercase() {
            // Check for PascalCase: first upper + internal upper.
            if chars[1..]
                .iter()
                .any(|c| c.is_alphabetic() && c.is_uppercase())
            {
                return CapsRegime::PascalCase;
            }
            // Just first-char upper → Normal (sentence-start or proper noun).
            return CapsRegime::Normal;
        }

        // Check for camelCase: first lower + internal upper.
        if first.is_lowercase()
            && chars[1..]
                .iter()
                .any(|c| c.is_alphabetic() && c.is_uppercase())
        {
            return CapsRegime::CamelCase;
        }

        CapsRegime::Normal
    }

    /// Apply caps regime to a lowercase prediction word.
    pub fn apply_caps(word: &str, regime: CapsRegime) -> String {
        match regime {
            CapsRegime::Normal => word.to_string(),
            CapsRegime::AllCaps => word.to_uppercase(),
            CapsRegime::PascalCase | CapsRegime::CamelCase => {
                // For camel/pascal, the trie stores lowercase. Since the
                // user's prefix already encodes the pattern, return as-is.
                word.to_string()
            }
        }
    }

    /// Capitalize first letter only, preserving the rest.
    pub fn capitalize_first(word: &str) -> String {
        let mut chars = word.chars();
        match chars.next() {
            None => String::new(),
            Some(first) => {
                let upper: String = first.to_uppercase().collect();
                upper + chars.as_str()
            }
        }
    }

    /// Check if a word is a known proper noun.
    pub fn is_proper_noun(&self, word: &str) -> bool {
        self.proper_nouns.contains(&word.to_lowercase())
    }

    /// Register a proper noun (stored lowercase for case-insensitive lookup).
    pub fn register_proper_noun(&mut self, word: &str) {
        self.proper_nouns.insert(word.to_lowercase());
    }
}

impl Default for CapsEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_normal_lowercase() {
        assert_eq!(CapsEngine::detect_regime("hello"), CapsRegime::Normal);
    }

    #[test]
    fn detect_normal_capitalized() {
        assert_eq!(CapsEngine::detect_regime("Hello"), CapsRegime::Normal);
    }

    #[test]
    fn detect_all_caps() {
        assert_eq!(CapsEngine::detect_regime("HTTP"), CapsRegime::AllCaps);
        assert_eq!(CapsEngine::detect_regime("NASA"), CapsRegime::AllCaps);
        assert_eq!(CapsEngine::detect_regime("AB"), CapsRegime::AllCaps);
    }

    #[test]
    fn detect_single_upper_is_normal() {
        // Single uppercase can't be distinguished from sentence-start.
        assert_eq!(CapsEngine::detect_regime("H"), CapsRegime::Normal);
    }

    #[test]
    fn detect_camel_case() {
        assert_eq!(
            CapsEngine::detect_regime("getElement"),
            CapsRegime::CamelCase
        );
    }

    #[test]
    fn detect_pascal_case() {
        assert_eq!(CapsEngine::detect_regime("MyClass"), CapsRegime::PascalCase);
    }

    #[test]
    fn apply_all_caps() {
        assert_eq!(
            CapsEngine::apply_caps("hello", CapsRegime::AllCaps),
            "HELLO"
        );
    }

    #[test]
    fn apply_normal_unchanged() {
        assert_eq!(CapsEngine::apply_caps("hello", CapsRegime::Normal), "hello");
    }

    #[test]
    fn capitalize_first_empty() {
        assert_eq!(CapsEngine::capitalize_first(""), "");
    }

    #[test]
    fn capitalize_first_ascii() {
        assert_eq!(CapsEngine::capitalize_first("hello"), "Hello");
    }

    #[test]
    fn capitalize_first_cyrillic() {
        assert_eq!(CapsEngine::capitalize_first("здравей"), "Здравей");
    }

    #[test]
    fn proper_noun_check() {
        let mut engine = CapsEngine::new();
        engine.register_proper_noun("London");
        assert!(engine.is_proper_noun("london"));
        assert!(engine.is_proper_noun("London"));
        assert!(engine.is_proper_noun("LONDON"));
        assert!(!engine.is_proper_noun("hello"));
    }
}
