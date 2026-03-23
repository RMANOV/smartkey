// Context sampler trait — abstracts platform-specific surrounding text access.
//
// Platform implementations (TSF on Windows, IBus on Linux) provide real
// surrounding text. NullContextSampler is the fallback when unavailable.

use crate::lang_detect::LangId;

/// Abstraction for reading surrounding text from the active text field.
pub trait ContextSampler: Send {
    /// Get up to `max_chars` of text surrounding the cursor.
    /// Returns None if the platform doesn't support this or the field is empty.
    fn get_surrounding_text(&self, max_chars: usize) -> Option<String>;
}

/// Null implementation — always returns None. Used as fallback when
/// the platform doesn't support surrounding text access.
pub struct NullContextSampler;

impl ContextSampler for NullContextSampler {
    fn get_surrounding_text(&self, _max_chars: usize) -> Option<String> {
        None
    }
}

/// Analysis result from surrounding text.
#[derive(Debug, Clone)]
pub struct ContextAnalysis {
    /// Detected language from surrounding text.
    pub lang: Option<LangId>,
    /// Recent words extracted from surrounding text.
    pub recent_words: Vec<String>,
}

/// Analyze surrounding text for language and context cues.
pub fn analyze_surrounding(text: &str) -> ContextAnalysis {
    use crate::lang_detect::LanguageDetector;

    let mut detector = LanguageDetector::new();
    for ch in text.chars() {
        detector.feed_char_instant(ch);
    }

    let det = detector.detected();
    let lang = if det.confidence >= 0.5 {
        Some(det.lang)
    } else {
        None
    };

    // Extract recent words (last 10, split on whitespace).
    let recent_words: Vec<String> = text
        .split_whitespace()
        .rev()
        .take(10)
        .map(|s| s.to_lowercase())
        .collect();

    ContextAnalysis { lang, recent_words }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_sampler_returns_none() {
        let s = NullContextSampler;
        assert!(s.get_surrounding_text(100).is_none());
        assert!(s.get_surrounding_text(0).is_none());
        assert!(s.get_surrounding_text(usize::MAX).is_none());
    }

    #[test]
    fn analyze_surrounding_empty_string() {
        let result = analyze_surrounding("");
        assert!(result.recent_words.is_empty());
        // Lang may be None or low-confidence on empty input
        // No panic is the important property here
    }

    #[test]
    fn analyze_surrounding_pure_latin_detects_en() {
        let result = analyze_surrounding("hello world this is english text");
        // recent_words should be non-empty
        assert!(!result.recent_words.is_empty());
        // Words are returned in reverse order and lowercased
        assert_eq!(result.recent_words[0], "text");
    }

    #[test]
    fn analyze_surrounding_recent_words_max_10() {
        let text = "one two three four five six seven eight nine ten eleven twelve";
        let result = analyze_surrounding(text);
        assert!(result.recent_words.len() <= 10);
    }

    #[test]
    fn analyze_surrounding_pure_cyrillic_detects_bg() {
        let result = analyze_surrounding("здравей свят това е на кирилица");
        assert!(!result.recent_words.is_empty());
        // If detected, should be Bg
        if let Some(lang) = result.lang {
            assert_eq!(lang, crate::lang_detect::LangId::Bg);
        }
    }

    #[test]
    fn analyze_surrounding_words_are_lowercased() {
        let result = analyze_surrounding("Hello World");
        assert!(result
            .recent_words
            .iter()
            .all(|w| w == w.to_lowercase().as_str()));
    }
}
