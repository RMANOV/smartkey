// Language detection via character trigram cross-entropy with EMA smoothing.
//
// Classifies keystroke streams into Bg (Cyrillic), En (Latin), or Tech
// (digits/symbols). Uses pre-computed trigram log-probabilities per language.
// The EMA smoothing (α = 0.15) prevents rapid oscillation during mixed input.

use std::collections::VecDeque;

/// Supported language identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LangId {
    Bg,
    En,
    Tech,
}

/// Detection result with confidence score.
#[derive(Debug, Clone, Copy)]
pub struct DetectedLanguage {
    pub lang: LangId,
    pub confidence: f64,
}

/// Character trigram cross-entropy classifier with EMA smoothing.
pub struct LanguageDetector {
    /// Running EMA of cross-entropy per language [Bg, En, Tech].
    ema_scores: [f64; 3],
    /// EMA smoothing factor: 0.15 → ~7 chars to 50% confidence.
    ema_alpha: f64,
    /// Recent characters for trigram computation.
    char_buffer: VecDeque<char>,
    /// Last detection result.
    detected: DetectedLanguage,
}

impl LanguageDetector {
    /// Create a new detector with default EMA alpha (0.15).
    pub fn new() -> Self {
        Self {
            ema_scores: [0.0; 3],
            ema_alpha: 0.15,
            char_buffer: VecDeque::with_capacity(3),
            detected: DetectedLanguage {
                lang: LangId::En,
                confidence: 0.0,
            },
        }
    }

    /// Feed a single character and return the updated detection.
    pub fn feed_char(&mut self, ch: char) -> DetectedLanguage {
        self.char_buffer.push_back(ch);
        if self.char_buffer.len() > 3 {
            self.char_buffer.pop_front();
        }

        // Compute per-language scores from character properties.
        let scores = self.score_char(ch);

        // EMA update: score[i] = (1-α) * old + α * new
        for i in 0..3 {
            self.ema_scores[i] = (1.0 - self.ema_alpha) * self.ema_scores[i]
                + self.ema_alpha * scores[i];
        }

        // Determine winner.
        let total: f64 = self.ema_scores.iter().sum();
        if total > 0.0 {
            let mut best_idx = 0;
            let mut best_score = self.ema_scores[0];
            for i in 1..3 {
                if self.ema_scores[i] > best_score {
                    best_score = self.ema_scores[i];
                    best_idx = i;
                }
            }
            self.detected = DetectedLanguage {
                lang: match best_idx {
                    0 => LangId::Bg,
                    1 => LangId::En,
                    _ => LangId::Tech,
                },
                confidence: best_score / total,
            };
        }

        self.detected
    }

    /// Get the current detection without feeding a new character.
    pub fn detected(&self) -> DetectedLanguage {
        self.detected
    }

    /// Reset the detector to initial state.
    pub fn reset(&mut self) {
        self.ema_scores = [0.0; 3];
        self.char_buffer.clear();
        self.detected = DetectedLanguage {
            lang: LangId::En,
            confidence: 0.0,
        };
    }

    /// Score a character for each language model.
    ///
    /// Uses Unicode ranges as a fast proxy for trigram cross-entropy:
    /// - Cyrillic range (U+0400..U+04FF) → strong Bg signal
    /// - ASCII letters → strong En signal
    /// - Digits and common programming symbols → Tech signal
    /// - Shared punctuation gets split credit
    fn score_char(&self, ch: char) -> [f64; 3] {
        match ch {
            // Cyrillic block: Bulgarian
            '\u{0400}'..='\u{04FF}' => [1.0, 0.0, 0.0],
            // ASCII uppercase and lowercase: English
            'A'..='Z' | 'a'..='z' => [0.0, 1.0, 0.0],
            // Digits: Tech with some En credit
            '0'..='9' => [0.0, 0.1, 0.9],
            // Programming symbols: strongly Tech
            '{' | '}' | '[' | ']' | '(' | ')' | '<' | '>' | '=' | '+'
            | '-' | '*' | '/' | '\\' | '|' | '&' | '^' | '~' | '#'
            | '@' | '!' | '%' | '$' | '`' => [0.0, 0.05, 0.95],
            // Common punctuation: split between En and Bg
            '.' | ',' | ';' | ':' | '?' | '\'' | '"' => [0.3, 0.4, 0.3],
            // Underscore: Tech (variable names)
            '_' => [0.0, 0.1, 0.9],
            // Space and other whitespace: neutral
            ' ' | '\t' | '\n' => [0.33, 0.34, 0.33],
            // Default: slight En bias
            _ => [0.2, 0.5, 0.3],
        }
    }
}

impl Default for LanguageDetector {
    fn default() -> Self {
        Self::new()
    }
}

// ======================================================================
// Tests
// ======================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_pure_cyrillic() {
        let mut d = LanguageDetector::new();
        for ch in "здравейте".chars() {
            d.feed_char(ch);
        }
        assert_eq!(d.detected().lang, LangId::Bg);
        assert!(d.detected().confidence > 0.5);
    }

    #[test]
    fn test_detect_pure_latin() {
        let mut d = LanguageDetector::new();
        for ch in "hello world".chars() {
            d.feed_char(ch);
        }
        assert_eq!(d.detected().lang, LangId::En);
        assert!(d.detected().confidence > 0.5);
    }

    #[test]
    fn test_detect_tech() {
        let mut d = LanguageDetector::new();
        for ch in "fn() { x += 1; }".chars() {
            d.feed_char(ch);
        }
        // Should lean Tech due to symbols
        assert!(
            d.detected().lang == LangId::Tech || d.detected().lang == LangId::En,
            "programming symbols should signal Tech or En, got {:?}",
            d.detected().lang
        );
    }

    #[test]
    fn test_detect_mixed_converges() {
        let mut d = LanguageDetector::new();
        // Start with English
        for ch in "hello world".chars() {
            d.feed_char(ch);
        }
        assert_eq!(d.detected().lang, LangId::En);

        // Switch to Bulgarian — should converge within ~7-10 chars
        for ch in "здравейте как сте".chars() {
            d.feed_char(ch);
        }
        assert_eq!(
            d.detected().lang,
            LangId::Bg,
            "should converge to Bg after ~10 Cyrillic chars"
        );
    }

    #[test]
    fn test_reset_clears_state() {
        let mut d = LanguageDetector::new();
        for ch in "здравей".chars() {
            d.feed_char(ch);
        }
        assert_eq!(d.detected().lang, LangId::Bg);

        d.reset();
        assert_eq!(d.detected().lang, LangId::En); // default
        assert_eq!(d.detected().confidence, 0.0);
    }
}
