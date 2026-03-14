// Dual-interpretation buffer for layout-agnostic input.
//
// Maintains two parallel buffers (EN and BG) built from hardware scancodes.
// After each keystroke, corpus frequency scores determine the winner.
// Once confidence exceeds a threshold, the language is locked for the
// remainder of the word. A confidence flip after lock triggers a
// ReplaceWord correction.

use crate::keymap;
use crate::lang_detect::LangId;

/// Configuration for the dual buffer.
#[derive(Debug, Clone)]
pub struct DualBufferConfig {
    /// Enable dual-buffer layout-agnostic input.
    pub enabled: bool,
    /// Minimum confidence to lock the language for the current word.
    pub lock_threshold: f64,
    /// Minimum characters before locking is allowed.
    pub min_lock_chars: usize,
}

impl Default for DualBufferConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            lock_threshold: 0.7,
            min_lock_chars: 2,
        }
    }
}

/// Dual-interpretation buffer that tracks both EN and BG readings
/// of the same physical keypress sequence.
pub struct DualBuffer {
    en_buf: String,
    bg_buf: String,
    /// Normalised score for EN interpretation [0.0, 1.0].
    en_score: f64,
    /// Normalised score for BG interpretation [0.0, 1.0].
    bg_score: f64,
    /// Currently winning language.
    winner: LangId,
    /// Confidence of the winner (max of en_score, bg_score).
    confidence: f64,
    /// Whether the language has been locked for this word.
    locked: bool,
    /// The language that was locked (if any).
    locked_lang: Option<LangId>,
    /// True when the winner changed after a lock (triggers ReplaceWord).
    flip_detected: bool,
    /// Lock threshold from config.
    lock_threshold: f64,
    /// Minimum chars before lock from config.
    min_lock_chars: usize,
}

impl DualBuffer {
    /// Create a new empty dual buffer with the given lock parameters.
    pub fn new(lock_threshold: f64, min_lock_chars: usize) -> Self {
        Self {
            en_buf: String::with_capacity(32),
            bg_buf: String::with_capacity(32),
            en_score: 0.5,
            bg_score: 0.5,
            winner: LangId::En,
            confidence: 0.5,
            locked: false,
            locked_lang: None,
            flip_detected: false,
            lock_threshold,
            min_lock_chars,
        }
    }

    /// Create from config.
    pub fn from_config(config: &DualBufferConfig) -> Self {
        Self::new(config.lock_threshold, config.min_lock_chars)
    }

    /// Push a character pair from a single physical keypress.
    ///
    /// `en_ch` is the US QWERTY interpretation, `bg_ch` is BG Phonetic.
    pub fn push(&mut self, en_ch: char, bg_ch: char) {
        self.en_buf.push(en_ch);
        self.bg_buf.push(bg_ch);
    }

    /// Push a character from a scancode.
    ///
    /// Returns the (EN, BG) character pair, or `None` if the scancode
    /// doesn't map to a character key.
    pub fn push_keycode(&mut self, code: u16, shift: bool) -> Option<(char, char)> {
        let (en, bg) = keymap::scancode_to_both(code, shift)?;
        self.push(en, bg);
        Some((en, bg))
    }

    /// Remove the last character from both buffers (backspace).
    pub fn pop(&mut self) {
        self.en_buf.pop();
        self.bg_buf.pop();
        // Reset lock state if we drop below min_lock_chars.
        if self.en_buf.len() < self.min_lock_chars {
            self.locked = false;
            self.locked_lang = None;
            self.flip_detected = false;
        }
    }

    /// Update scores from corpus frequency data and determine the winner.
    ///
    /// `en_freq` and `bg_freq` are the raw corpus frequencies (e.g. from
    /// `SmartKeyEngine::score_both()`).
    pub fn update_scores(&mut self, en_freq: f64, bg_freq: f64) {
        let total = en_freq + bg_freq;
        if total < 1.0 {
            // No corpus support for either — keep previous winner, low confidence.
            self.en_score = 0.5;
            self.bg_score = 0.5;
            self.confidence = 0.5;
            return;
        }

        self.en_score = en_freq / total;
        self.bg_score = bg_freq / total;

        let new_winner = if self.en_score >= self.bg_score {
            LangId::En
        } else {
            LangId::Bg
        };
        self.confidence = self.en_score.max(self.bg_score);

        // Check for flip after lock.
        if self.locked && Some(new_winner) != self.locked_lang {
            self.flip_detected = true;
            // Update lock to new winner.
            self.locked_lang = Some(new_winner);
        }

        self.winner = new_winner;

        // Lock when confidence is high enough and we have enough chars.
        if !self.locked
            && self.confidence >= self.lock_threshold
            && self.en_buf.len() >= self.min_lock_chars
        {
            self.locked = true;
            self.locked_lang = Some(self.winner);
        }
    }

    // -- Accessors ---------------------------------------------------------

    /// The currently winning language.
    pub fn winner_lang(&self) -> LangId {
        self.winner
    }

    /// The text of the winning interpretation.
    pub fn winner_text(&self) -> &str {
        match self.winner {
            LangId::En => &self.en_buf,
            LangId::Bg | LangId::Tech => &self.bg_buf,
        }
    }

    /// The last character of the winning buffer.
    pub fn last_winner_char(&self) -> Option<char> {
        self.winner_text().chars().last()
    }

    /// Confidence of the current winner [0.0, 1.0].
    pub fn confidence(&self) -> f64 {
        self.confidence
    }

    /// Whether the language has been locked for this word.
    pub fn is_locked(&self) -> bool {
        self.locked
    }

    /// Whether a confidence flip was detected after lock.
    ///
    /// When true, the caller should emit a `ReplaceWord` action to fix
    /// previously committed characters.
    pub fn flip_detected(&self) -> bool {
        self.flip_detected
    }

    /// Clear the flip flag after handling it.
    pub fn clear_flip(&mut self) {
        self.flip_detected = false;
    }

    /// The EN interpretation buffer.
    pub fn en_text(&self) -> &str {
        &self.en_buf
    }

    /// The BG interpretation buffer.
    pub fn bg_text(&self) -> &str {
        &self.bg_buf
    }

    /// Number of characters in the buffers (same for both).
    pub fn len(&self) -> usize {
        self.en_buf.len()
    }

    /// Whether the buffers are empty.
    pub fn is_empty(&self) -> bool {
        self.en_buf.is_empty()
    }

    /// Reset both buffers and all state (call on word boundary).
    pub fn clear(&mut self) {
        self.en_buf.clear();
        self.bg_buf.clear();
        self.en_score = 0.5;
        self.bg_score = 0.5;
        self.winner = LangId::En;
        self.confidence = 0.5;
        self.locked = false;
        self.locked_lang = None;
        self.flip_detected = false;
    }
}

// ======================================================================
// Tests
// ======================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> DualBuffer {
        DualBuffer::new(0.7, 2)
    }

    #[test]
    fn test_push_and_text() {
        let mut b = db();
        b.push('h', 'х');
        b.push('e', 'е');
        assert_eq!(b.en_text(), "he");
        assert_eq!(b.bg_text(), "хе");
        assert_eq!(b.len(), 2);
    }

    #[test]
    fn test_push_keycode() {
        let mut b = db();
        // Scancode 43 = 'h' / 'х'
        let pair = b.push_keycode(43, false);
        assert_eq!(pair, Some(('h', 'х')));
        assert_eq!(b.en_text(), "h");
        assert_eq!(b.bg_text(), "х");
    }

    #[test]
    fn test_pop() {
        let mut b = db();
        b.push('h', 'х');
        b.push('e', 'е');
        b.pop();
        assert_eq!(b.len(), 1);
        assert_eq!(b.en_text(), "h");
        assert_eq!(b.bg_text(), "х");
    }

    #[test]
    fn test_en_wins_with_high_freq() {
        let mut b = db();
        b.push('h', 'х');
        b.push('e', 'е');
        // EN "he" has much higher frequency than BG "хе"
        b.update_scores(4_897_788.0, 43_651.0);
        assert_eq!(b.winner_lang(), LangId::En);
        assert!(b.confidence() > 0.9);
        assert_eq!(b.winner_text(), "he");
    }

    #[test]
    fn test_bg_wins_with_high_freq() {
        let mut b = db();
        b.push('z', 'з');
        b.push('d', 'д');
        // BG "зд" (здравей) has much higher frequency than EN "zd" (zdnet)
        b.update_scores(120.0, 162_181.0);
        assert_eq!(b.winner_lang(), LangId::Bg);
        assert!(b.confidence() > 0.99);
        assert_eq!(b.winner_text(), "зд");
    }

    #[test]
    fn test_lock_on_high_confidence() {
        let mut b = db();
        b.push('h', 'х');
        b.push('e', 'е');
        b.update_scores(4_897_788.0, 43_651.0);
        // Confidence > 0.7 and len >= 2 → should lock.
        assert!(b.is_locked());
    }

    #[test]
    fn test_no_lock_below_threshold() {
        let mut b = db();
        b.push('a', 'а');
        b.push('b', 'б');
        // Close to 50/50 → confidence < 0.7 → no lock.
        b.update_scores(100.0, 90.0);
        assert!(!b.is_locked());
    }

    #[test]
    fn test_no_lock_single_char() {
        let mut b = db();
        b.push('h', 'х');
        // Only 1 char, min_lock_chars=2 → no lock even with high confidence.
        b.update_scores(4_897_788.0, 0.0);
        assert!(!b.is_locked());
    }

    #[test]
    fn test_flip_detection() {
        let mut b = db();
        b.push('h', 'х');
        b.push('e', 'е');
        // Lock to EN.
        b.update_scores(4_897_788.0, 43_651.0);
        assert!(b.is_locked());
        assert!(!b.flip_detected());

        // Add more chars where BG wins overwhelmingly.
        b.push('l', 'л');
        b.update_scores(10.0, 1_000_000.0);
        assert!(b.flip_detected());
        assert_eq!(b.winner_lang(), LangId::Bg);

        // Clear flip.
        b.clear_flip();
        assert!(!b.flip_detected());
    }

    #[test]
    fn test_no_corpus_support() {
        let mut b = db();
        b.push('q', 'я');
        b.push('x', 'ь');
        // Zero frequency for both → 50/50, low confidence.
        b.update_scores(0.0, 0.0);
        assert_eq!(b.confidence(), 0.5);
        assert!(!b.is_locked());
    }

    #[test]
    fn test_clear_resets_all() {
        let mut b = db();
        b.push('h', 'х');
        b.push('e', 'е');
        b.update_scores(4_897_788.0, 43_651.0);
        assert!(b.is_locked());

        b.clear();
        assert!(b.is_empty());
        assert_eq!(b.len(), 0);
        assert!(!b.is_locked());
        assert!(!b.flip_detected());
        assert_eq!(b.confidence(), 0.5);
    }

    #[test]
    fn test_pop_resets_lock_below_min() {
        let mut b = db();
        b.push('h', 'х');
        b.push('e', 'е');
        b.update_scores(4_897_788.0, 43_651.0);
        assert!(b.is_locked());

        // Pop to 1 char → below min_lock_chars → lock resets.
        b.pop();
        assert!(!b.is_locked());
    }

    #[test]
    fn test_last_winner_char() {
        let mut b = db();
        b.push('h', 'х');
        b.push('e', 'е');
        b.update_scores(4_897_788.0, 43_651.0); // EN wins
        assert_eq!(b.last_winner_char(), Some('e'));

        b.update_scores(10.0, 1_000_000.0); // BG wins
        assert_eq!(b.last_winner_char(), Some('е'));
    }
}
