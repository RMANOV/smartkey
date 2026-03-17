// Platform-agnostic input method state machine.
//
// This module contains the key event dispatcher, ghost text lifecycle,
// word commit logic, and context tracking — shared across Linux (IBus),
// Windows (TSF), and macOS (IMK). Platform adapters call `handle_key()`
// and execute the returned `Action` list.

use std::collections::VecDeque;
use std::path::Path;
use std::time::Instant;

use crate::dual_buffer::{DualBuffer, DualBufferConfig};
use crate::ensemble::{Prediction, SmartKeyEngine};
use crate::eval::PredictionMetrics;
use crate::keymap;
use crate::lang_detect::{self, LangId, LanguageDetector};
use crate::paths;
use crate::personal;
use crate::typing_regime::RegimeDetector;

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
    /// Raw hardware scancode (evdev) for dual-buffer layout-agnostic input.
    /// Routed through the dual buffer when enabled, resolved to `Char` or
    /// a special key otherwise.
    RawCode(u16),
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
    /// Composing preedit: typed prefix (normal color, underlined) +
    /// predicted suffix (grey, underlined). Used during hypothesis phase.
    ShowComposing { typed: String, ghost: String },
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
    // ── Dual buffer (v0.5.0) ─────────────────────────────────────
    /// Enable layout-agnostic dual-buffer input.
    pub dual_buffer: DualBufferConfig,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            ghost_text: true,
            max_candidates: 5,
            min_prefix_length: 2,
            weights: (0.50, 0.30, 0.20), // Previous: (0.4, 0.4, 0.2) — rollback if corpus-heavy weighting hurts one language
            cvm_initial_size: 500,
            cvm_max_size: 5000,
            cvm_decay_lambda: 0.001,
            fuzzy_max_edits: 2,
            fuzzy_discounts: [1.0, 0.7, 0.4],
            markov_lambdas: [0.6, 0.3, 0.1],
            ghost_text_min_confidence: 0.35,
            lang_detection: true,
            use_session_cache_lm: true,
            use_ppm: false,
            bpe_enabled: true,
            use_kneser_ney: true,
            use_hedge: true,
            dual_buffer: DualBufferConfig::default(),
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
            if let Some(db) = v.get("dual_buffer").and_then(|v| v.as_object()) {
                if let Some(b) = db.get("enabled").and_then(|v| v.as_bool()) {
                    config.dual_buffer.enabled = b;
                }
                if let Some(f) = db.get("lock_threshold").and_then(|v| v.as_f64()) {
                    config.dual_buffer.lock_threshold = f;
                }
                if let Some(n) = db.get("min_lock_chars").and_then(|v| v.as_u64()) {
                    config.dual_buffer.min_lock_chars = n as usize;
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
    /// Dual-interpretation buffer for layout-agnostic input (v0.5.0).
    /// `Some` when active (after first `Key::RawCode` in a word), `None` otherwise.
    dual_buffer: Option<DualBuffer>,
    /// Typing regime detector (v0.5+) — gates tech vocab δ weight in the ensemble.
    regime_detector: RegimeDetector,
    /// True when the current word triggered a dual-buffer layout flip (for regime detection).
    current_word_had_flip: bool,
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
            dual_buffer: None,
            regime_detector: RegimeDetector::new(),
            current_word_had_flip: false,
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

        // ── Resolve RawCode (v0.5.0) ─────────────────────────────────
        // Convert hardware scancode to either a dual-buffer operation or
        // a regular Key before entering the main match.
        let event = if let Key::RawCode(code) = event.key {
            let shift = event.modifiers.contains(Modifiers::SHIFT);
            // Special keys (Tab, Space, etc.) → rewrite to their Key variant.
            if let Some(special) = keymap::scancode_to_special(code) {
                let key = match special {
                    keymap::SpecialKey::Tab => Key::Tab,
                    keymap::SpecialKey::Escape => Key::Escape,
                    keymap::SpecialKey::Backspace => Key::Backspace,
                    keymap::SpecialKey::Return => Key::Return,
                    keymap::SpecialKey::Space => Key::Space,
                    keymap::SpecialKey::Right => Key::Right,
                };
                KeyEvent {
                    key,
                    modifiers: event.modifiers,
                }
            } else if let Some((en_ch, bg_ch)) = keymap::scancode_to_both(code, shift) {
                if en_ch != bg_ch && self.config.dual_buffer.enabled {
                    // Dual-interpretable character → handle via dual buffer.
                    return self.handle_dual_buffer_key(en_ch, bg_ch);
                }
                // Same char on both layouts → treat as regular Char.
                KeyEvent {
                    key: Key::Char(en_ch),
                    modifiers: event.modifiers,
                }
            } else {
                return vec![Action::ForwardKey];
            }
        } else {
            event
        };

        match event.key {
            // Tab: accept entire ghost text.
            Key::Tab => {
                if !self.ghost.is_empty() {
                    let full_word = format!("{}{}", self.current_word, self.ghost);
                    // In hypothesis phase: prefix is in preedit, commit everything.
                    // In locked/no-dual: prefix already in app, commit suffix only.
                    let in_hypothesis = self.in_hypothesis_phase();
                    let commit_text = if in_hypothesis {
                        full_word.clone()
                    } else {
                        self.ghost.clone()
                    };
                    let actions = vec![Action::HideGhost, Action::CommitText(commit_text)];
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
                } else if self.in_hypothesis_phase() && !self.current_word.is_empty() {
                    // Hypothesis phase, no ghost: commit preedit prefix + forward Tab.
                    let prefix = self.current_word.clone();
                    self.commit_word_internal(&prefix, true);
                    self.reset_word();
                    vec![
                        Action::HideGhost,
                        Action::CommitText(prefix),
                        Action::ForwardKey,
                    ]
                } else {
                    vec![Action::ForwardKey]
                }
            }

            // Right arrow: accept one character of ghost text.
            Key::Right => {
                if !self.ghost.is_empty() {
                    let ch = self.ghost.remove(0);
                    self.current_word.push(ch);
                    if self.in_hypothesis_phase() {
                        // Hypothesis phase: everything stays in preedit.
                        vec![Action::ShowComposing {
                            typed: self.current_word.clone(),
                            ghost: self.ghost.clone(),
                        }]
                    } else if self.ghost.is_empty() {
                        vec![Action::HideGhost]
                    } else {
                        vec![Action::ShowGhost(self.ghost.clone())]
                    }
                } else {
                    vec![Action::ForwardKey]
                }
            }

            // Escape: dismiss ghost text and notify engine of rejection.
            Key::Escape => {
                if self.in_hypothesis_phase() {
                    // Hypothesis phase: cancel preedit, nothing was committed.
                    self.reset_word();
                    vec![Action::HideGhost]
                } else if !self.ghost.is_empty() {
                    self.engine.on_prediction_rejected();
                    self.ghost.clear();
                    vec![Action::HideGhost]
                } else {
                    vec![Action::ForwardKey]
                }
            }

            // Space / Return: commit current word, delimit.
            Key::Space | Key::Return => {
                if let Some(ref db) = self.dual_buffer {
                    if !db.is_locked() && !self.current_word.is_empty() {
                        // Hypothesis phase: commit preedit prefix before forwarding.
                        let prefix = self.current_word.clone();
                        self.commit_word_internal(&prefix, true);
                        self.reset_word();
                        return vec![
                            Action::HideGhost,
                            Action::CommitText(prefix),
                            Action::ForwardKey,
                        ];
                    }
                }
                if !self.current_word.is_empty() {
                    let word = std::mem::take(&mut self.current_word);
                    self.commit_word_internal(&word, true);
                }
                self.reset_word();
                vec![Action::HideGhost, Action::ForwardKey]
            }

            // Backspace: trim the current word.
            Key::Backspace => {
                if let Some(ref mut db) = self.dual_buffer {
                    if !db.is_empty() {
                        let was_locked = db.is_locked();
                        db.pop();
                        if db.is_empty() {
                            self.dual_buffer = None;
                            self.current_word.clear();
                            self.ghost.clear();
                            if was_locked {
                                // Committed chars in app: forward backspace to delete last one.
                                return vec![Action::HideGhost, Action::ForwardKey];
                            } else {
                                // Hypothesis: nothing in app, just clear preedit.
                                return vec![Action::HideGhost];
                            }
                        }
                        if !was_locked {
                            // Hypothesis: re-score, update preedit.
                            let (ef, bf) = self.engine.score_both(db.en_text(), db.bg_text());
                            db.update_scores(ef, bf);
                            self.current_word = db.winner_text().to_string();
                            let ghost = self.compute_ghost_suffix();
                            return vec![Action::ShowComposing {
                                typed: self.current_word.clone(),
                                ghost,
                            }];
                        }
                        // Locked: forward backspace to app + update ghost.
                        self.current_word = db.winner_text().to_string();
                        let ghost_action = self.update_predictions();
                        return vec![Action::HideGhost, Action::ForwardKey, ghost_action];
                    }
                }
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
                        // The Latin key was consumed by SmartKey (not forwarded
                        // to the app), so just commit the Cyrillic char directly
                        // — no delete/replace needed.
                        return vec![Action::CommitText(cyrillic.to_string()), ghost_action];
                    }
                }

                // Check for wrong layout on 2nd+ Latin char.
                if !self.transliteration_active
                    && self.current_word.len() >= 2
                    && self.check_wrong_layout()
                {
                    self.transliteration_active = true;
                    let transliterated = lang_detect::transliterate(&self.current_word);
                    // Only previously-forwarded chars are in the application text.
                    // The current char that triggered detection was consumed (not forwarded),
                    // so subtract 1.
                    let replace_len = self.current_word.len() - 1;
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

            // RawCode is resolved before this match — unreachable.
            Key::RawCode(_) => unreachable!("RawCode resolved before match"),
            // Unknown key: pass through.
            Key::Other(_) => vec![Action::ForwardKey],
        }
    }

    /// Whether the dual buffer is active but not yet locked (hypothesis phase).
    fn in_hypothesis_phase(&self) -> bool {
        self.dual_buffer.as_ref().is_some_and(|db| !db.is_locked())
    }

    // -- focus / reset --------------------------------------------------

    /// Called when the input field loses focus.
    pub fn focus_lost(&mut self) -> Vec<Action> {
        let mut actions = Vec::new();
        // If in hypothesis phase, commit the preedit prefix to the app.
        if let Some(ref db) = self.dual_buffer {
            if !db.is_locked() && !self.current_word.is_empty() {
                actions.push(Action::CommitText(self.current_word.clone()));
            }
        }
        if !self.current_word.is_empty() {
            let word = std::mem::take(&mut self.current_word);
            self.commit_word_internal(&word, true);
        }
        self.reset_word();
        actions.push(Action::HideGhost);
        actions
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

    /// The current ghost text (completion suffix), or empty if none.
    pub fn ghost_text(&self) -> &str {
        &self.ghost
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
            let lang = if self.config.lang_detection {
                let l = self.lang_detector.detected().lang;
                self.engine.learn_with_lang(word, l);
                self.lang_detector.record_commit(l);
                l
            } else {
                crate::lang_detect::LangId::En // Default when lang detection disabled
            };

            // Feed word to session cache LM for burstiness tracking.
            if self.config.use_session_cache_lm {
                self.engine.observe_session(word);
            }

            // Feed word stats to regime detector (v0.5+).
            // is_oov: word not found in the trie (zero prefix-search candidates).
            let is_oov = self.engine.candidate_count(word, 1) == 0;
            let is_flip = self.current_word_had_flip;
            self.regime_detector
                .observe_word(word.chars().count(), is_oov, is_flip, lang);
            // Propagate the current regime to the engine so predict() can gate δ.
            let (regime, _conf) = self.regime_detector.regime();
            self.engine.set_regime(regime);

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
        self.dual_buffer = None;
        self.current_word_had_flip = false;
    }

    // ── Dual buffer key handling (v0.5.0) ────────────────────────────

    /// Handle a key that has two different interpretations (EN vs BG).
    ///
    /// The dual buffer scores both interpretations against the corpus and
    /// commits the winner character. On confidence flip, emits `ReplaceWord`
    /// to fix previously committed characters.
    fn handle_dual_buffer_key(&mut self, en_ch: char, bg_ch: char) -> Vec<Action> {
        // Initialize dual buffer on first dual-interpretable key in this word.
        if self.dual_buffer.is_none() {
            self.dual_buffer = Some(DualBuffer::from_config(&self.config.dual_buffer));
        }

        let db = self.dual_buffer.as_mut().unwrap();
        let was_locked = db.is_locked();
        db.push(en_ch, bg_ch);

        // Score both interpretations (skip if already locked — perf win).
        if !was_locked {
            let (en_freq, bg_freq) = self.engine.score_both(db.en_text(), db.bg_text());
            db.update_scores(en_freq, bg_freq);
        }

        self.current_word = db.winner_text().to_string();

        // Compute winner char once — reused for lang detection and locked-phase commit.
        let winner_char = match db.winner_lang() {
            LangId::En => en_ch,
            LangId::Bg | LangId::Tech => bg_ch,
        };

        // Feed language detection.
        if self.config.lang_detection {
            self.lang_detector.feed_char_instant(winner_char);
            if !was_locked {
                self.lang_detector
                    .feed_dual_result(db.winner_lang(), db.confidence());
            }
        }

        if !db.is_locked() {
            // ═══ HYPOTHESIS PHASE ═══
            // Nothing committed. Everything in preedit.
            let ghost_suffix = self.compute_ghost_suffix();
            return vec![Action::ShowComposing {
                typed: self.current_word.clone(),
                ghost: ghost_suffix,
            }];
        }

        if !was_locked {
            // ═══ JUST LOCKED (transition) ═══
            // Commit entire accumulated prefix at once.
            let ghost_action = self.update_predictions();
            return vec![
                Action::HideGhost,
                Action::CommitText(self.current_word.clone()),
                ghost_action,
            ];
        }

        // ═══ LOCKED PHASE ═══
        // One char at a time, language already decided.
        let ghost_action = self.update_predictions();
        vec![
            Action::HideGhost,
            Action::CommitText(winner_char.to_string()),
            ghost_action,
        ]
    }

    /// Check if the user is typing on the wrong keyboard layout.
    ///
    /// Compares the **corpus frequency of the top trie candidate** for the
    /// Latin prefix vs its Cyrillic transliteration. Frequency-based
    /// comparison is far more discriminative than candidate counts (which
    /// saturate at small limits with 100K+ word corpora).
    fn check_wrong_layout(&self) -> bool {
        if self.current_word.len() < 2 || !self.config.lang_detection {
            return false;
        }
        // Only check all-ASCII-alpha prefixes.
        if !self.current_word.chars().all(|c| c.is_ascii_alphabetic()) {
            return false;
        }

        let bg_prefix = lang_detect::transliterate(&self.current_word);
        let en_freq = self.engine.max_prefix_frequency(&self.current_word);
        let bg_freq = self.engine.max_prefix_frequency(&bg_prefix);

        let ratio = if en_freq > 0 {
            bg_freq as f64 / en_freq as f64
        } else if bg_freq > 0 {
            f64::INFINITY
        } else {
            0.0
        };

        #[cfg(debug_assertions)]
        eprintln!(
            "smartkey: check_wrong_layout: '{}' → '{}' | en_freq={} bg_freq={} ratio={:.1} momentum={:?}",
            self.current_word,
            bg_prefix,
            en_freq,
            bg_freq,
            ratio,
            self.lang_detector.momentum_lang(),
        );

        // Guard: if momentum is clearly English, never trigger.
        if self.lang_detector.momentum_lang() == Some(LangId::En) {
            return false;
        }

        // Trigger when BG frequency dominates EN by ≥50x (or EN has zero).
        // Momentum guard above already blocked EN momentum; here we allow
        // None (cold start) and Some(Bg) — that's what `!= Some(En)` achieves.
        // Conservative: "ok" (4.8x), "vs" (28x), "da" (32x) don't trigger.
        // Clear wrong-layout: "zd" (1351x), "mn" (550x), "ka" (224x) do.
        if bg_freq > 0 && ratio >= 50.0 {
            return true;
        }

        false
    }

    /// Compute predictions and return ghost suffix string (empty if none).
    /// Sets `self.ghost` and `self.last_predictions` as side effects.
    fn compute_ghost_suffix(&mut self) -> String {
        let effective_min = if self.config.use_ppm {
            1
        } else {
            self.config.min_prefix_length
        };
        if self.current_word.chars().count() < effective_min {
            self.ghost.clear();
            self.last_predictions.clear();
            return String::new();
        }

        let ctx_refs: arrayvec::ArrayVec<&str, 5> =
            self.context.iter().map(|s| s.as_str()).collect();
        let lang = if self.config.lang_detection {
            let det = self.lang_detector.detected();
            // Confidence gate: don't pass language hint when detection is uncertain.
            // This prevents LANG_CVM_BOOST from amplifying wrong-language vocabulary.
            if det.confidence >= 0.6 {
                Some(det.lang)
            } else {
                None
            }
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
            if top.score >= self.config.ghost_text_min_confidence {
                if let Some(suffix) = top.word.strip_prefix(self.current_word.as_str()) {
                    if !suffix.is_empty() && self.config.ghost_text {
                        let s = suffix.to_string();
                        self.ghost.clone_from(&s);
                        return s;
                    }
                }
            }
        }

        self.ghost.clear();
        String::new()
    }

    fn update_predictions(&mut self) -> Action {
        let suffix = self.compute_ghost_suffix();
        if suffix.is_empty() {
            Action::HideGhost
        } else {
            Action::ShowGhost(suffix)
        }
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

    // ── Wrong-layout detection tests (v0.4.3) ────────────────────────

    /// Build a core with both EN and BG vocabulary for layout detection tests.
    fn test_core_bilingual() -> InputMethodCore {
        let mut core = InputMethodCore::new(InputConfig::default());
        // EN words with realistic frequencies.
        core.load_word("hello", 4_897_788);
        core.load_word("help", 2_000_000);
        core.load_word("he", 4_897_788);
        core.load_word("we", 3_467_368);
        core.load_word("ok", 138_038);
        core.load_word("zdnet", 120); // weak EN word starting with "zd"
        core.load_word("mn", 5_370);
        // BG words (Cyrillic) — phonetic transliteration targets.
        // "zd" → "зд" (здравей)
        core.load_word("здравей", 162_181);
        core.load_word("здравейте", 100_000);
        // "mn" → "мн" (много)
        core.load_word("много", 2_951_209);
        // "ka" → "ка" (като)
        core.load_word("като", 5_248_074);
        // "he" → "хе" (хей) — weak BG
        core.load_word("хей", 43_651);
        core
    }

    #[test]
    fn test_wrong_layout_clear_signal() {
        // "zd" — EN top freq=120 (zdnet), BG top freq=162181 (здравей).
        // Ratio=1351x + BG momentum → should trigger.
        let mut core = test_core_bilingual();
        core.lang_detector.record_commit(LangId::Bg);
        core.lang_detector.record_commit(LangId::Bg);
        core.lang_detector.record_commit(LangId::Bg);
        core.current_word = "zd".into();
        assert!(
            core.check_wrong_layout(),
            "'zd' should trigger wrong-layout (ratio=1351x, momentum=Bg)"
        );
    }

    #[test]
    fn test_wrong_layout_cold_start_triggers() {
        // "zd" — ratio=1351x, momentum=None (cold start).
        // With != Some(En), None momentum allows triggering for extreme ratios.
        let mut core = test_core_bilingual();
        core.current_word = "zd".into();
        assert!(
            core.check_wrong_layout(),
            "'zd' at cold start (momentum=None) should trigger (ratio=1351x)"
        );
    }

    #[test]
    fn test_wrong_layout_english_word_safe() {
        // "he" — strong EN (4.8M), weak BG (43K). Should NOT trigger.
        let mut core = test_core_bilingual();
        core.current_word = "he".into();
        assert!(
            !core.check_wrong_layout(),
            "'he' should NOT trigger wrong-layout (strong EN word)"
        );
    }

    #[test]
    fn test_wrong_layout_with_momentum_bg() {
        // "mn" — EN freq=5370, BG freq=2.9M. Ratio=549x.
        // With momentum=Bg and ratio≥50, should trigger.
        let mut core = test_core_bilingual();
        // Set BG momentum (3 commits = 100% Bg → momentum_lang() = Some(Bg)).
        core.lang_detector.record_commit(LangId::Bg);
        core.lang_detector.record_commit(LangId::Bg);
        core.lang_detector.record_commit(LangId::Bg);
        core.current_word = "mn".into();
        assert!(
            core.check_wrong_layout(),
            "'mn' with BG momentum should trigger (ratio ≈ 549x)"
        );
    }

    #[test]
    fn test_wrong_layout_momentum_en_blocks() {
        // "mn" — would normally trigger on ratio, but EN momentum blocks it.
        let mut core = test_core_bilingual();
        core.lang_detector.record_commit(LangId::En);
        core.lang_detector.record_commit(LangId::En);
        core.lang_detector.record_commit(LangId::En);
        core.current_word = "mn".into();
        assert!(
            !core.check_wrong_layout(),
            "'mn' with EN momentum should NOT trigger (guard blocks)"
        );
    }

    #[test]
    fn test_no_trigger_without_corpus() {
        // Empty engine — no words loaded. Should never trigger.
        let mut core = InputMethodCore::new(InputConfig::default());
        core.current_word = "zd".into();
        assert!(
            !core.check_wrong_layout(),
            "empty corpus should never trigger wrong-layout"
        );
    }

    // ── Dual buffer tests (v0.5.0) ────────────────────────────────

    fn press_raw(code: u16) -> KeyEvent {
        KeyEvent {
            key: Key::RawCode(code),
            modifiers: Modifiers::empty(),
        }
    }

    /// Build a bilingual core for dual buffer tests.
    fn test_core_dual() -> InputMethodCore {
        let core = test_core_bilingual();
        // Ensure dual buffer is enabled.
        assert!(core.config.dual_buffer.enabled);
        core
    }

    #[test]
    fn test_dual_buffer_en_wins() {
        // Type "he" via scancodes — strong EN word "he" (4.8M) vs weak BG "хе" (43K).
        // With min_lock_chars=3, both chars stay in hypothesis phase (ShowComposing).
        let mut core = test_core_dual();
        // Scancode 35 = 'h'/'х', 18 = 'e'/'е' (evdev)
        let actions = core.handle_key(press_raw(35));
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::ShowComposing { .. })),
            "first char should produce ShowComposing (hypothesis phase)"
        );

        let actions = core.handle_key(press_raw(18));
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::ShowComposing { .. })),
            "second char should produce ShowComposing (hypothesis phase)"
        );
        // EN should win — current_word should be "he".
        assert_eq!(core.current_word(), "he");
    }

    #[test]
    fn test_dual_buffer_bg_wins() {
        // Type "zd" via scancodes — weak EN "zd" (120) vs strong BG "зд" (162K).
        // With min_lock_chars=3, both chars stay in hypothesis phase (ShowComposing).
        let mut core = test_core_dual();
        // Scancode 44 = 'z'/'з', 32 = 'd'/'д' (evdev)
        core.handle_key(press_raw(44));
        let actions = core.handle_key(press_raw(32));
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::ShowComposing { .. })),
            "second char should produce ShowComposing (hypothesis phase)"
        );
        // BG should win — current_word should be Cyrillic "зд".
        assert_eq!(core.current_word(), "зд");
    }

    #[test]
    fn test_dual_buffer_special_keys_resolve() {
        // Tab via RawCode (scancode 15) should be resolved to Key::Tab.
        let mut core = test_core_dual();
        // First type something to have a ghost.
        core.handle_key(press_raw(35)); // h
        core.handle_key(press_raw(18)); // e
        core.handle_key(press_raw(38)); // l
                                        // Now press Tab via RawCode.
        let actions = core.handle_key(press_raw(15));
        // Should either commit ghost or forward (depending on ghost presence).
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::CommitText(_) | Action::ForwardKey)),
            "Tab via RawCode should work like regular Tab"
        );
    }

    #[test]
    fn test_dual_buffer_space_clears() {
        let mut core = test_core_dual();
        core.handle_key(press_raw(35)); // h
        core.handle_key(press_raw(18)); // e
                                        // Space via RawCode (scancode 57) commits word and clears dual buffer.
        core.handle_key(press_raw(57));
        assert_eq!(core.current_word(), "");
        assert!(core.dual_buffer.is_none());
    }

    #[test]
    fn test_dual_buffer_numbers_passthrough() {
        // Number keys produce same char on both layouts → should go through
        // normal Key::Char path, not dual buffer.
        let mut core = test_core_dual();
        let actions = core.handle_key(press_raw(2)); // '1' (evdev)
                                                     // Should forward (no dual buffer involvement for identical chars).
        assert!(
            actions.iter().any(|a| matches!(a, Action::ForwardKey)),
            "number key should be forwarded normally"
        );
        assert!(core.dual_buffer.is_none());
    }

    #[test]
    fn test_dual_buffer_backspace() {
        let mut core = test_core_dual();
        core.handle_key(press_raw(35)); // h
        core.handle_key(press_raw(18)); // e
        assert_eq!(core.current_word().len(), 2);

        // Backspace via RawCode (evdev 14).
        core.handle_key(press_raw(14));
        assert_eq!(core.current_word().len(), 1);

        // Backspace again — clears dual buffer.
        core.handle_key(press_raw(14));
        assert_eq!(core.current_word(), "");
        assert!(core.dual_buffer.is_none());
    }

    #[test]
    fn test_dual_buffer_disabled() {
        let mut config = InputConfig::default();
        config.dual_buffer.enabled = false;
        let mut core = InputMethodCore::new(config);
        core.load_word("hello", 100);

        // RawCode with dual buffer disabled → should resolve to normal Char.
        let actions = core.handle_key(press_raw(35)); // h → resolved to Key::Char('h')
        assert!(
            actions.iter().any(|a| matches!(a, Action::ForwardKey)),
            "with dual buffer disabled, should forward key normally"
        );
        assert!(core.dual_buffer.is_none());
        assert_eq!(core.current_word(), "h");
    }

    #[test]
    fn test_dual_buffer_config_from_json() {
        let json = r#"{
            "dual_buffer": {
                "enabled": true,
                "lock_threshold": 0.8,
                "min_lock_chars": 3
            }
        }"#;
        let config = InputConfig::from_json(json);
        assert!(config.dual_buffer.enabled);
        assert!((config.dual_buffer.lock_threshold - 0.8).abs() < 1e-9);
        assert_eq!(config.dual_buffer.min_lock_chars, 3);
    }
}
