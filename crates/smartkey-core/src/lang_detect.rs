// Language detection via character trigram cross-entropy with EMA smoothing.
//
// Classifies keystroke streams into Bg (Cyrillic), En (Latin), or Tech
// (digits/symbols). Uses pre-computed trigram log-probabilities per language.
// The EMA smoothing (α = 0.30) prevents rapid oscillation during mixed input.

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

/// Character trigram cross-entropy classifier with EMA smoothing
/// and instant Unicode block classification (v0.4.1).
pub struct LanguageDetector {
    /// Running EMA of cross-entropy per language [Bg, En, Tech].
    ema_scores: [f64; 3],
    /// EMA smoothing factor: 0.30 → ~3 chars to 50% confidence.
    ema_alpha: f64,
    /// Recent characters for trigram computation.
    char_buffer: VecDeque<char>,
    /// Last detection result.
    detected: DetectedLanguage,
    /// Context momentum — tracks language of recently committed words.
    momentum: VecDeque<LangId>,
}

impl LanguageDetector {
    /// Create a new detector with default EMA alpha (0.30).
    pub fn new() -> Self {
        Self {
            ema_scores: [0.0; 3],
            ema_alpha: 0.30,
            char_buffer: VecDeque::with_capacity(3),
            detected: DetectedLanguage {
                lang: LangId::En,
                confidence: 0.0,
            },
            momentum: VecDeque::with_capacity(5),
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

        // Hard reset on script change: if the incoming character's Unicode
        // block differs from the current EMA winner, reset EMA to 50/50
        // instead of gradual shift. This eliminates cross-contamination lag.
        if let Some(incoming_lang) = Self::instant_classify(ch) {
            let current_winner = self.detected.lang;
            let winner_is_different = matches!(
                (current_winner, incoming_lang),
                (LangId::Bg, LangId::En) | (LangId::En, LangId::Bg)
            );
            if winner_is_different && self.detected.confidence > 0.3 {
                let n = self.ema_scores.len() as f64;
                for ema in self.ema_scores.iter_mut() {
                    *ema = 1.0 / n;
                }
                self.momentum.clear();
            }
        }

        // EMA update: score[i] = (1-α) * old + α * new
        for (ema, &s) in self.ema_scores.iter_mut().zip(scores.iter()) {
            *ema = (1.0 - self.ema_alpha) * *ema + self.ema_alpha * s;
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
        self.momentum.clear();
    }

    // ── Instant detection (v0.4.1) ─────────────────────────────────

    /// Instant language classification based on Unicode block.
    /// Returns Some(lang) for unambiguous characters, None for ambiguous.
    pub fn instant_classify(ch: char) -> Option<LangId> {
        match ch {
            '\u{0400}'..='\u{04FF}' => Some(LangId::Bg),
            'A'..='Z' | 'a'..='z' => Some(LangId::En),
            _ => None,
        }
    }

    /// Feed a single character with instant classification + adaptive EMA fallback.
    ///
    /// For unambiguous chars (Cyrillic/Latin), detection is immediate on char 1.
    /// For ambiguous chars (digits, symbols, punctuation), uses adaptive-α EMA
    /// with momentum fallback when confidence is low.
    pub fn feed_char_instant(&mut self, ch: char) -> DetectedLanguage {
        self.char_buffer.push_back(ch);
        if self.char_buffer.len() > 3 {
            self.char_buffer.pop_front();
        }

        if let Some(lang) = Self::instant_classify(ch) {
            // Unambiguous: instant switch, reset EMA to bias this language.
            let idx = match lang {
                LangId::Bg => 0,
                LangId::En => 1,
                LangId::Tech => 2,
            };
            self.ema_scores = [0.1, 0.1, 0.1];
            self.ema_scores[idx] = 0.8;
            self.detected = DetectedLanguage {
                lang,
                confidence: 1.0,
            };
        } else {
            // Ambiguous: use adaptive α.
            let scores = self.score_char(ch);
            let alpha = self.adaptive_alpha(ch);
            for (ema, &s) in self.ema_scores.iter_mut().zip(scores.iter()) {
                *ema = (1.0 - alpha) * *ema + alpha * s;
            }
            // Winner selection.
            let total: f64 = self.ema_scores.iter().sum();
            let mut best_idx = 0;
            let mut best_score = self.ema_scores[0];
            for i in 1..3 {
                if self.ema_scores[i] > best_score {
                    best_score = self.ema_scores[i];
                    best_idx = i;
                }
            }
            let lang = match best_idx {
                0 => LangId::Bg,
                1 => LangId::En,
                _ => LangId::Tech,
            };
            self.detected = DetectedLanguage {
                lang,
                confidence: if total > 0.0 { best_score / total } else { 0.0 },
            };
            // Low confidence: fall back to momentum.
            if self.detected.confidence < 0.5 {
                if let Some(mom_lang) = self.momentum_lang() {
                    self.detected = DetectedLanguage {
                        lang: mom_lang,
                        confidence: 0.6,
                    };
                }
            }
        }
        self.detected
    }

    /// Adaptive EMA alpha: faster for unambiguous signals, slower for noise.
    fn adaptive_alpha(&self, ch: char) -> f64 {
        match ch {
            '0'..='9' | '_' => 0.05,
            '.' | ',' | ';' | ':' | '?' | '\'' | '"' => 0.03,
            ' ' | '\t' | '\n' => 0.0,
            _ => self.ema_alpha,
        }
    }

    // ── Momentum (v0.4.1) ──────────────────────────────────────────

    /// Record the language of a committed word for momentum tracking.
    pub fn record_commit(&mut self, lang: LangId) {
        self.momentum.push_back(lang);
        if self.momentum.len() > 5 {
            self.momentum.pop_front();
        }
    }

    /// Feed the dual-buffer's winner into the detector's state.
    ///
    /// Called after each dual-buffer scoring round so that the language
    /// detector's EMA and momentum stay in sync with layout-agnostic input.
    pub fn feed_dual_result(&mut self, lang: LangId, confidence: f64) {
        let idx = match lang {
            LangId::Bg => 0,
            LangId::En => 1,
            LangId::Tech => 2,
        };
        // Bias EMA toward the dual-buffer winner, weighted by confidence.
        self.ema_scores = [0.1, 0.1, 0.1];
        self.ema_scores[idx] = 0.7 * confidence;
        self.detected = DetectedLanguage { lang, confidence };
    }

    /// Get the dominant language from recent commits (momentum).
    /// Returns None if no clear majority (< 60%).
    pub fn momentum_lang(&self) -> Option<LangId> {
        if self.momentum.is_empty() {
            return None;
        }
        let mut counts = [0u8; 3];
        for lang in &self.momentum {
            match lang {
                LangId::Bg => counts[0] += 1,
                LangId::En => counts[1] += 1,
                LangId::Tech => counts[2] += 1,
            }
        }
        let total = self.momentum.len() as f64;
        let (best_idx, &best_count) = counts.iter().enumerate().max_by_key(|(_, &c)| c).unwrap();
        if (best_count as f64 / total) >= 0.6 {
            Some(match best_idx {
                0 => LangId::Bg,
                1 => LangId::En,
                _ => LangId::Tech,
            })
        } else {
            None
        }
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
            '{' | '}' | '[' | ']' | '(' | ')' | '<' | '>' | '=' | '+' | '-' | '*' | '/' | '\\'
            | '|' | '&' | '^' | '~' | '#' | '@' | '!' | '%' | '$' | '`' => [0.0, 0.05, 0.95],
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

// ======================================================================
// Phonetic transliteration (v0.4.1)
// ======================================================================

/// Map a Latin character to its BG Phonetic keyboard Cyrillic equivalent.
pub fn phonetic_map(ch: char) -> Option<char> {
    Some(match ch {
        'a' => 'а',
        'A' => 'А',
        'b' => 'б',
        'B' => 'Б',
        'c' => 'ц',
        'C' => 'Ц',
        'd' => 'д',
        'D' => 'Д',
        'e' => 'е',
        'E' => 'Е',
        'f' => 'ф',
        'F' => 'Ф',
        'g' => 'г',
        'G' => 'Г',
        'h' => 'х',
        'H' => 'Х',
        'i' => 'и',
        'I' => 'И',
        'j' => 'й',
        'J' => 'Й',
        'k' => 'к',
        'K' => 'К',
        'l' => 'л',
        'L' => 'Л',
        'm' => 'м',
        'M' => 'М',
        'n' => 'н',
        'N' => 'Н',
        'o' => 'о',
        'O' => 'О',
        'p' => 'п',
        'P' => 'П',
        'q' => 'я',
        'Q' => 'Я',
        'r' => 'р',
        'R' => 'Р',
        's' => 'с',
        'S' => 'С',
        't' => 'т',
        'T' => 'Т',
        'u' => 'у',
        'U' => 'У',
        'v' => 'в',
        'V' => 'В',
        'w' => 'ш',
        'W' => 'Ш',
        'x' => 'ь',
        'X' => 'Ь',
        'y' => 'ъ',
        'Y' => 'Ъ',
        'z' => 'з',
        'Z' => 'З',
        _ => return None,
    })
}

/// Transliterate a Latin string to Cyrillic using BG Phonetic layout.
///
/// Non-alphabetic characters (digits, spaces, punctuation) are dropped.
/// Callers should pre-filter to ensure input contains only ASCII letters.
pub fn transliterate(input: &str) -> String {
    input.chars().filter_map(phonetic_map).collect()
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

    // ── Instant detection tests (v0.4.1) ────────────────────────────

    #[test]
    fn test_instant_classify_cyrillic() {
        assert_eq!(LanguageDetector::instant_classify('з'), Some(LangId::Bg));
        assert_eq!(LanguageDetector::instant_classify('Б'), Some(LangId::Bg));
    }

    #[test]
    fn test_instant_classify_latin() {
        assert_eq!(LanguageDetector::instant_classify('a'), Some(LangId::En));
        assert_eq!(LanguageDetector::instant_classify('Z'), Some(LangId::En));
    }

    #[test]
    fn test_instant_classify_ambiguous() {
        assert_eq!(LanguageDetector::instant_classify('5'), None);
        assert_eq!(LanguageDetector::instant_classify(' '), None);
        assert_eq!(LanguageDetector::instant_classify('.'), None);
    }

    #[test]
    fn test_instant_detect_first_char_cyrillic() {
        let mut d = LanguageDetector::new();
        d.feed_char_instant('з');
        assert_eq!(d.detected().lang, LangId::Bg);
        assert_eq!(d.detected().confidence, 1.0);
    }

    #[test]
    fn test_instant_switch_en_to_bg() {
        let mut d = LanguageDetector::new();
        // Start English
        for ch in "hello ".chars() {
            d.feed_char_instant(ch);
        }
        assert_eq!(d.detected().lang, LangId::En);
        // Single Cyrillic char → immediate switch
        d.feed_char_instant('з');
        assert_eq!(d.detected().lang, LangId::Bg);
        assert_eq!(d.detected().confidence, 1.0);
    }

    #[test]
    fn test_whitespace_preserves_language() {
        let mut d = LanguageDetector::new();
        for ch in "hello".chars() {
            d.feed_char_instant(ch);
        }
        assert_eq!(d.detected().lang, LangId::En);
        // Space should NOT dilute the signal (α = 0.0 for whitespace)
        d.feed_char_instant(' ');
        assert_eq!(d.detected().lang, LangId::En);
    }

    // ── Momentum tests (v0.4.1) ────────────────────────────────────

    #[test]
    fn test_momentum_empty() {
        let d = LanguageDetector::new();
        assert_eq!(d.momentum_lang(), None);
    }

    #[test]
    fn test_momentum_clear_majority() {
        let mut d = LanguageDetector::new();
        d.record_commit(LangId::Bg);
        d.record_commit(LangId::Bg);
        d.record_commit(LangId::Bg);
        assert_eq!(d.momentum_lang(), Some(LangId::Bg));
    }

    #[test]
    fn test_momentum_no_majority() {
        let mut d = LanguageDetector::new();
        d.record_commit(LangId::Bg);
        d.record_commit(LangId::En);
        d.record_commit(LangId::Tech);
        assert_eq!(d.momentum_lang(), None);
    }

    #[test]
    fn test_momentum_window_size() {
        let mut d = LanguageDetector::new();
        // Fill with Bg
        for _ in 0..5 {
            d.record_commit(LangId::Bg);
        }
        assert_eq!(d.momentum_lang(), Some(LangId::Bg));
        // Push 4 En — now window is [Bg, En, En, En, En]
        for _ in 0..4 {
            d.record_commit(LangId::En);
        }
        assert_eq!(d.momentum_lang(), Some(LangId::En));
    }

    // ── Phonetic map tests (v0.4.1) ────────────────────────────────

    #[test]
    fn test_phonetic_map_basic() {
        assert_eq!(phonetic_map('a'), Some('а'));
        assert_eq!(phonetic_map('A'), Some('А'));
        assert_eq!(phonetic_map('z'), Some('з'));
        assert_eq!(phonetic_map('1'), None);
    }

    #[test]
    fn test_transliterate() {
        assert_eq!(transliterate("zdrave"), "здраве");
        assert_eq!(transliterate("Hello"), "Хелло");
    }

    #[test]
    fn switch_en_to_bg_detected_within_3_chars() {
        let mut d = LanguageDetector::new();
        // Establish EN detection.
        for ch in "hello".chars() {
            d.feed_char(ch);
        }
        assert_eq!(d.detected().lang, LangId::En);
        // Switch to Cyrillic — with hard reset + α=0.30, must flip within 3 chars.
        for ch in "здр".chars() {
            d.feed_char(ch);
        }
        assert_eq!(
            d.detected().lang,
            LangId::Bg,
            "should detect Bg within 3 Cyrillic chars after hard reset"
        );
    }
}
