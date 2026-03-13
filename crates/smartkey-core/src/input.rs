// Platform-agnostic input method state machine.
//
// This module contains the key event dispatcher, ghost text lifecycle,
// word commit logic, and context tracking — shared across Linux (IBus),
// Windows (TSF), and macOS (IMK). Platform adapters call `handle_key()`
// and execute the returned `Action` list.

use std::collections::VecDeque;
use std::path::Path;
use std::time::Instant;

use crate::ensemble::{Prediction, SmartKeyEngine};
use crate::eval::PredictionMetrics;
use crate::lang_detect::{self, LangId, LanguageDetector};
use crate::paths;
use crate::personal;

// ======================================================================
// Key abstraction
// ======================================================================

bitflags::bitflags! {
    /// Modifier keys as a bitmask. Platform adapters translate OS-specific
    /// modifier representations into this common set.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Modifiers: u32 {
        const RELEASE = 0x01;
        const CTRL    = 0x02;
        const ALT     = 0x04;
        const SUPER   = 0x08;
        const SHIFT   = 0x10;
    }
}

/// Platform-neutral key identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Tab,
    Escape,
    Right,
    Backspace,
    Space,
    Return,
    /// Unrecognised key — pass through.
    Other(u32),
}

/// A single key event delivered by the platform.
#[derive(Debug, Clone)]
pub struct KeyEvent {
    pub key: Key,
    pub modifiers: Modifiers,
}

// ======================================================================
// Actions returned to the platform
// ======================================================================

/// An instruction for the platform adapter to execute.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Display `text` as greyed-out ghost completion.
    ShowGhost(String),
    /// Remove any displayed ghost text.
    HideGhost,
    /// Commit `text` to the application (insert it).
    CommitText(String),
    /// Do not consume the key — let it reach the application.
    ForwardKey,
    /// Replace `replace_len` characters before cursor with `text`.
    /// Used for transliteration: "zd" → "зд".
    ReplaceWord { replace_len: usize, text: String },
}

// ======================================================================
// Configuration
// ======================================================================

const CONTEXT_SIZE: usize = 5;

/// Platform-agnostic input method configuration.
#[derive(Debug, Clone)]
pub struct InputConfig {
    /// Initial enabled state. After `InputMethodCore::new()`, the core tracks
    /// its own `enabled` flag independently (toggled by the kill switch).
    pub enabled: bool,
    pub ghost_text: bool,
    pub max_candidates: usize,
    pub min_prefix_length: usize,
    pub weights: (f64, f64, f64),
    pub cvm_initial_size: usize,
    pub cvm_max_size: usize,
    pub cvm_decay_lambda: f64,
    pub fuzzy_max_edits: u8,
    pub fuzzy_discounts: [f64; 3],
    pub markov_lambdas: [f64; 3],
    /// Minimum absolute score for the top prediction to show ghost text.
    /// Predictions below this threshold are suppressed.
    pub ghost_text_min_confidence: f64,
    // ── Feature flags (v0.4.0) ──────────────────────────────────
    /// Enable language detection and per-language CVM tracks.
    pub lang_detection: bool,
    /// Enable session cache LM (burstiness boost for repeated words).
    pub use_session_cache_lm: bool,
    /// Enable PPM character-level prediction model.
    pub use_ppm: bool,
    /// Enable BPE-based OOV fallback suggestions.
    pub bpe_enabled: bool,
    /// Enable interpolated Kneser-Ney smoothing (experimental, replaces Katz).
    pub use_kneser_ney: bool,
    /// Enable Hedge/Exp3 adaptive weight mixer (experimental, replaces EMA).
    pub use_hedge: bool,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            ghost_text: true,
            max_candidates: 5,
            min_prefix_length: 2,
            weights: (0.4, 0.4, 0.2),
            cvm_initial_size: 500,
            cvm_max_size: 5000,
            cvm_decay_lambda: 0.001,
            fuzzy_max_edits: 2,
            fuzzy_discounts: [1.0, 0.7, 0.4],
            markov_lambdas: [0.6, 0.3, 0.1],
            ghost_text_min_confidence: 0.3,
            lang_detection: true,
            use_session_cache_lm: true,
            use_ppm: false,
            bpe_enabled: true,
            use_kneser_ney: false,
            use_hedge: false,
        }
    }
}

impl InputConfig {
    /// Parse a JSON config string, applying values over defaults.
    ///
    /// Unknown keys are silently ignored. Missing keys keep their defaults.
    pub fn from_json(json_str: &str) -> Self {
        let mut config = Self::default();
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(json_str) {
            if let Some(b) = v.get("enabled").and_then(|v| v.as_bool()) {
                config.enabled = b;
            }
            if let Some(b) = v.get("ghost_text").and_then(|v| v.as_bool()) {
                config.ghost_text = b;
            }
            if let Some(n) = v.get("max_candidates").and_then(|v| v.as_u64()) {
                config.max_candidates = n as usize;
            }
            if let Some(n) = v.get("min_prefix_length").and_then(|v| v.as_u64()) {
                config.min_prefix_length = n as usize;
            }
            if let Some(f) = v.get("ghost_text_min_confidence").and_then(|v| v.as_f64()) {
                config.ghost_text_min_confidence = f;
            }
            if let Some(w) = v.get("weights").and_then(|v| v.as_object()) {
                let a = w
                    .get("corpus")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(config.weights.0);
                let b = w
                    .get("markov")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(config.weights.1);
                let c = w
                    .get("personal")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(config.weights.2);
                let sum = a + b + c;
                if (sum - 1.0).abs() < 1e-6 {
                    config.weights = (a, b, c);
                } else {
                    eprintln!("smartkey: weights sum to {sum:.3}, expected 1.0 — using defaults");
                }
            }
            if let Some(t) = v.get("tuning").and_then(|v| v.as_object()) {
                if let Some(n) = t.get("cvm_initial_size").and_then(|v| v.as_u64()) {
                    config.cvm_initial_size = n as usize;
                }
                if let Some(n) = t.get("cvm_max_size").and_then(|v| v.as_u64()) {
                    config.cvm_max_size = n as usize;
                }
                if let Some(f) = t.get("cvm_decay_lambda").and_then(|v| v.as_f64()) {
                    config.cvm_decay_lambda = f;
                }
                if let Some(n) = t.get("fuzzy_max_edits").and_then(|v| v.as_u64()) {
                    config.fuzzy_max_edits = n as u8;
                }
                if let Some(arr) = t.get("fuzzy_discounts").and_then(|v| v.as_array()) {
                    if arr.len() == 3 {
                        if let (Some(a), Some(b), Some(c)) =
                            (arr[0].as_f64(), arr[1].as_f64(), arr[2].as_f64())
                        {
                            config.fuzzy_discounts = [a, b, c];
                        }
                    }
                }
                if let Some(arr) = t.get("markov_lambdas").and_then(|v| v.as_array()) {
                    if arr.len() == 3 {
                        if let (Some(a), Some(b), Some(c)) =
                            (arr[0].as_f64(), arr[1].as_f64(), arr[2].as_f64())
                        {
                            config.markov_lambdas = [a, b, c];
                        }
                    }
                }
            }
        }
        config
    }
}

// ======================================================================
// InputMethodCore
// ======================================================================

/// The core state machine shared by all platform adapters.
///
/// Platform code feeds key events via [`handle_key`] and executes the
/// returned [`Action`]s (show ghost text, commit text, etc.).
pub struct InputMethodCore {
    engine: SmartKeyEngine,
    config: InputConfig,
    current_word: String,
    ghost: String,
    context: VecDeque<String>,
    enabled: bool,
    last_predictions: Vec<Prediction>,
    /// Prediction quality and performance metrics (v0.4.0).
    metrics: PredictionMetrics,
    /// Character trigram language detector (v0.4.0).
    lang_detector: LanguageDetector,
    /// Whether transliteration mode is active for the current word (v0.4.1).
    transliteration_active: bool,
}

impl InputMethodCore {
    pub fn new(config: InputConfig) -> Self {
        let enabled = config.enabled;
        Self {
            engine: SmartKeyEngine::from_config(&config),
            lang_detector: LanguageDetector::new(),
            metrics: PredictionMetrics::new(),
            config,
            current_word: String::new(),
            ghost: String::new(),
            context: VecDeque::with_capacity(CONTEXT_SIZE),
            enabled,
            last_predictions: Vec::new(),
            transliteration_active: false,
        }
    }

    // -- corpus loading (delegate to engine) ----------------------------

    pub fn load_word(&mut self, word: &str, freq: u32) {
        self.engine.load_word(word, freq);
    }

    pub fn load_bigram(&mut self, ctx: &str, word: &str, count: u32) {
        self.engine.load_bigram(ctx, word, count);
    }

    pub fn load_trigram(&mut self, w1: &str, w2: &str, word: &str, count: u32) {
        self.engine.load_trigram(w1, w2, word, count);
    }

    // -- corpus file loading ---------------------------------------------

    /// Load a corpus file (JSON or bincode) and feed it into the engine.
    ///
    /// Auto-detects format by extension (`.bin` = bincode, anything else = JSON).
    /// On JSON load, writes a `.bin` cache alongside (best-effort, ignores write errors).
    pub fn load_corpus_file(&mut self, path: &Path) -> Result<(), String> {
        use crate::corpus::Corpus;

        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let corpus = match ext {
            "bin" => {
                let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
                Corpus::from_bincode(&bytes)?
            }
            _ => {
                let json_str = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
                let (corpus, warnings) = Corpus::from_json_with_warnings(&json_str)?;
                if warnings.dropped_bigrams > 0 || warnings.dropped_trigrams > 0 {
                    eprintln!(
                        "smartkey: corpus {}: dropped {} bigrams, {} trigrams (malformed entries)",
                        path.display(),
                        warnings.dropped_bigrams,
                        warnings.dropped_trigrams
                    );
                }
                // Best-effort: write .bin cache alongside.
                let bin_path = path.with_extension("bin");
                if let Ok(bytes) = corpus.to_bincode() {
                    let _ = std::fs::write(&bin_path, bytes);
                }
                corpus
            }
        };
        corpus.load_into_engine(&mut self.engine);
        Ok(())
    }

    // -- key event handling ---------------------------------------------

    /// Process a key event and return actions for the platform to execute.
    pub fn handle_key(&mut self, event: KeyEvent) -> Vec<Action> {
        let is_kill = event.key == Key::Escape && event.modifiers.contains(Modifiers::SUPER);

        // Disabled state — only the kill switch can re-enable.
        if !self.enabled {
            if is_kill {
                self.enabled = true;
                return vec![Action::HideGhost];
            }
            return vec![Action::ForwardKey];
        }

        // Kill switch: disable engine.
        if is_kill {
            self.enabled = false;
            self.reset_word();
            return vec![Action::HideGhost];
        }

        // Ignore key-release events.
        if event.modifiers.contains(Modifiers::RELEASE) {
            return vec![Action::ForwardKey];
        }

        // Ignore events with Ctrl or Alt held.
        if event.modifiers.intersects(Modifiers::CTRL | Modifiers::ALT) {
            return vec![Action::ForwardKey];
        }

        match event.key {
            // Tab: accept entire ghost text.
            Key::Tab => {
                if !self.ghost.is_empty() {
                    let full_word = format!("{}{}", self.current_word, self.ghost);
                    // Commit only the ghost suffix — typed chars were already forwarded.
                    let actions = vec![Action::CommitText(self.ghost.clone()), Action::HideGhost];
                    // Notify engine of accepted prediction for adaptive weight learning.
                    let ctx: Vec<&str> = self.context.iter().map(|s| s.as_str()).collect();
                    self.engine.on_prediction_accepted(&full_word, &ctx);
                    // Record acceptance metrics: find rank in last_predictions.
                    let rank = self
                        .last_predictions
                        .iter()
                        .position(|p| p.word == full_word)
                        .map(|i| i + 1) // 1-based
                        .unwrap_or(1);
                    self.metrics
                        .record_acceptance(rank, self.ghost.len(), full_word.len());
                    self.commit_word_internal(&full_word, false);
                    self.reset_word();
                    actions
                } else {
                    vec![Action::ForwardKey]
                }
            }

            // Right arrow: accept one character of ghost text.
            Key::Right => {
                if !self.ghost.is_empty() {
                    let ch = self.ghost.remove(0);
                    self.current_word.push(ch);
                    if self.ghost.is_empty() {
                        vec![Action::HideGhost]
                    } else {
                        vec![Action::ShowGhost(self.ghost.clone())]
                    }
                } else {
                    vec![Action::ForwardKey]
                }
            }

            // Escape: dismiss ghost text.
            Key::Escape => {
                if !self.ghost.is_empty() {
                    self.ghost.clear();
                    vec![Action::HideGhost]
                } else {
                    vec![Action::ForwardKey]
                }
            }

            // Space / Return: commit current word, delimit.
            Key::Space | Key::Return => {
                if !self.current_word.is_empty() {
                    let word = std::mem::take(&mut self.current_word);
                    self.commit_word_internal(&word, true);
                }
                self.reset_word();
                vec![Action::HideGhost, Action::ForwardKey]
            }

            // Backspace: trim the current word.
            Key::Backspace => {
                if !self.current_word.is_empty() {
                    self.current_word.pop();
                }
                let ghost_action = self.update_predictions();
                vec![ghost_action, Action::ForwardKey]
            }

            // Printable character: detect language, then predict.
            Key::Char(ch) => {
                self.current_word.push(ch);

                // v0.4.1: detection BEFORE prediction (instant classify).
                if self.config.lang_detection {
                    self.lang_detector.feed_char_instant(ch);
                }

                // v0.4.1: transliteration — auto-map Latin to Cyrillic when
                // the user is on the wrong keyboard layout.
                if self.transliteration_active {
                    if let Some(cyrillic) = lang_detect::phonetic_map(ch) {
                        self.current_word.pop(); // Remove the Latin char
                        self.current_word.push(cyrillic);
                        let ghost_action = self.update_predictions();
                        return vec![
                            Action::ReplaceWord {
                                replace_len: 1,
                                text: cyrillic.to_string(),
                            },
                            ghost_action,
                        ];
                    }
                }

                // Check for wrong layout on 2nd+ Latin char.
                if !self.transliteration_active
                    && self.current_word.len() >= 2
                    && self.check_wrong_layout()
                {
                    self.transliteration_active = true;
                    let transliterated = lang_detect::transliterate(&self.current_word);
                    let replace_len = self.current_word.len();
                    self.current_word = transliterated;
                    let ghost_action = self.update_predictions();
                    return vec![
                        Action::ReplaceWord {
                            replace_len,
                            text: self.current_word.clone(),
                        },
                        ghost_action,
                    ];
                }

                let ghost_action = self.update_predictions();
                vec![ghost_action, Action::ForwardKey]
            }

            // Unknown key: pass through.
            Key::Other(_) => vec![Action::ForwardKey],
        }
    }

    // -- focus / reset --------------------------------------------------

    /// Called when the input field loses focus.
    pub fn focus_lost(&mut self) -> Vec<Action> {
        if !self.current_word.is_empty() {
            let word = std::mem::take(&mut self.current_word);
            self.commit_word_internal(&word, true);
        }
        self.reset_word();
        vec![Action::HideGhost]
    }

    /// Called when the input field gains focus.
    pub fn focus_gained(&mut self) {
        // Nothing to do — ready for new input.
    }

    /// Reset internal state (e.g. on engine reset request).
    pub fn reset(&mut self) -> Vec<Action> {
        self.reset_word();
        vec![Action::HideGhost]
    }

    // -- read-only accessors --------------------------------------------

    /// The most recent predictions (from the last `handle_key` call).
    pub fn predictions(&self) -> &[Prediction] {
        &self.last_predictions
    }

    /// Whether the engine is currently enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// The word currently being typed.
    pub fn current_word(&self) -> &str {
        &self.current_word
    }

    /// Whether ghost text is currently displayed.
    pub fn has_ghost(&self) -> bool {
        !self.ghost.is_empty()
    }

    // -- personal profile persistence -----------------------------------

    /// Save the personal profile (CVM + Markov + weights) to the given path.
    pub fn save_personal(&self, path: &Path) -> Result<(), String> {
        let profile = self.engine.export_personal_profile();
        let json = serde_json::to_string_pretty(&profile).map_err(|e| e.to_string())?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(path, json).map_err(|e| e.to_string())
    }

    /// Load the personal profile from the given path.
    ///
    /// Supports both v2 (PersonalProfile) and v1 (bare CvmSnapshot) formats.
    /// Silent no-op if the file does not exist.
    pub fn load_personal(&mut self, path: &Path) -> Result<(), String> {
        if !path.is_file() {
            return Ok(());
        }
        let data = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let profile = personal::load_personal_json(&data)?;
        self.engine.import_personal_profile(&profile);
        Ok(())
    }

    /// Save personal profile to the default path (`~/.config/smartkey/personal.json`).
    pub fn save_personal_default(&self) -> Result<(), String> {
        self.save_personal(&paths::personal_profile_path())
    }

    /// Load personal profile from the default path (no-op if missing).
    pub fn load_personal_default(&mut self) -> Result<(), String> {
        self.load_personal(&paths::personal_profile_path())
    }

    /// Access prediction metrics (read-only).
    pub fn metrics(&self) -> &PredictionMetrics {
        &self.metrics
    }

    /// Access the current detected language.
    pub fn detected_language(&self) -> crate::lang_detect::DetectedLanguage {
        self.lang_detector.detected()
    }

    // -- internal helpers -----------------------------------------------

    /// Shared commit logic. `record_commit_metric` is false for Tab-accepted
    /// words (already recorded via `record_acceptance`).
    fn commit_word_internal(&mut self, word: &str, record_commit_metric: bool) {
        if !word.is_empty() {
            if record_commit_metric {
                self.metrics.record_commit(word.len());
            }

            // Extract context BEFORE pushing new word (for online Markov training).
            let prev1 = self.context.back().map(|s| s.as_str());
            let prev2 = if self.context.len() >= 2 {
                Some(self.context[self.context.len() - 2].as_str())
            } else {
                None
            };

            self.engine.learn(word);
            self.engine.learn_online_markov(prev1, prev2, word);

            // Feed word to per-language CVM track + momentum.
            if self.config.lang_detection {
                let lang = self.lang_detector.detected().lang;
                self.engine.learn_with_lang(word, lang);
                self.lang_detector.record_commit(lang);
            }

            // Feed word to session cache LM for burstiness tracking.
            if self.config.use_session_cache_lm {
                self.engine.observe_session(word);
            }

            if self.context.len() >= CONTEXT_SIZE {
                self.context.pop_front();
            }
            self.context.push_back(word.to_string());
        }
    }

    fn reset_word(&mut self) {
        self.current_word.clear();
        self.ghost.clear();
        self.last_predictions.clear();
        self.transliteration_active = false;
    }

    /// Check if the user is typing on the wrong keyboard layout.
    ///
    /// Compares trie candidate counts for the Latin prefix vs its Cyrillic
    /// transliteration. If Cyrillic has candidates but Latin doesn't (or has
    /// far fewer + momentum agrees), returns true.
    fn check_wrong_layout(&self) -> bool {
        if self.current_word.len() < 2 || !self.config.lang_detection {
            return false;
        }
        // Only check all-ASCII-alpha prefixes.
        if !self.current_word.chars().all(|c| c.is_ascii_alphabetic()) {
            return false;
        }
        let bg_prefix = lang_detect::transliterate(&self.current_word);
        let en_count = self.engine.candidate_count(&self.current_word, 3);
        let bg_count = self.engine.candidate_count(&bg_prefix, 3);
        // Clear signal: BG has candidates, EN doesn't.
        // Require momentum agreement to avoid false positives on English words
        // not in the corpus (e.g., rare proper nouns typed in Latin).
        if bg_count > 0 && en_count == 0 && self.lang_detector.momentum_lang() != Some(LangId::En) {
            return true;
        }
        // Probabilistic: BG much stronger + momentum agrees.
        if bg_count > en_count * 2 {
            if let Some(LangId::Bg) = self.lang_detector.momentum_lang() {
                return true;
            }
        }
        false
    }

    fn update_predictions(&mut self) -> Action {
        // When PPM is enabled, lower min_prefix_length to 1 so predictions
        // start on the first character (PPM guides candidate generation).
        let effective_min = if self.config.use_ppm {
            1
        } else {
            self.config.min_prefix_length
        };
        if self.current_word.chars().count() < effective_min {
            self.ghost.clear();
            self.last_predictions.clear();
            return Action::HideGhost;
        }

        let ctx_refs: arrayvec::ArrayVec<&str, 5> =
            self.context.iter().map(|s| s.as_str()).collect();
        let lang = if self.config.lang_detection {
            Some(self.lang_detector.detected().lang)
        } else {
            None
        };
        let t0 = Instant::now();
        self.last_predictions = self.engine.predict(
            &self.current_word,
            &ctx_refs,
            self.config.max_candidates,
            lang,
        );
        self.metrics.record_latency(t0.elapsed());

        if let Some(top) = self.last_predictions.first() {
            // T8: Suppress ghost text if top score is below confidence threshold.
            if top.score < self.config.ghost_text_min_confidence {
                self.ghost.clear();
                return Action::HideGhost;
            }
            let suffix = top
                .word
                .strip_prefix(self.current_word.as_str())
                .unwrap_or("");
            if !suffix.is_empty() && self.config.ghost_text {
                self.ghost = suffix.to_string();
                return Action::ShowGhost(self.ghost.clone());
            }
        }

        self.ghost.clear();
        Action::HideGhost
    }
}

// ======================================================================
// Tests
// ======================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a test engine with a small vocabulary.
    fn test_core() -> InputMethodCore {
        let mut core = InputMethodCore::new(InputConfig::default());
        core.load_word("hello", 100);
        core.load_word("help", 80);
        core.load_word("hero", 60);
        core.load_word("helicopter", 20);
        core.load_word("world", 90);
        core.load_bigram("i", "hello", 5);
        core.load_bigram("i", "help", 2);
        core
    }

    fn press(key: Key) -> KeyEvent {
        KeyEvent {
            key,
            modifiers: Modifiers::empty(),
        }
    }

    fn press_with(key: Key, mods: Modifiers) -> KeyEvent {
        KeyEvent {
            key,
            modifiers: mods,
        }
    }

    fn has_action(actions: &[Action], target: &Action) -> bool {
        actions.iter().any(|a| a == target)
    }

    fn ghost_text(actions: &[Action]) -> Option<String> {
        actions.iter().find_map(|a| match a {
            Action::ShowGhost(s) => Some(s.clone()),
            _ => None,
        })
    }

    #[test]
    fn test_basic_predict_cycle() {
        let mut core = test_core();

        // Type "h" — below min_prefix_length (2), no ghost.
        let actions = core.handle_key(press(Key::Char('h')));
        assert!(
            has_action(&actions, &Action::HideGhost)
                || ghost_text(&actions).is_none()
                || ghost_text(&actions) == Some(String::new())
        );

        // Type "e" → "he" — now above threshold, should get ghost.
        let actions = core.handle_key(press(Key::Char('e')));
        // "hello" is top by frequency → ghost should be "llo"
        assert!(has_action(&actions, &Action::ForwardKey));

        // Type "l" → "hel" — should get ghost "lo" (from "hello").
        let actions = core.handle_key(press(Key::Char('l')));
        let g = ghost_text(&actions);
        assert!(g.is_some(), "should have ghost text for 'hel'");
        assert_eq!(g.unwrap(), "lo");

        // Tab → commit ghost suffix "lo" (typed "hel" was already forwarded).
        let actions = core.handle_key(press(Key::Tab));
        assert!(has_action(&actions, &Action::CommitText("lo".into())));
        assert!(has_action(&actions, &Action::HideGhost));

        // State should be reset.
        assert_eq!(core.current_word(), "");
    }

    #[test]
    fn test_right_arrow_accepts_one() {
        let mut core = test_core();
        // Type "hel" to get ghost "lo".
        core.handle_key(press(Key::Char('h')));
        core.handle_key(press(Key::Char('e')));
        core.handle_key(press(Key::Char('l')));
        assert_eq!(core.ghost, "lo");

        // Right arrow → accept 'l', ghost becomes "o".
        let actions = core.handle_key(press(Key::Right));
        assert_eq!(ghost_text(&actions), Some("o".into()));
        assert_eq!(core.current_word(), "hell");

        // Right arrow again → accept 'o', ghost gone.
        let actions = core.handle_key(press(Key::Right));
        assert!(has_action(&actions, &Action::HideGhost));
        assert_eq!(core.current_word(), "hello");
    }

    #[test]
    fn test_escape_dismisses() {
        let mut core = test_core();
        core.handle_key(press(Key::Char('h')));
        core.handle_key(press(Key::Char('e')));
        core.handle_key(press(Key::Char('l')));
        assert!(!core.ghost.is_empty());

        let actions = core.handle_key(press(Key::Escape));
        assert!(has_action(&actions, &Action::HideGhost));
        assert!(core.ghost.is_empty());

        // Escape with no ghost → forward.
        let actions = core.handle_key(press(Key::Escape));
        assert!(has_action(&actions, &Action::ForwardKey));
    }

    #[test]
    fn test_space_commits_word() {
        let mut core = test_core();
        core.handle_key(press(Key::Char('h')));
        core.handle_key(press(Key::Char('e')));
        core.handle_key(press(Key::Char('l')));
        core.handle_key(press(Key::Char('l')));
        core.handle_key(press(Key::Char('o')));

        let actions = core.handle_key(press(Key::Space));
        assert!(has_action(&actions, &Action::ForwardKey));
        assert_eq!(core.current_word(), "");
        // "hello" should be in context now.
        assert!(core.context.iter().any(|w| w == "hello"));
    }

    #[test]
    fn test_backspace_trims() {
        let mut core = test_core();
        core.handle_key(press(Key::Char('h')));
        core.handle_key(press(Key::Char('e')));
        core.handle_key(press(Key::Char('l')));
        assert_eq!(core.current_word(), "hel");

        let actions = core.handle_key(press(Key::Backspace));
        assert_eq!(core.current_word(), "he");
        assert!(has_action(&actions, &Action::ForwardKey));
    }

    #[test]
    fn test_kill_switch() {
        let mut core = test_core();
        assert!(core.is_enabled());

        // Super+Escape → disable.
        let actions = core.handle_key(press_with(Key::Escape, Modifiers::SUPER));
        assert!(!core.is_enabled());
        assert!(has_action(&actions, &Action::HideGhost));

        // Normal key while disabled → forward.
        let actions = core.handle_key(press(Key::Char('a')));
        assert!(has_action(&actions, &Action::ForwardKey));
        assert_eq!(core.current_word(), "");

        // Super+Escape again → re-enable (symmetric with disable: returns HideGhost).
        let actions = core.handle_key(press_with(Key::Escape, Modifiers::SUPER));
        assert!(core.is_enabled());
        assert!(has_action(&actions, &Action::HideGhost));
    }

    #[test]
    fn test_context_tracking() {
        let mut core = test_core();

        // Type and commit "i".
        core.handle_key(press(Key::Char('i')));
        core.handle_key(press(Key::Space));
        assert!(core.context.iter().any(|w| w == "i"));

        // Type and commit "hello".
        for ch in "hello".chars() {
            core.handle_key(press(Key::Char(ch)));
        }
        core.handle_key(press(Key::Return));
        assert_eq!(core.context.len(), 2);
        assert_eq!(core.context[0], "i");
        assert_eq!(core.context[1], "hello");
    }

    #[test]
    fn test_min_prefix_length() {
        let mut core = test_core();

        // Single char → no ghost (min_prefix_length = 2).
        let actions = core.handle_key(press(Key::Char('h')));
        assert!(
            !has_action(&actions, &Action::ShowGhost("ello".into())),
            "should not show ghost for single char"
        );
    }

    #[test]
    fn test_release_events_forwarded() {
        let mut core = test_core();
        let actions = core.handle_key(press_with(Key::Char('a'), Modifiers::RELEASE));
        assert!(has_action(&actions, &Action::ForwardKey));
        assert_eq!(core.current_word(), "");
    }

    #[test]
    fn test_ctrl_alt_forwarded() {
        let mut core = test_core();
        let actions = core.handle_key(press_with(Key::Char('c'), Modifiers::CTRL));
        assert!(has_action(&actions, &Action::ForwardKey));
        assert_eq!(core.current_word(), "");
    }

    #[test]
    fn test_focus_lost_commits() {
        let mut core = test_core();
        for ch in "hello".chars() {
            core.handle_key(press(Key::Char(ch)));
        }

        let actions = core.focus_lost();
        assert!(has_action(&actions, &Action::HideGhost));
        assert_eq!(core.current_word(), "");
        assert!(core.context.iter().any(|w| w == "hello"));
    }

    #[test]
    fn test_config_from_json_defaults() {
        let config = InputConfig::from_json("{}");
        let default = InputConfig::default();
        assert_eq!(config.enabled, default.enabled);
        assert_eq!(config.ghost_text, default.ghost_text);
        assert_eq!(config.max_candidates, default.max_candidates);
        assert_eq!(config.min_prefix_length, default.min_prefix_length);
        assert_eq!(config.weights, default.weights);
    }

    #[test]
    fn test_config_from_json_overrides() {
        let json = r#"{"enabled": false, "max_candidates": 10, "weights": {"corpus": 0.5, "markov": 0.3, "personal": 0.2}}"#;
        let config = InputConfig::from_json(json);
        assert!(!config.enabled);
        assert!(config.ghost_text); // kept default
        assert_eq!(config.max_candidates, 10);
        assert_eq!(config.weights, (0.5, 0.3, 0.2));
    }

    #[test]
    fn test_config_from_json_invalid() {
        let config = InputConfig::from_json("not json at all");
        let default = InputConfig::default();
        assert_eq!(config.enabled, default.enabled);
    }

    #[test]
    fn test_config_from_json_tuning_overrides() {
        let json = r#"{
            "tuning": {
                "cvm_initial_size": 1000,
                "cvm_max_size": 10000,
                "cvm_decay_lambda": 0.05,
                "fuzzy_max_edits": 1,
                "fuzzy_discounts": [1.0, 0.5, 0.2],
                "markov_lambdas": [0.5, 0.3, 0.2]
            }
        }"#;
        let config = InputConfig::from_json(json);
        assert_eq!(config.cvm_initial_size, 1000);
        assert_eq!(config.cvm_max_size, 10000);
        assert!((config.cvm_decay_lambda - 0.05).abs() < 1e-9);
        assert_eq!(config.fuzzy_max_edits, 1);
        assert_eq!(config.fuzzy_discounts, [1.0, 0.5, 0.2]);
        assert_eq!(config.markov_lambdas, [0.5, 0.3, 0.2]);
    }

    // ── Personal profile persistence tests ─────────────────────────────

    #[test]
    fn test_personal_save_load_round_trip() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("personal_test.json");

        let mut core = test_core();
        // Learn some words so CVM has state.
        for _ in 0..50 {
            core.engine.learn("persisted");
        }
        core.save_personal(&path).expect("save should succeed");
        assert!(path.is_file(), "profile file should exist");

        let mut core2 = InputMethodCore::new(InputConfig::default());
        core2.load_personal(&path).expect("load should succeed");
    }

    #[test]
    fn test_personal_load_missing_file_is_noop() {
        let path = std::path::PathBuf::from("/tmp/smartkey_nonexistent_profile.json");
        let mut core = test_core();
        let result = core.load_personal(&path);
        assert!(result.is_ok(), "loading missing file should be a no-op");
    }
}
