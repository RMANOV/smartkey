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
    /// Whether the text appears formal or informal.
    pub formal: bool,
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

    // Simple formality heuristic: formal if no contractions and avg word length > 5.
    let avg_len = if recent_words.is_empty() {
        0.0
    } else {
        recent_words.iter().map(|w| w.len() as f64).sum::<f64>() / recent_words.len() as f64
    };
    let formal = avg_len > 5.0 && !text.contains('\'');

    ContextAnalysis {
        lang,
        recent_words,
        formal,
    }
}
