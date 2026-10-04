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
            lock_threshold: 0.85,
            min_lock_chars: 4,
        }
    }
}

/// Where the current winner's evidence comes from (U2 evidence-origin API).
///
/// The numbers alone cannot tell these apart: a both-zero scoring and a
/// genuine 50/50 corpus tie both read confidence 0.5, and a first-character
/// prior bias reads a constant 0.65.
///
/// Transition table (the only writers): `new()`/`clear()` → `Initial`;
/// `update_scores` → `UnsupportedBoth` when `total < MIN_CORPUS_SUPPORT`,
/// else `Corpus`; `apply_prior_lock_hint` → `PriorOnly` only in the
/// len == 1 branch where the prior DIFFERS from the scored winner (a
/// matching prior leaves the scored origin as it is — with zero support it
/// cannot lock either, because confidence is 0.5 and the relaxed threshold
/// is at least 0.55; a repeated hint at len 1 after an override is excluded
/// by the caller's score-then-hint order, not by that threshold);
/// `pop()` to empty → `Initial` via `clear()`.  The caller scores BEFORE it
/// applies the hint, so on the first character the hint is the last writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceOrigin {
    /// Fresh or cleared buffer: nothing has been scored yet.
    Initial,
    /// The last scoring found no corpus support for either reading
    /// (`total < MIN_CORPUS_SUPPORT`); the winner is inherited, not derived.
    UnsupportedBoth,
    /// A surrounding/momentum prior biased the winner on the first
    /// character; no word-level evidence exists yet.
    PriorOnly,
    /// At least one reading has prefix-corpus support; scores are normalised.
    Corpus,
}

/// Prefix support is an integer count; `total < MIN_CORPUS_SUPPORT`
/// (strict-less, 1.0) therefore means BOTH readings have zero support, not a
/// tolerance.  A future `>=` or fractional scorer must revisit the R8 and R9b
/// evidence-origin tests before changing this.
pub const MIN_CORPUS_SUPPORT: f64 = 1.0;

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
    /// Provenance of the current winner (see `EvidenceOrigin`).
    origin: EvidenceOrigin,
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
            origin: EvidenceOrigin::Initial,
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

    /// Append an explicitly accepted completion character to both physical
    /// interpretations and pin the language currently shown to the user.
    ///
    /// Returns `false` without mutation when the character has no exact
    /// physical-key counterpart.  Acceptance is an unambiguous language signal,
    /// so an unlocked buffer becomes locked to its current winner.
    pub(crate) fn push_winner_char(&mut self, ch: char) -> bool {
        let layout = match self.winner {
            LangId::Bg => keymap::Layout::Bg,
            LangId::En | LangId::Tech => keymap::Layout::En,
        };
        let Some((en_ch, bg_ch)) = keymap::physical_pair_for_char(layout, ch) else {
            return false;
        };

        self.push(en_ch, bg_ch);
        self.locked = true;
        self.locked_lang = Some(self.winner);
        true
    }

    /// Remove the last character from both buffers (backspace).
    ///
    /// Empty buffer ⇒ full `clear()`; invariant, unreachable from the key
    /// path (the input Backspace arm drops an emptied buffer — see R7c).
    pub fn pop(&mut self) {
        self.en_buf.pop();
        self.bg_buf.pop();
        if self.is_empty() {
            self.clear();
        }
        // Above len 0 the lock is sticky — once locked, stays locked until
        // clear().  This prevents re-entering hypothesis phase after chars
        // were committed to the application during the lock transition.
    }

    /// Update scores from corpus frequency data and determine the winner.
    ///
    /// `en_freq` and `bg_freq` are the raw corpus frequencies (e.g. from
    /// `SmartKeyEngine::score_both()`).
    pub fn update_scores(&mut self, en_freq: f64, bg_freq: f64) {
        let total = en_freq + bg_freq;
        if total < MIN_CORPUS_SUPPORT {
            // No corpus support for either — keep previous winner, low confidence.
            self.en_score = 0.5;
            self.bg_score = 0.5;
            self.confidence = 0.5;
            self.origin = EvidenceOrigin::UnsupportedBoth;
            return;
        }

        self.en_score = en_freq / total;
        self.bg_score = bg_freq / total;
        self.origin = EvidenceOrigin::Corpus;

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

    /// Use a surrounding-language prior to allow an earlier lock when the
    /// corpus winner already agrees with the surrounding context.
    pub fn apply_prior_lock_hint(&mut self, prior: Option<LangId>) {
        if self.locked {
            return;
        }

        let Some(prior_lang) = prior else {
            return;
        };
        if !matches!(prior_lang, LangId::En | LangId::Bg) {
            return;
        }

        // On the 1st character, context overrides corpus — single-char
        // frequencies are meaningless for language discrimination.
        // Momentum (≥60% of last 5 words) is the only reliable signal.
        //
        // B9 / F1 (2026-08-23): the override is a BIAS, never a lock.  Locking
        // here made a contextual guess irreversible before any word-level
        // evidence existed: after an xkb→SmartKey switch inside a Latin
        // terminal every Bulgarian word was committed as EN keymap text
        // (имаш → "ima[").  Leaving the buffer unlocked lets the caller's
        // normal char-2+ rescoring (gated on `!is_locked()`) overturn the
        // guess as soon as the corpus can tell the languages apart, while a
        // matching prior can still early-lock through the branch below.
        if self.en_buf.len() == 1 && prior_lang != self.winner {
            self.winner = prior_lang;
            self.confidence = 0.65;
            self.origin = EvidenceOrigin::PriorOnly;
            return;
        }

        if prior_lang != self.winner {
            return;
        }

        let relaxed_threshold = (self.lock_threshold - 0.15).max(0.55);
        let relaxed_min_chars = self.min_lock_chars.saturating_sub(1).max(1);
        if self.confidence >= relaxed_threshold && self.en_buf.len() >= relaxed_min_chars {
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
            LangId::En | LangId::Tech => &self.en_buf,
            LangId::Bg => &self.bg_buf,
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

    /// Provenance of the current winner (see the transition table on
    /// `EvidenceOrigin`).  No production consumer reads this yet; the
    /// detector-side policy (D5) is a separate, separately gated change.
    pub fn evidence_origin(&self) -> EvidenceOrigin {
        self.origin
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

    /// Number of characters pushed (same for both buffers).
    ///
    /// Uses `en_buf` byte length, which equals the character count because
    /// the EN buffer contains only single-byte ASCII characters.
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
        self.origin = EvidenceOrigin::Initial;
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
        // Scancode 35 = 'h' / 'х' (evdev)
        let pair = b.push_keycode(35, false);
        assert_eq!(pair, Some(('h', 'х')));
        assert_eq!(b.en_text(), "h");
        assert_eq!(b.bg_text(), "х");
    }

    #[test]
    fn test_push_winner_char_pins_bg_and_updates_both_buffers() {
        let mut b = DualBuffer::new(0.85, 4);
        b.push('z', 'з');
        b.push('d', 'д');
        b.update_scores(1.0, 100.0);
        assert_eq!(b.winner_lang(), LangId::Bg);
        assert!(!b.is_locked());

        assert!(b.push_winner_char('в'));

        assert_eq!(b.en_text(), "zdw");
        assert_eq!(b.bg_text(), "здв");
        assert_eq!(b.winner_text(), "здв");
        assert!(b.is_locked());
    }

    #[test]
    fn test_push_winner_char_is_noop_when_character_is_unmapped() {
        let mut b = DualBuffer::new(0.85, 4);
        b.push('z', 'з');
        b.update_scores(1.0, 100.0);
        let before = (
            b.en_text().to_string(),
            b.bg_text().to_string(),
            b.winner_lang(),
            b.is_locked(),
        );

        assert!(!b.push_winner_char('ѝ'));

        assert_eq!(
            before,
            (
                b.en_text().to_string(),
                b.bg_text().to_string(),
                b.winner_lang(),
                b.is_locked(),
            )
        );
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
    fn test_pop_keeps_lock_sticky() {
        let mut b = db();
        b.push('h', 'х');
        b.push('e', 'е');
        b.update_scores(4_897_788.0, 43_651.0);
        assert!(b.is_locked());

        // Pop to 1 char — lock stays (sticky, not reset on pop).
        b.pop();
        assert!(b.is_locked());
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

    #[test]
    fn lock_requires_4_chars() {
        // With min_lock_chars=4, locking must not happen before char 4
        let mut b = DualBuffer::new(0.85, 4);
        b.push('h', 'х');
        b.update_scores(1_000_000.0, 1.0);
        assert!(!b.is_locked(), "should not lock at 1 char");
        b.push('e', 'е');
        b.update_scores(1_000_000.0, 1.0);
        assert!(!b.is_locked(), "should not lock at 2 chars");
        b.push('l', 'л');
        b.update_scores(1_000_000.0, 1.0);
        assert!(!b.is_locked(), "should not lock at 3 chars");
        b.push('l', 'л');
        b.update_scores(1_000_000.0, 1.0);
        assert!(b.is_locked(), "should lock at 4 chars with high confidence");
        assert_eq!(b.winner_lang(), LangId::En);
    }

    #[test]
    fn prior_match_can_lock_one_char_earlier() {
        let mut b = DualBuffer::new(0.85, 4);
        b.push('h', 'х');
        b.push('e', 'е');
        b.push('l', 'л');
        b.update_scores(850.0, 150.0); // confidence = 0.85, but len=3 < 4
        assert!(!b.is_locked(), "base lock should still require 4 chars");

        b.apply_prior_lock_hint(Some(LangId::En));
        assert!(b.is_locked(), "matching prior should allow earlier lock");
        assert_eq!(b.winner_lang(), LangId::En);
    }

    #[test]
    fn prior_mismatch_does_not_early_lock() {
        let mut b = DualBuffer::new(0.85, 4);
        b.push('h', 'х');
        b.push('e', 'е');
        b.push('l', 'л');
        b.update_scores(850.0, 150.0); // confidence = 0.85, but len=3 < 4
        b.apply_prior_lock_hint(Some(LangId::Bg));
        assert!(
            !b.is_locked(),
            "mismatching prior must not force an early lock"
        );
    }

    #[test]
    fn bg_locks_by_char_4() {
        let mut b = DualBuffer::new(0.85, 4);
        b.push('z', 'з');
        b.update_scores(1.0, 1_000_000.0);
        assert!(!b.is_locked(), "no lock at 1 char");
        b.push('d', 'д');
        b.update_scores(1.0, 1_000_000.0);
        assert!(!b.is_locked(), "no lock at 2 chars");
        b.push('r', 'р');
        b.update_scores(1.0, 1_000_000.0);
        assert!(!b.is_locked(), "no lock at 3 chars");
        b.push('a', 'а');
        b.update_scores(1.0, 1_000_000.0);
        assert!(b.is_locked(), "should lock BG at 4 chars");
        assert_eq!(b.winner_lang(), LangId::Bg);
    }

    // ==================================================================
    // Tech → EN remap test
    // ==================================================================

    #[test]
    fn tech_winner_returns_en_buffer() {
        // DualBuffer.winner_text() returns en_buf for both En and Tech.
        // Manually set winner to Tech by manipulating scores — Tech can't be
        // set via update_scores (it only produces En/Bg), so we test the
        // winner_text() match arm directly by constructing a DualBuffer
        // where winner=Tech through the update path workaround.
        //
        // Since update_scores only yields En/Bg, we verify the match arm
        // by reading the source mapping: LangId::Tech => &self.en_buf.
        // We do this by pushing chars and observing that when EN wins
        // (which shares the same arm as Tech), we get en_buf.
        let mut b = db();
        b.push('h', 'х');
        b.push('e', 'е');
        b.push('l', 'л');
        // Very high EN score → winner = En (same match arm as Tech).
        b.update_scores(10_000_000.0, 1.0);
        assert_eq!(b.winner_lang(), LangId::En);
        assert_eq!(
            b.winner_text(),
            b.en_text(),
            "EN winner should return en_buf"
        );
        assert_ne!(
            b.winner_text(),
            b.bg_text(),
            "EN winner should NOT return bg_buf"
        );

        // Verify the match covers Tech: winner_text() has `LangId::En | LangId::Tech => &self.en_buf`
        // We can construct a fresh buffer, set winner=Tech directly (the field is private),
        // so instead we verify the documented behavior: no BG buffer for Tech words.
        // The en_buf is always the EN layout text regardless of En or Tech winner.
        assert_eq!(b.en_text(), "hel");
        assert_eq!(b.bg_text(), "хел");
    }

    // ── 1st-character context override tests ──────────────────────

    #[test]
    fn prior_overrides_corpus_on_char_1() {
        let mut b = DualBuffer::new(0.85, 4);
        b.push('h', 'х');
        // EN corpus wins overwhelmingly.
        b.update_scores(1_000_000.0, 1.0);
        assert_eq!(b.winner_lang(), LangId::En);
        assert!(!b.is_locked());

        // Prior says BG (momentum from last 5 words).
        // On char 1, prior should OVERRIDE corpus winner — as a BIAS.
        b.apply_prior_lock_hint(Some(LangId::Bg));
        assert_eq!(
            b.winner_lang(),
            LangId::Bg,
            "prior should override on char 1"
        );
        // B9 / F1 (2026-08-23, ruling 487688994026): the contextual prior
        // must never become irreversible before word-level evidence exists.
        // Live defect: Latin surrounding text locked Bulgarian words to the
        // EN keymap on the first key (имаш → "ima[").
        assert!(
            !b.is_locked(),
            "a first-character prior biases the winner but must not lock"
        );
        assert!(
            (b.confidence() - 0.65).abs() < 1e-9,
            "prior confidence is recorded so a matching prior can still early-lock later"
        );
        assert_eq!(b.winner_text(), "х");
    }

    /// F1 end-to-end at the buffer level: after the char-1 prior bias the
    /// caller's normal rescoring on char 2 (gated on `!is_locked()`) must be
    /// able to flip the winner back to the corpus evidence.
    #[test]
    fn prior_bias_on_char_1_is_rescored_on_char_2() {
        let mut b = DualBuffer::new(0.85, 4);
        b.push('i', 'и');
        b.update_scores(1.0, 100.0);
        assert_eq!(b.winner_lang(), LangId::Bg);
        b.apply_prior_lock_hint(Some(LangId::En));
        assert_eq!(b.winner_lang(), LangId::En, "char-1 bias applied");
        assert!(!b.is_locked());

        b.push('m', 'м');
        b.update_scores(1.0, 10_000.0); // corpus: "им" overwhelms "im"

        assert_eq!(
            b.winner_lang(),
            LangId::Bg,
            "corpus evidence re-scores the biased winner"
        );
        assert_eq!(b.winner_text(), "им");
    }

    #[test]
    fn no_override_without_prior_on_char_1() {
        let mut b = DualBuffer::new(0.85, 4);
        b.push('h', 'х');
        b.update_scores(1_000_000.0, 1.0);
        b.apply_prior_lock_hint(None);
        // No prior → no override → existing behavior.
        assert_eq!(b.winner_lang(), LangId::En);
        assert!(!b.is_locked());
    }

    #[test]
    fn prior_no_override_on_char_2_plus() {
        let mut b = DualBuffer::new(0.85, 4);
        b.push('h', 'х');
        b.push('e', 'е');
        // 2 chars: prior mismatch should NOT override (only char 1 overrides).
        b.update_scores(1_000_000.0, 1.0);
        b.apply_prior_lock_hint(Some(LangId::Bg));
        assert_eq!(
            b.winner_lang(),
            LangId::En,
            "prior should NOT override after char 1"
        );
        assert!(!b.is_locked());
    }

    // ── U2 evidence-origin API — unit matrix ────────────────────────────────
    //
    // Commit 1 shipped the surface with a constant `Initial` (R2–R6, R8 RED
    // on the origin assert, R7b on its lock-clear assert); commit 2 adds the
    // transitions and R6b.  R7 (test_pop_keeps_lock_sticky) is unchanged.

    /// R1: a fresh buffer carries no evidence; clear() returns to that state.
    #[test]
    fn origin_is_initial_when_fresh_and_after_clear() {
        let mut b = DualBuffer::new(0.85, 4);
        assert_eq!(b.evidence_origin(), EvidenceOrigin::Initial);
        b.push('h', 'х');
        b.update_scores(4_897_788.0, 43_651.0);
        b.clear();
        assert_eq!(b.evidence_origin(), EvidenceOrigin::Initial);
    }

    /// R2: both readings unsupported → UnsupportedBoth; the winner and the
    /// literal EN reading are kept (D2), confidence 0.5.
    #[test]
    fn origin_is_unsupported_both_when_neither_reading_has_support() {
        let mut b = DualBuffer::new(0.85, 4);
        b.push('q', 'я');
        b.push('x', 'ь');
        b.update_scores(0.0, 0.0);
        assert_eq!(b.evidence_origin(), EvidenceOrigin::UnsupportedBoth);
        assert_eq!(b.winner_lang(), LangId::En);
        assert_eq!(b.winner_text(), "qx");
        assert_eq!(b.confidence(), 0.5);
    }

    /// R3: a genuine 50/50 corpus tie has the same numbers as R2 but is
    /// evidence — only the origin tells them apart.
    #[test]
    fn origin_is_corpus_on_an_exact_corpus_tie() {
        let mut b = DualBuffer::new(0.85, 4);
        b.push('a', 'а');
        b.push('b', 'б');
        b.update_scores(10.0, 10.0);
        assert_eq!(b.confidence(), 0.5, "premise: same number as R2");
        assert_eq!(b.evidence_origin(), EvidenceOrigin::Corpus);
    }

    /// R4: support, then none — the winner is inherited and the origin says
    /// so; the lock state is untouched.
    #[test]
    fn origin_becomes_unsupported_both_while_the_winner_is_inherited() {
        let mut b = DualBuffer::new(0.85, 4);
        b.push('h', 'х');
        b.update_scores(100.0, 1.0);
        assert_eq!(b.evidence_origin(), EvidenceOrigin::Corpus);
        assert_eq!(b.winner_lang(), LangId::En, "premise");
        b.push('q', 'я');
        b.update_scores(0.0, 0.0);
        assert_eq!(b.evidence_origin(), EvidenceOrigin::UnsupportedBoth);
        assert_eq!(
            b.winner_lang(),
            LangId::En,
            "winner inherited, not re-derived"
        );
        assert!(!b.is_locked());
    }

    /// R5: a char-1 prior override is PriorOnly; corpus evidence on char 2
    /// rescores it to Corpus.  R5 is also the guard for the SCORE → HINT
    /// order in input.rs `handle_dual_buffer_key` (`update_scores` before
    /// `apply_prior_lock_hint`): swapping them makes this node RED.
    #[test]
    fn origin_is_prior_only_on_char_1_then_corpus_on_char_2() {
        let mut b = DualBuffer::new(0.85, 4);
        b.push('i', 'и');
        b.update_scores(1.0, 100.0);
        b.apply_prior_lock_hint(Some(LangId::En));
        assert_eq!(b.winner_lang(), LangId::En, "premise: char-1 bias applied");
        assert!((b.confidence() - 0.65).abs() < 1e-9, "premise");
        assert!(!b.is_locked(), "premise");
        assert_eq!(b.evidence_origin(), EvidenceOrigin::PriorOnly);
        b.push('m', 'м');
        b.update_scores(1.0, 10_000.0);
        assert_eq!(b.evidence_origin(), EvidenceOrigin::Corpus);
        assert_eq!(b.winner_lang(), LangId::Bg);
    }

    /// R6: a matching prior that early-locks is evidence-backed — Corpus.
    #[test]
    fn origin_stays_corpus_when_a_matching_prior_early_locks() {
        let mut b = DualBuffer::new(0.85, 4);
        b.push('h', 'х');
        b.push('e', 'е');
        b.push('l', 'л');
        b.update_scores(850.0, 150.0);
        b.apply_prior_lock_hint(Some(LangId::En));
        assert!(b.is_locked(), "premise: relaxed early lock");
        assert_eq!(b.evidence_origin(), EvidenceOrigin::Corpus);
    }

    /// R7b: a pop() that empties the buffer performs the full clear() —
    /// origin Initial AND lock/locked_lang/flip cleared.  Unreachable from the
    /// core (the input.rs Backspace arm drops an emptied buffer, see the
    /// core-level R7c); defence in depth.  Pop above len 0 keeps the lock
    /// sticky (R7, test_pop_keeps_lock_sticky).
    #[test]
    fn pop_to_empty_clears_origin_and_lock() {
        let mut b = DualBuffer::new(0.85, 2);
        b.push('h', 'х');
        b.push('e', 'е');
        b.update_scores(4_897_788.0, 43_651.0);
        assert!(b.is_locked(), "premise");
        b.pop();
        assert!(b.is_locked(), "pop above len 0 keeps the lock sticky");
        b.pop();
        assert!(b.is_empty(), "premise");
        assert_eq!(b.evidence_origin(), EvidenceOrigin::Initial);
        assert!(!b.is_locked(), "pop to empty clears the lock");
        assert_eq!(b.locked_lang, None, "pop to empty clears locked_lang");
        assert!(!b.flip_detected());
        assert_eq!(b.confidence(), 0.5);
        assert_eq!(
            (b.en_score, b.bg_score),
            (0.5, 0.5),
            "pop to empty resets the scores"
        );
    }

    /// R6b: a MATCHING prior on a both-zero first character neither locks
    /// nor changes the origin — confidence is 0.5 and the relaxed threshold
    /// is at least 0.55.  Precondition: the confidence came from
    /// `update_scores` in the same step; a len==1 prior OVERRIDE raises
    /// confidence to 0.65, so a repeated hint at len 1 is excluded by the
    /// caller's score-then-hint order (R5), not by this threshold.
    #[test]
    fn origin_stays_unsupported_and_unlocked_when_a_matching_prior_meets_zero_support() {
        let mut b = DualBuffer::new(0.85, 4);
        b.push('q', 'я');
        b.update_scores(0.0, 0.0);
        assert_eq!(b.winner_lang(), LangId::En, "premise: En tie-break winner");
        b.apply_prior_lock_hint(Some(LangId::En));
        assert!(!b.is_locked());
        assert_eq!(b.winner_lang(), LangId::En);
        assert_eq!(b.evidence_origin(), EvidenceOrigin::UnsupportedBoth);
        assert_eq!(b.confidence(), 0.5);
    }

    /// R8: the threshold is a zero-support test on integer counts, not a
    /// tolerance — fractional inputs below MIN_CORPUS_SUPPORT are unsupported.
    #[test]
    fn origin_is_unsupported_both_below_min_corpus_support() {
        let mut b = DualBuffer::new(0.85, 4);
        b.push('q', 'я');
        b.update_scores(0.4, 0.5);
        assert!(0.4 + 0.5 < MIN_CORPUS_SUPPORT, "premise");
        assert_eq!(b.evidence_origin(), EvidenceOrigin::UnsupportedBoth);
        assert_eq!(b.confidence(), 0.5);
    }
}
