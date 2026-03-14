// Typing regime detection — classifies the user's current typing mode.
//
// Inspired by market state detection from adaptive trading systems:
// just as a market can be trending/volatile/calm/reversing, a typing
// session has distinct regimes (fast coding, deliberate prose, mixed
// language, new topic, language switch) that benefit from different
// prediction strategies.
//
// All signals are tracked via exponential moving averages (EMA) with a
// configurable smoothing factor (default α = 0.15), matching the same
// principle used in the language detector and QAPOE.

use crate::lang_detect::LangId;

// ======================================================================
// Public types
// ======================================================================

/// Typing regime classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TypingRegime {
    /// High WPM, short words, technical vocabulary — coding or terminal use.
    FastCoding,
    /// Low WPM, long words, natural language — careful writing.
    DeliberateProse,
    /// Alternating EN/BG within sentences — bilingual input.
    MixedLanguage,
    /// Many OOV words, high unknown rate — new domain or topic.
    NewTopic,
    /// Dual-buffer flip rate spike — user is switching keyboard layout rapidly.
    LanguageSwitch,
}

// ======================================================================
// RegimeDetector
// ======================================================================

/// Detects the current typing regime from observed word statistics.
///
/// Each call to [`observe_word`] updates five EMA signals and
/// reclassifies the regime according to fixed thresholds.
pub struct RegimeDetector {
    // ── EMA signals ──────────────────────────────────────────────────
    /// Words per minute (EMA).
    wpm_ema: f64,
    /// Average word length in characters (EMA).
    word_len_ema: f64,
    /// Out-of-vocabulary rate: fraction of words unknown to the corpus (EMA).
    oov_rate_ema: f64,
    /// Dual-buffer layout-flip rate: fraction of words that triggered a flip (EMA).
    flip_rate_ema: f64,
    /// Language ratio: EN / (EN + BG) (EMA). 1.0 = all EN, 0.0 = all BG.
    lang_ratio_ema: f64,

    // ── Current classification ────────────────────────────────────────
    current_regime: TypingRegime,
    /// Confidence in the current classification, in `[0.0, 1.0]`.
    confidence: f64,

    // ── Bookkeeping ───────────────────────────────────────────────────
    /// Total words observed (used for warm-up period).
    word_count: u32,
    /// Timestamp of the most recent word commit.
    last_word_time: Option<std::time::Instant>,
    /// EMA smoothing factor α (default 0.15).
    ema_alpha: f64,
}

impl RegimeDetector {
    // ── Classification thresholds ──────────────────────────────────────

    /// WPM above this → candidate for FastCoding.
    const WPM_FAST: f64 = 60.0;
    /// WPM below this → candidate for DeliberateProse.
    const WPM_SLOW: f64 = 30.0;
    /// Average word length above this → candidate for DeliberateProse.
    const WORD_LEN_LONG: f64 = 7.0;
    /// Average word length below this → candidate for FastCoding.
    const WORD_LEN_SHORT: f64 = 6.0;
    /// OOV rate above this → FastCoding signal (tech identifiers).
    const OOV_FAST_CODING: f64 = 0.2;
    /// OOV rate above this → NewTopic (very high unknown fraction).
    const OOV_NEW_TOPIC: f64 = 0.4;
    /// Flip rate above this → LanguageSwitch.
    const FLIP_RATE_SWITCH: f64 = 0.2;
    /// lang_ratio in (LOW, HIGH) → MixedLanguage.
    const LANG_RATIO_LOW: f64 = 0.3;
    const LANG_RATIO_HIGH: f64 = 0.7;

    /// Default EMA smoothing factor (matches lang_detect.rs).
    const DEFAULT_ALPHA: f64 = 0.15;

    /// Number of words needed before regime classification is meaningful.
    const WARMUP_WORDS: u32 = 5;

    // ── Constructor ────────────────────────────────────────────────────

    /// Create a new detector with default settings.
    pub fn new() -> Self {
        Self {
            wpm_ema: 0.0,
            word_len_ema: 5.0, // Neutral start
            oov_rate_ema: 0.0,
            flip_rate_ema: 0.0,
            lang_ratio_ema: 0.5, // Neutral — neither all EN nor all BG
            current_regime: TypingRegime::DeliberateProse,
            confidence: 0.0,
            word_count: 0,
            last_word_time: None,
            ema_alpha: Self::DEFAULT_ALPHA,
        }
    }

    /// Create a detector with a custom EMA alpha (for testing/tuning).
    pub fn with_alpha(alpha: f64) -> Self {
        let mut d = Self::new();
        d.ema_alpha = alpha.clamp(0.01, 1.0);
        d
    }

    // ── Observation ────────────────────────────────────────────────────

    /// Feed one committed word's statistics into the detector.
    ///
    /// - `word_len`: character count of the committed word.
    /// - `is_oov`: true when the word was not found in the corpus.
    /// - `is_flip`: true when this word triggered a dual-buffer layout flip.
    /// - `lang`: the language detected for this word.
    pub fn observe_word(&mut self, word_len: usize, is_oov: bool, is_flip: bool, lang: LangId) {
        let now = std::time::Instant::now();
        let alpha = self.ema_alpha;

        // ── WPM ─────────────────────────────────────────────────────────
        // Estimate WPM from the inter-word interval.
        // WPM ≈ 1 word / elapsed_minutes. Cap at 300 WPM (unreachable in practice).
        let instant_wpm = if let Some(last) = self.last_word_time {
            let elapsed_secs = now.duration_since(last).as_secs_f64();
            if elapsed_secs > 0.0 {
                (60.0 / elapsed_secs).min(300.0)
            } else {
                self.wpm_ema // Can't measure — keep current
            }
        } else {
            // No previous timestamp — assume a neutral 40 WPM for the first word.
            40.0
        };
        self.last_word_time = Some(now);
        self.wpm_ema = ema_update(self.wpm_ema, instant_wpm, alpha);

        // ── Word length ──────────────────────────────────────────────────
        self.word_len_ema = ema_update(self.word_len_ema, word_len as f64, alpha);

        // ── OOV rate ─────────────────────────────────────────────────────
        let oov_signal = if is_oov { 1.0 } else { 0.0 };
        self.oov_rate_ema = ema_update(self.oov_rate_ema, oov_signal, alpha);

        // ── Flip rate ─────────────────────────────────────────────────────
        let flip_signal = if is_flip { 1.0 } else { 0.0 };
        self.flip_rate_ema = ema_update(self.flip_rate_ema, flip_signal, alpha);

        // ── Language ratio ────────────────────────────────────────────────
        // EN → 1.0, BG → 0.0, Tech → hold current ratio (neutral).
        let lang_signal = match lang {
            LangId::En => 1.0,
            LangId::Bg => 0.0,
            LangId::Tech => self.lang_ratio_ema, // No update for tech symbols
        };
        if lang != LangId::Tech {
            self.lang_ratio_ema = ema_update(self.lang_ratio_ema, lang_signal, alpha);
        }

        self.word_count += 1;

        // ── Reclassify ────────────────────────────────────────────────────
        self.classify();
    }

    // ── Accessors ──────────────────────────────────────────────────────

    /// Returns the current regime and its confidence.
    pub fn regime(&self) -> (TypingRegime, f64) {
        (self.current_regime, self.confidence)
    }

    /// Current WPM estimate.
    pub fn wpm(&self) -> f64 {
        self.wpm_ema
    }

    /// Current OOV rate estimate.
    pub fn oov_rate(&self) -> f64 {
        self.oov_rate_ema
    }

    /// Current lang ratio (EN / total).
    pub fn lang_ratio(&self) -> f64 {
        self.lang_ratio_ema
    }

    // ── Classification ─────────────────────────────────────────────────

    /// Reclassify the regime from current EMA signals.
    ///
    /// Priority order (highest specificity first):
    /// 1. LanguageSwitch — flip rate spike
    /// 2. NewTopic       — very high OOV
    /// 3. MixedLanguage  — balanced lang ratio
    /// 4. FastCoding     — fast + short words + elevated OOV
    /// 5. DeliberateProse — slow + long words
    ///
    /// Below the warmup threshold the confidence is capped at 0.5 to signal
    /// that the classification is tentative.
    fn classify(&mut self) {
        let warm = self.word_count >= Self::WARMUP_WORDS;
        let warmup_cap = if warm { 1.0 } else { 0.5 };

        // ── 1. LanguageSwitch ─────────────────────────────────────────────
        if self.flip_rate_ema > Self::FLIP_RATE_SWITCH {
            let excess =
                (self.flip_rate_ema - Self::FLIP_RATE_SWITCH) / (1.0 - Self::FLIP_RATE_SWITCH);
            self.current_regime = TypingRegime::LanguageSwitch;
            self.confidence = (0.5 + 0.5 * excess).min(warmup_cap);
            return;
        }

        // ── 2. NewTopic ────────────────────────────────────────────────────
        if self.oov_rate_ema > Self::OOV_NEW_TOPIC {
            let excess = (self.oov_rate_ema - Self::OOV_NEW_TOPIC) / (1.0 - Self::OOV_NEW_TOPIC);
            self.current_regime = TypingRegime::NewTopic;
            self.confidence = (0.5 + 0.5 * excess).min(warmup_cap);
            return;
        }

        // ── 3. MixedLanguage ──────────────────────────────────────────────
        if self.lang_ratio_ema > Self::LANG_RATIO_LOW && self.lang_ratio_ema < Self::LANG_RATIO_HIGH
        {
            // Confidence peaks at 0.5 ratio, falls off toward the boundaries.
            let distance_from_center = (self.lang_ratio_ema - 0.5).abs();
            let band_half = 0.5 - Self::LANG_RATIO_LOW;
            let mix_strength = 1.0 - distance_from_center / band_half;
            self.current_regime = TypingRegime::MixedLanguage;
            self.confidence = (0.4 + 0.6 * mix_strength).min(warmup_cap);
            return;
        }

        // ── 4. FastCoding ─────────────────────────────────────────────────
        let is_fast_wpm = self.wpm_ema > Self::WPM_FAST;
        let is_short_words = self.word_len_ema < Self::WORD_LEN_SHORT;
        let is_oov_coding = self.oov_rate_ema > Self::OOV_FAST_CODING;

        if is_fast_wpm && is_short_words && is_oov_coding {
            // All three signals agree — high confidence.
            let wpm_signal = sigmoid_norm(self.wpm_ema, Self::WPM_FAST, 120.0);
            let len_signal = 1.0 - sigmoid_norm(self.word_len_ema, 3.0, Self::WORD_LEN_SHORT);
            let oov_signal = sigmoid_norm(self.oov_rate_ema, Self::OOV_FAST_CODING, 0.6);
            self.current_regime = TypingRegime::FastCoding;
            self.confidence = ((wpm_signal + len_signal + oov_signal) / 3.0).min(warmup_cap);
            return;
        }

        // Partial FastCoding: two of three signals.
        let fast_signals = [is_fast_wpm, is_short_words, is_oov_coding]
            .iter()
            .filter(|&&s| s)
            .count();
        if fast_signals >= 2 {
            self.current_regime = TypingRegime::FastCoding;
            self.confidence = (0.45 * fast_signals as f64 / 3.0).min(warmup_cap);
            return;
        }

        // ── 5. DeliberateProse ────────────────────────────────────────────
        let is_slow_wpm = self.wpm_ema < Self::WPM_SLOW;
        let is_long_words = self.word_len_ema > Self::WORD_LEN_LONG;

        if is_slow_wpm && is_long_words {
            let wpm_signal = 1.0 - sigmoid_norm(self.wpm_ema, 0.0, Self::WPM_SLOW);
            let len_signal = sigmoid_norm(self.word_len_ema, Self::WORD_LEN_LONG, 12.0);
            self.current_regime = TypingRegime::DeliberateProse;
            self.confidence = ((wpm_signal + len_signal) / 2.0).min(warmup_cap);
            return;
        }

        // Partial DeliberateProse: at least one of the two signals.
        if is_slow_wpm || is_long_words {
            self.current_regime = TypingRegime::DeliberateProse;
            self.confidence = 0.35_f64.min(warmup_cap);
            return;
        }

        // ── Default: fall back to the previous regime with reduced confidence ──
        self.confidence = (self.confidence * 0.8).min(warmup_cap);
    }
}

impl Default for RegimeDetector {
    fn default() -> Self {
        Self::new()
    }
}

// ======================================================================
// Helper math
// ======================================================================

/// Exponential moving average update: new = (1 - α) * old + α * sample.
#[inline]
fn ema_update(current: f64, sample: f64, alpha: f64) -> f64 {
    (1.0 - alpha) * current + alpha * sample
}

/// Normalise a value into [0, 1] using a soft sigmoid centered at `low`,
/// saturating at `high`. Equivalent to a piece-wise linear ramp.
#[inline]
fn sigmoid_norm(value: f64, low: f64, high: f64) -> f64 {
    if high <= low {
        return 0.5;
    }
    ((value - low) / (high - low)).clamp(0.0, 1.0)
}

// ======================================================================
// Tests
// ======================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn fast_coding_detector() -> RegimeDetector {
        let mut d = RegimeDetector::with_alpha(0.5); // Fast convergence for tests
                                                     // Feed 10 words at ~150 WPM with 25% OOV (every 4th word).
                                                     // With alpha=0.5 the OOV EMA at the last step ≈ 0.27, which is:
                                                     //   - above FastCoding threshold (> 0.20) ✓
                                                     //   - below NewTopic threshold (> 0.40) ✓
                                                     // WPM EMA ≈ 150 >> 60 (FastCoding threshold) ✓
                                                     // word_len = 4 < 6 (FastCoding threshold) ✓
        for i in 0..10u32 {
            d.last_word_time = Some(
                std::time::Instant::now() - std::time::Duration::from_millis(400), // ~150 WPM
            );
            let is_oov = i % 4 == 0; // 25% OOV (indices 0, 4, 8)
            d.observe_word(4, is_oov, false, LangId::En); // short words
        }
        d
    }

    fn prose_detector() -> RegimeDetector {
        let mut d = RegimeDetector::with_alpha(0.5);
        for _ in 0..8 {
            d.last_word_time = Some(
                std::time::Instant::now() - std::time::Duration::from_millis(3000), // ~20 WPM
            );
            d.observe_word(9, false, false, LangId::En); // long words, in-vocab
        }
        d
    }

    #[test]
    fn test_fast_coding_detection() {
        let d = fast_coding_detector();
        let (regime, conf) = d.regime();
        assert_eq!(
            regime,
            TypingRegime::FastCoding,
            "expected FastCoding, got {regime:?}"
        );
        assert!(conf > 0.0, "confidence should be positive");
    }

    #[test]
    fn test_deliberate_prose_detection() {
        let d = prose_detector();
        let (regime, conf) = d.regime();
        assert_eq!(
            regime,
            TypingRegime::DeliberateProse,
            "expected DeliberateProse, got {regime:?}"
        );
        assert!(conf > 0.0, "confidence should be positive");
    }

    #[test]
    fn test_mixed_language_detection() {
        let mut d = RegimeDetector::with_alpha(0.5);
        for _ in 0..8 {
            d.last_word_time =
                Some(std::time::Instant::now() - std::time::Duration::from_millis(800));
            d.observe_word(5, false, false, LangId::En);
            d.last_word_time =
                Some(std::time::Instant::now() - std::time::Duration::from_millis(800));
            d.observe_word(5, false, false, LangId::Bg);
        }
        let (regime, _conf) = d.regime();
        assert_eq!(
            regime,
            TypingRegime::MixedLanguage,
            "expected MixedLanguage, got {regime:?}"
        );
    }

    #[test]
    fn test_language_switch_detection() {
        let mut d = RegimeDetector::with_alpha(0.5);
        for _ in 0..10 {
            d.last_word_time =
                Some(std::time::Instant::now() - std::time::Duration::from_millis(600));
            d.observe_word(5, false, true, LangId::En); // flip = true every word
        }
        let (regime, conf) = d.regime();
        assert_eq!(
            regime,
            TypingRegime::LanguageSwitch,
            "expected LanguageSwitch, got {regime:?}"
        );
        assert!(conf > 0.4, "confidence should be substantial: {conf}");
    }

    #[test]
    fn test_new_topic_detection() {
        let mut d = RegimeDetector::with_alpha(0.5);
        for _ in 0..8 {
            d.last_word_time =
                Some(std::time::Instant::now() - std::time::Duration::from_millis(1000));
            d.observe_word(7, true, false, LangId::En); // all OOV
        }
        let (regime, _) = d.regime();
        assert_eq!(
            regime,
            TypingRegime::NewTopic,
            "expected NewTopic, got {regime:?}"
        );
    }

    #[test]
    fn test_warmup_caps_confidence() {
        let mut d = RegimeDetector::with_alpha(0.9); // Very fast convergence
                                                     // Only 3 words (below WARMUP_WORDS = 5)
        for _ in 0..3 {
            d.last_word_time =
                Some(std::time::Instant::now() - std::time::Duration::from_millis(300));
            d.observe_word(3, true, false, LangId::En);
        }
        let (_, conf) = d.regime();
        assert!(
            conf <= 0.5,
            "confidence should be capped at 0.5 during warmup, got {conf}"
        );
    }

    #[test]
    fn test_regime_and_confidence_accessor() {
        let d = RegimeDetector::new();
        let (regime, conf) = d.regime();
        // Default state: DeliberateProse, confidence = 0.0 (no data)
        assert_eq!(regime, TypingRegime::DeliberateProse);
        assert_eq!(conf, 0.0);
    }

    #[test]
    fn test_ema_update() {
        let result = super::ema_update(0.0, 1.0, 0.5);
        assert!((result - 0.5).abs() < 1e-10);
    }

    #[test]
    fn test_sigmoid_norm_bounds() {
        assert!((super::sigmoid_norm(0.0, 0.0, 1.0) - 0.0).abs() < 1e-10);
        assert!((super::sigmoid_norm(1.0, 0.0, 1.0) - 1.0).abs() < 1e-10);
        assert!((super::sigmoid_norm(0.5, 0.0, 1.0) - 0.5).abs() < 1e-10);
        // Clamped below
        assert!((super::sigmoid_norm(-5.0, 0.0, 1.0) - 0.0).abs() < 1e-10);
        // Clamped above
        assert!((super::sigmoid_norm(5.0, 0.0, 1.0) - 1.0).abs() < 1e-10);
    }
}
