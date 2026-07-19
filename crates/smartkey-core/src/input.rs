// Platform-agnostic input method state machine.
//
// This module contains the key event dispatcher, ghost text lifecycle,
// word commit logic, and context tracking — shared across Linux (IBus),
// Windows (TSF), and macOS (IMK). Platform adapters call `handle_key()`
// and execute the returned `Action` list.

use std::collections::VecDeque;
use std::path::Path;
use std::time::Instant;

use crate::caps::{CapsEngine, CapsRegime};
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
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
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

const CONTEXT_SIZE: usize = 7;

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
    /// Minimum gap between top and runner-up confidence for ghost text.
    /// If the margin is smaller, the prediction is too ambiguous to show.
    /// Default: 0.10 (top must beat runner-up by 10 percentage points).
    pub ghost_text_separation_margin: f64,
    /// Minimum absolute score for the top prediction to show ghost text.
    /// Disabled when 0.0 (default). Set > 0 to reject low-quality predictions
    /// even when they have high confidence (e.g. single garbage candidate).
    pub ghost_text_min_score: f64,
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
    /// Enable neural reranker for nonlinear feature interactions (Phase 3).
    pub use_reranker: bool,
    /// Allow post-commit language/capitalization rewrites of text that the
    /// user already typed.  Off by default: raw input is authoritative, and
    /// predictions may be accepted only through the explicit accept path.
    pub post_commit_autocorrect: bool,
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
            ghost_text_min_confidence: 0.20,
            ghost_text_separation_margin: 0.05,
            ghost_text_min_score: 0.0,
            lang_detection: true,
            use_session_cache_lm: true,
            use_ppm: true,
            bpe_enabled: true,
            use_kneser_ney: true,
            use_hedge: true,
            use_reranker: true,
            post_commit_autocorrect: false,
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
            if let Some(b) = v.get("post_commit_autocorrect").and_then(|v| v.as_bool()) {
                config.post_commit_autocorrect = b;
            }
            if let Some(n) = v.get("max_candidates").and_then(|v| v.as_u64()) {
                config.max_candidates = (n as usize).max(1);
            }
            if let Some(n) = v.get("min_prefix_length").and_then(|v| v.as_u64()) {
                config.min_prefix_length = n as usize;
            }
            if let Some(f) = v.get("ghost_text_min_confidence").and_then(|v| v.as_f64()) {
                config.ghost_text_min_confidence = f;
            }
            if let Some(f) = v
                .get("ghost_text_separation_margin")
                .and_then(|v| v.as_f64())
            {
                config.ghost_text_separation_margin = f;
            }
            if let Some(f) = v.get("ghost_text_min_score").and_then(|v| v.as_f64()) {
                config.ghost_text_min_score = f;
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
                    log::warn!("smartkey: weights sum to {sum:.3}, expected 1.0 — using defaults");
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
                    config.fuzzy_max_edits = n.min(3) as u8;
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
        } else {
            log::warn!("smartkey: invalid config JSON, using defaults");
        }
        config
    }

    /// Parse and validate a JSON config string.
    ///
    /// Unlike [`from_json`], this method returns `Err` on JSON parse failure
    /// and validates field values for logical correctness.
    pub fn try_from_json(json_str: &str) -> Result<Self, String> {
        let v = serde_json::from_str::<serde_json::Value>(json_str)
            .map_err(|e| format!("smartkey: invalid config JSON: {e}"))?;

        let mut config = Self::default();

        if let Some(b) = v.get("enabled").and_then(|v| v.as_bool()) {
            config.enabled = b;
        }
        if let Some(b) = v.get("ghost_text").and_then(|v| v.as_bool()) {
            config.ghost_text = b;
        }
        if let Some(b) = v.get("post_commit_autocorrect").and_then(|v| v.as_bool()) {
            config.post_commit_autocorrect = b;
        }
        if let Some(n) = v.get("max_candidates").and_then(|v| v.as_u64()) {
            if n < 1 {
                return Err("smartkey: max_candidates must be >= 1".to_string());
            }
            config.max_candidates = n as usize;
        }
        if let Some(n) = v.get("min_prefix_length").and_then(|v| v.as_u64()) {
            if n < 1 {
                return Err("smartkey: min_prefix_length must be >= 1".to_string());
            }
            config.min_prefix_length = n as usize;
        }
        if let Some(f) = v.get("ghost_text_min_confidence").and_then(|v| v.as_f64()) {
            if !(0.0..=1.0).contains(&f) {
                return Err(format!(
                    "smartkey: ghost_text_min_confidence must be in [0.0, 1.0], got {f}"
                ));
            }
            config.ghost_text_min_confidence = f;
        }
        if let Some(f) = v
            .get("ghost_text_separation_margin")
            .and_then(|v| v.as_f64())
        {
            if f < 0.0 {
                return Err(format!(
                    "smartkey: ghost_text_separation_margin must be >= 0.0, got {f}"
                ));
            }
            config.ghost_text_separation_margin = f;
        }
        if let Some(f) = v.get("ghost_text_min_score").and_then(|v| v.as_f64()) {
            if f < 0.0 {
                return Err(format!(
                    "smartkey: ghost_text_min_score must be >= 0.0, got {f}"
                ));
            }
            config.ghost_text_min_score = f;
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
            if (sum - 1.0).abs() >= 1e-6 {
                return Err(format!("smartkey: weights sum to {sum:.6}, expected 1.0"));
            }
            config.weights = (a, b, c);
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

        Ok(config)
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
    /// Smart caps engine — detects capitalization regime and applies to predictions.
    caps_engine: CapsEngine,
    /// Hints injected by MasterLoop (v0.6: proactive anticipation).
    active_hints: Option<crate::master_loop::Hints>,
    /// True when an anticipatory (next-word) ghost is showing, not a prefix ghost.
    /// Used to distinguish Tab-accept behavior.
    anticipatory_ghost_active: bool,
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
            caps_engine: CapsEngine::new(),
            active_hints: None,
            anticipatory_ghost_active: false,
        }
    }

    /// Apply hints from MasterLoop to bias predictions.
    pub fn apply_hints(&mut self, hints: &crate::master_loop::Hints) {
        if let Some(lang) = hints.lang_prior {
            self.lang_detector.set_prior(lang);
        }
        self.active_hints = Some(hints.clone());
    }

    /// Clear any active hints (on word boundary).
    pub fn clear_hints(&mut self) {
        self.active_hints = None;
        self.lang_detector.clear_prior();
    }

    // -- passive calibration tap (delegates to the engine) --------------

    pub fn o1_ngram_snapshot(
        &self,
        ctx: &str,
        uni_top3_cache: &[String],
    ) -> (Vec<String>, crate::o1_shim::O1Numbers) {
        self.engine.o1_ngram_snapshot(ctx, uni_top3_cache)
    }

    pub fn o1_global_unigram_top3(&self) -> Vec<String> {
        self.engine.o1_global_unigram_top3()
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

    pub fn load_word_lang(&mut self, word: &str, freq: u32, lang: LangId) {
        self.engine.load_word_lang(word, freq, lang);
    }

    pub fn load_bigram_lang(&mut self, ctx: &str, word: &str, count: u32, lang: LangId) {
        self.engine.load_bigram_lang(ctx, word, count, lang);
    }

    pub fn load_trigram_lang(&mut self, w1: &str, w2: &str, word: &str, count: u32, lang: LangId) {
        self.engine.load_trigram_lang(w1, w2, word, count, lang);
    }

    // -- corpus file loading ---------------------------------------------

    /// Extract language from corpus filename: `corpus_en.json` → `Some(En)`.
    fn lang_from_filename(path: &Path) -> Option<LangId> {
        let stem = path.file_stem()?.to_str()?;
        if stem.ends_with("_en") {
            Some(LangId::En)
        } else if stem.ends_with("_bg") {
            Some(LangId::Bg)
        } else if stem.ends_with("_tech") {
            Some(LangId::Tech)
        } else {
            None
        }
    }

    /// Load a corpus file (JSON or bincode) and feed it into the engine.
    ///
    /// Auto-detects format by extension (`.bin` = bincode, anything else = JSON).
    /// Extracts language from filename (e.g. `corpus_en.json` → English model).
    /// On JSON load, writes a `.bin` cache alongside (best-effort, ignores write errors).
    pub fn load_corpus_file(&mut self, path: &Path) -> Result<(), String> {
        use crate::corpus::Corpus;

        let lang = Self::lang_from_filename(path);
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

        let (corpus, json_str) = match ext {
            "bin" => {
                let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
                (Corpus::from_bincode(&bytes)?, None)
            }
            _ => {
                let data = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
                let (corpus, warnings) = Corpus::from_json_with_warnings(&data)?;
                if warnings.dropped_bigrams > 0 || warnings.dropped_trigrams > 0 {
                    log::warn!(
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
                (corpus, Some(data))
            }
        };

        // Load proper nouns into caps engine (from JSON only).
        if let Some(ref json) = json_str {
            for noun in Corpus::parse_proper_nouns(json) {
                self.caps_engine.register_proper_noun(&noun);
            }
        }

        // Route corpus data to the correct language model.
        if let Some(lang) = lang {
            corpus.load_into_engine_lang(&mut self.engine, lang);
        } else {
            corpus.load_into_engine(&mut self.engine);
        }
        // Auto-tune Markov interpolation weights from corpus statistics (Phase 2).
        self.engine.compute_adaptive_lambdas();
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
                    keymap::SpecialKey::Left => Key::Left,
                    keymap::SpecialKey::Right => Key::Right,
                    keymap::SpecialKey::Up => Key::Up,
                    keymap::SpecialKey::Down => Key::Down,
                    keymap::SpecialKey::Home => Key::Home,
                    keymap::SpecialKey::End => Key::End,
                    keymap::SpecialKey::PageUp => Key::PageUp,
                    keymap::SpecialKey::PageDown => Key::PageDown,
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
                    self.accept_ghost_completion()
                } else if self.dual_buffer.is_some() && !self.current_word.is_empty() {
                    // Composing, no ghost: the prefix lives only in the preedit,
                    // so commit it before forwarding Tab — otherwise a locked
                    // dual buffer would forward Tab and abandon the typed word.
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
                        vec![Action::CommitText(ch.to_string()), Action::HideGhost]
                    } else {
                        vec![
                            Action::CommitText(ch.to_string()),
                            Action::ShowGhost(self.ghost.clone()),
                        ]
                    }
                } else {
                    let mut actions = self.cursor_moved();
                    actions.push(Action::ForwardKey);
                    actions
                }
            }

            Key::Left
            | Key::Up
            | Key::Down
            | Key::Home
            | Key::End
            | Key::PageUp
            | Key::PageDown => {
                let mut actions = self.cursor_moved();
                actions.push(Action::ForwardKey);
                actions
            }

            // Escape: dismiss ghost text and notify engine of rejection.
            Key::Escape => {
                if !self.ghost.is_empty() {
                    // Literal-space escape: dismiss ONLY the completion and
                    // KEEP the typed word — Escape then Space/Return commits
                    // the word verbatim.  (Previously the hypothesis-phase arm
                    // won and reset_word() silently discarded the typed
                    // prefix; a locked composing preedit was hidden whole,
                    // leaving the typed word invisible but still pending.)
                    // F6: calibration observation — top prediction was rejected.
                    if let Some(top) = self.last_predictions.first() {
                        self.engine.observe_calibration(top.confidence, false);
                    }
                    self.engine.on_prediction_rejected();
                    self.ghost.clear();
                    // Also drop the candidate list so the dismissed prediction
                    // cannot be re-committed by the aggressive unique-match
                    // Space path right after the user rejected it.
                    self.last_predictions.clear();
                    self.anticipatory_ghost_active = false;
                    if self.dual_buffer.is_some() && !self.current_word.is_empty() {
                        // Composing: re-show the preedit with the typed word only.
                        vec![Action::ShowComposing {
                            typed: self.current_word.clone(),
                            ghost: String::new(),
                        }]
                    } else {
                        vec![Action::HideGhost]
                    }
                } else if self.in_hypothesis_phase() {
                    // Hypothesis phase, no completion shown: cancel the
                    // preedit, nothing was committed.
                    self.reset_word();
                    vec![Action::HideGhost]
                } else {
                    vec![Action::ForwardKey]
                }
            }

            // Space / Return: commit current word, delimit.
            //
            // Tab-only accept (2026-07-12 operator decision): a word-boundary
            // key NEVER auto-commits a prediction.  Both prior auto-accept
            // paths are the same injection class — they can land a word the
            // user did not type (live-falsified: Bulgarian typing committed a
            // wrong English prediction on Space):
            //  - the Space/Return boundary-accept of the visible completion
            //    (reverted here);
            //  - the "aggressive" unique-candidate ReplaceWord, which could
            //    commit last_predictions[0] even though it was never SHOWN,
            //    breaking "accept == last displayed prediction" (removed).
            // Space/Return commit exactly the TYPED word; Tab remains the
            // only accept key.
            Key::Space | Key::Return => {
                if self.dual_buffer.is_some() && !self.current_word.is_empty() {
                    // Full-word composing: commit entire preedit word at once.
                    // The word has been in composing mode the whole time — the
                    // user sees the correct script; now we finalize it.
                    let word = self.current_word.clone();
                    self.commit_word_internal(&word, true);
                    self.reset_word();
                    let mut actions = vec![
                        Action::HideGhost,
                        Action::CommitText(word.clone()),
                        Action::ForwardKey,
                    ];
                    // A forwarded boundary is delivered to the client only
                    // after this action batch returns.  Showing a next-word
                    // preedit in the same batch races that delimiter: GTK,
                    // Electron and browser clients can apply the Space/Return
                    // to the new ghost instead of to the committed word.
                    actions.extend(self.post_commit_pipeline(&word, false));
                    return actions;
                }
                let committed = if !self.current_word.is_empty() {
                    let word = std::mem::take(&mut self.current_word);
                    self.commit_word_internal(&word, true);
                    Some(word)
                } else {
                    None
                };
                self.reset_word();
                let mut actions = vec![Action::HideGhost, Action::ForwardKey];
                // Post-commit intelligence pipeline: auto-correct, auto-caps, next-word ghost.
                if let Some(ref word) = committed {
                    actions.extend(self.post_commit_pipeline(word, false));
                }
                actions
            }

            // Backspace: trim the current word.
            Key::Backspace => {
                if let Some(ref mut db) = self.dual_buffer {
                    if !db.is_empty() {
                        db.pop();
                        if db.is_empty() {
                            // All chars deleted — clear preedit entirely.
                            self.dual_buffer = None;
                            self.current_word.clear();
                            self.ghost.clear();
                            return vec![Action::HideGhost];
                        }
                        // Full-word composing: re-score and update preedit.
                        let (ef, bf) = self.engine.score_both(db.en_text(), db.bg_text());
                        db.update_scores(ef, bf);
                        self.current_word = db.winner_text().to_string();
                        let ghost = self.compute_ghost_suffix();
                        return vec![Action::ShowComposing {
                            typed: self.current_word.clone(),
                            ghost,
                        }];
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
                // ── Composing guard (v0.5.1) ──
                // Full-word composing keeps the entire word in preedit until
                // Space/Enter.  Non-dual-buffer chars (digits, punctuation)
                // bypass the dual buffer, so they would clear the preedit via
                // HideGhost without committing the composed text — losing the
                // user's typed letters.  Commit the composed word first.
                if self.dual_buffer.is_some()
                    && !self.current_word.is_empty()
                    && !ch.is_alphabetic()
                {
                    let word = self.current_word.clone();
                    self.commit_word_internal(&word, true);
                    self.reset_word();
                    let mut actions = vec![Action::HideGhost, Action::CommitText(word.clone())];
                    // The punctuation key is forwarded only after the adapter
                    // finishes this batch.  Never install a fresh preedit in
                    // front of it; otherwise characters such as `!` can be
                    // consumed by the replacement preedit in some clients.
                    actions.extend(self.post_commit_pipeline(&word, false));
                    actions.push(Action::ForwardKey);
                    return actions;
                }

                // Clear anticipatory ghost if active — user is typing a new word.
                if self.anticipatory_ghost_active {
                    self.anticipatory_ghost_active = false;
                    self.ghost.clear();
                }
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
                    && self.current_word.chars().count() >= 2
                    && self.check_wrong_layout()
                {
                    self.transliteration_active = true;
                    let transliterated = lang_detect::transliterate(&self.current_word);
                    // Only previously-forwarded chars are in the application text.
                    // The current char that triggered detection was consumed (not forwarded),
                    // so subtract 1.
                    let replace_len = self.current_word.chars().count() - 1;
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

    /// Accept the visible ghost completion when the user presses Tab.
    /// Space/Return commit only the typed word and never call this path.
    ///
    /// Full-word composing (b3a2cb2) keeps the ENTIRE typed word in the
    /// preedit until commit, regardless of dual-buffer lock state — "lock is
    /// an internal scoring signal, not a commit trigger."  So whenever a dual
    /// buffer is active the prefix lives in the composing preedit and the
    /// accept must commit the full word; only the non-dual keyval path has
    /// already forwarded the prefix to the app, where committing the ghost
    /// suffix alone is correct.  (Using in_hypothesis_phase here dropped the
    /// typed prefix once the buffer locked.)
    ///
    /// Caller precondition: `!self.ghost.is_empty()`.
    fn accept_ghost_completion(&mut self) -> Vec<Action> {
        let full_text = format!("{}{}", self.current_word, self.ghost);
        // F8: multi-word ghost may contain spaces — split into words.
        let words: Vec<&str> = full_text.split_whitespace().collect();
        let first_word = words.first().copied().unwrap_or(&full_text);

        let prefix_in_preedit = self.dual_buffer.is_some();
        let commit_text = if prefix_in_preedit {
            full_text.clone()
        } else {
            self.ghost.clone()
        };
        let actions = vec![Action::HideGhost, Action::CommitText(commit_text)];
        // Notify engine of accepted prediction for adaptive weight learning.
        let ctx: Vec<&str> = self.context.iter().map(|s| s.as_str()).collect();
        // F6: calibration observation — top prediction was accepted.
        if let Some(top) = self.last_predictions.first() {
            self.engine.observe_calibration(top.confidence, true);
        }
        self.engine.on_prediction_accepted(first_word, &ctx);
        // Record acceptance metrics: find rank in last_predictions.
        let rank = self
            .last_predictions
            .iter()
            .position(|p| p.word == first_word)
            .map(|i| i + 1) // 1-based
            .unwrap_or(1);
        self.metrics
            .record_acceptance(rank, self.ghost.chars().count(), full_text.chars().count());
        // Commit each word separately for correct context/Markov tracking.
        for word in &words {
            self.commit_word_internal(word, false);
        }
        let last_word = words.last().copied().unwrap_or("").to_string();
        self.reset_word();
        // After accept: corrections still run, but NO anticipatory ghost —
        // an instant re-arm turns repeated Tabs into a word machine gun (S4).
        let mut actions = actions;
        actions.extend(self.post_commit_pipeline(&last_word, false));
        actions
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

    /// Called when the cursor moves away from the current typing location.
    ///
    /// Clears preedit state, ghost text, recent context words, and bursty
    /// session cache so stale predictions do not bleed into the new cursor
    /// location after navigation.
    pub fn cursor_moved(&mut self) -> Vec<Action> {
        self.reset_word();
        self.context.clear();
        self.lang_detector.reset();
        self.engine.clear_session_cache();
        self.active_hints = None;
        vec![Action::HideGhost]
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

    /// Diagnostic snapshot for the adapter's keystroke trace:
    /// `(dual_buffer_present, locked, hypothesis_phase)`.
    pub fn debug_state(&self) -> (bool, bool, bool) {
        let locked = self.dual_buffer.as_ref().is_some_and(DualBuffer::is_locked);
        (
            self.dual_buffer.is_some(),
            locked,
            self.in_hypothesis_phase(),
        )
    }

    /// The current ghost text (completion suffix), or empty if none.
    pub fn ghost_text(&self) -> &str {
        &self.ghost
    }

    /// Remove only the visible completion suffix while retaining the candidate
    /// list for outcome telemetry and diagnostics.
    pub fn clear_ghost(&mut self) {
        self.ghost.clear();
        self.anticipatory_ghost_active = false;
    }

    /// The last two committed words (prev1, prev2) for context hashing.
    pub fn context_words(&self) -> (Option<&str>, Option<&str>) {
        let prev1 = self.context.back().map(|s| s.as_str());
        let prev2 = if self.context.len() >= 2 {
            Some(self.context[self.context.len() - 2].as_str())
        } else {
            None
        };
        (prev1, prev2)
    }

    fn prediction_context(&self) -> Vec<&str> {
        if let Some(hints) = self.active_hints.as_ref() {
            if !hints.context_words.is_empty() {
                let keep_from = hints.context_words.len().saturating_sub(CONTEXT_SIZE);
                return hints.context_words[keep_from..]
                    .iter()
                    .map(|word| word.as_str())
                    .collect();
            }
        }

        self.context.iter().map(|s| s.as_str()).collect()
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

    /// Export the full personal profile without writing it to disk.
    pub fn export_personal_profile(&self) -> personal::PersonalProfile {
        self.engine.export_personal_profile()
    }

    /// Import the full personal profile without reading it from disk.
    pub fn import_personal_profile(&mut self, profile: &personal::PersonalProfile) {
        self.engine.import_personal_profile(profile);
    }

    /// Override the currently selected prediction with a corrected word.
    ///
    /// Returns a replacement display action when the correction can be shown
    /// for the current prefix; otherwise leaves state unchanged.
    pub fn apply_prediction_override(&mut self, replacement: &str) -> Option<Action> {
        if self.current_word.is_empty() {
            return None;
        }

        let replacement_lower = replacement.to_lowercase();
        let current_lower = self.current_word.to_lowercase();
        if !replacement_lower.starts_with(&current_lower) {
            return None;
        }

        let typed_chars = self.current_word.chars().count();
        let suffix: String = replacement.chars().skip(typed_chars).collect();
        if suffix.is_empty() || !self.config.ghost_text {
            return None;
        }

        if let Some(top) = self.last_predictions.first_mut() {
            top.word = replacement.to_string();
        } else {
            self.last_predictions.insert(
                0,
                Prediction {
                    word: replacement.to_string(),
                    score: 1.0,
                    confidence: 1.0,
                },
            );
        }

        self.ghost.clone_from(&suffix);

        if self.in_hypothesis_phase() {
            Some(Action::ShowComposing {
                typed: self.current_word.clone(),
                ghost: suffix,
            })
        } else {
            Some(Action::ShowGhost(suffix))
        }
    }

    // -- internal helpers -----------------------------------------------

    /// Shared commit logic. `record_commit_metric` is false for Tab-accepted
    /// words (already recorded via `record_acceptance`).
    fn commit_word_internal(&mut self, word: &str, record_commit_metric: bool) {
        if !word.is_empty() {
            let word_char_len = word.chars().count();
            if record_commit_metric {
                self.metrics.record_commit(word_char_len);
            }

            // Extract context BEFORE pushing new word (for online Markov training).
            let prev1 = self.context.back().map(|s| s.as_str());
            let prev2 = if self.context.len() >= 2 {
                Some(self.context[self.context.len() - 2].as_str())
            } else {
                None
            };

            let word_lower = word.to_lowercase();
            self.engine.learn(&word_lower);
            self.engine.learn_online_markov(prev1, prev2, &word_lower);

            // Extended n-gram learning (4-gram through 5-gram) for ∞-gram backoff.
            // Uses the full context window to learn higher-order patterns from
            // user typing, enabling longer-context predictions over time.
            if self.context.len() >= 3 {
                let ctx: Vec<&str> = self.context.iter().map(|s| s.as_str()).collect();
                let ctx_len = ctx.len().min(5); // learn up to 5-gram (4 context words)
                for n in 3..=ctx_len {
                    let start = ctx.len() - n;
                    self.engine.learn_online_ngram(&ctx[start..], &word_lower);
                }
            }

            // Feed word to per-language CVM track + momentum.
            let lang = if self.config.lang_detection {
                let l = self.lang_detector.detected().lang;
                self.engine.learn_with_lang(&word_lower, l);
                self.lang_detector.record_commit(l);
                l
            } else {
                crate::lang_detect::LangId::En // Default when lang detection disabled
            };

            // Feed word to session cache LM for burstiness tracking.
            if self.config.use_session_cache_lm {
                self.engine.observe_session(&word_lower);
            }

            // Feed word stats to regime detector (v0.5+).
            // is_oov: word not found in the trie (zero prefix-search candidates).
            let is_oov = self.engine.candidate_count(word, 1) == 0;

            // F11: Online corpus expansion — add OOV words to the trie so they
            // appear in future predictions. Only for words ≥ 3 chars to avoid
            // noise from typos and abbreviations.
            if is_oov && word_char_len >= 3 {
                self.engine.learn_oov_word(&word_lower, lang);
            }

            let is_flip = self.current_word_had_flip;
            self.regime_detector
                .observe_word(word_char_len, is_oov, is_flip, lang);
            // Propagate the current regime to the engine so predict() can gate δ.
            let (regime, _conf) = self.regime_detector.regime();
            self.engine.set_regime(regime);

            if self.context.len() >= CONTEXT_SIZE {
                self.context.pop_front();
            }
            // Store lowercase for Markov context matching (corpus trained lowercase).
            self.context.push_back(word.to_lowercase());
        }
    }

    // ── Post-Commit Intelligence Pipeline ─────────────────────────────
    //
    // Three steps that fire after every word commit (Space/Enter):
    //   1. Language auto-correction (EN↔BG)
    //   2. Auto-capitalization (sentence start, proper nouns)
    //   3. Anticipatory next-word ghost

    /// Step 1: Check if the committed word belongs to the wrong language.
    ///
    /// Transliterates the word via phonetic map and compares corpus frequencies.
    /// If the transliterated version is 10x+ more frequent → auto-correct.
    fn try_language_correction(&self, word: &str) -> Option<Action> {
        use crate::lang_detect::{reverse_transliterate, transliterate, LangId};

        let typed_chars = word.chars().count();
        if typed_chars < 2 {
            return None;
        }

        // Determine word's script: Latin → check BG transliteration, Cyrillic → check EN.
        let first_alpha = word.chars().find(|c| c.is_alphabetic())?;
        let (typed_lang, other_lang) = if first_alpha.is_ascii_alphabetic() {
            (LangId::En, LangId::Bg)
        } else if ('\u{0400}'..='\u{04FF}').contains(&first_alpha) {
            (LangId::Bg, LangId::En)
        } else {
            return None;
        };

        let typed_freq = self.engine.word_frequency(&word.to_lowercase(), typed_lang);

        // Transliterate to the other language.  A partial transliteration is
        // never a valid replacement: BG-only letters such as ч/щ/ю are not in
        // the legacy ASCII phonetic table, and dropping them produced live
        // corruptions such as "чака" -> "aka" and "защо" -> "zao".
        let other_word = if typed_lang == LangId::En {
            transliterate(word)
        } else {
            reverse_transliterate(word)
        };

        let other_chars = other_word.chars().count();
        if other_word.is_empty() || other_chars < 2 || other_chars != typed_chars {
            return None;
        }

        let other_freq = self
            .engine
            .word_frequency(&other_word.to_lowercase(), other_lang);

        // Only correct if the other language is overwhelmingly more likely.
        // Safety: if both exist, don't correct (ambiguous).
        if other_freq > 0.0 && (typed_freq < 1.0 || other_freq / typed_freq.max(1.0) >= 5.0) {
            return Some(Action::ReplaceWord {
                replace_len: typed_chars,
                text: other_word,
            });
        }

        None
    }

    /// Step 2: Auto-capitalize if previous context requires it.
    ///
    /// Fires after sentence-ending punctuation (. ! ? \n) and for proper nouns.
    fn try_auto_capitalize(&self, word: &str) -> Option<Action> {
        if word.is_empty() {
            return None;
        }

        let first_char = word.chars().next()?;
        if first_char.is_uppercase() {
            return None; // Already capitalized.
        }

        // Check sentence-start: previous committed word ends with .!?\n
        let needs_cap = if let Some(prev) = self.context.back() {
            CapsEngine::requires_capitalization(prev)
        } else {
            true // Start of input.
        };

        // Check proper noun.
        let is_proper = self.caps_engine.is_proper_noun(word);

        if needs_cap || is_proper {
            let capitalized = CapsEngine::capitalize_first(word);
            if capitalized != word {
                return Some(Action::ReplaceWord {
                    replace_len: word.chars().count(),
                    text: capitalized,
                });
            }
        }

        None
    }

    /// Step 3: Show anticipatory next-word ghost immediately after commit.
    ///
    /// Predicts the most likely next word with empty prefix. If confidence
    /// is above threshold, shows it as ghost text. Tab accepts, typing clears.
    fn try_anticipatory_ghost(&mut self) -> Option<Action> {
        let lang = if self.config.lang_detection {
            Some(self.lang_detector.detected().lang)
        } else {
            None
        };

        let context = self.prediction_context();
        let preds = self.engine.predict("", &context, 3, lang);

        let show = preds.first().and_then(|top| {
            if top.confidence >= self.config.ghost_text_min_confidence && !top.word.is_empty() {
                Some(top.word.clone())
            } else {
                None
            }
        });
        if let Some(word) = show {
            self.ghost.clone_from(&word);
            self.last_predictions = preds;
            self.anticipatory_ghost_active = true;
            return Some(Action::ShowGhost(word));
        }

        None
    }

    /// Run the full post-commit pipeline: language correction → caps → next-word ghost.
    /// Returns a list of actions to append to the Space/Enter handler.
    ///
    /// ``show_anticipatory`` (S4, 2026-07-12): after a Tab-ACCEPT the
    /// anticipatory next-word ghost re-armed instantly, so repeated Tabs
    /// machine-gunned the most frequent word ("на" ×4 in the live trace).
    /// Accept paths pass ``false``. Forwarded typing boundaries also pass
    /// ``false`` because the platform adapter cannot install a fresh preedit
    /// until the client has processed the delimiter. The user must type again
    /// before a new prediction is offered.
    fn post_commit_pipeline(
        &mut self,
        committed_word: &str,
        show_anticipatory: bool,
    ) -> Vec<Action> {
        let mut actions = Vec::new();
        let mut effective_word = committed_word.to_string();

        // Steps 1 and 2 rewrite text that is already committed.  Keep them
        // behind an explicit opt-in so Space/Return can never silently replace
        // the raw word the user typed (live-falsified on Bulgarian ч/ш/щ).
        if self.config.post_commit_autocorrect {
            // Step 1: Language auto-correction.
            if let Some(correction) = self.try_language_correction(&effective_word) {
                if let Action::ReplaceWord { ref text, .. } = correction {
                    effective_word = text.clone();
                }
                actions.push(correction);
            }

            // Step 2: Auto-capitalization.
            if let Some(cap_action) = self.try_auto_capitalize(&effective_word) {
                actions.push(cap_action);
            }
        }

        // Step 3: Anticipatory next-word ghost.
        if show_anticipatory && self.config.ghost_text {
            if let Some(ghost_action) = self.try_anticipatory_ghost() {
                actions.push(ghost_action);
            }
        }

        actions
    }

    fn reset_word(&mut self) {
        self.current_word.clear();
        self.ghost.clear();
        self.last_predictions.clear();
        self.transliteration_active = false;
        self.dual_buffer = None;
        self.current_word_had_flip = false;
        self.anticipatory_ghost_active = false;
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

        let Some(db) = self.dual_buffer.as_mut() else {
            return vec![Action::ForwardKey];
        };
        let was_locked = db.is_locked();
        db.push(en_ch, bg_ch);

        // Score both interpretations (skip if already locked — perf win).
        if !was_locked {
            let (en_freq, bg_freq) = self.engine.score_both(db.en_text(), db.bg_text());
            db.update_scores(en_freq, bg_freq);
            let lang_prior = self
                .active_hints
                .as_ref()
                .and_then(|h| h.lang_prior)
                .or_else(|| self.lang_detector.momentum_lang());
            db.apply_prior_lock_hint(lang_prior);
        }

        // Propagate flip detection from dual buffer to regime detector flag.
        if db.flip_detected() {
            self.current_word_had_flip = true;
            db.clear_flip();
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

        // ═══ FULL-WORD COMPOSING ═══
        // Keep the ENTIRE word in preedit until Space/Enter commits it.
        // The user sees the correct script (winner interpretation) in real-time
        // as underlined composing text — no jarring ReplaceWord corrections.
        // Lock is an internal scoring signal, not a commit trigger.
        let ghost_suffix = self.compute_ghost_suffix();
        vec![Action::ShowComposing {
            typed: self.current_word.clone(),
            ghost: ghost_suffix,
        }]
    }

    /// Check if the user is typing on the wrong keyboard layout.
    ///
    /// Compares the **corpus frequency of the top trie candidate** for the
    /// Latin prefix vs its Cyrillic transliteration. Frequency-based
    /// comparison is far more discriminative than candidate counts (which
    /// saturate at small limits with 100K+ word corpora).
    fn check_wrong_layout(&self) -> bool {
        if self.current_word.chars().count() < 2 || !self.config.lang_detection {
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

        log::debug!(
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
        // The early return above already blocks EN momentum, so only
        // None (cold start) and Some(Bg) reach this point.
        // Conservative: "ok" (4.8x), "vs" (28x), "da" (32x) don't trigger.
        // Clear wrong-layout: "zd" (1351x), "mn" (550x), "ka" (224x) do.
        if bg_freq > 0 && ratio >= 50.0 {
            return true;
        }

        false
    }

    /// Minimum confidence for the next-word in multi-word prediction.
    /// Higher threshold = fewer multi-word ghosts but higher precision.
    const MULTI_WORD_MIN_CONFIDENCE: f64 = 0.40;

    /// Attempt to extend a single-word ghost into a 2-word phrase (F8).
    ///
    /// Queries the prediction engine for the most likely word following
    /// `predicted_word`. If the top next-word has confidence ≥ threshold,
    /// returns `suffix + " " + next_word`; otherwise returns `suffix` unchanged.
    /// KAB-parked (S4, 2026-07-12): unused since the ghost became strictly
    /// single-word.  Kept for the labelled revival condition in
    /// compute_ghost_suffix.
    #[allow(dead_code)]
    fn try_multi_word_extension(
        &self,
        predicted_word: &str,
        context: &[&str],
        lang: Option<LangId>,
        suffix: &str,
    ) -> String {
        // Build extended context: append the predicted word to the context.
        let mut ext_ctx: Vec<&str> = context.to_vec();
        ext_ctx.push(predicted_word);
        // Truncate to last CONTEXT_SIZE entries.
        if ext_ctx.len() > CONTEXT_SIZE {
            ext_ctx.drain(..ext_ctx.len() - CONTEXT_SIZE);
        }

        // Query for next-word candidates (empty prefix = any word).
        // Use a small candidate pool to keep latency low.
        let next_preds = self.engine.predict("", &ext_ctx, 3, lang);

        if let Some(top_next) = next_preds.first() {
            if top_next.confidence >= Self::MULTI_WORD_MIN_CONFIDENCE
                && !top_next.word.is_empty()
                && top_next.word.chars().count() >= 2
            {
                return format!("{} {}", suffix, top_next.word);
            }
        }

        suffix.to_string()
    }

    /// Compute predictions and return ghost suffix string (empty if none).
    /// Sets `self.ghost` and `self.last_predictions` as side effects.
    ///
    /// Smart caps: searches with lowercase prefix (so trie always matches),
    /// then applies the user's capitalization pattern to predictions.
    fn compute_ghost_suffix(&mut self) -> String {
        // MasterLoop hint: suppress ghost entirely.
        if self.active_hints.as_ref().is_some_and(|h| h.suppress_ghost) {
            self.ghost.clear();
            self.last_predictions.clear();
            return String::new();
        }

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

        let ctx_words: Vec<String> = self
            .prediction_context()
            .into_iter()
            .map(str::to_owned)
            .collect();
        let ctx_refs: Vec<&str> = ctx_words.iter().map(|word| word.as_str()).collect();
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

        // Smart caps: search with lowercase prefix so trie always matches.
        let search_prefix = self.current_word.to_lowercase();

        let t0 = Instant::now();
        self.last_predictions =
            self.engine
                .predict(&search_prefix, &ctx_refs, self.config.max_candidates, lang);
        self.metrics.record_latency(t0.elapsed());

        // Apply capitalization regime to predictions so they match user's casing.
        let regime = CapsEngine::detect_regime(&self.current_word);
        let first_char_upper = self
            .current_word
            .chars()
            .next()
            .map(|c| c.is_uppercase())
            .unwrap_or(false);

        let prev_token = ctx_refs.last().copied().unwrap_or("");
        let force_sentence_start = CapsEngine::requires_capitalization(prev_token);

        for pred in &mut self.last_predictions {
            pred.word = CapsEngine::apply_caps(&pred.word, regime);
            // For Normal regime with uppercase first char (sentence-start, proper noun) or lazy typing.
            if regime == CapsRegime::Normal && (first_char_upper || force_sentence_start) {
                pred.word = CapsEngine::capitalize_first(&pred.word);
            }
            // Always capitalize known proper nouns.
            if self.caps_engine.is_proper_noun(&pred.word) {
                pred.word = CapsEngine::capitalize_first(&pred.word);
            }
        }

        if let Some(top) = self.last_predictions.first() {
            // MasterLoop hint: boost confidence threshold.
            let confidence_boost = self
                .active_hints
                .as_ref()
                .map(|h| h.confidence_boost)
                .unwrap_or(0.0);
            let effective_confidence = top.confidence + confidence_boost;

            // Gate 1: absolute confidence threshold.
            // Use adaptive floor from LightProfile (F14) when available,
            // falling back to static config value.
            let min_confidence = self
                .active_hints
                .as_ref()
                .and_then(|h| h.confidence_floor)
                .unwrap_or(self.config.ghost_text_min_confidence);
            if effective_confidence >= min_confidence {
                // Gate 2: separation margin — top must beat runner-up by enough.
                let second_conf = self
                    .last_predictions
                    .get(1)
                    .map(|p| p.confidence)
                    .unwrap_or(0.0);
                let margin_ok =
                    effective_confidence - second_conf >= self.config.ghost_text_separation_margin;

                // Gate 3: absolute score floor — reject garbage even at high confidence.
                let score_ok = top.score >= self.config.ghost_text_min_score;

                if margin_ok && score_ok {
                    // Unicode-safe case-insensitive prefix match: predictions may be
                    // capitalized by CapsEngine while typed text is lowercase.
                    // `to_lowercase()` handles all scripts (Cyrillic, Turkish, etc.).
                    let prefix_matches = top.word.chars().count()
                        >= self.current_word.chars().count()
                        && top
                            .word
                            .to_lowercase()
                            .starts_with(&self.current_word.to_lowercase());
                    if prefix_matches {
                        // Use char count for suffix extraction — byte offsets can
                        // differ between uppercase and lowercase in some scripts.
                        let typed_chars = self.current_word.chars().count();
                        let suffix: String = top.word.chars().skip(typed_chars).collect();
                        if !suffix.is_empty() && self.config.ghost_text {
                            // S4 (live trace 2026-07-12): the F8 multi-word
                            // extension chained the predicted NEXT word into
                            // the ghost, so ONE Tab committed "колко на" and
                            // repeated Tabs machine-gunned the most frequent
                            // word.  KAB removal: the ghost is exactly the
                            // completion of the CURRENT word — display ==
                            // commit == one word.  Revival condition: a
                            // separate multi-word UX where accept and display
                            // are word-granular.
                            self.ghost.clone_from(&suffix);
                            return suffix;
                        }
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
    /// Uses margin=0 so I/O-mechanics tests aren't affected by confidence gating.
    fn test_core() -> InputMethodCore {
        let config = InputConfig {
            ghost_text_separation_margin: 0.0,
            ..InputConfig::default()
        };
        let mut core = InputMethodCore::new(config);
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

        // Type "h" — at min_prefix_length (1), may get ghost.
        core.handle_key(press(Key::Char('h')));
        // With min_prefix_length=1, "h" is enough for predictions.

        // Type "e" → "he" — should get ghost.
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
        assert!(has_action(&actions, &Action::CommitText("o".to_string())));
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

    /// Build a LOCKED dual buffer whose composing prefix lives in the preedit.
    fn locked_dual_buffer(core: &InputMethodCore) -> DualBuffer {
        let mut db = DualBuffer::from_config(&core.config.dual_buffer);
        for _ in 0..4 {
            db.push('h', 'h');
        }
        db.update_scores(100.0, 1.0); // high confidence + >= min_lock_chars → lock
        assert!(db.is_locked(), "precondition: dual buffer must be locked");
        db
    }

    /// Regression (accept bug): full-word composing keeps the entire word in the
    /// preedit even after the dual buffer LOCKS.  Tab-accept must commit the full
    /// word (prefix + ghost), never just the ghost suffix — otherwise the typed
    /// prefix, held only in the composing preedit, is dropped by the IBus adapter
    /// (whose composing-commit replaces the whole preedit with the payload).
    #[test]
    fn test_tab_accept_locked_composing_commits_full_word() {
        let mut core = test_core();
        core.dual_buffer = Some(locked_dual_buffer(&core));
        core.current_word = "hel".to_string();
        core.ghost = "lo".to_string();

        let actions = core.handle_key(press(Key::Tab));
        assert!(
            has_action(&actions, &Action::CommitText("hello".to_string())),
            "locked composing Tab must commit the full word, got {actions:?}"
        );
        assert!(
            !has_action(&actions, &Action::CommitText("lo".to_string())),
            "must not commit only the ghost suffix (drops the typed prefix)"
        );
        assert!(has_action(&actions, &Action::HideGhost));
    }

    /// Regression (accept bug): Tab with no ghost while a LOCKED dual buffer is
    /// active must still commit the composing prefix (it lives only in the
    /// preedit) instead of forwarding Tab and abandoning the typed word.
    #[test]
    fn test_tab_no_ghost_locked_composing_commits_prefix() {
        let mut core = test_core();
        core.dual_buffer = Some(locked_dual_buffer(&core));
        core.current_word = "hel".to_string();
        core.ghost = String::new(); // no completion to accept

        let actions = core.handle_key(press(Key::Tab));
        assert!(
            has_action(&actions, &Action::CommitText("hel".to_string())),
            "locked composing Tab (no ghost) must commit the prefix, got {actions:?}"
        );
    }

    /// Guard: the non-dual keyval path is unchanged — the prefix was already
    /// forwarded to the app, so Tab commits only the ghost suffix.
    #[test]
    fn test_tab_accept_keyval_path_commits_suffix_only() {
        let mut core = test_core();
        core.handle_key(press(Key::Char('h')));
        core.handle_key(press(Key::Char('e')));
        core.handle_key(press(Key::Char('l')));
        assert!(
            core.dual_buffer.is_none(),
            "keyval path uses no dual buffer"
        );
        assert_eq!(core.ghost, "lo");

        let actions = core.handle_key(press(Key::Tab));
        assert!(has_action(&actions, &Action::CommitText("lo".to_string())));
        assert!(!has_action(
            &actions,
            &Action::CommitText("hello".to_string())
        ));
    }

    /// Tab-only accept (live-falsified Space-accept): Space with a VISIBLE
    /// completion commits exactly the TYPED word — the prediction must never
    /// be injected by a delimiter key.
    #[test]
    fn test_space_with_visible_completion_commits_typed_word() {
        let mut core = test_core();
        core.dual_buffer = Some(locked_dual_buffer(&core));
        core.current_word = "hel".to_string();
        core.ghost = "lo".to_string();

        let actions = core.handle_key(press(Key::Space));
        assert!(
            has_action(&actions, &Action::CommitText("hel".to_string())),
            "Space must commit the typed word verbatim, got {actions:?}"
        );
        assert!(
            !has_action(&actions, &Action::CommitText("hello".to_string())),
            "Space must NOT inject the completion"
        );
        assert!(has_action(&actions, &Action::ForwardKey));
        assert_eq!(core.current_word(), "");
    }

    /// Tab-only accept: Return behaves exactly like Space — typed word only.
    #[test]
    fn test_return_with_visible_completion_commits_typed_word() {
        let mut core = test_core();
        core.dual_buffer = Some(locked_dual_buffer(&core));
        core.current_word = "hel".to_string();
        core.ghost = "lo".to_string();

        let actions = core.handle_key(press(Key::Return));
        assert!(has_action(&actions, &Action::CommitText("hel".to_string())));
        assert!(!has_action(
            &actions,
            &Action::CommitText("hello".to_string())
        ));
        assert!(has_action(&actions, &Action::ForwardKey));
    }

    /// Regression (injection class): the removed "aggressive" unique-candidate
    /// path must NOT auto-commit an unshown prediction on Space — that path
    /// could replace typed Bulgarian with an English candidate that was never
    /// displayed ("accept == last shown prediction" invariant).
    #[test]
    fn test_space_unique_prediction_never_replaces_typed_word() {
        let mut core = test_core();
        core.dual_buffer = Some(locked_dual_buffer(&core));
        core.current_word = "hel".to_string();
        core.ghost = String::new(); // nothing displayed
        core.last_predictions = vec![Prediction {
            word: "hello".to_string(),
            score: 1.0,
            confidence: 0.9,
        }];

        let actions = core.handle_key(press(Key::Space));
        assert!(
            !actions.iter().any(|a| matches!(
                a,
                Action::ReplaceWord { text, .. } if text == "hello"
            ) || matches!(
                a,
                Action::CommitText(text) if text == "hello"
            )),
            "no unshown prediction may be auto-committed, got {actions:?}"
        );
        assert!(has_action(&actions, &Action::CommitText("hel".to_string())));
        assert!(has_action(&actions, &Action::ForwardKey));
    }

    /// Live regression: Bulgarian letters on EN punctuation positions must
    /// remain part of the composed word, and Space must commit the exact raw
    /// word without a post-commit rewrite.
    #[test]
    fn bg_punctuation_position_letters_commit_exact_words() {
        let cases: [(&str, &[u16]); 4] = [
            ("чака", &[41, 30, 37, 30]),
            ("защо", &[44, 30, 27, 24]),
            ("шина", &[26, 23, 49, 30]),
            ("юнак", &[43, 49, 30, 37]),
        ];

        for (word, codes) in cases {
            let mut core = InputMethodCore::new(InputConfig::default());
            core.load_word(word, 1_000_000);
            for code in codes {
                core.handle_key(press_raw(*code));
            }
            let actions = core.handle_key(press_raw(57));
            assert!(
                actions
                    .iter()
                    .any(|a| matches!(a, Action::CommitText(text) if text == word)),
                "Space must commit exact Bulgarian word {word:?}, got {actions:?}"
            );
            assert!(
                !actions
                    .iter()
                    .any(|a| matches!(a, Action::ReplaceWord { .. })),
                "raw-wins default must not rewrite {word:?}, got {actions:?}"
            );
        }
    }

    /// Defense in depth for the opt-in legacy rewrite: transliteration must be
    /// lossless.  Missing ч/щ/ю mappings previously shortened words and could
    /// turn "чака" into "aka" when that Latin token was frequent.
    #[test]
    fn post_commit_autocorrect_rejects_lossy_bg_transliteration() {
        let config = InputConfig {
            post_commit_autocorrect: true,
            ..InputConfig::default()
        };
        let mut core = InputMethodCore::new(config);
        core.load_word("aka", 1_000_000);
        core.current_word = "чака".to_string();

        let actions = core.handle_key(press(Key::Space));
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, Action::ReplaceWord { text, .. } if text == "aka")),
            "lossy reverse transliteration must never replace the raw word: {actions:?}"
        );
    }

    #[test]
    fn post_commit_autocorrect_is_explicit_opt_in() {
        assert!(!InputConfig::default().post_commit_autocorrect);
        assert!(
            InputConfig::from_json(r#"{"post_commit_autocorrect":true}"#).post_commit_autocorrect
        );
        assert!(
            InputConfig::try_from_json(r#"{"post_commit_autocorrect":true}"#)
                .expect("valid config")
                .post_commit_autocorrect
        );
    }

    /// Anti-desync + anti-double: Tab commits EXACTLY the last displayed
    /// composition (typed + ghost) as a single CommitText — never a doubled
    /// word, never a prediction differing from what was shown.
    #[test]
    fn test_tab_accept_commits_exactly_last_shown_composition() {
        let mut core = test_core();
        core.dual_buffer = Some(locked_dual_buffer(&core));
        core.current_word = "hel".to_string();
        core.ghost = "lo".to_string();
        let shown = format!("{}{}", core.current_word, core.ghost);

        let actions = core.handle_key(press(Key::Tab));
        let commits: Vec<&String> = actions
            .iter()
            .filter_map(|a| match a {
                Action::CommitText(s) => Some(s),
                _ => None,
            })
            .collect();
        assert_eq!(
            commits,
            vec![&shown],
            "Tab must emit exactly one CommitText equal to the shown composition"
        );
    }

    /// Guard: an anticipatory ghost (nothing typed) must NOT be committed by
    /// Space/Return — otherwise double-Space after a word would inject the
    /// predicted next word unasked.
    #[test]
    fn test_space_does_not_accept_anticipatory_ghost() {
        let mut core = test_core();
        core.ghost = "world".to_string();
        core.anticipatory_ghost_active = true;
        assert!(core.current_word.is_empty());

        let actions = core.handle_key(press(Key::Space));
        assert!(
            !has_action(&actions, &Action::CommitText("world".to_string())),
            "anticipatory ghost must not be committed by Space, got {actions:?}"
        );
        assert!(has_action(&actions, &Action::ForwardKey));
    }

    /// Literal-space escape: Escape dismisses ONLY the completion and keeps
    /// the typed word; the following Space commits the word verbatim (the
    /// dismissed completion must not come back via any accept path).
    #[test]
    fn test_escape_then_space_commits_typed_word_literal() {
        let mut core = test_core();
        core.dual_buffer = Some(locked_dual_buffer(&core));
        core.current_word = "hel".to_string();
        core.ghost = "lo".to_string();
        // Seed a unique candidate so the aggressive Space path WOULD fire if
        // the dismissal failed to clear it.
        core.last_predictions = vec![Prediction {
            word: "hello".to_string(),
            score: 1.0,
            confidence: 0.9,
        }];

        let actions = core.handle_key(press(Key::Escape));
        assert!(
            has_action(
                &actions,
                &Action::ShowComposing {
                    typed: "hel".to_string(),
                    ghost: String::new(),
                }
            ),
            "Escape must keep the typed word visible in the preedit, got {actions:?}"
        );
        assert_eq!(core.current_word(), "hel");
        assert!(core.last_predictions.is_empty());

        let actions = core.handle_key(press(Key::Space));
        assert!(
            has_action(&actions, &Action::CommitText("hel".to_string())),
            "Space after Escape must commit the typed word verbatim, got {actions:?}"
        );
        assert!(
            !has_action(&actions, &Action::CommitText("hello".to_string())),
            "the dismissed completion must not come back on Space"
        );
        assert!(has_action(&actions, &Action::ForwardKey));
    }

    /// Regression: Escape with a visible completion in HYPOTHESIS phase must
    /// dismiss only the completion — the old hypothesis-first arm reset the
    /// whole preedit and silently discarded the typed word.
    #[test]
    fn test_escape_hypothesis_with_ghost_keeps_typed_word() {
        let mut core = test_core();
        let mut db = DualBuffer::from_config(&core.config.dual_buffer);
        db.push('h', 'h');
        db.push('e', 'e');
        assert!(!db.is_locked(), "precondition: hypothesis phase");
        core.dual_buffer = Some(db);
        core.current_word = "he".to_string();
        core.ghost = "llo".to_string();

        let actions = core.handle_key(press(Key::Escape));
        assert!(
            has_action(
                &actions,
                &Action::ShowComposing {
                    typed: "he".to_string(),
                    ghost: String::new(),
                }
            ),
            "Escape in hypothesis phase must keep the typed word, got {actions:?}"
        );
        assert_eq!(core.current_word(), "he");
    }

    /// Guard: Escape with NO completion in hypothesis phase keeps the old
    /// cancel semantics — the preedit word is abandoned.
    #[test]
    fn test_escape_no_ghost_hypothesis_cancels_preedit() {
        let mut core = test_core();
        let mut db = DualBuffer::from_config(&core.config.dual_buffer);
        db.push('h', 'h');
        assert!(!db.is_locked());
        core.dual_buffer = Some(db);
        core.current_word = "h".to_string();
        core.ghost = String::new();

        let actions = core.handle_key(press(Key::Escape));
        assert!(has_action(&actions, &Action::HideGhost));
        assert_eq!(core.current_word(), "");
    }

    /// S4 regression: the ghost is strictly the completion of the CURRENT
    /// word — the F8 next-word chain ("lo" -> "lo world") must never come
    /// back, or one Tab commits an unasked extra word.
    #[test]
    fn test_ghost_suffix_never_chains_next_word() {
        let mut core = test_core();
        // A strong bigram after the completed word is exactly what fed the
        // old multi-word extension.
        core.load_bigram("hello", "world", 50);
        core.handle_key(press(Key::Char('h')));
        core.handle_key(press(Key::Char('e')));
        let actions = core.handle_key(press(Key::Char('l')));
        let ghost = ghost_text(&actions).unwrap_or_default();
        assert!(
            !ghost.contains(' '),
            "ghost must be single-word, got {ghost:?}"
        );
        assert_eq!(core.ghost, "lo");

        let actions = core.handle_key(press(Key::Tab));
        assert!(has_action(&actions, &Action::CommitText("lo".to_string())));
        assert!(
            !actions.iter().any(|a| matches!(
                a,
                Action::CommitText(text) if text.contains(' ') || text.contains("world")
            )),
            "Tab must never commit a chained next word, got {actions:?}"
        );
    }

    /// S4 regression: after a Tab-accept the anticipatory next-word ghost
    /// must NOT re-arm — otherwise repeated Tabs machine-gun the most
    /// frequent word (live trace: 4 Tabs -> 4x "на").
    #[test]
    fn test_tab_accept_does_not_rearm_ghost() {
        let mut core = test_core();
        core.load_bigram("hello", "world", 50);
        core.dual_buffer = Some(locked_dual_buffer(&core));
        core.current_word = "hel".to_string();
        core.ghost = "lo".to_string();

        let actions = core.handle_key(press(Key::Tab));
        assert!(has_action(
            &actions,
            &Action::CommitText("hello".to_string())
        ));
        assert!(
            !actions.iter().any(|a| matches!(a, Action::ShowGhost(_))),
            "no anticipatory ghost may follow an accept, got {actions:?}"
        );
        assert!(core.ghost.is_empty());

        // A second Tab has nothing to accept — it must be a plain forward,
        // never another commit.
        let actions = core.handle_key(press(Key::Tab));
        assert!(
            !actions.iter().any(|a| matches!(a, Action::CommitText(_))),
            "second Tab must not commit anything, got {actions:?}"
        );
        assert!(has_action(&actions, &Action::ForwardKey));
    }

    /// Client-ordering regression: Space must not install the next-word ghost
    /// before the forwarded delimiter has reached the application.
    #[test]
    fn test_space_commit_does_not_rearm_ghost_before_forward() {
        let mut core = test_core();
        core.load_bigram("hello", "world", 500);
        core.dual_buffer = Some(locked_dual_buffer(&core));
        core.current_word = "hello".to_string();
        core.ghost = String::new();

        let actions = core.handle_key(press(Key::Space));
        assert!(has_action(
            &actions,
            &Action::CommitText("hello".to_string())
        ));
        assert!(has_action(&actions, &Action::ForwardKey));
        assert!(
            !actions.iter().any(|a| matches!(a, Action::ShowGhost(_))),
            "no fresh preedit may race the forwarded Space, got {actions:?}"
        );
        assert!(!core.has_ghost());
    }

    #[test]
    fn test_return_commit_does_not_rearm_ghost_before_forward() {
        let mut core = test_core();
        core.load_bigram("hello", "world", 500);
        core.dual_buffer = Some(locked_dual_buffer(&core));
        core.current_word = "hello".to_string();

        let actions = core.handle_key(press(Key::Return));
        assert!(has_action(
            &actions,
            &Action::CommitText("hello".to_string())
        ));
        assert!(has_action(&actions, &Action::ForwardKey));
        assert!(
            !actions.iter().any(|a| matches!(a, Action::ShowGhost(_))),
            "no fresh preedit may race the forwarded Return, got {actions:?}"
        );
        assert!(!core.has_ghost());
    }

    /// Guard: Space with a composing prefix and NO visible completion keeps
    /// the existing behavior — commit the typed word and forward the space.
    #[test]
    fn test_space_no_ghost_commits_typed_word() {
        let mut core = test_core();
        core.dual_buffer = Some(locked_dual_buffer(&core));
        core.current_word = "hel".to_string();
        core.ghost = String::new();

        let actions = core.handle_key(press(Key::Space));
        assert!(has_action(&actions, &Action::CommitText("hel".to_string())));
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
        // Explicitly test min_prefix_length=2 gating (PPM disabled to respect it).
        let config = InputConfig {
            ghost_text_separation_margin: 0.0,
            min_prefix_length: 2,
            use_ppm: false,
            ..InputConfig::default()
        };
        let mut core = InputMethodCore::new(config);
        core.load_word("hello", 100);
        core.load_word("help", 80);

        // Single char → no ghost (min_prefix_length = 2).
        let actions = core.handle_key(press(Key::Char('h')));
        assert!(
            !has_action(&actions, &Action::ShowGhost("ello".into())),
            "should not show ghost for single char with min_prefix_length=2"
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

    fn press_raw_with(code: u16, modifiers: Modifiers) -> KeyEvent {
        KeyEvent {
            key: Key::RawCode(code),
            modifiers,
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
    fn test_digit_during_composing_commits_word() {
        // When dual buffer composing is active (user typed letters), a digit
        // must commit the composed word before forwarding the digit.
        // This prevents the preedit from vanishing without the text reaching
        // the application — the root cause of "can't type numbers in browser".
        let mut core = test_core_dual();
        core.handle_key(press_raw(35)); // h → composing starts
        core.handle_key(press_raw(18)); // e → composing continues
        assert!(core.dual_buffer.is_some(), "dual buffer should be active");

        let actions = core.handle_key(press_raw(2)); // '1' digit
                                                     // Must commit the composed word.
        assert!(
            actions.iter().any(|a| matches!(a, Action::CommitText(_))),
            "digit during composing must commit the composed word"
        );
        // Must forward the digit key.
        assert!(
            actions.iter().any(|a| matches!(a, Action::ForwardKey)),
            "digit must be forwarded to the application"
        );
        // Composing state must be cleared.
        assert!(core.dual_buffer.is_none(), "dual buffer should be reset");
        assert!(
            core.current_word().is_empty(),
            "current word should be empty"
        );
    }

    #[test]
    fn test_exclamation_during_composing_commits_word_and_forwards_without_ghost() {
        let mut core = test_core_dual();
        core.load_bigram("hello", "world", 500);
        core.handle_key(press_raw(35)); // h -> composing starts
        core.handle_key(press_raw(18)); // e -> composing continues
        assert!(core.dual_buffer.is_some(), "dual buffer should be active");

        // Shift+evdev 2 is `!` on both supported layouts.
        let actions = core.handle_key(press_raw_with(2, Modifiers::SHIFT));
        assert!(
            actions.iter().any(|a| matches!(a, Action::CommitText(_))),
            "punctuation must first land the composed word, got {actions:?}"
        );
        assert!(has_action(&actions, &Action::ForwardKey));
        assert!(
            !actions.iter().any(|a| matches!(a, Action::ShowGhost(_))),
            "no fresh preedit may intercept the forwarded `!`, got {actions:?}"
        );
        assert!(core.dual_buffer.is_none());
        assert!(core.current_word().is_empty());
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

    // ── Ghost text gating tests (separation margin + score floor) ──

    #[test]
    fn ghost_gate_default_separation_margin() {
        let config = InputConfig::default();
        assert!(
            (config.ghost_text_separation_margin - 0.05).abs() < 1e-9,
            "default separation margin should be 0.05"
        );
    }

    #[test]
    fn ghost_gate_default_min_score_disabled() {
        let config = InputConfig::default();
        assert_eq!(
            config.ghost_text_min_score, 0.0,
            "default min_score should be 0.0 (disabled)"
        );
    }

    #[test]
    fn ghost_gate_json_override_separation_margin() {
        let json = r#"{"ghost_text_separation_margin": 0.25}"#;
        let config = InputConfig::from_json(json);
        assert!(
            (config.ghost_text_separation_margin - 0.25).abs() < 1e-9,
            "JSON should override separation margin"
        );
    }

    #[test]
    fn ghost_gate_json_override_min_score() {
        let json = r#"{"ghost_text_min_score": 10.0}"#;
        let config = InputConfig::from_json(json);
        assert!(
            (config.ghost_text_min_score - 10.0).abs() < 1e-9,
            "JSON should override min_score"
        );
    }

    #[test]
    fn ghost_gate_score_floor_suppresses_ghost() {
        // With a high score floor, even a clear winner should be suppressed.
        let config = InputConfig {
            ghost_text_min_score: 999_999.0,
            ghost_text_separation_margin: 0.0,
            ..InputConfig::default()
        };
        let mut core = InputMethodCore::new(config);
        core.load_word("hello", 100);
        core.handle_key(press(Key::Char('h')));
        core.handle_key(press(Key::Char('e')));
        let actions = core.handle_key(press(Key::Char('l')));
        assert!(
            ghost_text(&actions).is_none() || has_action(&actions, &Action::HideGhost),
            "high score floor should suppress ghost text"
        );
    }

    #[test]
    fn ghost_gate_single_candidate_passes_margin() {
        // Single candidate: margin = confidence(~1.0) - 0.0 = ~1.0 >= 0.10.
        let config = InputConfig {
            ghost_text_separation_margin: 0.10,
            ghost_text_min_score: 0.0,
            ..InputConfig::default()
        };
        let mut core = InputMethodCore::new(config);
        core.load_word("unique", 100);
        core.handle_key(press(Key::Char('u')));
        core.handle_key(press(Key::Char('n')));
        let actions = core.handle_key(press(Key::Char('i')));
        let g = ghost_text(&actions);
        assert!(
            g.is_some() && !g.as_ref().unwrap().is_empty(),
            "single candidate should pass margin check: got {:?}",
            g
        );
    }

    #[test]
    fn prediction_uses_hint_context_when_available() {
        let config = InputConfig {
            use_ppm: false,
            ghost_text_separation_margin: 0.0,
            ..InputConfig::default()
        };
        let mut core = InputMethodCore::new(config);
        core.load_word("world", 50);
        core.load_word("worry", 50);
        core.load_bigram("hello", "world", 10);
        core.apply_hints(&crate::master_loop::Hints {
            context_words: vec!["hello".into()],
            ..Default::default()
        });

        core.handle_key(press(Key::Char('w')));
        core.handle_key(press(Key::Char('o')));
        core.handle_key(press(Key::Char('r')));

        let world_pos = core.predictions().iter().position(|p| p.word == "world");
        let worry_pos = core.predictions().iter().position(|p| p.word == "worry");
        assert!(
            world_pos.is_some() && worry_pos.is_some(),
            "both context-sensitive candidates should appear"
        );
        assert!(
            world_pos.unwrap() < worry_pos.unwrap(),
            "hint context should rank 'world' above 'worry'"
        );
    }

    #[test]
    fn navigation_key_clears_stale_context() {
        let mut core = test_core();
        core.handle_key(press(Key::Char('h')));
        core.handle_key(press(Key::Char('e')));
        core.handle_key(press(Key::Char('l')));
        let accept_actions = core.handle_key(press(Key::Tab));
        assert!(has_action(
            &accept_actions,
            &Action::CommitText("lo".into())
        ));
        assert_eq!(core.context_words(), (Some("hello"), None));

        core.handle_key(press(Key::Char('w')));
        let predict_actions = core.handle_key(press(Key::Char('o')));
        assert!(
            ghost_text(&predict_actions).is_some() || !core.predictions().is_empty(),
            "typing after committed context should create an active prediction before navigation"
        );
        assert_eq!(core.current_word(), "wo");

        let actions = core.handle_key(press(Key::Left));

        assert!(has_action(&actions, &Action::HideGhost));
        assert!(has_action(&actions, &Action::ForwardKey));
        assert_eq!(core.current_word(), "");
        assert!(!core.has_ghost());
        assert_eq!(core.context_words(), (None, None));
    }

    // ── Config parsing hardening tests ─────────────────────────────────

    #[test]
    fn test_config_from_json_invalid_still_returns_defaults() {
        let config = InputConfig::from_json("not valid json {{{");
        let defaults = InputConfig::default();
        assert_eq!(config.max_candidates, defaults.max_candidates);
        assert_eq!(config.min_prefix_length, defaults.min_prefix_length);
        assert_eq!(config.enabled, defaults.enabled);
        assert_eq!(config.ghost_text, defaults.ghost_text);
    }

    #[test]
    fn test_try_from_json_returns_error_on_invalid() {
        let result = InputConfig::try_from_json("not valid json {{{");
        assert!(result.is_err(), "garbage JSON should return Err");
        let msg = result.unwrap_err();
        assert!(
            msg.contains("invalid config JSON"),
            "error message should mention invalid config JSON, got: {msg}"
        );
    }

    // ==================================================================
    // Config clamping edge-case tests
    // ==================================================================

    #[test]
    fn config_from_json_fuzzy_max_edits_above_3_clamped() {
        // from_json path uses n.min(3) — value 10 must become 3.
        let json = r#"{"tuning": {"fuzzy_max_edits": 10}}"#;
        let config = InputConfig::from_json(json);
        assert_eq!(
            config.fuzzy_max_edits, 3,
            "fuzzy_max_edits > 3 should be clamped to 3, got {}",
            config.fuzzy_max_edits
        );
    }

    #[test]
    fn config_from_json_max_candidates_zero_clamped_to_one() {
        // from_json path uses (n as usize).max(1) — value 0 must become 1.
        let json = r#"{"max_candidates": 0}"#;
        let config = InputConfig::from_json(json);
        assert_eq!(
            config.max_candidates, 1,
            "max_candidates=0 should be clamped to 1, got {}",
            config.max_candidates
        );
    }

    #[test]
    fn config_try_from_json_ghost_text_min_confidence_above_1_rejected() {
        // try_from_json validates range [0.0, 1.0].
        let json = r#"{"ghost_text_min_confidence": 1.5}"#;
        let result = InputConfig::try_from_json(json);
        assert!(
            result.is_err(),
            "ghost_text_min_confidence > 1.0 should be rejected"
        );
    }

    #[test]
    fn config_try_from_json_ghost_text_min_confidence_below_0_rejected() {
        let json = r#"{"ghost_text_min_confidence": -0.1}"#;
        let result = InputConfig::try_from_json(json);
        assert!(
            result.is_err(),
            "ghost_text_min_confidence < 0.0 should be rejected"
        );
    }

    #[test]
    fn config_from_json_ghost_text_min_confidence_stored_unclamped() {
        // from_json silently stores without validation — it's the caller's choice.
        // This test documents the actual behavior.
        let json = r#"{"ghost_text_min_confidence": 0.75}"#;
        let config = InputConfig::from_json(json);
        assert!(
            (config.ghost_text_min_confidence - 0.75).abs() < 1e-9,
            "valid confidence 0.75 should be accepted, got {}",
            config.ghost_text_min_confidence
        );
    }

    #[test]
    fn test_config_validation_fields() {
        // max_candidates = 0 should be rejected
        let result = InputConfig::try_from_json(r#"{"max_candidates": 0}"#);
        assert!(result.is_err(), "max_candidates=0 should be rejected");

        // min_prefix_length = 0 should be rejected
        let result = InputConfig::try_from_json(r#"{"min_prefix_length": 0}"#);
        assert!(result.is_err(), "min_prefix_length=0 should be rejected");

        // weights that don't sum to 1.0 should be rejected
        let result = InputConfig::try_from_json(
            r#"{"weights": {"corpus": 0.5, "markov": 0.5, "personal": 0.5}}"#,
        );
        assert!(result.is_err(), "weights summing to 1.5 should be rejected");

        // valid config should succeed
        let result = InputConfig::try_from_json(r#"{"max_candidates": 3, "min_prefix_length": 2}"#);
        assert!(result.is_ok(), "valid config should return Ok");
        let config = result.unwrap();
        assert_eq!(config.max_candidates, 3);
        assert_eq!(config.min_prefix_length, 2);
    }
}
