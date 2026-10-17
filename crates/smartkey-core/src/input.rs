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
        /// Caps Lock **latch state** (not the Caps_Lock key itself).
        /// Inverts Shift for letter keys only — digits and punctuation are
        /// unaffected by the lock latch on every platform we target.
        const CAPS_LOCK = 0x20;
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

impl Key {
    pub(crate) fn is_pure_modifier(&self) -> bool {
        matches!(
            self,
            Self::RawCode(29 | 42 | 54 | 56 | 58 | 97 | 100 | 125 | 126)
                | Self::Other(0xffe1..=0xffee)
        )
    }
}

/// A single key event delivered by the platform.
#[derive(Debug, Clone)]
pub struct KeyEvent {
    pub key: Key,
    pub modifiers: Modifiers,
}

impl KeyEvent {
    /// Resolve physical special keys before either input layer classifies the
    /// event. Letter codes stay raw so the dual buffer can interpret layouts.
    pub(crate) fn normalize_special_key(mut self) -> Self {
        if let Key::RawCode(code) = self.key {
            if let Some(special) = keymap::scancode_to_special(code) {
                self.key = match special {
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
            }
        }
        self
    }
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
    /// Opt-in (2026-08-22, debate 4e02928c0537): Space accepts a visible,
    /// eligible word-completion ghost as one atomic `typed+ghost+" "` commit.
    /// Default OFF — Space stays exactly literal unless the operator enables
    /// `accept.space_accept` in the config.
    pub space_accept: bool,
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
            space_accept: false,
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
            if let Some(b) = v
                .get("accept")
                .and_then(|v| v.get("space_accept"))
                .and_then(|v| v.as_bool())
            {
                config.space_accept = b;
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
        if let Some(b) = v
            .get("accept")
            .and_then(|v| v.get("space_accept"))
            .and_then(|v| v.as_bool())
        {
            config.space_accept = b;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommitCause {
    Boundary,
    FocusFlush,
    AcceptedCompletion,
    PartialCompletionFallback,
    AcceptedLanguageChoice,
}

impl CommitCause {
    fn is_prediction_accept(self) -> bool {
        matches!(
            self,
            Self::AcceptedCompletion | Self::PartialCompletionFallback
        )
    }
}

/// Language evidence is word-specific; it does not carry typed text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CommitObservation {
    pub language: Option<LangId>,
}

/// One public core operation's learning events. Replaced on the next operation
/// even when a standalone consumer never drains it.
#[derive(Debug, Default)]
pub(crate) struct CommitLearningBatch {
    pub words: Vec<CommitObservation>,
    pub accepted_prediction: bool,
}

/// A correction carries the exact model which justified its target text.
#[derive(Debug)]
struct LanguageCorrection {
    replace_len: usize,
    text: String,
    language: LangId,
}

impl LanguageCorrection {
    fn into_action(self) -> Action {
        Action::ReplaceWord {
            replace_len: self.replace_len,
            text: self.text,
        }
    }
}

#[derive(Debug, Default)]
struct WordEvidence {
    language: Option<LangId>,
    confirmed_word: Option<LangId>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WordCharOrigin {
    Direct(char),
    Derived(char),
}

impl WordCharOrigin {
    fn character(self) -> char {
        match self {
            Self::Direct(ch) | Self::Derived(ch) => ch,
        }
    }
}

enum WordDetectorBaseline {
    Unstarted,
    Captured(LanguageDetector),
    Invalid,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SentenceBoundaryHint {
    None,
    /// Literal closers/repeated terminators following the original terminator.
    AwaitingWhitespace {
        tail_count: usize,
    },
    /// Known trailing plain Spaces; zero means opaque Tab/Return whitespace.
    SentenceStart {
        spaces: usize,
        tail_count: usize,
    },
}

impl SentenceBoundaryHint {
    fn is_sentence_start(self) -> bool {
        matches!(self, Self::SentenceStart { .. })
    }
}

/// The core state machine shared by all platform adapters.
///
/// Platform code feeds key events via [`handle_key`] and executes the
/// returned [`Action`]s (show ghost text, commit text, etc.).
pub struct InputMethodCore {
    engine: SmartKeyEngine,
    commit_learning: CommitLearningBatch,
    /// Exact rendered characters and their retractable observation origins.
    word_origins: Vec<WordCharOrigin>,
    /// Editing-segment baseline, retained across deletion to an empty word.
    word_detector_baseline: WordDetectorBaseline,
    config: InputConfig,
    current_word: String,
    ghost: String,
    context: VecDeque<String>,
    /// Presentation evidence for forwarded delimiters, never model context.
    sentence_boundary: SentenceBoundaryHint,
    /// Current-position casing reconciliation, independent of lexical hints.
    sentence_presentation_override: Option<bool>,
    enabled: bool,
    /// Whether the current host can replace text already delivered to its client.
    /// Ephemeral transport capability; never part of config or personal state.
    external_rewrite_supported: bool,
    /// Ephemeral host UI capability, independent of language configuration.
    language_choice_supported: bool,
    /// Keep ambiguous boundary policy after the offer is dismissed or hidden.
    language_choice_token_policy: bool,
    last_predictions: Vec<Prediction>,
    /// Model selection used for the displayed prediction's scoring components.
    last_prediction_language: Option<LangId>,
    /// Prediction quality and performance metrics (v0.4.0).
    metrics: PredictionMetrics,
    /// Character trigram language detector (v0.4.0).
    lang_detector: LanguageDetector,
    /// Whether transliteration mode is active for the current word (v0.4.1).
    transliteration_active: bool,
    /// Dual-interpretation buffer for layout-agnostic input (v0.5.0).
    /// `Some` when active (after first `Key::RawCode` in a word), `None` otherwise.
    dual_buffer: Option<DualBuffer>,
    /// Rendering layout after a token leaves preedit; never language evidence.
    word_delivery_layout: Option<keymap::Layout>,
    /// Typing regime detector (v0.5+) — gates tech vocab δ weight in the ensemble.
    regime_detector: RegimeDetector,
    /// True when the current word triggered a dual-buffer layout flip (for regime detection).
    current_word_had_flip: bool,
    /// Smart caps engine — detects capitalization regime and applies to predictions.
    caps_engine: CapsEngine,
    /// Hints injected by MasterLoop (v0.6: proactive anticipation).
    active_hints: Option<crate::master_loop::Hints>,
    /// Latest client context for terminal routing; never detector evidence.
    terminal_context_lang: Option<LangId>,
    /// True when an anticipatory (next-word) ghost is showing, not a prefix ghost.
    /// Used to distinguish Tab-accept behavior.
    anticipatory_ghost_active: bool,
    /// One-shot display attestation from the master loop (GREEN v4); see
    /// `attest_space_accept_display`.  Consumed by every `handle_key`.
    space_accept_display_attested: bool,
}

impl InputMethodCore {
    pub fn new(config: InputConfig) -> Self {
        let enabled = config.enabled;
        Self {
            engine: SmartKeyEngine::from_config(&config),
            commit_learning: CommitLearningBatch::default(),
            word_origins: Vec::new(),
            word_detector_baseline: WordDetectorBaseline::Unstarted,
            lang_detector: LanguageDetector::new(),
            metrics: PredictionMetrics::new(),
            config,
            current_word: String::new(),
            ghost: String::new(),
            context: VecDeque::with_capacity(CONTEXT_SIZE),
            sentence_boundary: SentenceBoundaryHint::None,
            sentence_presentation_override: None,
            enabled,
            external_rewrite_supported: true,
            language_choice_supported: false,
            language_choice_token_policy: false,
            last_predictions: Vec::new(),
            last_prediction_language: None,
            transliteration_active: false,
            dual_buffer: None,
            word_delivery_layout: None,
            regime_detector: RegimeDetector::new(),
            current_word_had_flip: false,
            caps_engine: CapsEngine::new(),
            active_hints: None,
            terminal_context_lang: None,
            anticipatory_ghost_active: false,
            space_accept_display_attested: false,
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
        self.terminal_context_lang = None;
        self.lang_detector.clear_prior();
    }

    /// Update the host's ability to replace already-delivered client text.
    pub fn set_external_rewrite_supported(&mut self, supported: bool) {
        self.external_rewrite_supported = supported;
    }

    pub(crate) fn set_language_choice_supported(&mut self, supported: bool) {
        self.latch_language_choice_policy();
        self.language_choice_supported = supported;
        self.latch_language_choice_policy();
    }

    /// Read the current complete physical alternatives without selecting one.
    pub(crate) fn language_choice_readings(&self) -> Option<(LangId, &str, &str)> {
        let db = self.dual_buffer.as_ref()?;
        if !self.enabled
            || self.current_word.is_empty()
            || db.is_explicitly_locked()
            || db.len() < self.config.min_prefix_length
            || self.current_word != db.winner_text()
            || db.en_text() == db.bg_text()
            || !db.en_text().chars().all(char::is_alphabetic)
            || !db.bg_text().chars().all(char::is_alphabetic)
        {
            return None;
        }
        let (en, bg) = self.engine.score_exact_both(db.en_text(), db.bg_text());
        if (en > 0.0) != (bg > 0.0) {
            return None;
        }
        Some((db.winner_lang(), db.en_text(), db.bg_text()))
    }

    pub(crate) fn latch_language_choice_policy(&mut self) {
        if self.language_choice_supported && self.language_choice_readings().is_some() {
            self.language_choice_token_policy = true;
        }
    }

    fn language_choice_policy_active(&self) -> bool {
        (self.language_choice_supported || self.language_choice_token_policy)
            && self.language_choice_readings().is_some()
    }

    /// Commit exactly the selected whole reading, without accepting a ghost
    /// or running a later automatic replacement over the explicit choice.
    pub(crate) fn accept_language_choice(&mut self, language: LangId) -> Option<Vec<Action>> {
        let (_, en, bg) = self.language_choice_readings()?;
        let selected = match language {
            LangId::En => en.to_owned(),
            LangId::Bg => bg.to_owned(),
            LangId::Tech => return None,
        };
        self.commit_learning = CommitLearningBatch::default();
        self.commit_word_internal(&selected, CommitCause::AcceptedLanguageChoice);
        self.reset_word();
        Some(vec![Action::HideGhost, Action::CommitText(selected)])
    }

    pub(crate) fn set_terminal_context_lang(&mut self, lang: Option<LangId>) {
        self.terminal_context_lang = lang.filter(|lang| matches!(lang, LangId::En | LangId::Bg));
    }

    /// Local delimiter projection only, independent of a reconciliation veto.
    pub(crate) fn has_sentence_boundary_hint(&self) -> bool {
        self.sentence_boundary != SentenceBoundaryHint::None
    }

    /// A contradictory client report also vetoes stale lexical casing hints.
    pub(crate) fn withdraw_sentence_boundary_hint(&mut self) {
        self.sentence_boundary = SentenceBoundaryHint::None;
        self.sentence_presentation_override = Some(false);
    }

    /// Reconcile a valid current-position report, never lexical context.
    pub(crate) fn reconcile_sentence_boundary_hint(&mut self, before_cursor: &str) {
        let trimmed = before_cursor.trim_end();
        let whitespace = &before_cursor[trimmed.len()..];
        self.sentence_boundary = if trimmed.is_empty() || whitespace.contains(['\r', '\n']) {
            SentenceBoundaryHint::SentenceStart {
                spaces: 0,
                tail_count: 0,
            }
        } else if CapsEngine::requires_capitalization(trimmed) {
            // Count characters after the earliest terminator in this literal suffix.
            let mut tail_count = 0;
            for (offset, ch) in trimmed
                .chars()
                .rev()
                .take_while(|&ch| {
                    CapsEngine::is_closing_punctuation(ch) || matches!(ch, '.' | '!' | '?')
                })
                .enumerate()
            {
                if matches!(ch, '.' | '!' | '?') {
                    tail_count = offset;
                }
            }
            if whitespace.is_empty() {
                SentenceBoundaryHint::AwaitingWhitespace { tail_count }
            } else {
                SentenceBoundaryHint::SentenceStart {
                    spaces: if whitespace.bytes().all(|byte| byte == b' ') {
                        whitespace.len()
                    } else {
                        0
                    },
                    tail_count,
                }
            }
        } else {
            SentenceBoundaryHint::None
        };
        // A fresh positive lives in the enum. If an uncertain edit withdraws
        // it, stale lexical casing must not reappear through the fallback.
        self.sentence_presentation_override = Some(false);
    }

    fn sentence_start_override(&self) -> Option<bool> {
        // A genuine new local boundary supersedes an older external veto.
        self.sentence_boundary
            .is_sentence_start()
            .then_some(true)
            .or(self.sentence_presentation_override)
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
        self.commit_learning = CommitLearningBatch::default();
        let event = event.normalize_special_key();
        // The display attestation covers exactly this key event (one-shot).
        let display_attested = std::mem::take(&mut self.space_accept_display_attested);

        // A release cannot activate the kill switch, including while disabled.
        if event.modifiers.contains(Modifiers::RELEASE) {
            return vec![Action::ForwardKey];
        }

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

        if event.key.is_pure_modifier() {
            return vec![Action::ForwardKey];
        }

        // Ignore shortcuts with Ctrl or Alt held, not the modifier key itself.
        if event.modifiers.intersects(Modifiers::CTRL | Modifiers::ALT) {
            self.sentence_boundary = SentenceBoundaryHint::None;
            return vec![Action::ForwardKey];
        }

        // A literal closing tail belongs to the forwarded terminator, not a
        // new lexical token. Raw keys qualify only when both layouts agree.
        if matches!(
            self.sentence_boundary,
            SentenceBoundaryHint::AwaitingWhitespace { .. }
        ) && self.current_word.is_empty()
            && self.dual_buffer.is_none()
        {
            let tail = match event.key {
                Key::Char(ch) => Some(ch),
                Key::RawCode(code) => keymap::scancode_to_both_with_caps(
                    code,
                    event.modifiers.contains(Modifiers::SHIFT),
                    event.modifiers.contains(Modifiers::CAPS_LOCK),
                )
                .and_then(|(en, bg)| (en == bg).then_some(en)),
                _ => None,
            };
            if tail.is_some_and(|ch| {
                CapsEngine::is_closing_punctuation(ch) || matches!(ch, '.' | '!' | '?')
            }) {
                if let SentenceBoundaryHint::AwaitingWhitespace { tail_count } =
                    &mut self.sentence_boundary
                {
                    *tail_count = tail_count.saturating_add(1);
                }
                return vec![Action::ForwardKey];
            }
        }

        // An adjacent printable character is not the whitespace required after
        // a forwarded terminator (decimal/technical tokens included). Withdraw
        // the pending hint before any prediction for that character.
        if matches!(
            self.sentence_boundary,
            SentenceBoundaryHint::AwaitingWhitespace { .. }
        ) && matches!(event.key, Key::Char(_) | Key::RawCode(_))
        {
            self.sentence_boundary = SentenceBoundaryHint::None;
        }

        // ── Resolve RawCode (v0.5.0) ─────────────────────────────────
        // Special identities were resolved above; remaining hardware codes
        // become either a dual-buffer operation or a regular character.
        let mut inferred_raw_char = false;
        let event = if let Key::RawCode(code) = event.key {
            // Caps Lock inverts Shift, but only on letter keys — the lock latch
            // must never uppercase digits or punctuation.  Which keys are
            // letters differs per layout (the bracket keys are ш щ ч ю in
            // Bulgarian), so the latch is applied per layout by the keymap.
            let shift_held = event.modifiers.contains(Modifiers::SHIFT);
            let caps_lock = event.modifiers.contains(Modifiers::CAPS_LOCK);
            if let Some((en_ch, bg_ch)) =
                keymap::scancode_to_both_with_caps(code, shift_held, caps_lock)
            {
                if en_ch != bg_ch && self.config.dual_buffer.enabled {
                    let layout = self.word_delivery_layout.or_else(|| {
                        (self.dual_buffer.is_none() && !self.current_word.is_empty()).then_some(
                            if self.transliteration_active {
                                keymap::Layout::Bg
                            } else {
                                keymap::Layout::En
                            },
                        )
                    });
                    if let Some(layout) = layout {
                        // A semantic prefix is already in the client. Keep it
                        // intact and retain any active legacy transliteration.
                        // Unconverted semantic prefixes keep the EN fallback.
                        self.word_delivery_layout = Some(layout);
                        let ch = match layout {
                            keymap::Layout::En => en_ch,
                            keymap::Layout::Bg => bg_ch,
                        };
                        return self.append_delivered_raw_char(ch);
                    }
                    // Dual-interpretable character → handle via dual buffer.
                    return self.handle_dual_buffer_key(en_ch, bg_ch);
                }
                // Shared glyphs are direct. With composition disabled, the
                // EN fallback keeps its ambiguity for learning provenance.
                inferred_raw_char = en_ch != bg_ch;
                KeyEvent {
                    key: Key::Char(en_ch),
                    modifiers: event.modifiers,
                }
            } else {
                self.sentence_boundary = SentenceBoundaryHint::None;
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
                } else if let Some(commit) = self.flush_preedit() {
                    // Composing, no ghost: the prefix lives only in the preedit,
                    // so commit it before forwarding Tab — otherwise a locked
                    // dual buffer would forward Tab and abandon the typed word.
                    let prefix = self.current_word.clone();
                    self.commit_word_internal(&prefix, CommitCause::Boundary);
                    self.reset_word();
                    vec![Action::HideGhost, commit, Action::ForwardKey]
                } else if !self.current_word.is_empty() {
                    // Semantic and handed-off text is already in the client.
                    // Tab ends its token without inserting the prefix again.
                    let word = self.current_word.clone();
                    self.commit_word_internal(&word, CommitCause::Boundary);
                    self.reset_word();
                    vec![Action::HideGhost, Action::ForwardKey]
                } else {
                    // This empty-word branch forwards Tab without a word commit.
                    // Tab may focus, complete, or insert nothing: later Backspace
                    // cannot safely restore the preceding counted Space/tail.
                    if self.sentence_boundary != SentenceBoundaryHint::None {
                        self.sentence_boundary = SentenceBoundaryHint::SentenceStart {
                            spaces: 0,
                            tail_count: 0,
                        };
                    }
                    vec![Action::ForwardKey]
                }
            }

            // Right arrow: accept one character of ghost text.
            Key::Right => {
                if !self.ghost.is_empty() && self.dual_buffer.is_some() {
                    let mut ghost_chars = self.ghost.chars();
                    let Some(ch) = ghost_chars.next() else {
                        unreachable!("non-empty ghost must contain a character");
                    };

                    // The final accepted character completes the prediction;
                    // use the shared full-preedit commit so learning, metrics,
                    // and state reset happen exactly once.
                    if ghost_chars.next().is_none() {
                        return self.accept_ghost_completion();
                    }

                    let accepted = self
                        .dual_buffer
                        .as_mut()
                        .is_some_and(|db| db.push_winner_char(ch));
                    if !accepted {
                        // No exact physical counterpart means the two buffers
                        // cannot remain synchronized.  Fail safely by accepting
                        // the already-visible word atomically, without a partial
                        // mutation.
                        return self.accept_ghost_completion_with_cause(
                            CommitCause::PartialCompletionFallback,
                        );
                    }

                    self.ghost.remove(0);
                    let word = self
                        .dual_buffer
                        .as_ref()
                        .expect("dual buffer accepted the character")
                        .winner_text()
                        .to_string();
                    self.replace_word_with_derived(word);
                    vec![Action::ShowComposing {
                        typed: self.current_word.clone(),
                        ghost: self.ghost.clone(),
                    }]
                } else if !self.ghost.is_empty() {
                    if self.ghost.chars().count() == 1 {
                        return self.accept_ghost_completion();
                    }
                    let ch = self.ghost.remove(0);
                    self.push_word_char(ch, false);
                    if self.ghost.is_empty() {
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
                    self.last_prediction_language = None;
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
                    // preedit, nothing was committed. The client's preceding
                    // delimiter was not edited by this local cancellation.
                    let sentence_boundary = self.sentence_boundary;
                    let sentence_presentation_override = self.sentence_presentation_override;
                    self.reset_word();
                    self.sentence_boundary = sentence_boundary;
                    self.sentence_presentation_override = sentence_presentation_override;
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
                // Opt-in Space-accept (2026-08-22, debate 4e02928c0537): Space —
                // never Return — accepts an eligible visible completion as ONE
                // atomic `typed+ghost+" "` commit with the key consumed.  With
                // the flag off, or without an eligible ghost, both keys stay
                // exactly literal (the unchanged paths below).
                if Self::is_space_event(&event)
                    && self.config.space_accept
                    && display_attested
                    && self.space_accept_eligible()
                {
                    return self.accept_ghost_with_delimiter();
                }
                // Capture whole-word ownership before taking/resetting the token.
                let rewrite_allowed = self.external_rewrite_supported || self.dual_buffer.is_some();
                let sentence_start = self.sentence_start_override();
                // Empty Space does not finish a word; retain its current-position
                // decision. Return instead establishes a new local boundary.
                let next_presentation_override =
                    if event.key == Key::Space && self.current_word.is_empty() {
                        self.sentence_presentation_override
                    } else {
                        None
                    };
                let next_boundary = if event.key == Key::Return {
                    SentenceBoundaryHint::SentenceStart {
                        spaces: 0,
                        tail_count: 0,
                    }
                } else if self.current_word.is_empty() {
                    match self.sentence_boundary {
                        SentenceBoundaryHint::AwaitingWhitespace { tail_count } => {
                            SentenceBoundaryHint::SentenceStart {
                                spaces: 1,
                                tail_count,
                            }
                        }
                        SentenceBoundaryHint::SentenceStart { spaces, tail_count } => {
                            SentenceBoundaryHint::SentenceStart {
                                spaces: if spaces > 0 {
                                    spaces.saturating_add(1)
                                } else {
                                    0
                                },
                                tail_count,
                            }
                        }
                        SentenceBoundaryHint::None => SentenceBoundaryHint::None,
                    }
                } else {
                    SentenceBoundaryHint::None
                };
                if self.dual_buffer.is_some() && !self.current_word.is_empty() {
                    // Full-word composing: commit entire preedit word at once.
                    // The word has been in composing mode the whole time — the
                    // user sees the displayed script; the boundary resolves
                    // exact one-sided evidence before anything is learned.
                    let word = self.resolve_word_boundary();
                    let correction = self.commit_boundary_word(&word, rewrite_allowed);
                    self.reset_word();
                    self.sentence_boundary = next_boundary;
                    self.sentence_presentation_override = next_presentation_override;
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
                    actions.extend(self.post_commit_pipeline(
                        &word,
                        false,
                        correction,
                        rewrite_allowed,
                        sentence_start,
                    ));
                    return actions;
                }
                let committed = if !self.current_word.is_empty() {
                    let word = std::mem::take(&mut self.current_word);
                    let correction = self.commit_boundary_word(&word, rewrite_allowed);
                    Some((word, correction))
                } else {
                    None
                };
                self.reset_word();
                self.sentence_boundary = next_boundary;
                self.sentence_presentation_override = next_presentation_override;
                let mut actions = vec![Action::HideGhost, Action::ForwardKey];
                // Post-commit intelligence pipeline: auto-correct, auto-caps, next-word ghost.
                if let Some((word, correction)) = committed {
                    // Same client-ordering rule as the composing branch above:
                    // never install a fresh preedit in front of the forwarded
                    // Space/Return.
                    actions.extend(self.post_commit_pipeline(
                        &word,
                        false,
                        correction,
                        rewrite_allowed,
                        sentence_start,
                    ));
                }
                actions
            }

            // Backspace: trim the current word.
            Key::Backspace => {
                let lang_prior = self
                    .active_hints
                    .as_ref()
                    .and_then(|h| h.lang_prior)
                    .or_else(|| self.lang_detector.momentum_lang());
                if !self.current_word.is_empty() {
                    self.begin_word_edit();
                }
                if let Some(ref mut db) = self.dual_buffer {
                    if !db.is_empty() {
                        db.pop();
                        if db.is_empty() {
                            // All chars deleted — clear preedit entirely.
                            self.dual_buffer = None;
                            self.current_word.clear();
                            self.word_origins.clear();
                            self.terminal_context_lang = None;
                            self.word_delivery_layout = None;
                            self.language_choice_token_policy = false;
                            self.ghost.clear();
                            return vec![Action::HideGhost];
                        }
                        // Full-word composing: re-score and update preedit.
                        // Re-scoring is only legal while the buffer is still in
                        // the hypothesis phase — `handle_dual_buffer_key` gates
                        // the identical call the same way.  Under a LOCKED
                        // buffer `update_scores` rewrites `locked_lang` and
                        // flips the whole word's alphabet, so a single deletion
                        // could turn Cyrillic into Latin mid-word (live trace:
                        // 15 flips on BackSpace, 9 of them locked→locked).
                        if !db.is_locked() {
                            let (ef, bf) = self.engine.score_both(db.en_text(), db.bg_text());
                            db.update_scores(ef, bf);
                            db.apply_soft_context_prior(lang_prior);
                            // The surviving corpus rescore replaces deleted
                            // detector evidence before choosing a ghost model.
                            if self.config.lang_detection
                                && db.evidence_origin()
                                    == crate::dual_buffer::EvidenceOrigin::Corpus
                            {
                                self.lang_detector
                                    .feed_dual_result(db.winner_lang(), db.confidence());
                            }
                        }
                        let word = db.winner_text().to_string();
                        self.replace_word_with_derived(word);
                        let ghost = self.compute_ghost_suffix();
                        return vec![Action::ShowComposing {
                            typed: self.current_word.clone(),
                            ghost,
                        }];
                    }
                }
                // Only an empty token lets this provisional event projection
                // trim a known literal tail. A delivered word or opaque whitespace
                // requires conservative invalidation. Local preedit deletion
                // above, including to empty, retains the complete hint payload.
                self.sentence_boundary = if self.current_word.is_empty() {
                    match self.sentence_boundary {
                        SentenceBoundaryHint::SentenceStart { spaces, tail_count }
                            if spaces > 1 =>
                        {
                            SentenceBoundaryHint::SentenceStart {
                                spaces: spaces - 1,
                                tail_count,
                            }
                        }
                        SentenceBoundaryHint::SentenceStart {
                            spaces: 1,
                            tail_count,
                        } => SentenceBoundaryHint::AwaitingWhitespace { tail_count },
                        SentenceBoundaryHint::AwaitingWhitespace { tail_count }
                            if tail_count > 0 =>
                        {
                            SentenceBoundaryHint::AwaitingWhitespace {
                                tail_count: tail_count - 1,
                            }
                        }
                        _ => SentenceBoundaryHint::None,
                    }
                } else {
                    SentenceBoundaryHint::None
                };
                if !self.current_word.is_empty() {
                    self.pop_word_char();
                }
                if self.current_word.is_empty() {
                    self.terminal_context_lang = None;
                    self.word_delivery_layout = None;
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
                    let rewrite_allowed =
                        self.external_rewrite_supported || self.dual_buffer.is_some();
                    let sentence_start = self.sentence_start_override();
                    let word = self.resolve_word_boundary();
                    let correction = self.commit_boundary_word(&word, rewrite_allowed);
                    self.reset_word();
                    let mut actions = vec![Action::HideGhost, Action::CommitText(word.clone())];
                    // The punctuation key is forwarded only after the adapter
                    // finishes this batch.  Never install a fresh preedit in
                    // front of it; otherwise characters such as `!` can be
                    // consumed by the replacement preedit in some clients.
                    actions.extend(self.post_commit_pipeline(
                        &word,
                        false,
                        correction,
                        rewrite_allowed,
                        sentence_start,
                    ));
                    // Record the delimiter only after processing the word it
                    // terminates; punctuation never enters lexical learning.
                    if matches!(ch, '.' | '!' | '?') {
                        self.sentence_boundary =
                            SentenceBoundaryHint::AwaitingWhitespace { tail_count: 0 };
                    }
                    actions.push(Action::ForwardKey);
                    return actions;
                }

                // Semantic letters are exact glyphs, not physical pairs. Deliver
                // the old preedit once, then keep this entire token in the
                // noncomposing path without committing a word or its evidence.
                let mut actions = if ch.is_alphabetic() {
                    self.handoff_preedit()
                } else {
                    Vec::new()
                };

                // Clear anticipatory ghost if active — user is typing a new word.
                if self.anticipatory_ghost_active {
                    self.anticipatory_ghost_active = false;
                    self.ghost.clear();
                }
                self.push_word_char(ch, !inferred_raw_char);

                // v0.4.1: detection BEFORE prediction (instant classify).
                if self.config.lang_detection {
                    self.lang_detector.feed_char_instant(ch);
                }

                // v0.4.1: transliteration — auto-map Latin to Cyrillic when
                // the user is on the wrong keyboard layout.
                if self.transliteration_active {
                    if let Some(cyrillic) = lang_detect::phonetic_map(ch) {
                        self.pop_word_char(); // Remove the observed Latin char.
                        self.push_word_char(cyrillic, false);
                        let ghost_action = self.update_predictions();
                        // The Latin key was consumed by SmartKey (not forwarded
                        // to the app), so just commit the Cyrillic char directly
                        // — no delete/replace needed.
                        return vec![Action::CommitText(cyrillic.to_string()), ghost_action];
                    }
                }

                // Check for wrong layout on 2nd+ Latin char.
                if self.external_rewrite_supported
                    && self.word_delivery_layout.is_none()
                    && !self.transliteration_active
                    && self.current_word.chars().count() >= 2
                    && self.check_wrong_layout()
                {
                    self.transliteration_active = true;
                    let transliterated = lang_detect::transliterate(&self.current_word);
                    // Only previously-forwarded chars are in the application text.
                    // The current char that triggered detection was consumed (not forwarded),
                    // so subtract 1.
                    let replace_len = self.current_word.chars().count() - 1;
                    self.replace_word_with_derived(transliterated);
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
                if actions.is_empty() {
                    actions.push(ghost_action);
                } else {
                    // The adapter drops a fresh ghost before this forwarded
                    // letter lands. Do not attest or accept that hidden pair.
                    self.ghost.clear();
                    self.last_predictions.clear();
                    self.last_prediction_language = None;
                    self.space_accept_display_attested = false;
                    actions.push(Action::HideGhost);
                }
                actions.push(Action::ForwardKey);
                actions
            }

            // RawCode is resolved before this match — unreachable.
            Key::RawCode(_) => unreachable!("RawCode resolved before match"),
            // An unknown forwarded press may edit the client (e.g. paste).
            Key::Other(_) => {
                self.sentence_boundary = SentenceBoundaryHint::None;
                vec![Action::ForwardKey]
            }
        }
    }

    /// Whether the dual buffer is active but not yet locked (hypothesis phase).
    fn in_hypothesis_phase(&self) -> bool {
        self.dual_buffer.as_ref().is_some_and(|db| !db.is_locked())
    }

    /// Script class for the Space-accept gate: `Some('c')` all-Cyrillic,
    /// `Some('l')` all-Latin, `None` for empty, mixed-script or any other
    /// character (digits, punctuation).  A completion is alphabetic by
    /// construction, so anything else is ineligible — fail closed.
    fn script_class(text: &str) -> Option<char> {
        let mut class = None;
        for ch in text.chars() {
            let c = if ('\u{0400}'..='\u{04FF}').contains(&ch) {
                'c'
            } else if ch.is_ascii_alphabetic() {
                'l'
            } else {
                return None;
            };
            match class {
                None => class = Some(c),
                Some(prev) if prev == c => {}
                _ => return None,
            }
        }
        class
    }

    /// Opt-in Space-accept eligibility (spec 2026-08-22 §2).  Every clause is a
    /// known injection or staleness class: the prefix must live in the
    /// composing preedit (dual buffer), the ghost must be the visible
    /// word-completion (not an anticipatory next-word ghost, not a
    /// transliteration suggestion), and typed + ghost must share one script —
    /// the 12.07 incident committed an English candidate onto Bulgarian typing.
    /// Sensitive fields and focus/cursor transitions never reach this point:
    /// the adapter bypasses the core, and `reset_word()` clears the ghost.
    fn space_accept_eligible(&self) -> bool {
        if self.dual_buffer.is_none()
            || self.current_word.is_empty()
            || self.ghost.is_empty()
            || self.anticipatory_ghost_active
            || self.transliteration_active
            || self.language_choice_policy_active()
        {
            return false;
        }
        // Identity with the DISPLAYED prediction: the composition the user
        // sees (typed + ghost) must be exactly the tracked top prediction the
        // ghost was derived from — normalised like the adapter's "accept ==
        // last shown prediction" check.  No tracked prediction, or a ghost
        // that drifted from it (e.g. injected), is never accepted.
        let shown = format!("{}{}", self.current_word, self.ghost);
        let identity_ok = self
            .last_predictions
            .first()
            .is_some_and(|top| Self::same_normalized(&top.word, &shown));
        if !identity_ok {
            return false;
        }
        matches!(
            (
                Self::script_class(&self.current_word),
                Self::script_class(&self.ghost)
            ),
            (Some(typed), Some(ghost)) if typed == ghost
        )
    }

    fn same_normalized(left: &str, right: &str) -> bool {
        left.trim().to_lowercase() == right.trim().to_lowercase()
    }

    /// Whether the NEXT Space would be an acceptance (flag on + core-local
    /// eligibility).  Read by the master loop before delegation; the master
    /// loop then ATTESTS the display identity (`attest_space_accept_display`)
    /// — the core never accepts on its own predicate alone.
    pub fn space_accept_armed(&self) -> bool {
        self.config.space_accept && self.space_accept_eligible()
    }

    /// Whether this event is a plain Space press (Caps Lock latch allowed),
    /// on either entry path: the keyval
    /// path (`Key::Space`) or the raw-scancode path the live adapter uses for
    /// every key (`Key::RawCode` that the keymap resolves to Space).  The
    /// master loop must not match `Key::Space` alone — raw codes are resolved
    /// only inside `handle_key`, so a raw Space would never be attested.
    pub fn is_space_event(event: &KeyEvent) -> bool {
        if !event.modifiers.difference(Modifiers::CAPS_LOCK).is_empty() {
            return false;
        }
        match event.key {
            Key::Space => true,
            Key::RawCode(code) => matches!(
                keymap::scancode_to_special(code),
                Some(keymap::SpecialKey::Space)
            ),
            _ => false,
        }
    }

    /// One-shot attestation from the display owner (the master loop) that the
    /// composition the core holds is exactly what is currently shown and
    /// tracked for the user.  Consumed by the next key event; absent → Space
    /// stays literal.  (GREEN v4: the adapter's public rejection feedback can
    /// clear the display attribution while core ghost/top remain; the core
    /// alone cannot know that.)
    pub fn attest_space_accept_display(&mut self, ok: bool) {
        self.space_accept_display_attested = ok;
    }

    /// Fold one delimiter into the accept batch.
    ///
    /// The accept path commits the full composition and may follow it with
    /// post-commit `ReplaceWord`s (language correction, casing) sized for that
    /// commit.  Lengthening the commit would break the adapter's same-batch
    /// coalescing and turn the later delete into text corruption
    /// (`hello` + Space + `ReplaceWord(5, Hello)` → `hHello`).  So every
    /// replacement that exactly covers the current commit is absorbed INTO
    /// the commit first, and the delimiter is appended last: the client sees
    /// one commit = final word + one space.  Anything else — no full commit,
    /// a replacement of another length — is refused (`None`).
    fn fold_delimiter(actions: Vec<Action>, full_text: &str) -> Option<Vec<Action>> {
        let commit_idx = actions
            .iter()
            .position(|a| matches!(a, Action::CommitText(t) if t == full_text))?;
        let mut out: Vec<Action> = Vec::with_capacity(actions.len());
        let mut committed = full_text.to_string();
        for (idx, action) in actions.into_iter().enumerate() {
            if idx <= commit_idx {
                out.push(action);
                continue;
            }
            match action {
                Action::ReplaceWord { replace_len, text }
                    if replace_len == committed.chars().count() =>
                {
                    committed = text;
                }
                Action::ReplaceWord { .. } => return None,
                other => out.push(other),
            }
        }
        committed.push(' ');
        out[commit_idx] = Action::CommitText(committed);
        Some(out)
    }

    /// Fold the delimiter, or fall back to the CANONICAL accepted batch.
    ///
    /// By the time this runs the core has already accepted and learned the
    /// completion, so the layers above must see exactly one acceptance: the
    /// safest fallback drops the unfoldable post-commit rewrites and emits
    /// `HideGhost + CommitText(accepted word + one space)` with no
    /// `ForwardKey`.  (Forwarding the key here would make the adapter record a
    /// word-boundary rejection of a word the core just accepted — blocker 3.)
    fn fold_or_canonical(actions: Vec<Action>, full_text: &str) -> Vec<Action> {
        match Self::fold_delimiter(actions, full_text) {
            Some(folded) => folded,
            None => vec![
                Action::HideGhost,
                Action::CommitText(format!("{full_text} ")),
            ],
        }
    }

    /// One-press Space-accept: the shared Tab accept path (learning, metrics,
    /// reset, no ghost re-arm) with the delimiter folded INTO its single
    /// full-composition commit.  No `ForwardKey` is emitted, so the client
    /// cannot receive the space a second time — one press, one delimiter.
    fn accept_ghost_with_delimiter(&mut self) -> Vec<Action> {
        let full_text = format!("{}{}", self.current_word, self.ghost);
        let actions = self.accept_ghost_completion();
        Self::fold_or_canonical(actions, &full_text)
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
        self.accept_ghost_completion_with_cause(CommitCause::AcceptedCompletion)
    }

    fn accept_ghost_completion_with_cause(&mut self, cause: CommitCause) -> Vec<Action> {
        let full_text = format!("{}{}", self.current_word, self.ghost);
        // F8: multi-word ghost may contain spaces — split into words.
        let words: Vec<&str> = full_text.split_whitespace().collect();
        let first_word = words.first().copied().unwrap_or(&full_text);

        let prefix_in_preedit = self.dual_buffer.is_some();
        // A suffix-only or multiword commit cannot replace an externally owned
        // whole token. Capture this before confirmation, learning and reset.
        let rewrite_allowed =
            self.external_rewrite_supported || (prefix_in_preedit && words.len() == 1);
        // Conserve the exact shown composition. First-word phrase casing belongs
        // in its pre-display producer, not this final-word post-commit pipeline.
        let sentence_start = if words.len() == 1 {
            self.sentence_start_override()
        } else {
            None
        };
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
        self.engine.on_prediction_accepted_with_lang(
            first_word,
            &ctx,
            self.last_prediction_language,
        );
        // Record acceptance metrics: find rank in last_predictions.
        let rank = self
            .last_predictions
            .iter()
            .position(|p| p.word == first_word)
            .map(|i| i + 1) // 1-based
            .unwrap_or(1);
        self.metrics
            .record_acceptance(rank, self.ghost.chars().count(), full_text.chars().count());
        let last_word = words.last().copied().unwrap_or("").to_string();
        // A fully accepted word's dictionary confirmation precedes the
        // accepted correction decision, while generic/profile learning occurs
        // exactly once for the text that will actually remain in the client.
        if cause == CommitCause::AcceptedCompletion && self.config.lang_detection {
            for word in &words {
                if word.chars().count() >= 3 {
                    if let Some(language) = Self::word_script(word) {
                        self.engine.confirm_word(&word.to_lowercase(), language);
                    }
                }
            }
        }
        let correction = (self.config.post_commit_autocorrect && rewrite_allowed)
            .then(|| self.prepare_language_correction(&last_word))
            .flatten();
        for (index, word) in words.iter().enumerate() {
            if index + 1 == words.len() {
                if let Some(prepared) = correction.as_ref() {
                    self.commit_corrected_word(prepared, cause);
                    continue;
                }
            }
            self.commit_word_internal(word, cause);
        }
        self.reset_word();
        // Keep the existing accepted correction timing and no anticipatory rearm.
        let mut actions = actions;
        actions.extend(self.post_commit_pipeline(
            &last_word,
            false,
            correction.map(LanguageCorrection::into_action),
            rewrite_allowed,
            sentence_start,
        ));
        actions
    }

    // -- focus / reset --------------------------------------------------

    /// Deliver owned preedit before handing input to a manually selected layout.
    ///
    /// A layout switch is not a word boundary: the user may continue the same
    /// word outside this core. Keep previously learned state, but discard the
    /// old composition and context without accepting or learning a completion.
    pub fn flush_preedit_for_layout(&mut self) -> Vec<Action> {
        self.commit_learning = CommitLearningBatch::default();
        let mut actions = Vec::from_iter(self.flush_preedit());
        self.reset_word();
        self.context.clear();
        self.lang_detector.reset();
        self.clear_hints();
        self.space_accept_display_attested = false;
        actions.push(Action::HideGhost);
        actions
    }

    /// Called when the input field loses focus.
    pub fn focus_lost(&mut self) -> Vec<Action> {
        self.commit_learning = CommitLearningBatch::default();
        // Flush the preedit to the application BEFORE learning from it —
        // the lock gate that used to sit here made the engine learn a word
        // the application never received.
        let mut actions = Vec::from_iter(self.flush_preedit());
        if !self.current_word.is_empty() {
            let word = std::mem::take(&mut self.current_word);
            self.commit_word_internal(&word, CommitCause::FocusFlush);
        }
        self.reset_word();
        actions.push(Action::HideGhost);
        actions
    }

    /// Called when the input field gains focus.
    pub fn focus_gained(&mut self) {
        self.commit_learning = CommitLearningBatch::default();
        self.sentence_boundary = SentenceBoundaryHint::None;
        self.sentence_presentation_override = None;
    }

    /// Called when the cursor moves away from the current typing location.
    ///
    /// Clears preedit state, ghost text, recent context words, and bursty
    /// session cache so stale predictions do not bleed into the new cursor
    /// location after navigation.
    pub fn cursor_moved(&mut self) -> Vec<Action> {
        self.commit_learning = CommitLearningBatch::default();
        // The in-flight word lives ONLY in the preedit; clearing state without
        // flushing it destroys characters the application never received.
        let mut actions = Vec::from_iter(self.flush_preedit());
        self.reset_word();
        self.context.clear();
        self.lang_detector.reset();
        self.engine.clear_session_cache();
        self.active_hints = None;
        actions.push(Action::HideGhost);
        actions
    }

    /// Reset internal state — the CANCEL path. Discards the in-flight preedit.
    ///
    /// Deliberately NOT symmetric with [`Self::focus_lost`] and
    /// [`Self::cursor_moved`], which flush. The asymmetry follows the platform
    /// contract, not a preference:
    ///
    /// * IBus `Reset` means *clear the composition*. It is raised by Escape, by
    ///   a client-side context reset and by an IME switch, and the adapter
    ///   renders the preedit with `PREEDIT_CLEAR` — so those characters were
    ///   never in the application. Emitting `CommitText` here turns a cancel
    ///   into an insertion: the user presses Escape and gets the word anyway.
    /// * Focus loss and cursor navigation are not cancels. The word is real, the
    ///   application never received it, and discarding it silently loses typing.
    ///   Those two paths flush; this one must not.
    ///
    /// An earlier revision flushed here too. A combined core+adapter replay
    /// caught `do_reset` committing a pending preedit — invisible to either
    /// worktree alone, because the core change and the adapter that executes its
    /// actions live in different worktrees.
    pub fn reset(&mut self) -> Vec<Action> {
        self.commit_learning = CommitLearningBatch::default();
        self.reset_word();
        vec![Action::HideGhost]
    }

    pub(crate) fn take_commit_learning(&mut self) -> CommitLearningBatch {
        std::mem::take(&mut self.commit_learning)
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

        // Same rule as accept_ghost_completion (see its doc comment): whenever a
        // dual buffer is active the typed prefix lives in the composing preedit,
        // so the display action must carry it. `in_hypothesis_phase()` is
        // `dual_buffer.is_some() && !locked` — once the buffer locked (the steady
        // state for Bulgarian) it fell to ShowGhost(suffix), and the adapter
        // replaces the whole preedit with the suffix alone: the typed prefix
        // visually vanished mid-word. That bug was fixed on the accept path and
        // left standing here.
        if self.dual_buffer.is_some() {
            Some(Action::ShowComposing {
                typed: self.current_word.clone(),
                ghost: suffix,
            })
        } else {
            Some(Action::ShowGhost(suffix))
        }
    }

    // -- internal helpers -----------------------------------------------

    fn handoff_preedit(&mut self) -> Vec<Action> {
        let Some(db) = self.dual_buffer.take() else {
            return Vec::new();
        };
        // This raw preedit owns the literal semantic key. A legacy latch from
        // an erased token must not map it or bypass delivery of the prefix.
        self.transliteration_active = false;
        self.word_delivery_layout = Some(match db.winner_lang() {
            LangId::Bg => keymap::Layout::Bg,
            LangId::En | LangId::Tech => keymap::Layout::En,
        });
        vec![
            Action::HideGhost,
            Action::CommitText(self.current_word.clone()),
        ]
    }

    fn append_delivered_raw_char(&mut self, ch: char) -> Vec<Action> {
        self.push_word_char(ch, false);
        let ghost = self.update_predictions();
        // ForwardKey delivers the original keyval, which need not match this
        // rendering layout. Consume the key and insert the selected glyph.
        vec![Action::CommitText(ch.to_string()), ghost]
    }

    fn begin_word_edit(&mut self) {
        if !self
            .word_origins
            .iter()
            .map(|origin| origin.character())
            .eq(self.current_word.chars())
        {
            // Missing or externally replaced text cannot borrow observations
            // from another spelling, even after subsequent suffix deletion.
            self.word_origins = self
                .current_word
                .chars()
                .map(WordCharOrigin::Derived)
                .collect();
            self.word_detector_baseline = WordDetectorBaseline::Invalid;
        }
        if matches!(
            &self.word_detector_baseline,
            WordDetectorBaseline::Unstarted
        ) {
            self.word_detector_baseline =
                WordDetectorBaseline::Captured(self.lang_detector.clone());
        }
    }

    fn push_word_char(&mut self, ch: char, direct: bool) {
        self.begin_word_edit();
        self.current_word.push(ch);
        self.word_origins.push(if direct {
            WordCharOrigin::Direct(ch)
        } else {
            WordCharOrigin::Derived(ch)
        });
    }

    fn pop_word_char(&mut self) {
        self.begin_word_edit();
        self.current_word.pop();
        self.word_origins.pop();
        if self.current_word.is_empty() {
            self.language_choice_token_policy = false;
        }
    }

    fn replace_word_with_derived(&mut self, word: String) {
        self.begin_word_edit();
        self.word_origins = word.chars().map(WordCharOrigin::Derived).collect();
        self.current_word = word;
    }

    fn word_is_directly_observed(&self, word: &str) -> bool {
        !word.is_empty()
            && self
                .word_origins
                .iter()
                .copied()
                .eq(word.chars().map(WordCharOrigin::Direct))
    }

    /// Script of the complete rendered word. This is evidence only when
    /// characters were directly observed or the full completion was accepted.
    fn word_script(word: &str) -> Option<LangId> {
        let mut script = None;
        for ch in word.chars().filter(|ch| ch.is_alphabetic()) {
            let lang = LanguageDetector::instant_classify(ch)?;
            match script {
                None => script = Some(lang),
                Some(previous) if previous == lang => {}
                _ => return None,
            }
        }
        script
    }

    /// Candidate routing is independent of learning eligibility. Inferred
    /// display words remain suggestible without becoming language evidence.
    fn candidate_language(&self, word: &str) -> LangId {
        Self::word_script(word).unwrap_or_else(|| self.lang_detector.detected().lang)
    }

    fn word_language_evidence(&self, word: &str, cause: CommitCause) -> WordEvidence {
        if !self.config.lang_detection {
            return WordEvidence::default();
        }
        let script = Self::word_script(word);
        let directly_observed = self.word_is_directly_observed(word);
        if matches!(
            cause,
            CommitCause::AcceptedCompletion | CommitCause::AcceptedLanguageChoice
        ) || (directly_observed && script.is_some())
        {
            return WordEvidence {
                language: script,
                confirmed_word: script,
            };
        }

        // Direct digits/symbols carry real detector evidence without an
        // unambiguous script. Priors, inferred text and partial accepts cannot
        // attest a complete Tech token or promote it to a trusted word.
        if directly_observed
            && word
                .chars()
                .all(|ch| self.lang_detector.has_positive_tech_signal(ch))
            && matches!(&self.word_detector_baseline, WordDetectorBaseline::Captured(start)
                if start.replay_word_detection(word, &self.lang_detector).lang == LangId::Tech)
        {
            return WordEvidence {
                language: Some(LangId::Tech),
                confirmed_word: None,
            };
        }

        // Query the CURRENT rendered full word, never a stored prefix lock or
        // derived winner. This also follows an exact boundary correction and
        // an unmappable-character delivery fallback without fabricating an
        // observed script for either one.
        let (en_support, bg_support) = if cause == CommitCause::PartialCompletionFallback {
            self.engine.score_exact_both(word, word)
        } else {
            self.engine.score_both(word, word)
        };
        let language = match script {
            Some(LangId::En) if en_support > 0.0 => Some(LangId::En),
            Some(LangId::Bg) if bg_support > 0.0 => Some(LangId::Bg),
            _ => None,
        };
        WordEvidence {
            language,
            confirmed_word: None,
        }
    }

    /// Generic learning applies to every delivered word; commit cause and
    /// independently established language/word confirmation have separate roles.
    fn commit_word_internal(&mut self, word: &str, cause: CommitCause) {
        let evidence = self.word_language_evidence(word, cause);
        self.commit_word_with_evidence(word, cause, evidence);
    }

    /// An ordinary boundary cannot change its correction decision by learning
    /// from itself. A prepared replacement has its own exact corpus evidence;
    /// the original character observation does not attest the replacement.
    fn commit_boundary_word(&mut self, word: &str, rewrite_allowed: bool) -> Option<Action> {
        let correction = (self.config.post_commit_autocorrect
            && rewrite_allowed
            && !self.language_choice_policy_active())
        .then(|| self.prepare_language_correction(word))
        .flatten();
        if let Some(prepared) = correction.as_ref() {
            self.commit_corrected_word(prepared, CommitCause::Boundary);
        } else {
            self.commit_word_internal(word, CommitCause::Boundary);
        }
        correction.map(LanguageCorrection::into_action)
    }

    fn commit_corrected_word(&mut self, correction: &LanguageCorrection, cause: CommitCause) {
        let language = (self.config.lang_detection
            && self
                .engine
                .word_frequency(&correction.text.to_lowercase(), correction.language)
                > 0.0)
            .then_some(correction.language);
        if let Some(language) = language {
            self.lang_detector.feed_dual_result(language, 1.0);
        }
        self.commit_word_with_evidence(
            &correction.text,
            cause,
            WordEvidence {
                language,
                confirmed_word: None,
            },
        );
    }

    fn commit_word_with_evidence(
        &mut self,
        word: &str,
        cause: CommitCause,
        evidence: WordEvidence,
    ) {
        if !word.is_empty() {
            let candidate_route = self.candidate_language(word);
            let word_lower = word.to_lowercase();
            let is_oov = self.engine.word_frequency(&word_lower, candidate_route) == 0.0;
            let word_char_len = word.chars().count();
            if !cause.is_prediction_accept() {
                self.metrics.record_commit(word_char_len);
            }

            // Extract context BEFORE pushing new word (for online Markov training).
            let prev1 = self.context.back().map(|s| s.as_str());
            let prev2 = if self.context.len() >= 2 {
                Some(self.context[self.context.len() - 2].as_str())
            } else {
                None
            };

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

            if let Some(language) = evidence.language {
                self.engine.learn_with_lang(&word_lower, language);
                self.lang_detector.record_commit(language);
            }

            // Feed word to session cache LM for burstiness tracking.
            if self.config.use_session_cache_lm {
                self.engine.observe_session(&word_lower);
            }

            // Prefix support can prove language without confirming a new
            // exact word. Only direct/full acceptance promotes trusted words.
            if word_char_len >= 3 {
                if let Some(language) = evidence.confirmed_word {
                    self.engine.confirm_word(&word_lower, language);
                } else if is_oov {
                    self.engine.learn_oov_word(&word_lower, candidate_route);
                }
            }

            let is_flip = self.current_word_had_flip;
            self.regime_detector.observe_word_with_evidence(
                word_char_len,
                is_oov,
                is_flip,
                evidence.language,
            );
            // Propagate the current regime to the engine so predict() can gate δ.
            let (regime, _conf) = self.regime_detector.regime();
            self.engine.set_regime(regime);

            if self.context.len() >= CONTEXT_SIZE {
                self.context.pop_front();
            }
            // Store lowercase for Markov context matching (corpus trained lowercase).
            self.context.push_back(word_lower);
            self.commit_learning.words.push(CommitObservation {
                language: evidence.language,
            });
            self.commit_learning.accepted_prediction |= cause.is_prediction_accept();
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
    fn prepare_language_correction(&self, word: &str) -> Option<LanguageCorrection> {
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
        // never a valid replacement: BG-only letters such as ч/щ/ю were absent
        // from the former ASCII phonetic table, and dropping them produced live
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
            return Some(LanguageCorrection {
                replace_len: typed_chars,
                text: other_word,
                language: other_lang,
            });
        }

        None
    }

    /// Step 2: Auto-capitalize if previous context requires it.
    ///
    /// Fires after sentence-ending punctuation (. ! ? \n) and for proper nouns.
    fn try_auto_capitalize(&self, word: &str, sentence_start: Option<bool>) -> Option<Action> {
        if word.is_empty() {
            return None;
        }

        let first_char = word.chars().next()?;
        if first_char.is_uppercase() {
            return None; // Already capitalized.
        }

        // The current word is already appended; inspect the preceding committed
        // word for sentence-ending punctuation (.!?\n).
        let needs_cap = sentence_start.unwrap_or_else(|| {
            if let Some(prev) = self.context.iter().rev().nth(1) {
                CapsEngine::requires_capitalization(prev)
            } else {
                true // Start of input.
            }
        });

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
            self.last_prediction_language = lang;
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
    /// Accept paths pass ``false`` — the user must type again before a new
    /// prediction is offered.
    ///
    /// Typing-boundary commits (Space/Return, digit and punctuation guards)
    /// originally kept the v0.6.1 behavior and passed ``true``; PR #7 turned
    /// them off as well, because the platform adapter cannot install a fresh
    /// preedit until the client has processed the forwarded delimiter — GTK,
    /// Electron and browser clients otherwise apply the delimiter to the new
    /// ghost instead of to the committed word (the browser typing-duplication
    /// class).  Every call site therefore passes ``false`` today; the parameter
    /// is kept because the anticipatory step itself is unchanged and the
    /// suppression is a per-call-site policy, not a removal.
    fn post_commit_pipeline(
        &mut self,
        committed_word: &str,
        show_anticipatory: bool,
        language_correction: Option<Action>,
        rewrite_allowed: bool,
        sentence_start: Option<bool>,
    ) -> Vec<Action> {
        let mut actions = Vec::new();
        let mut effective_word = committed_word.to_string();

        // Steps 1 and 2 rewrite text that is already committed.  Keep them
        // behind an explicit opt-in so Space/Return can never silently replace
        // the raw word the user typed (live-falsified on Bulgarian ч/ш/щ).
        if self.config.post_commit_autocorrect && rewrite_allowed {
            // Step 1: Language auto-correction.
            if let Some(correction) = language_correction {
                if let Action::ReplaceWord { ref text, .. } = correction {
                    effective_word = text.clone();
                }
                actions.push(correction);
            }

            // Step 2: Auto-capitalization.
            if let Some(cap_action) = self.try_auto_capitalize(&effective_word, sentence_start) {
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

    /// Choose the word to commit at a boundary of a dual-buffer composition.
    ///
    /// The display lock is a scoring signal on the PREFIX; it must not decide
    /// the committed word when the full physical word has exact corpus
    /// support on exactly one side. Both-supported words may switch only
    /// while unlocked, when exact-frequency dominance and current client
    /// context agree. Unsupported words and a `Tech` winner are left alone.
    /// Only one-sided evidence realigns the detector here; client context
    /// never supplies detector confidence or confirms a new trusted word.
    fn resolve_word_boundary(&mut self) -> String {
        // A visible choice leaves genuinely ambiguous readings to the user.
        // Re-check the full token: an early unsupported offer must not block
        // the existing one-sided completed-word correction.
        if self.language_choice_policy_active() {
            return self.current_word.clone();
        }
        let Some(db) = self.dual_buffer.as_ref() else {
            return self.current_word.clone();
        };
        // Semantic input may extend the rendered token without extending the
        // physical buffer. A shorter buffer cannot attest the complete word.
        if self.current_word != db.winner_text() {
            return self.current_word.clone();
        }
        let (en_exact, bg_exact) = self.engine.score_exact_both(db.en_text(), db.bg_text());
        let one_sided = (en_exact > 0.0) != (bg_exact > 0.0);
        let selected = match (en_exact > 0.0, bg_exact > 0.0, db.winner_lang()) {
            (true, false, LangId::Bg) => LangId::En,
            (false, true, LangId::En) => LangId::Bg,
            (true, true, LangId::En | LangId::Bg) if !db.is_locked() => {
                let dominant = if en_exact > bg_exact {
                    LangId::En
                } else if bg_exact > en_exact {
                    LangId::Bg
                } else {
                    return self.current_word.clone();
                };
                if self.terminal_context_lang != Some(dominant) || dominant == db.winner_lang() {
                    return self.current_word.clone();
                }
                dominant
            }
            _ => return self.current_word.clone(),
        };
        let word = match selected {
            LangId::En => db.en_text().to_string(),
            LangId::Bg | LangId::Tech => db.bg_text().to_string(),
        };
        self.replace_word_with_derived(word.clone());
        self.current_word_had_flip = true;
        if self.config.lang_detection && one_sided {
            // Normalised one-sided exact evidence, not a calibrated probability.
            self.lang_detector.feed_dual_result(selected, 1.0);
        }
        word
    }

    /// Commit action for the in-flight composing preedit, if there is one.
    ///
    /// Full-word composing keeps the ENTIRE typed word in the preedit until an
    /// explicit commit, so the application has received **nothing** while a
    /// dual buffer is alive.  Every exit path that clears state must call this
    /// first, otherwise the word is destroyed.
    ///
    /// The dual-buffer lock is an internal scoring signal, **not** a commit
    /// trigger: a locked buffer holds exactly as much unflushed text as an
    /// unlocked one, which is why there is deliberately no `is_locked()` gate
    /// here (72.5% of boundary commits in the live trace came from the locked
    /// state — gating on the lock discarded almost all of them).
    ///
    /// The caller owns the surrounding `reset_word()` / `HideGhost` /
    /// `commit_word_internal()`.
    fn flush_preedit(&self) -> Option<Action> {
        if self.dual_buffer.is_some() && !self.current_word.is_empty() {
            Some(Action::CommitText(self.current_word.clone()))
        } else {
            None
        }
    }

    fn reset_word(&mut self) {
        self.sentence_boundary = SentenceBoundaryHint::None;
        self.sentence_presentation_override = None;
        self.current_word.clear();
        self.terminal_context_lang = None;
        self.word_origins.clear();
        self.word_detector_baseline = WordDetectorBaseline::Unstarted;
        self.ghost.clear();
        self.last_predictions.clear();
        self.last_prediction_language = None;
        self.transliteration_active = false;
        self.dual_buffer = None;
        self.word_delivery_layout = None;
        self.language_choice_token_policy = false;
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
        self.begin_word_edit();
        // Initialize dual buffer on first dual-interpretable key in this word.
        if self.dual_buffer.is_none() {
            self.dual_buffer = Some(DualBuffer::from_config(&self.config.dual_buffer));
        }

        let Some(db) = self.dual_buffer.as_mut() else {
            return vec![Action::ForwardKey];
        };
        let was_locked = db.is_locked();
        db.push(en_ch, bg_ch);

        // An inferred prefix lock can outlive its corpus support as the word
        // grows. Recover only when the current side has none and the other
        // still has support; explicit acceptance and ambiguity stay unchanged.
        let mut rescored = false;
        if !db.is_explicitly_locked() {
            let (en_freq, bg_freq) = self.engine.score_both(db.en_text(), db.bg_text());
            let unsupported_winner = match db.winner_lang() {
                LangId::En => en_freq == 0.0 && bg_freq > 0.0,
                LangId::Bg => bg_freq == 0.0 && en_freq > 0.0,
                LangId::Tech => false,
            };
            if !was_locked || unsupported_winner {
                db.update_scores(en_freq, bg_freq);
                let lang_prior = self
                    .active_hints
                    .as_ref()
                    .and_then(|h| h.lang_prior)
                    .or_else(|| self.lang_detector.momentum_lang());
                db.apply_prior_lock_hint(lang_prior);
                db.apply_soft_context_prior(lang_prior);
                rescored = true;
            }
        }

        // Propagate flip detection from dual buffer to regime detector flag.
        if db.flip_detected() {
            self.current_word_had_flip = true;
            db.clear_flip();
        }

        let word = db.winner_text().to_string();

        // The winner is derived from physical-key interpretations, never an
        // observed Unicode script. Only current corpus scoring may feed it
        // back as independent evidence; a prior/fallback or sticky display
        // lock cannot attest a new character.
        if self.config.lang_detection
            && rescored
            && db.evidence_origin() == crate::dual_buffer::EvidenceOrigin::Corpus
        {
            self.lang_detector
                .feed_dual_result(db.winner_lang(), db.confidence());
        }
        self.replace_word_with_derived(word);

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
            self.last_prediction_language = None;
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
            self.last_prediction_language = None;
            return String::new();
        }

        // Perf gate (§12.4: ≤1% of decisions over 20 ms).  A prefix with no
        // letters cannot complete to a word, but it DOES drive a Levenshtein-2
        // walk of every trie: with a one-character query every depth-1 node is
        // inside the edit budget and the only prune never engages.  Measured on
        // the live trace: '-' n=41 median 32.30 ms / max 41.40, '=' max 53.30,
        // '!' median 30.50 — against letters n=1619 median 0.50 ms, p99 3.10,
        // ZERO samples over 20 ms.  47 of the 49 gate breaches are this one key
        // class.  This is the whole prediction call site for punctuation and
        // digits: `update_predictions()` (Char + Backspace) funnels through it,
        // while the dual-buffer call sites only ever hold letters.
        //
        // WHAT THIS COSTS, ACCEPTED EXPLICITLY.  The mechanism above is about
        // prefix LENGTH — any query at or under the edit budget defeats the
        // prune, including a one-letter one.  This guard is deliberately
        // narrower than the mechanism: it drops only prefixes with no letter at
        // all, which is where 47 of the 49 measured breaches were, and leaves
        // short letter prefixes predicting.  The exchange is not free:
        //
        //   * A word whose prefix so far is all digits or punctuation gets no
        //     prediction until its first letter arrives — "3" in "3D", the
        //     leading run of "5-year".  The prediction reappears at "3D"; the
        //     loss is bounded by that run, never by the whole word.
        //   * A vocabulary entry that begins with a digit is unreachable by
        //     completion for as long as the prefix stays letter-free.
        //
        // That is accepted over the alternatives.  A minimum-length guard would
        // also silence one-letter prefixes, which are the highest-value
        // completions there are.  Raising the prune's aggressiveness inside
        // `ensemble.rs` changes ranking for every query, letters included, and
        // no evaluation has been run for that.  Revisit here only with a
        // measured accuracy comparison, not on the strength of the mechanism.
        if !self.current_word.chars().any(char::is_alphabetic) {
            self.ghost.clear();
            self.last_predictions.clear();
            self.last_prediction_language = None;
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
        self.last_prediction_language = lang;
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
        let force_sentence_start = self
            .sentence_start_override()
            .unwrap_or_else(|| CapsEngine::requires_capitalization(prev_token));

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
        assert!(
            core.current_word().is_empty(),
            "final Right completes and resets the word"
        );
        assert_eq!(core.metrics().summary().accepted_predictions, 1);
        let batch = core.take_commit_learning();
        assert!(batch.accepted_prediction);
        assert_eq!(
            batch.words,
            vec![CommitObservation {
                language: Some(LangId::En)
            }]
        );
        core.handle_key(press(Key::Space));
        assert!(core.take_commit_learning().words.is_empty());
        assert_eq!(core.metrics().summary().accepted_predictions, 1);
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

    /// Build the real physical-key interpretations for Bulgarian "здрав" and
    /// lock the displayed winner to BG.
    fn locked_bg_dual_buffer(core: &InputMethodCore) -> DualBuffer {
        let mut db = DualBuffer::from_config(&core.config.dual_buffer);
        for pair in [('z', 'з'), ('d', 'д'), ('r', 'р'), ('a', 'а'), ('w', 'в')] {
            db.push(pair.0, pair.1);
        }
        db.update_scores(1.0, 100.0);
        assert!(db.is_locked(), "precondition: BG buffer must be locked");
        assert_eq!(db.winner_lang(), LangId::Bg);
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

    /// Right-arrow still accepts one character under locked composing, but the
    /// accepted character must join both physical interpretations and remain
    /// in the preedit.  A one-character CommitText replaces and loses the
    /// composing prefix in the adapter.
    #[test]
    fn test_right_accept_locked_composing_keeps_prefix_and_next_raw_key() {
        let mut core = test_core();
        core.dual_buffer = Some(locked_bg_dual_buffer(&core));
        core.current_word = "здрав".to_string();
        core.ghost = "ей".to_string();

        let actions = core.handle_key(press(Key::Right));

        assert_eq!(
            actions,
            vec![Action::ShowComposing {
                typed: "здраве".to_string(),
                ghost: "й".to_string(),
            }]
        );
        assert_eq!(core.current_word(), "здраве");

        // Physical j/й follows the accepted е. Rebuilding current_word from
        // stale buffers used to drop the accepted character here.
        core.handle_key(press(Key::RawCode(36)));
        assert_eq!(core.current_word(), "здравей");

        let boundary = core.handle_key(press(Key::Space));
        let commits: Vec<&str> = boundary
            .iter()
            .filter_map(|action| match action {
                Action::CommitText(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(commits, ["здравей"]);
        assert!(has_action(&boundary, &Action::ForwardKey));
    }

    /// An unlocked dual buffer has the same preedit ownership as a locked one.
    /// Accepting the displayed completion pins that winner before a later raw
    /// key can re-score the word into a different script.
    #[test]
    fn test_right_accept_unlocked_composing_pins_winner_and_keeps_character() {
        let mut core = test_core();
        let mut db = DualBuffer::from_config(&core.config.dual_buffer);
        db.push('m', 'м');
        db.push('n', 'н');
        db.update_scores(1.0, 100.0);
        assert!(!db.is_locked(), "precondition: dual buffer is unlocked");
        assert_eq!(db.winner_lang(), LangId::Bg);
        core.dual_buffer = Some(db);
        core.current_word = "мн".to_string();
        core.ghost = "ого".to_string();

        let actions = core.handle_key(press(Key::Right));

        assert_eq!(
            actions,
            vec![Action::ShowComposing {
                typed: "мно".to_string(),
                ghost: "го".to_string(),
            }]
        );
        assert_eq!(core.debug_state(), (true, true, false));

        core.handle_key(press(Key::RawCode(34))); // g/г
        assert_eq!(core.current_word(), "мног");
    }

    /// On the final ghost character there is no partial state left to retain:
    /// commit the complete composing word once, record the acceptance, reset.
    #[test]
    fn test_right_accept_final_composing_character_commits_full_word() {
        let mut core = test_core();
        core.dual_buffer = Some(locked_bg_dual_buffer(&core));
        core.current_word = "здраве".to_string();
        core.ghost = "й".to_string();

        let actions = core.handle_key(press(Key::Right));

        assert!(has_action(
            &actions,
            &Action::CommitText("здравей".to_string())
        ));
        assert!(has_action(&actions, &Action::HideGhost));
        assert!(core.dual_buffer.is_none());
        assert_eq!(core.current_word(), "");
        let summary = core.metrics().summary();
        assert_eq!(summary.accepted_predictions, 1);
        assert_eq!(summary.total_commits, 1);
    }

    /// A character absent from the physical layout must never half-update the
    /// dual buffer.  The safe fallback accepts the already-visible word in one
    /// batch and resets the composition.
    #[test]
    fn test_right_accept_unmapped_character_falls_back_atomically() {
        let mut core = test_core();
        core.dual_buffer = Some(locked_bg_dual_buffer(&core));
        core.current_word = "здрав".to_string();
        core.ghost = "ѝx".to_string();

        let actions = core.handle_key(press(Key::Right));

        assert!(has_action(
            &actions,
            &Action::CommitText("здравѝx".to_string())
        ));
        assert!(has_action(&actions, &Action::HideGhost));
        assert!(core.dual_buffer.is_none());
        assert_eq!(core.current_word(), "");
    }

    /// Same bug class as the Tab-accept fix above, on the correction-override
    /// path: with a LOCKED dual buffer the typed prefix lives only in the
    /// composing preedit, so the display action must carry it.
    ///
    /// `apply_prediction_override` tested `in_hypothesis_phase()`
    /// (= `dual_buffer.is_some() && !locked`), so once the buffer locked — the
    /// steady state for Bulgarian — it emitted `ShowGhost(suffix)`. The adapter
    /// replaces the whole preedit with that payload, and the prefix the user had
    /// typed visually disappeared mid-word.
    #[test]
    fn correction_override_keeps_typed_prefix_when_buffer_locked() {
        let mut core = test_core();
        core.load_word("hello", 1_000_000);
        core.dual_buffer = Some(locked_dual_buffer(&core));
        core.current_word = "hel".to_string();

        let (_, locked, hypothesis) = core.debug_state();
        assert!(locked, "test precondition: the dual buffer must be locked");
        assert!(
            !hypothesis,
            "test precondition: a locked buffer is NOT the hypothesis phase — \
             that is exactly the case the old check got wrong"
        );

        let action = core
            .apply_prediction_override("hello")
            .expect("override should produce a display action");

        match action {
            Action::ShowComposing { typed, ghost } => {
                assert_eq!(typed, "hel", "the typed prefix must survive");
                assert_eq!(ghost, "lo");
            }
            other => {
                panic!("locked composing must keep the prefix via ShowComposing, got {other:?}")
            }
        }
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

    /// The gate itself must be load-bearing in BOTH directions.
    ///
    /// The losslessness guard cannot catch "цар" -> "car": both are three
    /// characters, so the transliteration is not lossy and only the default-off
    /// gate refuses it. Until this test existed, forcing the gate to `if true`
    /// (restoring the live corruption) *or* to `if false` (deleting the whole
    /// post-commit subsystem) both left the suite fully green — the one line
    /// this merge cares most about was verified by nothing.
    #[test]
    fn post_commit_autocorrect_gate_is_load_bearing_both_ways() {
        // Default (off): a lossless transliteration must still be refused.
        let mut core = InputMethodCore::new(InputConfig::default());
        core.load_word("car", 5_000_000);
        core.current_word = "цар".to_string();
        let actions = core.handle_key(press(Key::Space));
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, Action::ReplaceWord { .. })),
            "default config must never rewrite text the user typed: {actions:?}"
        );

        // Opt-in (on): the same input must now produce the replacement, which
        // proves the gate suppressed it above rather than some unrelated guard.
        let mut core = InputMethodCore::new(InputConfig {
            post_commit_autocorrect: true,
            ..InputConfig::default()
        });
        core.load_word("car", 5_000_000);
        core.current_word = "цар".to_string();
        let actions = core.handle_key(press(Key::Space));
        assert!(
            actions.iter().any(|a| matches!(
                a,
                Action::ReplaceWord { replace_len: 3, text } if text == "car"
            )),
            "opt-in must re-enable language correction: {actions:?}"
        );
    }

    #[test]
    fn post_commit_autocorrect_physical_brackets_stay_raw_by_default() {
        for (typed, target) in [("шщчю", "[]`\\"), ("ШЩЧЮ", "{}~|")] {
            let mut core = InputMethodCore::new(InputConfig::default());
            core.load_word(target, 20);
            assert_eq!(core.engine.word_frequency(target, LangId::En), 20.0);
            core.current_word = typed.to_string();
            let actions = core.handle_key(press(Key::Space));
            // This keyval path already forwarded the typed characters: Space
            // must forward only the delimiter, without recommitting/replacing.
            assert_eq!(actions, vec![Action::HideGhost, Action::ForwardKey]);
        }
    }

    #[test]
    fn post_commit_autocorrect_physical_brackets_replace_full_word_when_opted_in() {
        for (typed, target) in [("шщчю", "[]`\\"), ("ШЩЧЮ", "{}~|")] {
            let mut core = InputMethodCore::new(InputConfig {
                post_commit_autocorrect: true,
                ..InputConfig::default()
            });
            core.load_word(target, 20);
            assert_eq!(core.engine.word_frequency(target, LangId::En), 20.0);
            assert_eq!(
                core.engine
                    .word_frequency(&typed.to_lowercase(), LangId::Bg),
                0.0
            );
            core.current_word = typed.to_string();
            let actions = core.handle_key(press(Key::Space));
            let replacements: Vec<_> = actions
                .iter()
                .filter_map(|a| match a {
                    Action::ReplaceWord { replace_len, text } => {
                        Some((*replace_len, text.as_str()))
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(replacements, vec![(4, target)]);
        }
    }

    #[test]
    fn post_commit_autocorrect_physical_brackets_require_supported_target() {
        for typed in ["шщчю", "ШЩЧЮ"] {
            let mut core = InputMethodCore::new(InputConfig {
                post_commit_autocorrect: true,
                ..InputConfig::default()
            });
            core.current_word = typed.to_string();
            let actions = core.handle_key(press(Key::Space));
            let replacements: Vec<_> = actions
                .iter()
                .filter_map(|a| match a {
                    Action::ReplaceWord { replace_len, text } => {
                        Some((*replace_len, text.as_str()))
                    }
                    _ => None,
                })
                .collect();
            // The shared opt-in also enables first-word capitalization; it
            // must not permit an unsupported language/layout substitution.
            let expected = if typed == "шщчю" {
                vec![(4, "Шщчю")]
            } else {
                vec![]
            };
            assert_eq!(replacements, expected);
        }
    }

    #[test]
    fn post_commit_autocorrect_physical_brackets_preserve_below_ratio_word() {
        for (typed, target) in [("шщчю", "[]`\\"), ("ШЩЧЮ", "{}~|")] {
            let mut core = InputMethodCore::new(InputConfig {
                post_commit_autocorrect: true,
                ..InputConfig::default()
            });
            core.load_word(target, 20);
            core.load_word(&typed.to_lowercase(), 7);
            assert_eq!(core.engine.word_frequency(target, LangId::En), 20.0);
            assert_eq!(
                core.engine
                    .word_frequency(&typed.to_lowercase(), LangId::Bg),
                7.0
            );
            core.current_word = typed.to_string();
            let actions = core.handle_key(press(Key::Space));
            let replacements: Vec<_> = actions
                .iter()
                .filter_map(|a| match a {
                    Action::ReplaceWord { replace_len, text } => {
                        Some((*replace_len, text.as_str()))
                    }
                    _ => None,
                })
                .collect();
            let expected = if typed == "шщчю" {
                vec![(4, "Шщчю")]
            } else {
                vec![]
            };
            assert_eq!(replacements, expected);
        }
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

    /// Guard (S4 scope fence, live line): the anticipatory pipeline itself was
    /// never dismantled — only specific call sites stopped asking for it.  The
    /// two client-ordering guards above are the stricter statement for the
    /// Space boundary (PR #7 suppresses the re-arm there too, so no ghost is
    /// armed and this parity check holds vacuously); this test survives them
    /// because it pins something they do not: *if* a ghost is ever armed at a
    /// boundary again, it must have come from the anticipatory step and not
    /// from an accept path leaking into the commit batch.
    #[test]
    fn test_space_commit_keeps_anticipatory_pipeline() {
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
        // Whether the ghost SHOWS depends on confidence gates; parity check:
        // if a ghost is armed after Space, it must have come from the
        // anticipatory step (which the accept path never reaches).
        if core.has_ghost() {
            assert!(
                actions.iter().any(|a| matches!(a, Action::ShowGhost(_))),
                "shown ghost must come from the anticipatory step"
            );
        }
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

    // ── Caps Lock (FIX 1) ────────────────────────────────────────────

    /// Live regression (trace seq 2909–2927): pressing Caps_Lock latched
    /// `IBUS_LOCK_MASK`, which never reached `Modifiers`, so an eight-letter
    /// Bulgarian word composed and committed entirely in lowercase.
    #[test]
    fn test_caps_lock_composes_and_commits_uppercase() {
        let mut core = test_core_dual();
        // "zdrawej" on BG phonetic → "здравей" (in the bilingual test corpus).
        // evdev: z=44 d=32 r=19 a=30 w=17 e=18 j=36
        for code in [44u16, 32, 19, 30, 17, 18, 36] {
            core.handle_key(press_raw_with(code, Modifiers::CAPS_LOCK));
        }
        assert_eq!(
            core.current_word(),
            "ЗДРАВЕЙ",
            "Caps Lock must compose uppercase (and stay Cyrillic)"
        );

        let actions = core.handle_key(press_raw_with(57, Modifiers::CAPS_LOCK)); // Space
        assert!(
            has_action(&actions, &Action::CommitText("ЗДРАВЕЙ".to_string())),
            "committed text must be uppercase, got {actions:?}"
        );
    }

    /// Caps Lock is a *letter* latch: it must never uppercase digits or
    /// punctuation — that is what the `is_alpha_scancode` guard buys.
    #[test]
    fn test_caps_lock_does_not_shift_digits_or_punctuation() {
        // evdev 2 = '1'/'!', 12 = '-'/'_', 51 = ','/'<'
        for (code, expected) in [(2u16, "1"), (12, "-"), (51, ",")] {
            let mut core = test_core_dual();
            core.handle_key(press_raw_with(code, Modifiers::CAPS_LOCK));
            assert_eq!(
                core.current_word(),
                expected,
                "Caps Lock must not shift scancode {code}"
            );
        }
        // Shift alone still shifts them.
        let mut core = test_core_dual();
        core.handle_key(press_raw_with(2, Modifiers::SHIFT));
        assert_eq!(core.current_word(), "!", "Shift must still reach digits");
    }

    /// Shift and Caps Lock cancel out on letters (standard latch semantics).
    #[test]
    fn test_caps_lock_plus_shift_yields_lowercase_letter() {
        let mut core = test_core_dual();
        core.handle_key(press_raw_with(
            35, // h / х
            Modifiers::CAPS_LOCK | Modifiers::SHIFT,
        ));
        assert_eq!(
            core.current_word(),
            "h",
            "Shift ^ CapsLock must produce a lowercase letter"
        );
    }

    // ── Backspace vs the dual-buffer lock (FIX 2) ────────────────────

    /// An explicitly accepted buffer keeps its selected alphabet on Backspace.
    /// Unlike inferred prefix confidence, this pin comes from a user accepting
    /// a displayed character; it remains sticky until the token is cleared.
    #[test]
    fn test_backspace_under_explicit_lock_keeps_locked_alphabet() {
        let mut core = InputMethodCore::new(InputConfig::default());
        // 4-char prefix: BG-only.  3-char prefix: EN too — so deleting one
        // character is exactly the case that used to flip the word.
        core.load_word_lang("зднк", 5_000_000, LangId::Bg);
        core.load_word_lang("zdn", 5_000_000, LangId::En);

        let mut db = DualBuffer::from_config(&core.config.dual_buffer);
        for (en, bg) in [('z', 'з'), ('d', 'д'), ('n', 'н')] {
            db.push(en, bg);
        }
        db.update_scores(0.0, 5_000_000.0);
        assert!(db.push_winner_char('к'));
        assert!(db.is_locked(), "precondition: buffer must lock");
        assert_eq!(
            db.winner_text(),
            "зднк",
            "precondition: BG must own the word"
        );
        core.current_word = db.winner_text().to_string();
        core.dual_buffer = Some(db);

        let actions = core.handle_key(press(Key::Backspace));

        assert_eq!(
            core.current_word(),
            "здн",
            "locked Cyrillic buffer must stay Cyrillic after Backspace, got {actions:?}"
        );
        assert!(
            core.dual_buffer.as_ref().is_some_and(DualBuffer::is_locked),
            "the lock must survive the deletion"
        );
        assert!(
            actions.iter().any(|a| matches!(
                a,
                Action::ShowComposing { typed, .. } if typed == "здн"
            )),
            "preedit must be redrawn in the locked alphabet, got {actions:?}"
        );
    }

    /// The hypothesis phase must keep re-scoring on Backspace — the FIX 2
    /// guard must not freeze an unlocked buffer.
    #[test]
    fn test_backspace_unlocked_still_rescores() {
        let mut core = test_core_dual();
        // "mn" → BG "мн" (много, 2.95M) beats EN "mn" (5 370); 2 chars is
        // below min_lock_chars (4), so the buffer is still unlocked.
        core.handle_key(press_raw(50)); // m / м
        core.handle_key(press_raw(49)); // n / н
        core.handle_key(press_raw(34)); // g / г  → "mng" / "мнг": neither scores
        assert!(
            !core.dual_buffer.as_ref().unwrap().is_locked(),
            "precondition: buffer must still be in the hypothesis phase"
        );

        core.handle_key(press_raw(14)); // Backspace → back to "mn" / "мн"
        assert_eq!(
            core.current_word(),
            "мн",
            "unlocked buffer must re-score after Backspace"
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

    // ── Preedit flush on exit paths (FIX 3) ──────────────────────────

    /// Build a core whose composing preedit holds a LOCKED Cyrillic word that
    /// the application has not received a single character of.
    fn composing_locked_core() -> InputMethodCore {
        let mut core = test_core_dual();
        core.dual_buffer = Some(locked_dual_buffer(&core));
        core.current_word = "здрав".to_string();
        core
    }

    fn commit_index(actions: &[Action]) -> Option<usize> {
        actions
            .iter()
            .position(|a| matches!(a, Action::CommitText(t) if t == "здрав"))
    }

    fn hide_index(actions: &[Action]) -> Option<usize> {
        actions.iter().position(|a| *a == Action::HideGhost)
    }

    /// Navigation away from a LOCKED composing preedit must flush the word to
    /// the application first.  `cursor_moved()` used to `reset_word()` straight
    /// into `[HideGhost]`, destroying text the app never saw.
    ///
    /// NOTE: the older `navigation_key_clears_stale_context` test cannot catch
    /// this — it types via `Key::Char`, so no dual buffer ever exists.
    #[test]
    fn test_navigation_flushes_locked_preedit() {
        let mut core = composing_locked_core();
        let actions = core.handle_key(press(Key::Left));

        let ci = commit_index(&actions)
            .unwrap_or_else(|| panic!("Key::Left must commit the composing word, got {actions:?}"));
        let hi = hide_index(&actions).expect("HideGhost must still be emitted");
        assert!(
            ci < hi,
            "CommitText must precede HideGhost, got {actions:?}"
        );
        assert!(has_action(&actions, &Action::ForwardKey));
        assert!(core.dual_buffer.is_none(), "state must still be cleared");
        assert_eq!(core.current_word(), "");
    }

    /// Same for the direct `cursor_moved()` entry point.
    #[test]
    fn test_cursor_moved_flushes_locked_preedit() {
        let mut core = composing_locked_core();
        let actions = core.cursor_moved();
        let ci = commit_index(&actions)
            .unwrap_or_else(|| panic!("cursor_moved must flush the preedit, got {actions:?}"));
        let hi = hide_index(&actions).expect("HideGhost must still be emitted");
        assert!(
            ci < hi,
            "CommitText must precede HideGhost, got {actions:?}"
        );
    }

    /// `reset()` is the CANCEL path and must NOT commit.
    ///
    /// IBus raises Reset for Escape, a client-side context reset and an IME
    /// switch. The adapter runs those actions in both `do_reset` and
    /// `do_disable`, and renders the preedit with `PREEDIT_CLEAR` — so the
    /// characters were never in the application. A flush here turns "the user
    /// cancelled" into "the word was typed", which is text injection.
    #[test]
    fn test_reset_discards_locked_preedit_instead_of_committing() {
        let mut core = composing_locked_core();
        let actions = core.reset();
        assert!(
            commit_index(&actions).is_none(),
            "Reset is a cancel; committing here injects discarded text: {actions:?}"
        );
        hide_index(&actions).expect("HideGhost must still be emitted");
        assert!(core.dual_buffer.is_none());
        assert!(core.current_word.is_empty());
    }

    /// The other half of the split: the non-cancel exits still flush, so a real
    /// word is never silently lost. This pair is the contract — neither test is
    /// meaningful without the other.
    #[test]
    fn test_focus_loss_and_navigation_still_flush_the_preedit() {
        for (label, mut core) in [
            ("focus_lost", composing_locked_core()),
            ("cursor_moved", composing_locked_core()),
        ] {
            let actions = if label == "focus_lost" {
                core.focus_lost()
            } else {
                core.cursor_moved()
            };
            let ci = commit_index(&actions)
                .unwrap_or_else(|| panic!("{label} must flush the preedit, got {actions:?}"));
            let hi = hide_index(&actions).expect("HideGhost must still be emitted");
            assert!(
                ci < hi,
                "{label}: CommitText must precede HideGhost: {actions:?}"
            );
        }
    }

    /// `focus_lost()` learned the word unconditionally but only COMMITTED it
    /// while unlocked — so the engine trained on text the application never
    /// received.  Both halves must now fire regardless of the lock.
    #[test]
    fn test_focus_lost_flushes_locked_preedit() {
        let mut core = composing_locked_core();
        let actions = core.focus_lost();
        let ci = commit_index(&actions)
            .unwrap_or_else(|| panic!("focus_lost must flush the preedit, got {actions:?}"));
        let hi = hide_index(&actions).expect("HideGhost must still be emitted");
        assert!(
            ci < hi,
            "CommitText must precede HideGhost, got {actions:?}"
        );
    }

    /// The flush must be a no-op when there is no composing preedit — plain
    /// `Key::Char` typing is already forwarded character by character, so
    /// re-committing it would double the text.
    #[test]
    fn test_exits_do_not_double_commit_forwarded_chars() {
        for exit in ["cursor_moved", "reset", "focus_lost"] {
            let mut core = test_core();
            core.handle_key(press(Key::Char('h')));
            core.handle_key(press(Key::Char('e')));
            assert!(core.dual_buffer.is_none(), "precondition: no dual buffer");

            let actions = match exit {
                "cursor_moved" => core.cursor_moved(),
                "reset" => core.reset(),
                _ => core.focus_lost(),
            };
            assert!(
                !actions
                    .iter()
                    .any(|a| matches!(a, Action::CommitText(t) if t == "he")),
                "{exit} must not re-commit already-forwarded characters, got {actions:?}"
            );
        }
    }

    // ── Punctuation latency (FIX 4) ──────────────────────────────────

    /// A corpus wide enough that a full Levenshtein-2 walk of the trie is
    /// unmistakable in wall-clock terms, plus a couple of very short words
    /// that a 1-char fuzzy query WOULD match (distance ≤ 2).
    fn wide_corpus_core() -> InputMethodCore {
        let mut core = InputMethodCore::new(InputConfig::default());
        let letters: Vec<char> = ('a'..='z').collect();
        let mut n: usize = 7;
        for _ in 0..20_000 {
            let mut word = String::with_capacity(6);
            for k in 0..6 {
                n = n.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                word.push(letters[(n >> (8 + k)) % 26]);
            }
            core.load_word(&word, 1 + (n % 500) as u32);
        }
        // Reachable from a 1-char query by ≤ 2 edits.
        core.load_word("ab", 900);
        core.load_word("cd", 900);
        core
    }

    /// Ratified performance gate §12.4: ≤1% of decisions over 20 ms.
    ///
    /// Punctuation at a word boundary used to walk every node of every trie
    /// (`ensemble.rs` fuzzy fallback with a one-character query).  Live
    /// measurement: '-' n=41 median 32.30 ms / max 41.40 ms, '=' max 53.30 ms,
    /// '!' median 30.50 ms — 47 of the 49 gate breaches — against letters
    /// n=1619 median 0.50 ms, p99 3.10 ms, zero samples over 20 ms.
    ///
    /// Benchmark-style: real synthetic corpus (20 002 words), real key path.
    ///
    /// The gate is asserted STRUCTURALLY — the expensive call is proven not to
    /// happen — rather than with a stopwatch.  A wall-clock threshold is a
    /// statement about the machine that ran it: it passes on a quiet laptop and
    /// goes red on a loaded CI box, which trains people to re-run red builds.
    /// `latency_sample_count()` only moves when `engine.predict()` is entered,
    /// so pinning it at zero pins the guard itself.  The timing measurement is
    /// kept, one test down, but not as the gate.
    #[test]
    fn test_punctuation_key_never_reaches_the_predictor() {
        let mut core = wide_corpus_core();
        // Warm the engine on the normal (letter) path first — this is what makes
        // the assertion meaningful: the counter is already moving.
        core.handle_key(press(Key::Char('a')));
        core.handle_key(press(Key::Space));
        let warmed = core.metrics().latency_sample_count();
        assert!(warmed > 0, "the letter path must have recorded a sample");

        for ch in ['-', '=', '!', '.', '/'] {
            core.handle_key(press(Key::Char(ch)));
            assert!(
                core.predictions().is_empty(),
                "a letter-free prefix must not run prediction at all ({ch:?})"
            );
            assert_eq!(
                core.metrics().latency_sample_count(),
                warmed,
                "{ch:?} entered engine.predict() — the guard is bypassed, and \
                 that call is the 30-50 ms full-trie Levenshtein-2 walk"
            );
            core.handle_key(press(Key::Space)); // reset for the next key
        }
    }

    /// The same path, measured rather than asserted.
    ///
    /// `#[ignore]` on purpose: this is a machine-dependent measurement, not a
    /// contract, and it must never be the thing that fails a build.  Run it
    /// deliberately when touching the fuzzy walk or the guard:
    /// `cargo test -p smartkey-core -- --ignored --nocapture punctuation_latency`.
    #[test]
    #[ignore = "timing measurement, not a contract — see the structural test above"]
    fn punctuation_latency_measurement() {
        let mut core = wide_corpus_core();
        core.handle_key(press(Key::Char('a')));
        core.handle_key(press(Key::Space));

        let mut worst = std::time::Duration::ZERO;
        for ch in ['-', '=', '!', '.', '/'] {
            let t0 = Instant::now();
            core.handle_key(press(Key::Char(ch)));
            worst = worst.max(t0.elapsed());
            core.handle_key(press(Key::Space));
        }
        println!(
            "worst letter-free keystroke: {:.3} ms (§12.4 budget 20 ms)",
            worst.as_secs_f64() * 1000.0
        );
    }

    /// The guard must be narrow: a word that merely *contains* punctuation or
    /// digits still predicts normally.
    #[test]
    fn test_prediction_still_runs_for_mixed_alphanumeric_prefix() {
        let mut core = test_core();
        core.load_word("utf8mb4", 500);
        for ch in "utf8".chars() {
            core.handle_key(press(Key::Char(ch)));
        }
        assert!(
            core.predictions()
                .iter()
                .any(|p| p.word.eq_ignore_ascii_case("utf8mb4")),
            "a prefix with at least one letter must still predict, got {:?}",
            core.predictions()
                .iter()
                .map(|p| &p.word)
                .collect::<Vec<_>>()
        );
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

/// Step-1 contract for the OPT-IN Space-accept mode (debate 843f34443fd7 →
/// 9c7b7dd85ca6 / 4e02928c0537; spec: smartkey_space_accept_spec_2026-08-22).
///
/// Written RED-first: the `space_accept_on_*` tests below fail on d8b01c0
/// because the flag does not exist yet and Space commits the typed word only.
/// The `*_literal` / `*_unchanged` tests are the guards the ADVOCATE gate
/// requires — they must stay green before, during and after the change.
#[cfg(test)]
mod space_accept_tests {
    use super::*;

    const FLAG_ON: &str = r#"{"accept": {"space_accept": true}}"#;
    const FLAG_ABSENT: &str = "{}";

    #[test]
    fn space_event_requires_a_plain_press_but_allows_caps_lock() {
        for key in [Key::Space, Key::RawCode(57)] {
            for modifiers in [Modifiers::empty(), Modifiers::CAPS_LOCK] {
                assert!(InputMethodCore::is_space_event(&KeyEvent {
                    key: key.clone(),
                    modifiers,
                }));
            }
            for modifiers in [
                Modifiers::SHIFT,
                Modifiers::SUPER,
                Modifiers::CTRL,
                Modifiers::ALT,
                Modifiers::RELEASE,
                Modifiers::SHIFT | Modifiers::CAPS_LOCK,
            ] {
                assert!(
                    !InputMethodCore::is_space_event(&KeyEvent {
                        key: key.clone(),
                        modifiers,
                    }),
                    "{key:?} with {modifiers:?}"
                );
            }
        }
        assert!(!InputMethodCore::is_space_event(&press(Key::Return)));
    }

    #[test]
    fn modified_space_cannot_accept_even_with_display_attestation() {
        for key in [Key::Space, Key::RawCode(57)] {
            for modifiers in [Modifiers::SHIFT, Modifiers::SUPER] {
                let mut core = core_with(FLAG_ON);
                composing(&mut core, "hel", "lo", true);
                let actions = core.handle_key(KeyEvent {
                    key: key.clone(),
                    modifiers,
                });
                assert_eq!(commits(&actions), vec!["hel".to_string()]);
                assert!(has(&actions, &Action::ForwardKey));
            }
        }
    }

    // ── Delimiter folding vs post-commit replacement (bd9219aa448c B) ────

    /// `hello` + Space with a post-commit `ReplaceWord(5, "Hello")` must reach
    /// the client as exactly `Hello ` — one commit, correct casing, one
    /// delimiter, no delete-length mismatch (never `hHello`).
    #[test]
    fn fold_delimiter_absorbs_same_length_post_commit_replace() {
        let actions = vec![
            Action::HideGhost,
            Action::CommitText("hello".to_string()),
            Action::ReplaceWord {
                replace_len: 5,
                text: "Hello".to_string(),
            },
        ];

        let folded = InputMethodCore::fold_delimiter(actions, "hello");

        assert_eq!(
            folded,
            Some(vec![
                Action::HideGhost,
                Action::CommitText("Hello ".to_string())
            ])
        );
    }

    /// Two chained replacements (correction, then casing) fold in order.
    #[test]
    fn fold_delimiter_absorbs_chained_replacements() {
        let actions = vec![
            Action::HideGhost,
            Action::CommitText("hello".to_string()),
            Action::ReplaceWord {
                replace_len: 5,
                text: "hullo".to_string(),
            },
            Action::ReplaceWord {
                replace_len: 5,
                text: "Hullo".to_string(),
            },
        ];

        let folded = InputMethodCore::fold_delimiter(actions, "hello");

        assert_eq!(
            folded,
            Some(vec![
                Action::HideGhost,
                Action::CommitText("Hullo ".to_string())
            ])
        );
    }

    /// A replacement that is NOT sized for the committed word cannot be folded
    /// safely: refuse (caller falls back to a literal, forwarded delimiter).
    #[test]
    fn fold_delimiter_refuses_mismatched_replace() {
        let actions = vec![
            Action::HideGhost,
            Action::CommitText("hello".to_string()),
            Action::ReplaceWord {
                replace_len: 3,
                text: "x".to_string(),
            },
        ];

        assert_eq!(InputMethodCore::fold_delimiter(actions, "hello"), None);
    }

    /// No full-composition commit in the batch: nothing to fold, refuse.
    #[test]
    fn fold_delimiter_refuses_when_full_commit_is_absent() {
        let actions = vec![Action::HideGhost, Action::CommitText("hel".to_string())];

        assert_eq!(InputMethodCore::fold_delimiter(actions, "hello"), None);
    }

    /// Blocker 3 (4f9970749edd): refusal must not contradict the layers. The
    /// core has already accepted/learned, so the safest fallback DROPS the
    /// unfoldable post-commit rewrites and emits the canonical accepted batch —
    /// HideGhost + CommitText(accepted word + exactly one space), no
    /// ForwardKey — so the adapter still sees one acceptance, never a
    /// word_boundary rejection, and O1 gets the accepted word.
    #[test]
    fn space_accept_on_refusal_emits_canonical_commit_without_forward() {
        let actions = vec![
            Action::HideGhost,
            Action::CommitText("hello".to_string()),
            Action::ReplaceWord {
                replace_len: 3,
                text: "x".to_string(),
            },
        ];

        let out = InputMethodCore::fold_or_canonical(actions, "hello");

        assert_eq!(
            out,
            vec![Action::HideGhost, Action::CommitText("hello ".to_string())]
        );
    }

    /// Even a batch without the full-composition commit degrades to the
    /// canonical shape (synthesised), never to a forwarded key.
    #[test]
    fn space_accept_on_refusal_without_full_commit_synthesises_canonical() {
        let actions = vec![Action::HideGhost, Action::CommitText("hel".to_string())];

        let out = InputMethodCore::fold_or_canonical(actions, "hello");

        assert_eq!(
            out,
            vec![Action::HideGhost, Action::CommitText("hello ".to_string())]
        );
    }

    // ── Blocker 1 (4f9970749edd): displayed-prediction identity ─────────────

    fn with_top_prediction(core: &mut InputMethodCore, word: &str) {
        core.last_predictions = vec![Prediction {
            word: word.to_string(),
            score: 1.0,
            confidence: 1.0,
        }];
    }

    /// A ghost that is not exactly the tracked top prediction (what the user
    /// was shown) is never accepted — e.g. a synthetically injected ghost.
    #[test]
    fn space_accept_on_requires_tracked_top_prediction() {
        let mut core = core_with(FLAG_ON);
        composing(&mut core, "hel", "lo", true);
        core.last_predictions.clear();

        let actions = core.handle_key(press(Key::Space));

        assert_eq!(commits(&actions), vec!["hel".to_string()]);
        assert!(has(&actions, &Action::ForwardKey));
    }

    #[test]
    fn space_accept_on_rejects_ghost_mismatching_top_prediction() {
        let mut core = core_with(FLAG_ON);
        composing(&mut core, "hel", "lo", true);
        with_top_prediction(&mut core, "help");

        let actions = core.handle_key(press(Key::Space));

        assert_eq!(commits(&actions), vec!["hel".to_string()]);
        assert!(has(&actions, &Action::ForwardKey));
    }

    /// Identity is normalized (case/whitespace) exactly like the adapter's
    /// "accept == last shown prediction" comparison.
    #[test]
    fn space_accept_on_accepts_exact_normalized_identity() {
        let mut core = core_with(FLAG_ON);
        composing(&mut core, "Hel", "lo", true);
        with_top_prediction(&mut core, "hello");

        let actions = core.handle_key(press(Key::Space));

        assert_eq!(commits(&actions), vec!["Hello ".to_string()]);
        assert!(!has(&actions, &Action::ForwardKey));
    }

    fn core_with(json: &str) -> InputMethodCore {
        let mut config = InputConfig::from_json(json);
        config.ghost_text_separation_margin = 0.0;
        let mut core = InputMethodCore::new(config);
        core.load_word("hello", 100);
        core.load_word("world", 90);
        core
    }

    fn press(key: Key) -> KeyEvent {
        KeyEvent {
            key,
            modifiers: Modifiers::empty(),
        }
    }

    fn has(actions: &[Action], target: &Action) -> bool {
        actions.iter().any(|a| a == target)
    }

    fn commits(actions: &[Action]) -> Vec<String> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::CommitText(s) => Some(s.clone()),
                _ => None,
            })
            .collect()
    }

    /// Locked buffer = steady state after `min_lock_chars`.
    fn locked_db(core: &InputMethodCore) -> DualBuffer {
        let mut db = DualBuffer::from_config(&core.config.dual_buffer);
        for _ in 0..4 {
            db.push('h', 'h');
        }
        db.update_scores(100.0, 1.0);
        assert!(db.is_locked(), "precondition: locked");
        db
    }

    /// Unlocked buffer = the hypothesis phase the operator reports the bug in
    /// („на 2–3 букви": fewer than `min_lock_chars` = 4).
    fn unlocked_db(core: &InputMethodCore) -> DualBuffer {
        let mut db = DualBuffer::from_config(&core.config.dual_buffer);
        db.push('h', 'х');
        db.push('e', 'е');
        db.update_scores(100.0, 1.0);
        assert!(!db.is_locked(), "precondition: hypothesis phase");
        db
    }

    /// Put the core into full-word composing: typed prefix in the preedit plus
    /// a visible word-completion ghost — exactly what the structural trace shows
    /// right before the operator's Space (seq 47 / 205, 2026-08-22).
    fn composing(core: &mut InputMethodCore, typed: &str, ghost: &str, locked: bool) {
        let db = if locked {
            locked_db(core)
        } else {
            unlocked_db(core)
        };
        core.dual_buffer = Some(db);
        core.current_word = typed.to_string();
        core.ghost = ghost.to_string();
        // Direct core tests stand in for the MasterLoop: attest that the
        // composition is the currently displayed one (one-shot, next key).
        core.attest_space_accept_display(!ghost.is_empty());
        // The displayed completion IS the tracked top prediction (what the
        // user was shown) — the identity the accept gate must verify.
        core.last_predictions = if ghost.is_empty() {
            Vec::new()
        } else {
            vec![Prediction {
                word: format!("{typed}{ghost}"),
                score: 1.0,
                confidence: 1.0,
            }]
        };
    }

    // ── RED: the new behaviour ────────────────────────────────────────────

    /// One press, one atomic payload: `typed + ghost + one delimiter`, and the
    /// key is CONSUMED (no ForwardKey) so IBus cannot deliver a second space.
    #[test]
    fn space_accept_on_commits_typed_plus_ghost_plus_one_delimiter() {
        let mut core = core_with(FLAG_ON);
        composing(&mut core, "hel", "lo", true);

        let actions = core.handle_key(press(Key::Space));

        assert_eq!(
            commits(&actions),
            vec!["hello ".to_string()],
            "exactly one commit carrying the word and ONE literal space, got {actions:?}"
        );
        assert!(has(&actions, &Action::HideGhost));
        assert!(
            !has(&actions, &Action::ForwardKey),
            "the Space must be consumed — forwarding it would double the delimiter"
        );
        assert_eq!(core.current_word(), "");
        assert!(!core.has_ghost());
    }

    /// The reported case lives in the hypothesis phase (2–3 letters, unlocked).
    #[test]
    fn space_accept_on_works_in_hypothesis_phase() {
        let mut core = core_with(FLAG_ON);
        composing(&mut core, "he", "llo", false);

        let actions = core.handle_key(press(Key::Space));

        assert_eq!(commits(&actions), vec!["hello ".to_string()]);
        assert!(!has(&actions, &Action::ForwardKey));
    }

    /// Acceptance must feed learning with the ACCEPTED word, not the prefix.
    #[test]
    fn space_accept_on_learns_the_accepted_word() {
        let mut core = core_with(FLAG_ON);
        composing(&mut core, "hel", "lo", true);

        core.handle_key(press(Key::Space));

        assert_eq!(core.context_words(), (Some("hello"), None));
    }

    /// One-shot: the acceptance consumes the ghost; a rapid second Space is an
    /// ordinary literal space (forwarded, nothing committed).
    #[test]
    fn space_accept_on_second_space_is_literal() {
        let mut core = core_with(FLAG_ON);
        composing(&mut core, "hel", "lo", true);
        let first = core.handle_key(press(Key::Space));
        assert_eq!(commits(&first), vec!["hello ".to_string()]);

        let second = core.handle_key(press(Key::Space));

        assert!(
            commits(&second).is_empty(),
            "second Space must not commit, got {second:?}"
        );
        assert!(has(&second, &Action::ForwardKey));
    }

    // ── GUARDS: literal Space everywhere else; Tab/Return unchanged ────────

    #[test]
    fn space_accept_default_off_keeps_literal_space() {
        let mut core = core_with(FLAG_ABSENT);
        composing(&mut core, "hel", "lo", true);

        let actions = core.handle_key(press(Key::Space));

        assert_eq!(commits(&actions), vec!["hel".to_string()]);
        assert!(has(&actions, &Action::ForwardKey));
    }

    #[test]
    fn space_accept_on_no_ghost_is_literal() {
        let mut core = core_with(FLAG_ON);
        composing(&mut core, "hel", "", true);

        let actions = core.handle_key(press(Key::Space));

        assert_eq!(commits(&actions), vec!["hel".to_string()]);
        assert!(has(&actions, &Action::ForwardKey));
    }

    #[test]
    fn space_accept_on_ignores_anticipatory_ghost() {
        let mut core = core_with(FLAG_ON);
        core.ghost = "world".to_string();
        core.anticipatory_ghost_active = true;
        assert!(core.current_word.is_empty());

        let actions = core.handle_key(press(Key::Space));

        assert!(
            commits(&actions).iter().all(|c| !c.contains("world")),
            "anticipatory ghost must never be committed by Space, got {actions:?}"
        );
        assert!(has(&actions, &Action::ForwardKey));
    }

    #[test]
    fn space_accept_on_ignores_transliteration() {
        let mut core = core_with(FLAG_ON);
        composing(&mut core, "hel", "lo", true);
        core.transliteration_active = true;

        let actions = core.handle_key(press(Key::Space));

        assert_eq!(commits(&actions), vec!["hel".to_string()]);
        assert!(has(&actions, &Action::ForwardKey));
    }

    /// Cross-script completion = the 12.07 injection class. Never accepted.
    #[test]
    fn space_accept_on_rejects_script_mismatch() {
        let mut core = core_with(FLAG_ON);
        composing(&mut core, "зд", "rav", false);

        let actions = core.handle_key(press(Key::Space));

        assert_eq!(commits(&actions), vec!["зд".to_string()]);
        assert!(has(&actions, &Action::ForwardKey));
    }

    /// A ghost that was visible before a focus transition is stale: the word
    /// was flushed on focus-out, the following Space is literal.
    #[test]
    fn space_accept_on_stale_ghost_after_focus_lost_is_literal() {
        let mut core = core_with(FLAG_ON);
        composing(&mut core, "hel", "lo", true);
        let flushed = core.focus_lost();
        assert_eq!(commits(&flushed), vec!["hel".to_string()]);

        let actions = core.handle_key(press(Key::Space));

        assert!(
            commits(&actions).is_empty(),
            "stale ghost must not be accepted, got {actions:?}"
        );
        assert!(has(&actions, &Action::ForwardKey));
    }

    #[test]
    fn space_accept_on_return_unchanged() {
        let mut core = core_with(FLAG_ON);
        composing(&mut core, "hel", "lo", true);

        let actions = core.handle_key(press(Key::Return));

        assert_eq!(commits(&actions), vec!["hel".to_string()]);
        assert!(has(&actions, &Action::ForwardKey));
    }

    /// GREEN v4: without the MasterLoop's one-shot display attestation the
    /// core never accepts on Space, however eligible its own state looks.
    #[test]
    fn space_accept_on_requires_display_attestation() {
        let mut core = core_with(FLAG_ON);
        composing(&mut core, "hel", "lo", true);
        core.attest_space_accept_display(false);

        let actions = core.handle_key(press(Key::Space));

        assert_eq!(commits(&actions), vec!["hel".to_string()]);
        assert!(has(&actions, &Action::ForwardKey));
    }

    /// The attestation is one-shot: it covers exactly the next key.
    #[test]
    fn space_accept_display_attestation_is_consumed_by_the_next_key() {
        let mut core = core_with(FLAG_ON);
        composing(&mut core, "hel", "lo", true); // helper attests for the next key
        core.handle_key(press(Key::Escape)); // consumes the attestation, keeps the prefix
                                             // Restore an eligible composition WITHOUT a fresh attestation.
        core.ghost = "lo".to_string();
        with_top_prediction(&mut core, "hello");

        let actions = core.handle_key(press(Key::Space));

        assert_eq!(commits(&actions), vec!["hel".to_string()]);
        assert!(has(&actions, &Action::ForwardKey));
    }

    #[test]
    fn space_accept_on_tab_unchanged() {
        let mut core = core_with(FLAG_ON);
        composing(&mut core, "hel", "lo", true);

        let actions = core.handle_key(press(Key::Tab));

        assert_eq!(
            commits(&actions),
            vec!["hello".to_string()],
            "Tab never adds a delimiter"
        );
        assert!(!has(&actions, &Action::ForwardKey));
    }

    // ── Config parser coverage (ADVOCATE_CODEX 3037035ecb52) ───────────────

    #[test]
    fn config_space_accept_defaults_off() {
        assert!(!InputConfig::default().space_accept);
        assert!(!InputConfig::from_json(FLAG_ABSENT).space_accept);
        assert!(
            !InputConfig::try_from_json(FLAG_ABSENT)
                .unwrap()
                .space_accept
        );
    }

    #[test]
    fn config_space_accept_parses_true_and_false() {
        assert!(InputConfig::from_json(FLAG_ON).space_accept);
        assert!(InputConfig::try_from_json(FLAG_ON).unwrap().space_accept);
        let off = r#"{"accept": {"space_accept": false}}"#;
        assert!(!InputConfig::from_json(off).space_accept);
        assert!(!InputConfig::try_from_json(off).unwrap().space_accept);
    }

    /// Fail-closed: a non-boolean value never switches the feature on.
    #[test]
    fn config_space_accept_invalid_type_is_off() {
        for bad in [
            r#"{"accept": {"space_accept": "yes"}}"#,
            r#"{"accept": {"space_accept": 1}}"#,
            r#"{"accept": "space_accept"}"#,
            r#"{"accept": null}"#,
        ] {
            assert!(!InputConfig::from_json(bad).space_accept, "{bad}");
            assert!(
                !InputConfig::try_from_json(bad).unwrap().space_accept,
                "{bad}"
            );
        }
    }

    // ── Boundary / multilingual guards (ADVOCATE_CODEX 3037035ecb52) ──────

    /// Punctuation never goes through the Space arm: the typed word is
    /// committed verbatim and the key is forwarded, flag or no flag.
    #[test]
    fn space_accept_on_punctuation_boundary_stays_literal() {
        let mut core = core_with(FLAG_ON);
        composing(&mut core, "hel", "lo", true);

        let actions = core.handle_key(press(Key::Char('!')));

        assert_eq!(commits(&actions), vec!["hel".to_string()]);
        assert!(has(&actions, &Action::ForwardKey));
    }

    #[test]
    fn space_accept_on_digit_boundary_stays_literal() {
        let mut core = core_with(FLAG_ON);
        composing(&mut core, "hel", "lo", true);

        let actions = core.handle_key(press(Key::Char('1')));

        assert_eq!(commits(&actions), vec!["hel".to_string()]);
        assert!(has(&actions, &Action::ForwardKey));
    }

    /// Latin prefix with a Cyrillic completion, and a prefix that already
    /// mixes scripts — both are the mid-word flip the 12.07 incident was made
    /// of. Never accepted.
    #[test]
    fn space_accept_on_multilingual_mid_word_rejected() {
        let mut core = core_with(FLAG_ON);
        composing(&mut core, "hel", "ло", true);
        let actions = core.handle_key(press(Key::Space));
        assert_eq!(commits(&actions), vec!["hel".to_string()]);
        assert!(has(&actions, &Action::ForwardKey));

        let mut core = core_with(FLAG_ON);
        composing(&mut core, "hе", "llo", false); // 'h' Latin + 'е' Cyrillic
        let actions = core.handle_key(press(Key::Space));
        assert_eq!(commits(&actions), vec!["hе".to_string()]);
        assert!(has(&actions, &Action::ForwardKey));
    }
}

/// B9 / F1 (2026-08-23, ADVOCATE_CODEX ruling 487688994026): a contextual
/// language prior (surrounding text / typing momentum) may bias the first
/// character of a dual-buffer word but must never lock it — corpus evidence
/// on char 2+ decides. Live defect: after an xkb→SmartKey switch inside a
/// Latin-surrounded terminal, Bulgarian words were committed as the EN keymap
/// text („имаш" → "ima[", „след" → "sled", „превключване" → "prewkl`wane").
#[cfg(test)]
mod lang_prior_tests {
    use super::*;
    use crate::lang_detect::LangId;

    fn press_raw(code: u16) -> KeyEvent {
        KeyEvent {
            key: Key::RawCode(code),
            modifiers: Modifiers::empty(),
        }
    }

    /// Bilingual corpus shaped like the live one: the Bulgarian words of the
    /// operator's report plus ordinary English words that share physical keys.
    fn bilingual_core() -> InputMethodCore {
        let mut core = InputMethodCore::new(InputConfig::default());
        assert!(core.config.dual_buffer.enabled);
        for (w, f) in [
            ("имаш", 120_000u32),
            ("има", 900_000),
            ("след", 400_000),
            ("превключване", 30_000),
            ("отговор", 250_000),
            ("то", 700_000),
        ] {
            core.load_word(w, f);
        }
        for (w, f) in [
            ("is", 4_000_000u32),
            ("image", 600_000),
            ("sled", 20_000),
            ("the", 9_000_000),
            ("hello", 4_800_000),
            ("to", 5_000_000),
        ] {
            core.load_word(w, f);
        }
        core
    }

    fn with_prior(core: &mut InputMethodCore, prior: LangId) {
        core.apply_hints(&crate::master_loop::Hints {
            lang_prior: Some(prior),
            ..Default::default()
        });
    }

    /// Physical keys (evdev) for the reported words.
    const IMASH: [u16; 4] = [23, 50, 30, 26]; // i m a [  → имаш
    const SLED: [u16; 4] = [31, 38, 18, 32]; // s l e d  → след
    const OTGOWOR: [u16; 7] = [24, 20, 34, 24, 17, 24, 19]; // o t g o w o r → отговор
    const PREWKL: [u16; 6] = [25, 19, 18, 17, 37, 38]; // p r e w k l → превкл(ючване)
    const IS: [u16; 2] = [23, 31];
    const HELLO: [u16; 5] = [35, 18, 38, 38, 24];
    const TO: [u16; 2] = [20, 24];

    fn type_word(core: &mut InputMethodCore, codes: &[u16]) -> String {
        for &c in codes {
            core.handle_key(press_raw(c));
        }
        core.current_word().to_string()
    }

    // ── RED: the reported alternating pattern must come out Cyrillic ───────

    #[test]
    fn bulgarian_words_after_english_context_are_cyrillic() {
        let mut core = bilingual_core();
        for (codes, expected) in [
            (&IMASH[..], "имаш"),
            (&SLED[..], "след"),
            (&OTGOWOR[..], "отговор"),
            (&PREWKL[..], "превкл"),
        ] {
            with_prior(&mut core, LangId::En); // Latin surrounding / EN momentum
            let got = type_word(&mut core, codes);
            assert_eq!(got, expected, "EN prior must not lock a Bulgarian word");
            core.handle_key(press_raw(57)); // Space
        }
    }

    #[test]
    fn first_char_prior_is_a_bias_not_a_lock() {
        let mut core = bilingual_core();
        with_prior(&mut core, LangId::En);
        core.handle_key(press_raw(23)); // i / и

        let (dual, locked, hypothesis) = core.debug_state();
        assert!(dual);
        assert!(!locked, "char-1 prior must leave the buffer unlocked");
        assert!(hypothesis);
    }

    // ── GUARDS: the prior still helps genuine English; invariants hold ─────

    #[test]
    fn genuine_english_after_english_context_stays_english() {
        let mut core = bilingual_core();
        with_prior(&mut core, LangId::En);
        assert_eq!(type_word(&mut core, &IS), "is");
        core.handle_key(press_raw(57));
        with_prior(&mut core, LangId::En);
        assert_eq!(type_word(&mut core, &HELLO), "hello");
    }

    /// Short, corpus-ambiguous word ("to" vs „то"): the English context must
    /// still decide it — the bias is kept when the corpus cannot separate.
    #[test]
    fn short_ambiguous_english_word_keeps_the_english_context() {
        let mut core = bilingual_core();
        with_prior(&mut core, LangId::En);

        assert_eq!(type_word(&mut core, &TO), "to");
    }

    /// Symmetry: a Bulgarian context must not trap a genuine English word.
    #[test]
    fn english_word_after_bulgarian_context_is_rescored_to_english() {
        let mut core = bilingual_core();
        with_prior(&mut core, LangId::Bg);

        assert_eq!(type_word(&mut core, &HELLO), "hello");
    }

    /// Typing momentum is the other prior source: after a committed
    /// Bulgarian word the momentum says BG, yet a clearly English word typed
    /// next must still be re-scored to English (on the old code it locked to
    /// „ис").
    #[test]
    fn momentum_prior_after_bulgarian_word_does_not_lock_english() {
        let mut core = bilingual_core();
        assert_eq!(type_word(&mut core, &IMASH), "имаш");
        core.handle_key(press_raw(57));
        assert_eq!(type_word(&mut core, &IS), "is");
    }

    // ── S03/P3a RED: exact one-sided evidence must win at the boundary ─────
    //
    // An early EN display lock on the prefix ("stat") must not decide the
    // committed word when the full physical word has exact support on
    // exactly ONE side (BG "статия"; no EN word starts with "statiq").
    // Desired: one CommitText("статия"), one forwarded delimiter, no
    // ReplaceWord, empty composition.  The guards below pin the readings
    // that must NOT change with that fix.

    /// s t a t i q → EN "statiq" / BG phonetic "статия".
    const STATIQ: [u16; 6] = [31, 20, 30, 20, 23, 16];
    /// s t a t u s / s t a t i c / s t a t i o n.
    const STATUS: [u16; 6] = [31, 20, 30, 20, 22, 31];
    const STATIC: [u16; 6] = [31, 20, 30, 20, 23, 46];
    const STATION: [u16; 7] = [31, 20, 30, 20, 23, 24, 49];
    /// s t a t i x: no exact support on either side.
    const STATIX: [u16; 6] = [31, 20, 30, 20, 23, 45];

    /// Controlled one-sided corpus: EN has strong prefix support for "stat"
    /// through longer words only (EN-exact for "statiq" = 0); BG has exact
    /// support for the full word only, weaker than the EN prefix, so the
    /// display locks EN at char 4.
    fn one_sided_exact_core() -> InputMethodCore {
        let mut core = InputMethodCore::new(InputConfig::default());
        assert!(core.config.dual_buffer.enabled);
        for (w, f) in [
            ("status", 900_000u32),
            ("static", 600_000),
            ("station", 500_000),
        ] {
            core.load_word_lang(w, f, LangId::En);
        }
        core.load_word_lang("статия", 50_000, LangId::Bg);
        core
    }

    /// (committed texts, forwarded delimiters, replacements) of one action list.
    fn delimiter_shape(actions: &[Action]) -> (Vec<String>, usize, usize) {
        let commits = actions
            .iter()
            .filter_map(|a| match a {
                Action::CommitText(t) => Some(t.clone()),
                _ => None,
            })
            .collect();
        let forwards = actions
            .iter()
            .filter(|a| matches!(a, Action::ForwardKey))
            .count();
        let replaces = actions
            .iter()
            .filter(|a| matches!(a, Action::ReplaceWord { .. }))
            .count();
        (commits, forwards, replaces)
    }

    #[test]
    fn exact_statia_boundary_reaches_one_committed_word() {
        let mut core = one_sided_exact_core();
        // "stat": EN 900_000 vs BG 50_000 → 0.947 ≥ 0.85 at four chars.
        type_word(&mut core, &STATIQ[..4]);
        let (dual, locked, _) = core.debug_state();
        assert!(
            dual && locked,
            "precondition: early EN display lock on the prefix"
        );
        // "iq": no EN word continues "statiq"; BG "статия" is exact.
        type_word(&mut core, &STATIQ[4..]);

        let (commits, forwards, replaces) = delimiter_shape(&core.handle_key(press_raw(57)));
        assert_eq!(commits, vec!["статия".to_string()]);
        assert_eq!(forwards, 1);
        assert_eq!(replaces, 0);
        assert_eq!(core.current_word(), "");
    }

    // ── Guards (expected PASS): readings a boundary fix must not change ────

    /// EN-exact > 0, BG-exact = 0: the English word stays English.
    #[test]
    fn english_status_static_station_commit_english() {
        for (codes, expected) in [
            (&STATUS[..], "status"),
            (&STATIC[..], "static"),
            (&STATION[..], "station"),
        ] {
            let mut core = one_sided_exact_core();
            type_word(&mut core, codes);
            let (commits, forwards, replaces) = delimiter_shape(&core.handle_key(press_raw(57)));
            assert_eq!(commits, vec![expected.to_string()]);
            assert_eq!(forwards, 1);
            assert_eq!(replaces, 0);
        }
    }

    /// EN-exact = 0 and BG-exact = 0: no crossover, the displayed reading is
    /// committed as typed (unsupported input acquires no new policy).
    #[test]
    fn unsupported_both_keeps_displayed_reading() {
        let mut core = one_sided_exact_core();
        type_word(&mut core, &STATIX);
        let (commits, forwards, replaces) = delimiter_shape(&core.handle_key(press_raw(57)));
        assert_eq!(commits, vec!["statix".to_string()]);
        assert_eq!(forwards, 1);
        assert_eq!(replaces, 0);
    }

    /// Both sides have exact support ("to" 5_000_000 / "то" 700_000): the
    /// existing evidence decides, the two-supported policy is untouched.
    #[test]
    fn both_supported_short_word_keeps_current_reading() {
        let mut core = bilingual_core();
        type_word(&mut core, &TO);
        let (commits, forwards, replaces) = delimiter_shape(&core.handle_key(press_raw(57)));
        assert_eq!(commits, vec!["to".to_string()]);
        assert_eq!(forwards, 1);
        assert_eq!(replaces, 0);
    }

    // ── GREEN state guards: what the boundary selection must carry along ──

    fn press_key(key: Key) -> KeyEvent {
        KeyEvent {
            key,
            modifiers: Modifiers::empty(),
        }
    }

    /// The selected script drives the detector, momentum and context that
    /// `commit_word_internal` learns from — not the display lock.
    #[test]
    fn detector_and_momentum_follow_the_selected_word() {
        let mut core = one_sided_exact_core();
        core.config.lang_detection = true;
        type_word(&mut core, &STATIQ);
        core.handle_key(press_raw(57));
        assert_eq!(core.lang_detector.detected().lang, LangId::Bg);
        assert_eq!(core.lang_detector.momentum_lang(), Some(LangId::Bg));
        assert_eq!(core.context.back().map(String::as_str), Some("статия"));

        let mut core = one_sided_exact_core();
        core.config.lang_detection = true;
        type_word(&mut core, &STATUS);
        core.handle_key(press_raw(57));
        assert_eq!(core.lang_detector.detected().lang, LangId::En);
        assert_eq!(core.lang_detector.momentum_lang(), Some(LangId::En));
        assert_eq!(core.context.back().map(String::as_str), Some("status"));
    }

    /// Return is a literal delimiter like Space: same selection, same shape.
    #[test]
    fn return_boundary_uses_the_same_selection() {
        let mut core = one_sided_exact_core();
        type_word(&mut core, &STATIQ);
        let (commits, forwards, replaces) =
            delimiter_shape(&core.handle_key(press_key(Key::Return)));
        assert_eq!(commits, vec!["статия".to_string()]);
        assert_eq!(forwards, 1);
        assert_eq!(replaces, 0);
        assert_eq!(core.current_word(), "");
    }

    /// The non-alphabetic boundary (punctuation) uses the same selection.
    #[test]
    fn punctuation_boundary_uses_the_same_selection() {
        let mut core = one_sided_exact_core();
        type_word(&mut core, &STATIQ);
        let (commits, forwards, replaces) =
            delimiter_shape(&core.handle_key(press_key(Key::Char(','))));
        assert_eq!(commits, vec!["статия".to_string()]);
        assert_eq!(forwards, 1);
        assert_eq!(replaces, 0);
        assert_eq!(core.current_word(), "");
    }

    /// Focus loss delivers the displayed reading; prefix recovery has already
    /// selected Bulgarian before this flush, which makes no new layout choice.
    #[test]
    fn focus_lost_flush_keeps_the_displayed_reading() {
        let mut core = one_sided_exact_core();
        type_word(&mut core, &STATIQ);
        assert_eq!(core.current_word(), "статия");
        let (commits, forwards, replaces) = delimiter_shape(&core.focus_lost());
        assert_eq!(commits, vec!["статия".to_string()]);
        assert_eq!(forwards, 0);
        assert_eq!(replaces, 0);
        assert_eq!(core.current_word(), "");
    }

    // ── Deferred flush task: Tab/Right characterizations on the same fixture ──

    /// One-sided fixture with ghost text disabled, so Tab/Right exercise the
    /// flush paths and never an accept.
    fn one_sided_exact_core_without_ghost() -> InputMethodCore {
        let mut core = one_sided_exact_core();
        core.config.ghost_text = false;
        core
    }

    /// Tab without a ghost flushes the already-recovered displayed reading,
    /// without accepting a prediction or making another boundary selection.
    #[test]
    fn tab_flush_keeps_the_displayed_reading() {
        let mut core = one_sided_exact_core_without_ghost();
        type_word(&mut core, &STATIQ);
        assert_eq!(core.current_word(), "статия");
        assert!(core.ghost.is_empty(), "precondition: no ghost to accept");
        let (commits, forwards, replaces) = delimiter_shape(&core.handle_key(press_key(Key::Tab)));
        assert_eq!(commits, vec!["статия".to_string()]);
        assert_eq!(forwards, 1);
        assert_eq!(replaces, 0);
        assert_eq!(core.current_word(), "");
    }

    /// Right without a ghost is a cursor move: flush the recovered reading,
    /// then reset the word, context and detector.
    #[test]
    fn right_without_ghost_flushes_and_resets_context() {
        let mut core = one_sided_exact_core_without_ghost();
        type_word(&mut core, &STATIQ);
        assert_eq!(core.current_word(), "статия");
        assert!(core.ghost.is_empty(), "precondition: no ghost to accept");
        let (commits, forwards, replaces) =
            delimiter_shape(&core.handle_key(press_key(Key::Right)));
        assert_eq!(commits, vec!["статия".to_string()]);
        assert_eq!(forwards, 1);
        assert_eq!(replaces, 0);
        assert_eq!(core.current_word(), "");
        assert!(core.context.is_empty());
        let detected = core.lang_detector.detected();
        assert_eq!(detected.lang, LangId::En);
        assert_eq!(detected.confidence, 0.0);
    }

    // ── S03-EXACT-FREQ-SHADOW (RED): post-commit correction, public path ──

    /// EN "stat" (7) is shadowed by "status" (900_000) in the completion
    /// search; BG "стат" (20) and "ви" (20) are exact.  Post-commit
    /// auto-correction is switched on for this fixture only.
    fn shadowed_exact_core() -> InputMethodCore {
        let config = InputConfig {
            ghost_text: false,
            space_accept: false,
            post_commit_autocorrect: true,
            ..InputConfig::default()
        };
        let mut core = InputMethodCore::new(config);
        assert!(core.config.dual_buffer.enabled);
        core.load_word_lang("stat", 7, LangId::En);
        core.load_word_lang("status", 900_000, LangId::En);
        core.load_word_lang("стат", 20, LangId::Bg);
        core.load_word_lang("ви", 20, LangId::Bg);
        core
    }

    /// Desired: a typed word that exists (7) is not language-corrected to a
    /// transliteration that is merely 20/7 ≈ 2.9× more frequent (below the
    /// 5× rule).  RED today: `word_frequency("stat")` is shadowed to 0.0, so
    /// the correction fires and a ReplaceWord("стат") follows the commit.
    #[test]
    fn shadowed_typed_word_is_not_language_corrected_on_the_public_path() {
        let mut core = shadowed_exact_core();
        type_word(&mut core, &STATIQ[..4]);
        assert_eq!(core.current_word(), "stat");
        assert_eq!(core.engine.score_exact_both("stat", "стат"), (7.0, 20.0));

        let actions = core.handle_key(press_raw(57));
        let (commits, forwards, replaces) = delimiter_shape(&actions);
        assert_eq!(commits, vec!["stat".to_string()]);
        assert_eq!(forwards, 1);
        assert_eq!(replaces, 1);
        let replacements: Vec<_> = actions
            .iter()
            .filter_map(|a| match a {
                Action::ReplaceWord { replace_len, text } => Some((*replace_len, text.as_str())),
                _ => None,
            })
            .collect();
        // First-word capitalization is enabled by the same opt-in; the
        // exact action must preserve the typed language and whole word.
        assert_eq!(replacements, vec![(4, "Stat")]);
        assert_eq!(core.current_word(), "");
    }

    /// Guard (expected PASS): a typed word that is genuinely absent from its
    /// own model is still corrected when the other side is supported.
    #[test]
    fn genuinely_absent_typed_word_is_still_corrected() {
        let core = shadowed_exact_core();
        // Physical phonetic spelling of the same supported BG word "ви".
        assert_eq!(crate::lang_detect::transliterate("wi"), "ви");
        assert_eq!(
            core.prepare_language_correction("wi")
                .map(LanguageCorrection::into_action),
            Some(Action::ReplaceWord {
                replace_len: 2,
                text: "ви".to_string(),
            })
        );
    }

    // ── S03/P3b Caps: BG-only letters on the bracket keys ─────────────────
    //
    // Before `keymap::scancode_to_both_with_caps` the RawCode arm resolved
    // ONE shift state for both layouts and treated evdev 26/27/41/43 as
    // non-letters (is_alpha_scancode), so Caps Lock never uppercased
    // ш/щ/ч/ю and Shift+Caps never lowercased them, even inside a locked
    // Bulgarian word.  The eight cases below were the RED for that defect
    // and now pin the per-layout latch; the guards pin the English bracket
    // symbols that must not change.  Fixtures are one-sided synthetic
    // corpora, ghost text off, post-commit flags at their defaults.

    fn press_raw_with(code: u16, modifiers: Modifiers) -> KeyEvent {
        KeyEvent {
            key: Key::RawCode(code),
            modifiers,
        }
    }

    /// m a m a on the raw route (evdev 50, 30, 50, 30) → EN "mama" / BG "мама".
    const MAMA: [u16; 4] = [50, 30, 50, 30];
    /// t e s t on the raw route (evdev 20, 18, 31, 20) → EN "test" / BG "тест".
    const TEST: [u16; 4] = [20, 18, 31, 20];
    /// (evdev code, BG lower, BG upper, EN unshifted, EN shifted).
    const BRACKET_KEYS: [(u16, char, char, char, char); 4] = [
        (26, 'ш', 'Ш', '[', '{'),
        (27, 'щ', 'Щ', ']', '}'),
        (41, 'ч', 'Ч', '`', '~'),
        (43, 'ю', 'Ю', '\\', '|'),
    ];

    fn no_ghost_config() -> InputConfig {
        InputConfig {
            ghost_text: false,
            ..InputConfig::default()
        }
    }

    /// One-sided BG fixture: only "мама" is known, no English support at all.
    fn bg_only_core() -> InputMethodCore {
        let mut core = InputMethodCore::new(no_ghost_config());
        assert!(core.config.dual_buffer.enabled);
        core.load_word_lang("мама", 100_000, LangId::Bg);
        core
    }

    /// One-sided EN fixture: only "test" is known, no Bulgarian support at all.
    fn en_only_core() -> InputMethodCore {
        let mut core = InputMethodCore::new(no_ghost_config());
        assert!(core.config.dual_buffer.enabled);
        core.load_word_lang("test", 100_000, LangId::En);
        core
    }

    /// Types МАМА with Caps Lock on the public raw route and asserts the BG
    /// winner + lock precondition independently of the target key.
    fn locked_bg_mama_with_caps() -> InputMethodCore {
        let mut core = bg_only_core();
        for code in MAMA {
            core.handle_key(press_raw_with(code, Modifiers::CAPS_LOCK));
        }
        let (dual, locked, _) = core.debug_state();
        assert!(dual && locked, "precondition: BG lock on МАМА");
        let db = core.dual_buffer.as_ref().expect("dual buffer");
        assert_eq!(db.winner_lang(), LangId::Bg);
        assert_eq!(db.bg_text(), "МАМА");
        assert_eq!(db.en_text(), "MAMA");
        assert_eq!(core.current_word(), "МАМА");
        core
    }

    /// Types TEST with Caps Lock on the public raw route and asserts the EN
    /// winner + lock precondition.
    fn locked_en_test_with_caps() -> InputMethodCore {
        let mut core = en_only_core();
        for code in TEST {
            core.handle_key(press_raw_with(code, Modifiers::CAPS_LOCK));
        }
        let (dual, locked, _) = core.debug_state();
        assert!(dual && locked, "precondition: EN lock on TEST");
        let db = core.dual_buffer.as_ref().expect("dual buffer");
        assert_eq!(db.winner_lang(), LangId::En);
        assert_eq!(db.en_text(), "TEST");
        assert_eq!(db.bg_text(), "ТЕСТ");
        assert_eq!(core.current_word(), "TEST");
        core
    }

    /// Presses one bracket key on a locked composition and returns
    /// (BG buffer, EN buffer, visible ShowComposing text, current_word).
    fn press_bracket(
        core: &mut InputMethodCore,
        code: u16,
        mods: Modifiers,
    ) -> (String, String, String, String) {
        let actions = core.handle_key(press_raw_with(code, mods));
        let typed = actions
            .iter()
            .find_map(|a| match a {
                Action::ShowComposing { typed, .. } => Some(typed.clone()),
                _ => None,
            })
            .expect("composing preedit after a bracket key");
        let db = core.dual_buffer.as_ref().expect("dual buffer");
        (
            db.bg_text().to_string(),
            db.en_text().to_string(),
            typed,
            core.current_word().to_string(),
        )
    }

    /// RED: Caps Lock must uppercase the BG-only letter inside a locked
    /// Bulgarian word; the English shadow keeps the unshifted symbol.
    fn assert_caps_uppercases(idx: usize) {
        let (code, _lower, upper, en_plain, _en_shift) = BRACKET_KEYS[idx];
        let mut core = locked_bg_mama_with_caps();
        let (bg, en, typed, word) = press_bracket(&mut core, code, Modifiers::CAPS_LOCK);
        let expected = format!("МАМА{upper}");
        assert_eq!(bg, expected, "BG buffer under Caps Lock, code {code}");
        assert_eq!(
            en,
            format!("MAMA{en_plain}"),
            "EN shadow under Caps Lock, code {code}"
        );
        assert_eq!(typed, expected, "visible composing text, code {code}");
        assert_eq!(word, expected, "current_word, code {code}");
    }

    /// RED: Shift + Caps Lock cancel out on a letter, so the BG-only letter
    /// must be lowercase; the English shadow keeps the shifted symbol.
    fn assert_shift_caps_lowercases(idx: usize) {
        let (code, lower, _upper, _en_plain, en_shift) = BRACKET_KEYS[idx];
        let mut core = locked_bg_mama_with_caps();
        let mods = Modifiers::CAPS_LOCK | Modifiers::SHIFT;
        let (bg, en, typed, word) = press_bracket(&mut core, code, mods);
        let expected = format!("МАМА{lower}");
        assert_eq!(bg, expected, "BG buffer under Shift+Caps, code {code}");
        assert_eq!(
            en,
            format!("MAMA{en_shift}"),
            "EN shadow under Shift+Caps, code {code}"
        );
        assert_eq!(typed, expected, "visible composing text, code {code}");
        assert_eq!(word, expected, "current_word, code {code}");
    }

    #[test]
    fn caps_lock_uppercases_bg_letter_on_code_26_sh() {
        assert_caps_uppercases(0);
    }

    #[test]
    fn caps_lock_uppercases_bg_letter_on_code_27_sht() {
        assert_caps_uppercases(1);
    }

    #[test]
    fn caps_lock_uppercases_bg_letter_on_code_41_ch() {
        assert_caps_uppercases(2);
    }

    #[test]
    fn caps_lock_uppercases_bg_letter_on_code_43_yu() {
        assert_caps_uppercases(3);
    }

    #[test]
    fn shift_plus_caps_lowercases_bg_letter_on_code_26_sh() {
        assert_shift_caps_lowercases(0);
    }

    #[test]
    fn shift_plus_caps_lowercases_bg_letter_on_code_27_sht() {
        assert_shift_caps_lowercases(1);
    }

    #[test]
    fn shift_plus_caps_lowercases_bg_letter_on_code_41_ch() {
        assert_shift_caps_lowercases(2);
    }

    #[test]
    fn shift_plus_caps_lowercases_bg_letter_on_code_43_yu() {
        assert_shift_caps_lowercases(3);
    }

    /// Guard (expected PASS): on a locked English word the bracket keys keep
    /// their English semantics under Caps, Shift and Shift+Caps.
    #[test]
    fn english_bracket_symbols_are_unchanged_by_caps_lock() {
        for (code, _lower, _upper, en_plain, en_shift) in BRACKET_KEYS {
            for (mods, symbol) in [
                (Modifiers::CAPS_LOCK, en_plain),
                (Modifiers::SHIFT, en_shift),
                (Modifiers::CAPS_LOCK | Modifiers::SHIFT, en_shift),
            ] {
                let mut core = locked_en_test_with_caps();
                let (_bg, en, typed, word) = press_bracket(&mut core, code, mods);
                let expected = format!("TEST{symbol}");
                assert_eq!(en, expected, "EN buffer, code {code} mods {mods:?}");
                assert_eq!(typed, expected, "visible text, code {code} mods {mods:?}");
                assert_eq!(word, expected, "current_word, code {code} mods {mods:?}");
            }
        }
    }

    /// Guard (expected PASS): without the dual buffer the bracket keys are
    /// plain English punctuation, unaffected by Caps Lock; a fresh core per
    /// code and modifier set.
    #[test]
    fn bracket_symbols_without_dual_buffer_ignore_caps_lock() {
        for (code, _lower, _upper, en_plain, en_shift) in BRACKET_KEYS {
            for (mods, symbol) in [
                (Modifiers::CAPS_LOCK, en_plain),
                (Modifiers::SHIFT, en_shift),
                (Modifiers::CAPS_LOCK | Modifiers::SHIFT, en_shift),
            ] {
                let mut config = no_ghost_config();
                config.dual_buffer.enabled = false;
                let mut core = InputMethodCore::new(config);
                core.handle_key(press_raw_with(code, mods));
                assert_eq!(
                    core.current_word(),
                    symbol.to_string(),
                    "code {code} mods {mods:?}"
                );
            }
        }
    }

    // ── U2 evidence-origin API, core-level nodes (commit 1) ─────────────────
    //
    // The push path scores BEFORE it applies the prior (input.rs
    // handle_dual_buffer_key: `update_scores` then `apply_prior_lock_hint`),
    // so the hint is the last writer at len == 1 and only when the prior
    // differs from the scored winner.  The Backspace arm rescoring never
    // re-applies the hint.  A fresh core with no loaded words scores (0, 0)
    // for every reading, and `with_prior` stands in for Bg momentum.

    use crate::dual_buffer::EvidenceOrigin;

    fn press_backspace() -> KeyEvent {
        KeyEvent {
            key: Key::Backspace,
            modifiers: Modifiers::empty(),
        }
    }

    /// R7c (characterization, expected PASS): a Backspace that empties the
    /// dual buffer drops it, so no reader can see a live buffer at len 0;
    /// the next dual key starts a fresh, unlocked buffer.
    #[test]
    fn backspace_to_empty_drops_the_dual_buffer_and_the_next_key_starts_fresh() {
        let mut core = InputMethodCore::new(no_ghost_config());
        core.handle_key(press_raw(16)); // q / я
        assert!(core.dual_buffer.is_some(), "premise: dual buffer alive");
        let actions = core.handle_key(press_backspace());
        assert_eq!(actions, vec![Action::HideGhost]);
        assert!(core.dual_buffer.is_none(), "buffer dropped at len 0");
        assert_eq!(core.current_word(), "");
        core.handle_key(press_raw(16));
        let db = core
            .dual_buffer
            .as_ref()
            .expect("premise: fresh dual buffer");
        assert_eq!(db.len(), 1);
        assert!(!db.is_locked());
    }

    /// R9: a both-zero word on a fresh core keeps the literal EN reading (D2)
    /// and the buffer reports UnsupportedBoth — unsupported is not "force BG".
    #[test]
    fn unsupported_word_on_a_fresh_core_keeps_the_literal_reading() {
        let mut core = InputMethodCore::new(no_ghost_config());
        core.handle_key(press_raw(16)); // q / я
        core.handle_key(press_raw(45)); // x / ь
        assert_eq!(core.current_word(), "qx", "premise: literal EN reading");
        let db = core
            .dual_buffer
            .as_ref()
            .expect("premise: dual buffer alive");
        assert_eq!(db.winner_lang(), LangId::En, "premise: En tie-break winner");
        assert_eq!(db.evidence_origin(), EvidenceOrigin::UnsupportedBoth);
    }

    /// R9b (reading-only characterization of CURRENT behaviour while ROOT Q4
    /// is open): after a Bg prior the first character is rendered Cyrillic by
    /// the prior override, and a both-zero continuation inherits that winner,
    /// so the code token "qx" composes as "яь".  A Q4 reversal REQUIRES this
    /// node to be changed; a green run is not an endorsement of the policy.
    /// Asserts nothing about `evidence_origin()`.
    #[test]
    fn unsupported_word_after_a_bg_prior_composes_in_cyrillic_today() {
        let mut core = InputMethodCore::new(no_ghost_config());
        with_prior(&mut core, LangId::Bg);
        core.handle_key(press_raw(16)); // q / я
        assert_eq!(
            core.current_word(),
            "я",
            "char 1: the prior override renders Cyrillic"
        );
        core.handle_key(press_raw(45)); // x / ь
        assert_eq!(core.current_word(), "яь");
    }

    /// R9b-origin: the same sequence reports PriorOnly at char 1 (the hint
    /// overrode the En tie-break winner) and UnsupportedBoth at char 2 — the
    /// winner is inherited from the prior, which is what Q4 is about.
    #[test]
    fn unsupported_word_after_a_bg_prior_reports_unsupported_both_inherited_from_prior_only() {
        let mut core = InputMethodCore::new(no_ghost_config());
        with_prior(&mut core, LangId::Bg);
        core.handle_key(press_raw(16)); // q / я
        assert!(core.dual_buffer.is_some(), "premise: dual buffer alive");
        {
            let db = core.dual_buffer.as_ref().unwrap();
            assert_eq!(
                db.winner_lang(),
                LangId::Bg,
                "premise: the Bg prior overrode the En tie-break winner"
            );
            assert_eq!(
                db.evidence_origin(),
                EvidenceOrigin::PriorOnly,
                "char 1: prior override → PriorOnly (origin table)"
            );
        }
        core.handle_key(press_raw(45)); // x / ь
        let db = core
            .dual_buffer
            .as_ref()
            .expect("premise: dual buffer alive");
        assert_eq!(db.winner_lang(), LangId::Bg, "premise: winner inherited");
        assert_eq!(db.evidence_origin(), EvidenceOrigin::UnsupportedBoth);
    }

    /// R9c (characterization of an asymmetry, pinned deliberately): Backspace
    /// back to one character rescores through the Backspace arm, which never
    /// re-applies the prior — push-to-1 reports PriorOnly, backspace-to-1
    /// reports UnsupportedBoth for the same buffer.  A later phase changes
    /// this on purpose or not at all.  If the asymmetry is ever ruled a bug,
    /// the Backspace arm (`Key::Backspace` in `handle_key`: rescoring only) is
    /// the odd one out — it drops the prior entirely, not just the origin;
    /// the push path (`handle_dual_buffer_key`) is the reference behaviour.
    #[test]
    fn backspace_to_one_char_rescores_without_reapplying_the_prior() {
        let mut core = InputMethodCore::new(no_ghost_config());
        with_prior(&mut core, LangId::Bg);
        core.handle_key(press_raw(16)); // q / я
        core.handle_key(press_raw(45)); // x / ь
        core.handle_key(press_backspace());
        assert_eq!(
            core.current_word(),
            "я",
            "premise: one char left, Cyrillic reading kept"
        );
        let db = core
            .dual_buffer
            .as_ref()
            .expect("premise: dual buffer alive");
        assert_eq!(db.len(), 1, "premise");
        assert_eq!(db.winner_lang(), LangId::Bg, "premise: winner inherited");
        assert_eq!(db.evidence_origin(), EvidenceOrigin::UnsupportedBoth);
    }

    /// R11: an unsupported inherited winner is a display fallback, not a
    /// detector observation. The initial detector state must remain unchanged.
    #[test]
    fn unsupported_winner_does_not_feed_detector_evidence() {
        let mut core = InputMethodCore::new(no_ghost_config());
        assert!(core.config.lang_detection, "premise: detection on");
        core.handle_key(press_raw(16)); // q / я
        core.handle_key(press_raw(45)); // x / ь
        let det = core.lang_detector.detected();
        assert_eq!(det.lang, LangId::En);
        assert!(
            det.confidence.abs() < 1e-9,
            "an unsupported inherited winner must not manufacture confidence"
        );
    }
}

#[cfg(test)]
mod review_tech_language_evidence_tests {
    use super::*;

    fn press(key: Key) -> KeyEvent {
        KeyEvent {
            key,
            modifiers: Modifiers::empty(),
        }
    }

    fn fresh() -> InputMethodCore {
        InputMethodCore::new(InputConfig {
            ghost_text: false,
            ..InputConfig::default()
        })
    }

    fn assert_tech_commit(core: &mut InputMethodCore, word: &str, boundary: Key, confidence: f64) {
        assert_eq!(core.current_word(), word);
        let detected = core.detected_language();
        assert_eq!(detected.lang, LangId::Tech);
        assert!((detected.confidence - confidence).abs() < 1e-12);
        let actions = core.handle_key(press(boundary));
        assert!(actions.contains(&Action::ForwardKey));
        assert!(core.current_word().is_empty());
        let batch = core.take_commit_learning();
        assert!(!batch.accepted_prediction);
        assert_eq!(
            batch.words,
            vec![CommitObservation {
                language: Some(LangId::Tech)
            }],
            "positive direct Tech input must retain its language observation"
        );
        assert_eq!(core.lang_detector.momentum_lang(), Some(LangId::Tech));
        let tracks = core.export_personal_profile().lang_cvm.unwrap().tracks;
        assert!(tracks
            .iter()
            .any(|(language, track)| language == "tech" && track.words.iter().any(|w| w == word)));
        assert_eq!(
            core.engine.word_frequency(word, LangId::Tech),
            0.0,
            "a Tech language observation does not confirm a new corpus terminal"
        );
    }

    fn assert_unconfirmed_boundary(core: &mut InputMethodCore) {
        core.handle_key(press(Key::Space));
        assert_eq!(
            core.take_commit_learning().words,
            vec![CommitObservation { language: None }]
        );
        assert_eq!(core.lang_detector.momentum_lang(), None);
        assert!(core
            .export_personal_profile()
            .lang_cvm
            .unwrap()
            .tracks
            .is_empty());
    }

    // Break caught: instant_classify(None) discards independently observed
    // positive Tech symbols before language CVM, momentum and receipt learning.
    #[test]
    fn review_tech_direct_symbols_keep_language_learning() {
        let mut core = fresh();
        for ch in "___".chars() {
            assert!(core
                .handle_key(press(Key::Char(ch)))
                .contains(&Action::ForwardKey));
        }
        assert_tech_commit(&mut core, "___", Key::Space, 0.9);
    }

    #[test]
    fn review_tech_direct_digits_keep_return_language_learning() {
        let mut core = fresh();
        for ch in "123".chars() {
            core.handle_key(press(Key::Char(ch)));
        }
        assert_tech_commit(&mut core, "123", Key::Return, 0.9);
    }

    #[test]
    fn review_tech_shared_physical_shift_key_keeps_language_learning() {
        let mut core = fresh();
        for _ in 0..3 {
            core.handle_key(KeyEvent {
                key: Key::RawCode(12),
                modifiers: Modifiers::SHIFT,
            });
        }
        assert_tech_commit(&mut core, "___", Key::RawCode(57), 0.9);
    }

    fn nondual() -> InputMethodCore {
        let mut config = InputConfig {
            ghost_text: false,
            ..InputConfig::default()
        };
        config.dual_buffer.enabled = false;
        InputMethodCore::new(config)
    }

    #[test]
    fn review_tech_deleted_script_does_not_shadow_direct_tech_remainder() {
        let mut core = fresh();
        for key in [
            Key::Char('_'),
            Key::Char('a'),
            Key::Backspace,
            Key::Char('!'),
            Key::Char('!'),
        ] {
            core.handle_key(press(key));
        }
        assert_tech_commit(&mut core, "_!!", Key::Space, 0.5335);
    }

    #[test]
    fn review_tech_nondual_ambiguous_raw_symbols_are_not_direct_evidence() {
        let mut core = nondual();
        for _ in 0..3 {
            assert!(core
                .handle_key(press(Key::RawCode(26)))
                .contains(&Action::ForwardKey));
        }
        assert_eq!(core.current_word(), "[[[");
        assert_eq!(core.detected_language().lang, LangId::Tech);
        assert!((core.detected_language().confidence - 0.95).abs() < 1e-12);
        assert_unconfirmed_boundary(&mut core);
        assert!(core
            .export_personal_profile()
            .cvm
            .words
            .iter()
            .any(|w| w == "[[["));
    }

    #[test]
    fn review_tech_nondual_inferred_suffix_invalidates_direct_tech_provenance() {
        let mut core = nondual();
        core.handle_key(press(Key::Char('_')));
        core.handle_key(press(Key::RawCode(26)));
        assert_eq!(core.current_word(), "_[");
        assert_unconfirmed_boundary(&mut core);
    }

    #[test]
    fn review_tech_nondual_raw_letters_do_not_manufacture_english_evidence() {
        let mut core = nondual();
        for code in [16, 45, 44] {
            core.handle_key(press(Key::RawCode(code)));
        }
        assert_eq!(core.current_word(), "qxz");
        assert_eq!(core.detected_language().lang, LangId::En);
        assert_eq!(core.detected_language().confidence, 1.0);
        assert_unconfirmed_boundary(&mut core);
        assert!(core
            .export_personal_profile()
            .cvm
            .words
            .iter()
            .any(|w| w == "qxz"));
        assert_eq!(core.engine.word_frequency("qxz", LangId::En), 0.0);
    }

    #[test]
    fn review_tech_nondual_shared_glyph_retains_direct_evidence() {
        let mut core = nondual();
        for _ in 0..3 {
            core.handle_key(KeyEvent {
                key: Key::RawCode(12),
                modifiers: Modifiers::SHIFT,
            });
        }
        assert_tech_commit(&mut core, "___", Key::RawCode(57), 0.9);
    }

    #[test]
    fn review_tech_prior_with_neutral_punctuation_is_not_evidence() {
        let mut core = fresh();
        core.apply_hints(&crate::master_loop::Hints {
            lang_prior: Some(LangId::Tech),
            ..Default::default()
        });
        for ch in "???".chars() {
            core.handle_key(press(Key::Char(ch)));
        }
        assert_eq!(core.detected_language().lang, LangId::Tech);
        assert_unconfirmed_boundary(&mut core);
    }

    #[test]
    fn review_tech_unobserved_synthetic_word_is_not_evidence() {
        let mut core = fresh();
        core.lang_detector.feed_char_instant('_');
        core.current_word = "___".into();
        assert_eq!(core.detected_language().lang, LangId::Tech);
        assert_unconfirmed_boundary(&mut core);
    }

    #[test]
    fn review_tech_raw_unknown_fallback_is_not_evidence() {
        let mut core = fresh();
        core.apply_hints(&crate::master_loop::Hints {
            lang_prior: Some(LangId::Tech),
            ..Default::default()
        });
        for code in [16, 45, 44] {
            core.handle_key(press(Key::RawCode(code)));
        }
        assert_unconfirmed_boundary(&mut core);
    }

    #[test]
    fn review_tech_partial_right_is_not_direct_character_evidence() {
        let mut core = InputMethodCore::new(InputConfig::default());
        core.handle_key(press(Key::Char('_')));
        assert!(core.apply_prediction_override("___").is_some());
        core.handle_key(press(Key::Right));
        assert_eq!(core.current_word(), "__");
        assert_eq!(core.detected_language().lang, LangId::Tech);
        assert_unconfirmed_boundary(&mut core);
    }

    #[test]
    fn review_tech_disabled_language_detection_is_not_evidence() {
        let mut core = InputMethodCore::new(InputConfig {
            ghost_text: false,
            lang_detection: false,
            ..InputConfig::default()
        });
        for ch in "___".chars() {
            core.handle_key(press(Key::Char(ch)));
        }
        assert_unconfirmed_boundary(&mut core);
    }

    #[test]
    fn review_tech_complete_token_must_have_only_positive_tech_characters() {
        let mut core = fresh();
        for ch in "_?".chars() {
            core.handle_key(press(Key::Char(ch)));
        }
        assert_eq!(core.detected_language().lang, LangId::Tech);
        assert_unconfirmed_boundary(&mut core);
    }

    #[test]
    fn review_tech_positive_character_needs_actual_tech_detection() {
        let mut core = fresh();
        core.lang_detector.feed_char_instant('a');
        core.handle_key(press(Key::Char('_')));
        assert_eq!(core.current_word(), "_");
        assert_eq!(core.detected_language().lang, LangId::En);
        assert_unconfirmed_boundary(&mut core);
    }

    #[test]
    fn review_tech_deleted_observation_does_not_attest_a_later_word() {
        let mut core = fresh();
        core.handle_key(press(Key::Char('_')));
        core.handle_key(press(Key::Backspace));
        assert!(core.current_word().is_empty());
        core.current_word = "___".into();
        assert_unconfirmed_boundary(&mut core);
    }
}

#[cfg(test)]
mod retraction_word_evidence_tests {
    use super::*;

    fn press(key: Key) -> KeyEvent {
        KeyEvent {
            key,
            modifiers: Modifiers::empty(),
        }
    }

    fn fresh() -> InputMethodCore {
        let mut config = InputConfig {
            ghost_text: false,
            ..InputConfig::default()
        };
        config.dual_buffer.enabled = false;
        InputMethodCore::new(config)
    }

    fn assert_surviving_tech_commit(core: &mut InputMethodCore, word: &str, boundary: Key) {
        assert_eq!(core.current_word(), word);
        let before = core.detected_language();
        let actions = core.handle_key(press(boundary));
        assert!(actions.contains(&Action::ForwardKey));
        let batch = core.take_commit_learning();
        assert_eq!(
            batch.words,
            vec![CommitObservation {
                language: Some(LangId::Tech)
            }]
        );
        assert!(!batch.accepted_prediction);
        assert_eq!(core.lang_detector.momentum_lang(), Some(LangId::Tech));
        assert!(core
            .export_personal_profile()
            .lang_cvm
            .unwrap()
            .tracks
            .iter()
            .any(|(lang, track)| lang == "tech" && track.words.iter().any(|text| text == word)));
        let after = core.detected_language();
        assert_eq!(
            (after.lang, after.confidence),
            (before.lang, before.confidence),
            "word proof must not mutate the global detector used for display routing"
        );
    }

    #[test]
    fn retraction_deleted_inferred_raw_suffix_restores_direct_tech_evidence() {
        let mut core = fresh();
        for key in [
            Key::Char('_'),
            Key::RawCode(26),
            Key::Backspace,
            Key::Char('_'),
            Key::Char('_'),
        ] {
            core.handle_key(press(key));
        }
        assert_surviving_tech_commit(&mut core, "___", Key::Space);
    }

    #[test]
    fn retraction_deleted_script_replays_surviving_tech_from_word_start() {
        let mut core = fresh();
        for ch in "123a".chars() {
            core.handle_key(press(Key::Char(ch)));
        }
        core.handle_key(press(Key::Backspace));
        assert_eq!(
            core.detected_language().lang,
            LangId::En,
            "the display detector still reflects the deleted character"
        );
        assert_surviving_tech_commit(&mut core, "123", Key::Return);
    }

    #[test]
    fn retraction_deleted_entire_prefix_cannot_seed_the_next_direct_token() {
        let mut core = fresh();
        core.handle_key(press(Key::Char('a')));
        core.handle_key(press(Key::Backspace));
        assert!(core.current_word().is_empty());
        for ch in "123".chars() {
            core.handle_key(press(Key::Char(ch)));
        }
        assert_surviving_tech_commit(&mut core, "123", Key::Space);
    }

    #[test]
    fn retraction_deleted_inferred_suffix_restores_direct_script_proof() {
        for (word, language) in [("abc", LangId::En), ("зор", LangId::Bg)] {
            let mut core = fresh();
            for ch in word.chars() {
                core.handle_key(press(Key::Char(ch)));
            }
            core.handle_key(press(Key::RawCode(26)));
            core.handle_key(press(Key::Backspace));
            assert_eq!(core.current_word(), word);
            core.handle_key(press(Key::Space));
            assert_eq!(
                core.take_commit_learning().words,
                vec![CommitObservation {
                    language: Some(language)
                }]
            );
            assert!(core.engine.word_frequency(word, language) > 0.0);
        }
    }

    #[test]
    fn retraction_surviving_inferred_prefix_cannot_borrow_deleted_observation() {
        let mut core = fresh();
        for _ in 0..3 {
            core.handle_key(press(Key::RawCode(26)));
        }
        core.handle_key(press(Key::Char('_')));
        core.handle_key(press(Key::Backspace));
        assert_eq!(core.current_word(), "[[[");
        core.handle_key(press(Key::Space));
        assert_eq!(
            core.take_commit_learning().words,
            vec![CommitObservation { language: None }]
        );
        assert!(core
            .export_personal_profile()
            .cvm
            .words
            .iter()
            .any(|word| word == "[[["));
    }

    #[test]
    fn retraction_removing_all_partial_right_chars_restores_direct_prefix() {
        let mut core = fresh();
        core.config.ghost_text = true;
        for _ in 0..3 {
            core.handle_key(press(Key::Char('_')));
        }
        assert!(core.apply_prediction_override("_____").is_some());
        core.handle_key(press(Key::Right));
        assert_eq!(core.current_word(), "____");
        core.handle_key(press(Key::Backspace));
        assert_surviving_tech_commit(&mut core, "___", Key::Space);
    }

    #[test]
    fn retraction_surviving_partial_right_char_still_blocks_direct_proof() {
        let mut core = fresh();
        core.config.ghost_text = true;
        core.handle_key(press(Key::Char('_')));
        assert!(core.apply_prediction_override("_____").is_some());
        core.handle_key(press(Key::Right));
        core.handle_key(press(Key::Right));
        core.handle_key(press(Key::Backspace));
        assert_eq!(core.current_word(), "__");
        core.handle_key(press(Key::Space));
        assert_eq!(
            core.take_commit_learning().words,
            vec![CommitObservation { language: None }]
        );
    }

    #[test]
    fn retraction_unicode_pop_retracts_one_scalar_observation() {
        let mut core = fresh();
        for ch in "здравей".chars() {
            core.handle_key(press(Key::Char(ch)));
        }
        for _ in 0..4 {
            core.handle_key(press(Key::Backspace));
        }
        assert_eq!(core.current_word(), "здр");
        core.handle_key(press(Key::Space));
        assert_eq!(
            core.take_commit_learning().words,
            vec![CommitObservation {
                language: Some(LangId::Bg)
            }]
        );
    }

    #[test]
    fn retraction_same_length_replacement_cannot_borrow_observations() {
        let mut core = fresh();
        for _ in 0..2 {
            core.handle_key(press(Key::Char('_')));
        }
        core.current_word = "a_".into();
        core.handle_key(press(Key::Backspace));
        assert_eq!(core.current_word(), "a");
        core.handle_key(press(Key::Space));
        assert_eq!(
            core.take_commit_learning().words,
            vec![CommitObservation { language: None }]
        );
    }

    #[test]
    fn retraction_missing_trace_after_external_clear_does_not_recapture_history() {
        let mut core = fresh();
        core.handle_key(press(Key::Char('_')));
        core.current_word.clear();
        core.handle_key(press(Key::Char('1')));
        core.handle_key(press(Key::Space));
        assert_eq!(
            core.take_commit_learning().words,
            vec![CommitObservation { language: None }]
        );
    }

    #[test]
    fn retraction_supported_dual_to_empty_keeps_pre_dual_detector_baseline() {
        let mut core = InputMethodCore::new(InputConfig {
            ghost_text: false,
            ..InputConfig::default()
        });
        core.load_word_lang("hello", 1_000_000, LangId::En);
        for code in [35, 18] {
            core.handle_key(press(Key::RawCode(code)));
        }
        assert_eq!(core.detected_language().lang, LangId::En);
        for _ in 0..2 {
            core.handle_key(press(Key::Backspace));
        }
        assert!(core.current_word().is_empty());
        for ch in "123".chars() {
            core.handle_key(press(Key::Char(ch)));
        }
        assert_surviving_tech_commit(&mut core, "123", Key::Space);
    }

    #[test]
    fn retraction_dual_backspace_rebuild_does_not_create_direct_observations() {
        let mut core = InputMethodCore::new(InputConfig {
            ghost_text: false,
            ..InputConfig::default()
        });
        for code in [16, 45, 44] {
            core.handle_key(press(Key::RawCode(code)));
        }
        core.handle_key(press(Key::Backspace));
        assert_eq!(core.current_word(), "qx");
        core.handle_key(press(Key::Space));
        assert_eq!(
            core.take_commit_learning().words,
            vec![CommitObservation { language: None }]
        );
    }

    #[test]
    fn retraction_true_boundary_starts_a_new_baseline_with_committed_context() {
        let mut core = fresh();
        for ch in "hello".chars() {
            core.handle_key(press(Key::Char(ch)));
        }
        core.handle_key(press(Key::Space));
        core.take_commit_learning();
        core.handle_key(press(Key::Char('_')));
        core.handle_key(press(Key::Space));
        assert_eq!(
            core.take_commit_learning().words,
            vec![CommitObservation { language: None }]
        );
        assert_eq!(core.lang_detector.momentum_lang(), Some(LangId::En));
    }

    #[test]
    fn retraction_focus_flush_uses_detached_word_observations() {
        let mut core = fresh();
        for ch in "123a".chars() {
            core.handle_key(press(Key::Char(ch)));
        }
        core.handle_key(press(Key::Backspace));
        core.focus_lost();
        assert!(core.current_word().is_empty());
        assert_eq!(
            core.take_commit_learning().words,
            vec![CommitObservation {
                language: Some(LangId::Tech)
            }]
        );
    }

    #[test]
    fn retraction_transliterated_and_edited_rendering_stays_unconfirmed() {
        let mut core = fresh();
        core.load_word_lang("здравей", 1_000_000, LangId::Bg);
        for ch in "zdra".chars() {
            core.handle_key(press(Key::Char(ch)));
        }
        assert_eq!(core.current_word(), "здра");
        core.handle_key(press(Key::Backspace));
        assert_eq!(core.current_word(), "здр");
        core.handle_key(press(Key::Space));
        assert_eq!(
            core.take_commit_learning().words,
            vec![CommitObservation {
                language: Some(LangId::Bg)
            }]
        );
        assert_eq!(
            core.engine.word_frequency("здр", LangId::Bg),
            0.0,
            "corpus prefix evidence does not turn transliterated text into a confirmed exact word"
        );
    }
}

#[cfg(test)]
mod language_evidence_contract_tests {
    use super::*;
    use crate::dual_buffer::EvidenceOrigin;

    fn press(key: Key) -> KeyEvent {
        KeyEvent {
            key,
            modifiers: Modifiers::empty(),
        }
    }

    fn fresh(ghost_text: bool) -> InputMethodCore {
        InputMethodCore::new(InputConfig {
            ghost_text,
            ..InputConfig::default()
        })
    }

    fn prior(core: &mut InputMethodCore, lang: LangId) {
        core.apply_hints(&crate::master_loop::Hints {
            lang_prior: Some(lang),
            ..Default::default()
        });
    }

    fn raw(core: &mut InputMethodCore, codes: &[u16]) {
        for &code in codes {
            core.handle_key(press(Key::RawCode(code)));
        }
    }

    fn assert_no_language_words(core: &InputMethodCore) {
        let profile = core.export_personal_profile();
        assert!(
            profile.lang_cvm.unwrap().tracks.is_empty(),
            "an unconfirmed inferred word must not enter a language CVM track"
        );
    }

    // Break caught: feeding a derived dual winner through the observed-script
    // classifier or accepting UnsupportedBoth as independent detector evidence.
    #[test]
    fn unsupported_push_preserves_detector_state_and_literal_display() {
        let mut core = fresh(false);
        let before = core.detected_language();
        raw(&mut core, &[16, 45]); // synthetic qx / яь, no corpus
        assert_eq!(core.current_word(), "qx");
        assert_eq!(
            core.dual_buffer.as_ref().unwrap().evidence_origin(),
            EvidenceOrigin::UnsupportedBoth
        );
        let after = core.detected_language();
        assert_eq!(
            (after.lang, after.confidence),
            (before.lang, before.confidence),
            "inferred fallback display is not a new detector observation"
        );
    }

    #[test]
    fn prior_only_push_preserves_detector_state_and_inherited_display() {
        let mut core = fresh(false);
        prior(&mut core, LangId::Bg);
        let before = core.detected_language();
        core.handle_key(press(Key::RawCode(16)));
        assert_eq!(core.current_word(), "я");
        assert_eq!(
            core.dual_buffer.as_ref().unwrap().evidence_origin(),
            EvidenceOrigin::PriorOnly
        );
        let after = core.detected_language();
        assert_eq!(
            (after.lang, after.confidence),
            (before.lang, before.confidence),
            "a surrounding prior may choose display without becoming a new vote"
        );
    }

    #[test]
    fn ordinary_delimiter_keeps_generic_learning_without_language_cvm() {
        let mut core = fresh(false);
        core.context.push_back("seed".into());
        prior(&mut core, LangId::Bg);
        raw(&mut core, &[16, 45, 44]); // synthetic novel Cyrillic name яьз
        let actions = core.handle_key(press(Key::Space));
        assert!(actions.contains(&Action::CommitText("яьз".into())));
        let profile = core.export_personal_profile();
        assert!(
            profile.cvm.words.iter().any(|word| word == "яьз"),
            "generic vocabulary still learns"
        );
        assert!(
            profile
                .markov_bigrams
                .iter()
                .any(|(ctx, word, count)| ctx == "seed" && word == "яьз" && *count == 1),
            "generic Markov still learns"
        );
        assert_no_language_words(&core);
    }

    #[test]
    fn ordinary_delimiter_does_not_manufacture_momentum() {
        let mut core = fresh(false);
        raw(&mut core, &[16, 45, 44]);
        core.handle_key(press(Key::Space));
        assert_eq!(
            core.lang_detector.momentum_lang(),
            None,
            "a delimiter confirms text delivery, not the inferred script"
        );
    }

    #[test]
    fn ordinary_delimiter_does_not_manufacture_regime_language_ratio() {
        let mut core = fresh(false);
        raw(&mut core, &[16, 45, 44]);
        core.handle_key(press(Key::Space));
        assert_eq!(
            core.regime_detector.lang_ratio(),
            0.5,
            "non-language word statistics may update while language ratio stays neutral"
        );
    }

    #[test]
    fn repeated_unconfirmed_word_does_not_become_corpus_evidence() {
        let mut core = fresh(false);
        raw(&mut core, &[16, 45, 44]);
        core.handle_key(press(Key::Space));
        raw(&mut core, &[16, 45, 44]);
        assert_eq!(core.current_word(), "qxz");
        assert_eq!(
            core.dual_buffer.as_ref().unwrap().evidence_origin(),
            EvidenceOrigin::UnsupportedBoth,
            "prediction-only self-insertion cannot attest its own script"
        );
    }

    #[test]
    fn prediction_only_word_is_excluded_from_exact_and_correction_frequency() {
        let mut core = fresh(false);
        raw(&mut core, &[16, 45, 44]);
        core.handle_key(press(Key::Space));
        assert_eq!(core.engine.score_exact_both("qxz", "яьз"), (0.0, 0.0));
        assert_eq!(core.engine.word_frequency("qxz", LangId::En), 0.0);
    }

    #[test]
    fn lower_ranked_trusted_prefix_is_not_hidden_by_prediction_candidate() {
        let mut core = fresh(false);
        raw(&mut core, &[16, 45, 44]);
        core.handle_key(press(Key::Space));
        core.load_word_lang("qxtrusted", 2, LangId::En);
        assert_eq!(core.engine.score_both("qx", "яь"), (2.0, 0.0),
            "trusted prefix search must ignore the higher-frequency unconfirmed candidate and still find trusted evidence");
    }

    #[test]
    fn unsupported_suffix_after_a_corpus_lock_does_not_gain_commit_language() {
        let mut core = fresh(false);
        core.load_word_lang("мама", 100_000, LangId::Bg);
        raw(&mut core, &[50, 30, 50, 30]);
        assert!(
            core.debug_state().1,
            "premise: trusted prefix locked the display"
        );
        raw(&mut core, &[45]);
        assert_eq!(core.current_word(), "мамаь");
        assert_eq!(
            core.engine.score_both("mamax", "мамаь"),
            (0.0, 0.0),
            "premise: current full word has no trusted support"
        );
        core.handle_key(press(Key::Space));
        assert_no_language_words(&core);
    }

    #[test]
    fn focus_flush_is_unconfirmed_language() {
        let mut core = fresh(false);
        raw(&mut core, &[16, 45, 44]);
        assert!(core
            .focus_lost()
            .contains(&Action::CommitText("qxz".into())));
        assert_no_language_words(&core);
    }

    #[test]
    fn tab_without_ghost_is_unconfirmed_language() {
        let mut core = fresh(false);
        raw(&mut core, &[16, 45, 44]);
        assert!(core
            .handle_key(press(Key::Tab))
            .contains(&Action::CommitText("qxz".into())));
        assert_no_language_words(&core);
    }

    #[test]
    fn partial_right_then_delimiter_is_unconfirmed_language() {
        let mut core = fresh(true);
        raw(&mut core, &[16, 45]);
        assert!(core.apply_prediction_override("qxzz").is_some());
        core.handle_key(press(Key::Right));
        assert_eq!(core.current_word(), "qxz");
        assert_eq!(core.ghost_text(), "z");
        let actions = core.handle_key(press(Key::Space));
        assert!(actions.contains(&Action::CommitText("qxz".into())));
        assert_no_language_words(&core);
    }

    #[test]
    fn unmappable_partial_right_keeps_delivery_without_full_word_confirmation() {
        let mut core = fresh(true);
        prior(&mut core, LangId::Bg);
        raw(&mut core, &[16, 45]);
        assert_eq!(core.current_word(), "яь");
        assert!(core.apply_prediction_override("яьѝя").is_some());
        let actions = core.handle_key(press(Key::Right));
        assert!(
            actions.contains(&Action::CommitText("яьѝя".into())),
            "preserve the existing unmappable-character full delivery fallback"
        );
        assert_eq!(core.metrics().summary().accepted_predictions, 1);
        assert_no_language_words(&core);
        assert_eq!(
            core.engine.word_frequency("яьѝя", LangId::Bg),
            0.0,
            "partial Right did not explicitly confirm the full visible completion"
        );
        let batch = core.take_commit_learning();
        assert!(
            batch.accepted_prediction,
            "physical acceptance feedback remains positive"
        );
        assert_eq!(batch.words, vec![CommitObservation { language: None }]);
    }

    #[test]
    fn unmappable_partial_right_requires_exact_trust_not_just_a_known_prefix() {
        for exact in [false, true] {
            let mut core = fresh(true);
            core.load_word_lang(if exact { "яьѝя" } else { "яьѝяз" }, 100, LangId::Bg);
            raw(&mut core, &[16, 45]);
            assert_eq!(core.current_word(), "яь");
            assert!(core.apply_prediction_override("яьѝя").is_some());
            core.handle_key(press(Key::Right));
            let batch = core.take_commit_learning();
            assert!(batch.accepted_prediction);
            assert_eq!(
                batch.words,
                vec![CommitObservation {
                    language: if exact { Some(LangId::Bg) } else { None }
                }]
            );
            assert_eq!(
                core.engine.word_frequency("яьѝя", LangId::Bg),
                if exact { 100.0 } else { 0.0 }
            );
        }
    }

    // Guards protect useful learning and real evidence from a skip-everything fix.
    #[test]
    fn novel_bg_name_remains_available_for_prediction() {
        let mut core = fresh(false);
        prior(&mut core, LangId::Bg);
        raw(&mut core, &[44, 24, 19, 23, 17, 30, 49]); // synthetic зориван
        assert_eq!(core.current_word(), "зориван");
        core.handle_key(press(Key::Space));
        assert!(
            core.engine
                .predict("зори", &[], 5, Some(LangId::Bg))
                .iter()
                .any(|p| p.word == "зориван"),
            "unknown names need candidate generation as well as personal Markov"
        );
    }

    #[test]
    fn corpus_prefix_language_does_not_confirm_a_new_exact_word() {
        let mut core = fresh(false);
        core.load_word_lang("зориван", 100_000, LangId::Bg);
        raw(&mut core, &[44, 24, 19, 23]);
        assert_eq!(core.current_word(), "зори");
        core.handle_key(press(Key::Space));
        assert_eq!(
            core.lang_detector.momentum_lang(),
            Some(LangId::Bg),
            "trusted prefix still supplies real language evidence"
        );
        assert!(
            core.engine
                .predict("зор", &[], 5, Some(LangId::Bg))
                .iter()
                .any(|p| p.word == "зори"),
            "the newly typed shorter word must remain a prediction candidate"
        );
        assert_eq!(
            core.engine.score_exact_both("zori", "зори"),
            (0.0, 0.0),
            "prefix language evidence must not manufacture exact word confirmation"
        );
    }

    #[test]
    fn direct_observed_script_can_confirm_a_new_exact_word_below_a_known_prefix() {
        let mut core = fresh(false);
        core.load_word_lang("зориван", 100_000, LangId::Bg);
        for ch in "зори".chars() {
            core.handle_key(press(Key::Char(ch)));
        }
        core.handle_key(press(Key::Space));
        assert!(core.engine.score_exact_both("zori", "зори").1 > 0.0,
            "direct observed script confirms this exact new word independently of the corpus prefix");
    }

    #[test]
    fn unobserved_current_word_state_cannot_confirm_an_exact_word() {
        let mut core = fresh(false);
        core.current_word = "qxz".into();
        core.handle_key(press(Key::Space));
        assert_eq!(core.engine.score_exact_both("qxz", "яьз"), (0.0, 0.0));
        assert_no_language_words(&core);
    }

    #[test]
    fn direct_observed_prefix_keeps_existing_opt_in_correction_output() {
        let mut core = InputMethodCore::new(InputConfig {
            ghost_text: false,
            post_commit_autocorrect: true,
            ..InputConfig::default()
        });
        core.load_word_lang("шщчюха", 1, LangId::Bg);
        core.load_word_lang("[]`\\", 20, LangId::En);
        for ch in "шщчю".chars() {
            core.handle_key(press(Key::Char(ch)));
        }
        assert_eq!(core.current_word(), "шщчю");
        assert_eq!(core.engine.score_exact_both("[]`\\", "шщчю"), (20.0, 0.0));
        let actions = core.handle_key(press(Key::Space));
        assert!(actions.contains(&Action::ReplaceWord { replace_len: 4, text: "[]`\\".into() }),
            "the existing opt-in pipeline replaces this exact-OOV trusted prefix before any new exact confirmation: {actions:?}");
        let batch = core.take_commit_learning();
        assert_eq!(
            batch.words,
            vec![CommitObservation {
                language: Some(LangId::En)
            }]
        );
        assert_eq!(core.context_words().0, Some("[]`\\"));
        let profile = core.export_personal_profile();
        assert!(profile.cvm.words.iter().any(|word| word == "[]`\\"));
        assert!(!profile.cvm.words.iter().any(|word| word == "шщчю"));
        assert_eq!(core.engine.score_exact_both("[]`\\", "шщчю"), (20.0, 0.0));
    }

    #[test]
    fn full_visible_accept_can_promote_word_specific_language_evidence() {
        let mut core = fresh(true);
        raw(&mut core, &[16, 45]);
        assert!(core.apply_prediction_override("qxz").is_some());
        let actions = core.handle_key(press(Key::Tab));
        assert!(actions.contains(&Action::CommitText("qxz".into())));
        assert_eq!(core.lang_detector.momentum_lang(), Some(LangId::En));
        assert!(core.engine.score_exact_both("qxz", "яьз").0 > 0.0);
        let before = core.metrics().commit_count();
        core.handle_key(press(Key::Tab));
        assert_eq!(
            core.metrics().commit_count(),
            before,
            "second Tab has no acceptance to learn"
        );
    }

    #[test]
    fn review_accepted_post_confirmation_correction_receipt_follows_effective_word() {
        let mut core = InputMethodCore::new(InputConfig {
            post_commit_autocorrect: true,
            ..InputConfig::default()
        });
        core.load_word_lang("car", 100, LangId::En);
        core.handle_key(press(Key::Char('ц')));
        assert!(core.apply_prediction_override("цар").is_some());
        let actions = core.handle_key(press(Key::Tab));
        assert!(actions.contains(&Action::ReplaceWord {
            replace_len: 3,
            text: "car".into()
        }));
        assert_eq!(core.metrics().summary().accepted_predictions, 1);
        let batch = core.take_commit_learning();
        assert!(batch.accepted_prediction);
        assert_eq!(
            batch.words,
            vec![CommitObservation {
                language: Some(LangId::En)
            }]
        );
        assert_eq!(core.context_words().0, Some("car"));
        let profile = core.export_personal_profile();
        assert!(profile.cvm.words.iter().any(|word| word == "car"));
        assert!(!profile.cvm.words.iter().any(|word| word == "цар"));
        assert_eq!(
            core.engine.word_frequency("цар", LangId::Bg),
            5.0,
            "full visible confirmation still precedes the accepted correction decision"
        );
    }

    #[test]
    fn accepted_two_character_word_keeps_existing_confirmation_threshold() {
        let mut core = InputMethodCore::new(InputConfig {
            post_commit_autocorrect: true,
            ..InputConfig::default()
        });
        core.load_word_lang("ca", 20, LangId::En);
        core.handle_key(press(Key::Char('ц')));
        assert!(core.apply_prediction_override("ца").is_some());
        let actions = core.handle_key(press(Key::Tab));
        assert!(actions.contains(&Action::ReplaceWord {
            replace_len: 2,
            text: "ca".into()
        }));
        assert_eq!(core.engine.word_frequency("ца", LangId::Bg), 0.0);
        assert_eq!(core.metrics().summary().accepted_predictions, 1);
        assert_eq!(
            core.take_commit_learning().words,
            vec![CommitObservation {
                language: Some(LangId::En)
            }]
        );
    }

    #[test]
    fn final_right_on_direct_text_records_one_full_acceptance() {
        let mut core = fresh(true);
        for ch in "qx".chars() {
            core.handle_key(press(Key::Char(ch)));
        }
        assert!(core.apply_prediction_override("qxz").is_some());
        let actions = core.handle_key(press(Key::Right));
        assert!(
            actions.contains(&Action::CommitText("z".into())),
            "the direct prefix is already delivered"
        );
        assert_eq!(
            core.metrics().summary().accepted_predictions,
            1,
            "the last Right consumes the full visible completion"
        );
        assert!(core.current_word().is_empty());
        assert_eq!(core.lang_detector.momentum_lang(), Some(LangId::En));
    }

    #[test]
    fn multiword_full_accept_records_word_specific_language_tracks() {
        let mut core = fresh(true);
        for ch in "hel".chars() {
            core.handle_key(press(Key::Char(ch)));
        }
        assert!(core.apply_prediction_override("hello здравей").is_some());
        core.handle_key(press(Key::Tab));
        let tracks = core.export_personal_profile().lang_cvm.unwrap().tracks;
        let en = tracks.iter().find(|(lang, _)| lang == "en").unwrap();
        assert!(en.1.words.iter().any(|word| word == "hello"));
        assert!(
            !en.1.words.iter().any(|word| word == "здравей"),
            "each accepted word needs its own script evidence"
        );
        let bg = tracks
            .iter()
            .find(|(lang, _)| lang == "bg")
            .expect("accepted Cyrillic word belongs in Bg");
        assert!(bg.1.words.iter().any(|word| word == "здравей"));
    }

    #[test]
    fn genuine_corpus_tie_still_reaches_detector_at_half_confidence() {
        let mut core = fresh(false);
        core.load_word_lang("qx", 100, LangId::En);
        core.load_word_lang("яь", 100, LangId::Bg);
        raw(&mut core, &[16, 45]);
        assert_eq!(
            core.dual_buffer.as_ref().unwrap().evidence_origin(),
            EvidenceOrigin::Corpus
        );
        let detected = core.detected_language();
        assert_eq!((detected.lang, detected.confidence), (LangId::En, 0.5));
    }

    #[test]
    fn commit_batch_is_consumed_once_and_replaced_by_the_next_operation() {
        let mut core = fresh(false);
        raw(&mut core, &[16, 45, 44]);
        core.handle_key(press(Key::Space));
        let batch = core.take_commit_learning();
        assert_eq!(batch.words, vec![CommitObservation { language: None }]);
        assert!(!batch.accepted_prediction);
        assert!(core.take_commit_learning().words.is_empty());

        raw(&mut core, &[16, 45, 44]);
        core.handle_key(press(Key::Space));
        // A standalone core user deliberately leaves that batch undrained.
        core.handle_key(press(Key::Space));
        assert!(core.take_commit_learning().words.is_empty());
        core.focus_lost();
        assert!(core.take_commit_learning().words.is_empty());
    }

    #[test]
    fn final_right_is_one_full_accept_for_dual_and_direct_semantic_and_raw_keys() {
        for dual in [false, true] {
            for right in [Key::Right, Key::RawCode(106)] {
                let mut core = fresh(true);
                if dual {
                    raw(&mut core, &[16, 45]);
                } else {
                    for ch in "qx".chars() {
                        core.handle_key(press(Key::Char(ch)));
                    }
                }
                assert!(core.apply_prediction_override("qxzz").is_some());
                let released = core.handle_key(KeyEvent {
                    key: right.clone(),
                    modifiers: Modifiers::RELEASE,
                });
                assert_eq!(released, vec![Action::ForwardKey]);
                assert_eq!(core.ghost_text(), "zz");
                assert!(core.take_commit_learning().words.is_empty());
                let partial = core.handle_key(press(right.clone()));
                assert_eq!(core.current_word(), "qxz");
                assert_eq!(core.metrics().summary().accepted_predictions, 0);
                assert!(core.take_commit_learning().words.is_empty());
                if !dual {
                    assert!(partial.contains(&Action::CommitText("z".into())));
                }
                let final_actions = core.handle_key(press(right));
                let commits: Vec<&str> = final_actions
                    .iter()
                    .filter_map(|action| match action {
                        Action::CommitText(text) => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                assert_eq!(commits, if dual { vec!["qxzz"] } else { vec!["z"] });
                assert!(core.current_word().is_empty());
                assert_eq!(core.metrics().summary().accepted_predictions, 1);
                let batch = core.take_commit_learning();
                assert!(batch.accepted_prediction);
                assert_eq!(
                    batch.words,
                    vec![CommitObservation {
                        language: Some(LangId::En)
                    }]
                );
                assert!(core.take_commit_learning().words.is_empty());
                core.handle_key(press(Key::Space));
                assert!(core.take_commit_learning().words.is_empty());
                assert_eq!(core.metrics().summary().accepted_predictions, 1);
            }
        }
    }
}

#[cfg(test)]
mod shortword_routing_contract_tests {
    use super::*;
    use crate::dual_buffer::EvidenceOrigin;

    fn raw(code: u16) -> KeyEvent {
        KeyEvent {
            key: Key::RawCode(code),
            modifiers: Modifiers::empty(),
        }
    }

    fn reduced(prior: Option<LangId>) -> InputMethodCore {
        let mut core = InputMethodCore::new(InputConfig::default());
        // Reduced from the frozen current corpus JSON facts. The native replay
        // remains authoritative for the separately loaded binary corpus.
        core.load_word_lang("like", 2_570_395, LangId::En);
        core.load_word_lang("li", 17_782, LangId::En);
        core.load_word_lang("ли", 2_511_886, LangId::Bg);
        core.apply_hints(&crate::master_loop::Hints {
            lang_prior: prior,
            ..Default::default()
        });
        core
    }

    #[test]
    fn shortword_bg_context_selects_live_supported_particle_without_lock() {
        let mut core = reduced(Some(LangId::Bg));
        core.handle_key(raw(38));
        assert_eq!(core.current_word(), "л");
        let actions = core.handle_key(raw(23));
        assert_eq!(core.current_word(), "ли");
        assert!(actions
            .iter()
            .any(|action| matches!(action, Action::ShowComposing { typed, .. } if typed == "ли")));
        assert_eq!(core.debug_state(), (true, false, true));
        let actions = core.handle_key(raw(57));
        assert_eq!(
            actions
                .iter()
                .filter(|action| matches!(action, Action::CommitText(word) if word == "ли"))
                .count(),
            1
        );
        assert_eq!(
            actions
                .iter()
                .filter(|action| matches!(action, Action::ForwardKey))
                .count(),
            1
        );
        assert!(!actions
            .iter()
            .any(|action| matches!(action, Action::ReplaceWord { .. })));
    }

    #[test]
    fn shortword_context_selection_is_not_independent_detector_confidence() {
        let mut core = reduced(Some(LangId::Bg));
        let before = core.detected_language();
        core.handle_key(raw(38));
        core.handle_key(raw(23));
        assert_eq!(
            core.dual_buffer.as_ref().unwrap().evidence_origin(),
            EvidenceOrigin::PriorOnly
        );
        let after = core.detected_language();
        assert_eq!(
            (after.lang, after.confidence),
            (before.lang, before.confidence)
        );
        core.handle_key(raw(57));
        assert_eq!(
            core.take_commit_learning().words,
            vec![CommitObservation {
                language: Some(LangId::Bg)
            }]
        );
        assert!(
            core.export_personal_profile().confirmed_words.is_empty(),
            "routing does not confirm a new exact word"
        );
        assert_eq!(core.engine.word_frequency("li", LangId::En), 17_782.0);
        assert_eq!(core.engine.word_frequency("ли", LangId::Bg), 2_511_886.0);
    }

    #[test]
    fn shortword_no_context_and_english_context_keep_english() {
        for prior in [None, Some(LangId::En)] {
            let mut core = reduced(prior);
            core.handle_key(raw(38));
            core.handle_key(raw(23));
            assert_eq!(core.current_word(), "li");
            assert_eq!(
                core.dual_buffer.as_ref().unwrap().evidence_origin(),
                EvidenceOrigin::Corpus
            );
            let actions = core.handle_key(raw(57));
            assert_eq!(
                actions
                    .iter()
                    .filter(|action| matches!(action, Action::CommitText(word) if word == "li"))
                    .count(),
                1
            );
        }
    }

    #[test]
    fn shortword_strong_english_recovers_after_bg_context_and_semantic_chars_stay_direct() {
        let mut core = reduced(Some(LangId::Bg));
        for code in [38, 23, 37, 18] {
            core.handle_key(raw(code));
        }
        assert_eq!(core.current_word(), "like");
        assert_eq!(
            core.dual_buffer.as_ref().unwrap().evidence_origin(),
            EvidenceOrigin::Corpus
        );

        let mut core = reduced(Some(LangId::Bg));
        for ch in "li".chars() {
            core.handle_key(KeyEvent {
                key: Key::Char(ch),
                modifiers: Modifiers::empty(),
            });
        }
        assert_eq!(core.current_word(), "li");
        assert!(core.dual_buffer.is_none());
    }

    #[test]
    fn shortword_backspace_rescores_fresh_without_compounding_prior() {
        let mut core = reduced(Some(LangId::Bg));
        core.handle_key(raw(38));
        core.handle_key(raw(23));
        assert_eq!(core.current_word(), "ли");
        let confidence = core.dual_buffer.as_ref().unwrap().confidence();
        for _ in 0..4 {
            core.handle_key(raw(37));
            assert_eq!(core.current_word(), "lik");
            core.handle_key(raw(14));
            assert_eq!(core.current_word(), "ли");
            assert_eq!(core.dual_buffer.as_ref().unwrap().confidence(), confidence);
            assert!(!core.dual_buffer.as_ref().unwrap().is_locked());
        }
    }

    #[test]
    fn shortword_unsupported_context_does_not_create_language_evidence() {
        let mut core = InputMethodCore::new(InputConfig {
            ghost_text: false,
            ..Default::default()
        });
        core.apply_hints(&crate::master_loop::Hints {
            lang_prior: Some(LangId::Bg),
            ..Default::default()
        });
        let before = core.detected_language();
        core.handle_key(raw(38));
        core.handle_key(raw(23));
        assert_eq!(core.current_word(), "ли");
        assert_eq!(
            core.dual_buffer.as_ref().unwrap().evidence_origin(),
            EvidenceOrigin::UnsupportedBoth
        );
        core.handle_key(raw(57));
        assert_eq!(
            core.take_commit_learning().words,
            vec![CommitObservation { language: None }]
        );
        let after = core.detected_language();
        assert_eq!(
            (after.lang, after.confidence),
            (before.lang, before.confidence)
        );
        assert!(core.export_personal_profile().confirmed_words.is_empty());
        assert_eq!(core.engine.word_frequency("ли", LangId::Bg), 0.0);
    }
}

#[cfg(test)]
mod terminal_context_evidence_tests {
    use super::*;

    fn raw(code: u16) -> KeyEvent {
        KeyEvent {
            key: Key::RawCode(code),
            modifiers: Modifiers::empty(),
        }
    }

    #[test]
    fn terminal_context_exact_selection_does_not_create_detector_confidence_or_trust() {
        let mut core = InputMethodCore::new(InputConfig::default());
        core.load_word_lang("with", 7_079_457, LangId::En);
        core.load_word_lang("wi", 12_302, LangId::En);
        core.load_word_lang("ви", 2_137_962, LangId::Bg);
        core.set_terminal_context_lang(Some(LangId::Bg));
        core.handle_key(raw(17));
        core.handle_key(raw(23));
        assert_eq!(core.current_word(), "wi");
        let detected = core.detected_language();

        let actions = core.handle_key(raw(57));

        assert!(actions
            .iter()
            .any(|a| matches!(a, Action::CommitText(word) if word == "ви")));
        assert_eq!(core.detected_language().lang, detected.lang);
        assert_eq!(core.detected_language().confidence, detected.confidence);
        assert_eq!(
            core.take_commit_learning().words,
            vec![CommitObservation {
                language: Some(LangId::Bg)
            }]
        );
        assert_eq!(core.lang_detector.momentum_lang(), Some(LangId::Bg));
        assert!(core.export_personal_profile().confirmed_words.is_empty());
        assert_eq!(core.engine.word_frequency("wi", LangId::En), 12_302.0);
        assert_eq!(core.engine.word_frequency("ви", LangId::Bg), 2_137_962.0);
        assert_eq!(core.terminal_context_lang, None);
    }

    #[test]
    fn terminal_context_agreement_with_current_word_does_not_mark_a_flip() {
        let mut core = InputMethodCore::new(InputConfig::default());
        core.load_word_lang("wi", 1_000_000, LangId::En);
        core.load_word_lang("ви", 1, LangId::Bg);
        core.set_terminal_context_lang(Some(LangId::En));
        core.handle_key(raw(17));
        core.handle_key(raw(23));
        assert!(!core.current_word_had_flip);
        let origins = core.word_origins.clone();

        assert_eq!(core.resolve_word_boundary(), "wi");

        assert!(!core.current_word_had_flip);
        assert!(core.word_origins == origins);
    }
}

#[cfg(test)]
mod hybrid_space_eligibility_tests {
    use super::*;

    #[test]
    fn hybrid_space_regained_ghost_attestation_cannot_bypass_delivery_guard() {
        for space in [Key::Space, Key::RawCode(57)] {
            let mut core = InputMethodCore::new(InputConfig {
                ghost_text: true,
                space_accept: true,
                ..InputConfig::default()
            });
            core.load_word("qaxyz", 1_000_000);
            for key in [Key::RawCode(16), Key::Char('a'), Key::RawCode(45)] {
                core.handle_key(KeyEvent {
                    key,
                    modifiers: Modifiers::empty(),
                });
            }
            assert_eq!(core.current_word(), "qax");
            assert_eq!(core.ghost_text(), "yz");
            assert!(core.dual_buffer.is_none());
            assert!(!core.space_accept_armed());
            core.attest_space_accept_display(true);
            let actions = core.handle_key(KeyEvent {
                key: space,
                modifiers: Modifiers::empty(),
            });
            assert_eq!(actions, vec![Action::HideGhost, Action::ForwardKey]);
            let batch = core.take_commit_learning();
            assert_eq!(batch.words.len(), 1);
            assert!(!batch.accepted_prediction);
            assert!(core.current_word().is_empty() && !core.has_ghost());
        }
    }
}

#[cfg(test)]
mod caps_context_followup_tests {
    use super::*;

    fn fresh(opt_in: bool) -> InputMethodCore {
        let mut core = InputMethodCore::new(InputConfig {
            post_commit_autocorrect: opt_in,
            ghost_text: false,
            ..InputConfig::default()
        });
        for word in ["hello", "there", "london"] {
            core.load_word_lang(word, 1_000, LangId::En);
        }
        core
    }

    fn press(core: &mut InputMethodCore, key: Key) -> Vec<Action> {
        core.handle_key(KeyEvent {
            key,
            modifiers: Modifiers::empty(),
        })
    }

    fn type_semantic(core: &mut InputMethodCore, word: &str) {
        for ch in word.chars() {
            let actions = press(core, Key::Char(ch));
            assert_eq!(
                actions
                    .iter()
                    .filter(|a| matches!(a, Action::ForwardKey))
                    .count(),
                1,
                "semantic character must reach the client once: {actions:?}"
            );
            assert!(
                !actions
                    .iter()
                    .any(|a| matches!(a, Action::CommitText(_) | Action::ReplaceWord { .. })),
                "fixture must use literal semantic typing, not an injected rewrite: {actions:?}"
            );
        }
        assert_eq!(core.current_word(), word);
    }

    fn boundary(core: &mut InputMethodCore) -> Vec<Action> {
        let actions = press(core, Key::Space);
        eprintln!("semantic Space: {actions:?}");
        assert!(core.current_word().is_empty());
        assert_eq!(
            actions
                .iter()
                .filter(|a| matches!(a, Action::ForwardKey))
                .count(),
            1
        );
        assert!(!actions.iter().any(|a| matches!(a, Action::CommitText(_))));
        actions
    }

    fn replacements(actions: &[Action]) -> Vec<(usize, String)> {
        actions
            .iter()
            .filter_map(|action| match action {
                Action::ReplaceWord { replace_len, text } => Some((*replace_len, text.clone())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn caps_context_followup_start_of_input_capitalizes_real_committed_word() {
        let mut core = fresh(true);
        type_semantic(&mut core, "hello");
        assert_eq!(
            replacements(&boundary(&mut core)),
            vec![(5, "Hello".into())]
        );
    }

    #[test]
    fn caps_context_followup_previous_ordinary_word_keeps_middle_lowercase() {
        let mut core = fresh(true);
        type_semantic(&mut core, "there");
        boundary(&mut core);
        type_semantic(&mut core, "hello");
        assert!(replacements(&boundary(&mut core)).is_empty());
    }

    #[test]
    fn caps_context_followup_current_punctuation_does_not_capitalize_middle_word() {
        let mut core = fresh(true);
        type_semantic(&mut core, "there");
        boundary(&mut core);
        type_semantic(&mut core, "hello!");
        assert!(replacements(&boundary(&mut core)).is_empty());
    }

    #[test]
    fn caps_context_followup_previous_semantic_sentence_end_capitalizes_next_word() {
        let mut core = fresh(true);
        type_semantic(&mut core, "there!");
        boundary(&mut core);
        type_semantic(&mut core, "hello");
        assert_eq!(
            replacements(&boundary(&mut core)),
            vec![(5, "Hello".into())]
        );
    }

    #[test]
    fn caps_context_followup_default_off_never_rewrites_these_boundaries() {
        assert!(!InputConfig::default().post_commit_autocorrect);
        let mut core = fresh(false);
        for word in ["hello", "there!", "hello"] {
            type_semantic(&mut core, word);
            assert!(replacements(&boundary(&mut core)).is_empty());
        }
    }

    #[test]
    fn caps_context_followup_registered_proper_noun_capitalizes_in_middle() {
        let mut core = fresh(true);
        let dir =
            std::env::temp_dir().join(format!("smartkey-caps-context-noun-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let corpus = dir.join("proper.json");
        std::fs::write(
            &corpus,
            r#"{"unigrams":{"london":1000},"proper_nouns":["London"]}"#,
        )
        .unwrap();
        core.load_corpus_file(&corpus).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        type_semantic(&mut core, "there");
        boundary(&mut core);
        type_semantic(&mut core, "london");
        assert_eq!(
            replacements(&boundary(&mut core)),
            vec![(6, "London".into())]
        );
    }
}

#[cfg(test)]
mod external_rewrite_contract_tests {
    use super::*;

    fn fresh(external: bool, post: bool, ghost: bool) -> InputMethodCore {
        let mut core = InputMethodCore::new(InputConfig {
            post_commit_autocorrect: post,
            ghost_text: ghost,
            ..InputConfig::default()
        });
        assert!(
            core.external_rewrite_supported,
            "other hosts retain the default"
        );
        core.set_external_rewrite_supported(external);
        core
    }

    fn key(core: &mut InputMethodCore, key: Key) -> Vec<Action> {
        core.handle_key(KeyEvent {
            key,
            modifiers: Modifiers::empty(),
        })
    }

    fn replacements(actions: &[Action]) -> Vec<(usize, &str)> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::ReplaceWord { replace_len, text } => Some((*replace_len, text.as_str())),
                _ => None,
            })
            .collect()
    }

    fn commits(actions: &[Action]) -> Vec<&str> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::CommitText(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    // ForwardKey is delivered only after the entire action batch returns.
    // Preedit/ghost text is never part of the committed client document.
    fn project(document: &mut String, actions: &[Action], original: Option<char>) {
        for action in actions {
            match action {
                Action::CommitText(text) => document.push_str(text),
                Action::ReplaceWord { replace_len, text } => {
                    for _ in 0..*replace_len {
                        assert!(document.pop().is_some());
                    }
                    document.push_str(text);
                }
                _ => {}
            }
        }
        if actions.iter().any(|a| matches!(a, Action::ForwardKey)) {
            document.push(original.expect("only modeled semantic/Space events may forward"));
        }
    }

    fn semantic(core: &mut InputMethodCore, document: &mut String, word: &str) {
        for ch in word.chars() {
            let a = key(core, Key::Char(ch));
            project(document, &a, Some(ch));
        }
    }

    fn raw(core: &mut InputMethodCore, codes: &[u16]) {
        for code in codes {
            key(core, Key::RawCode(*code));
        }
    }

    fn learned_once(core: &mut InputMethodCore, expected: &[&str]) {
        assert!(core.current_word().is_empty());
        let batch = core.take_commit_learning();
        assert_eq!(batch.words.len(), expected.len());
        let profile = core.export_personal_profile();
        for word in expected {
            assert!(profile.cvm.words.iter().any(|w| w == word));
        }
        key(core, Key::Space);
        assert!(
            core.take_commit_learning().words.is_empty(),
            "empty boundary cannot relearn"
        );
    }

    #[test]
    fn external_rewrite_legacy_false_preserves_literal_and_does_not_activate() {
        let mut core = fresh(false, false, false);
        core.load_word_lang("здравей", 1_000_000, LangId::Bg);
        let mut document = String::new();
        semantic(&mut core, &mut document, "z");
        let second = key(&mut core, Key::Char('d'));
        eprintln!(
            "unsupported legacy d: {second:?}; word={:?}",
            core.current_word()
        );
        assert!(replacements(&second).is_empty());
        project(&mut document, &second, Some('d'));
        assert_eq!((core.current_word(), document.as_str()), ("zd", "zd"));
        assert!(!core.transliteration_active);
        semantic(&mut core, &mut document, "r");
        assert_eq!((core.current_word(), document.as_str()), ("zdr", "zdr"));
        assert!(core.export_personal_profile().cvm.words.is_empty());
    }

    #[test]
    fn external_rewrite_legacy_true_preserves_real_replacement_producer() {
        let mut core = fresh(true, false, false);
        core.load_word_lang("здравей", 1_000_000, LangId::Bg);
        let mut document = String::new();
        semantic(&mut core, &mut document, "z");
        let second = key(&mut core, Key::Char('d'));
        assert_eq!(replacements(&second), vec![(1, "зд")]);
        project(&mut document, &second, Some('d'));
        assert_eq!((core.current_word(), document.as_str()), ("зд", "зд"));
        assert!(core.transliteration_active);
    }

    #[test]
    fn external_rewrite_unowned_boundary_language_false_learns_literal_before_any_correction() {
        let mut core = fresh(false, true, false);
        core.load_word_lang("car", 100, LangId::En);
        let mut document = String::new();
        semantic(&mut core, &mut document, "цар");
        assert_eq!(core.current_word(), "цар");
        assert!(core.export_personal_profile().cvm.words.is_empty());
        let a = key(&mut core, Key::Space);
        eprintln!("unsupported language boundary: {a:?}");
        assert!(replacements(&a).is_empty());
        project(&mut document, &a, Some(' '));
        assert_eq!(document, "цар ");
        assert_eq!(core.context_words().0, Some("цар"));
        let profile = core.export_personal_profile();
        assert!(!profile.cvm.words.iter().any(|w| w == "car"));
        learned_once(&mut core, &["цар"]);
    }

    #[test]
    fn external_rewrite_unowned_boundary_caps_false_preserves_literal() {
        let mut core = fresh(false, true, false);
        core.load_word_lang("hello", 1_000, LangId::En);
        let mut document = String::new();
        semantic(&mut core, &mut document, "hello");
        let a = key(&mut core, Key::Space);
        eprintln!("unsupported caps boundary: {a:?}");
        assert!(replacements(&a).is_empty());
        project(&mut document, &a, Some(' '));
        assert_eq!(document, "hello ");
        learned_once(&mut core, &["hello"]);
    }

    #[test]
    fn external_rewrite_owned_singleword_boundary_language_and_caps_remain_available() {
        for external in [false, true] {
            for (boundary, delimiter) in [
                (Key::Space, ' '),
                (Key::Return, '\n'),
                (Key::Char('!'), '!'),
            ] {
                let mut core = fresh(external, true, false);
                core.load_word_lang("царство", 1_000_000, LangId::Bg);
                core.load_word_lang("цар", 5, LangId::Bg);
                core.load_word_lang("car", 100, LangId::En);
                raw(&mut core, &[46, 30, 19]);
                assert_eq!(core.current_word(), "цар");
                assert!(core.debug_state().0, "whole word is still owned preedit");
                let a = key(&mut core, boundary);
                eprintln!("owned language/caps external={external}: {a:?}");
                assert_eq!(commits(&a), vec!["цар"]);
                assert_eq!(replacements(&a), vec![(3, "car"), (3, "Car")]);
                let mut document = String::new();
                project(&mut document, &a, Some(delimiter));
                assert_eq!(document, format!("Car{delimiter}"));
                let profile = core.export_personal_profile();
                assert!(!profile.cvm.words.iter().any(|w| w == "цар"));
                learned_once(&mut core, &["car"]);
            }
        }
    }

    #[test]
    fn external_rewrite_owned_singleword_caps_remain_available() {
        let mut core = fresh(false, true, false);
        core.load_word_lang("hello", 1_000, LangId::En);
        raw(&mut core, &[35, 18, 38, 38, 24]);
        assert_eq!(core.current_word(), "hello");
        let a = key(&mut core, Key::Space);
        assert_eq!(commits(&a), vec!["hello"]);
        assert_eq!(replacements(&a), vec![(5, "Hello")]);
        let mut document = String::new();
        project(&mut document, &a, Some(' '));
        assert_eq!(document, "Hello ");
        learned_once(&mut core, &["hello"]);
    }

    #[test]
    fn external_rewrite_suffix_only_accept_false_learns_visible_word_once() {
        let mut core = fresh(false, true, true);
        core.load_word_lang("hello", 1_000, LangId::En);
        let mut document = String::new();
        semantic(&mut core, &mut document, "hel");
        assert_eq!((core.current_word(), core.ghost_text()), ("hel", "lo"));
        assert!(!core.debug_state().0);
        let a = key(&mut core, Key::Tab);
        eprintln!("unsupported suffix accept: {a:?}");
        assert_eq!(commits(&a), vec!["lo"]);
        assert!(replacements(&a).is_empty());
        project(&mut document, &a, None);
        assert_eq!(document, "hello");
        assert_eq!(core.metrics().summary().accepted_predictions, 1);
        learned_once(&mut core, &["hello"]);
    }

    #[test]
    fn external_rewrite_multiword_accept_false_learns_visible_phrase_not_corrected_last_word() {
        let mut core = fresh(false, true, true);
        core.load_word_lang("hello", 1_000, LangId::En);
        core.load_word_lang("цар", 5, LangId::Bg);
        core.load_word_lang("car", 100, LangId::En);
        raw(&mut core, &[35, 18, 38]);
        // Public override is the existing multiword producer; ordinary ghost
        // generation is intentionally singleword. No private word/ghost writes.
        assert!(
            matches!(core.apply_prediction_override("hello цар"), Some(Action::ShowComposing { typed, ghost }) if typed == "hel" && ghost == "lo цар")
        );
        let a = key(&mut core, Key::Tab);
        eprintln!("unsupported multiword accept: {a:?}");
        assert_eq!(commits(&a), vec!["hello цар"]);
        assert!(replacements(&a).is_empty());
        let mut document = String::new();
        project(&mut document, &a, None);
        assert_eq!(document, "hello цар");
        assert_eq!(core.metrics().summary().accepted_predictions, 1);
        let profile = core.export_personal_profile();
        assert!(!profile.cvm.words.iter().any(|w| w == "car"));
        assert!(profile
            .markov_bigrams
            .iter()
            .any(|(p, w, n)| p == "hello" && w == "цар" && *n == 1));
        learned_once(&mut core, &["hello", "цар"]);
    }

    #[test]
    fn external_rewrite_owned_singleword_accept_keeps_language_and_caps() {
        let mut core = fresh(false, true, true);
        core.load_word_lang("царство", 1_000_000, LangId::Bg);
        core.load_word_lang("цар", 5, LangId::Bg);
        core.load_word_lang("car", 100, LangId::En);
        raw(&mut core, &[46]);
        assert_eq!(core.current_word(), "ц");
        assert!(matches!(
            core.apply_prediction_override("цар"),
            Some(Action::ShowComposing { typed, ghost }) if typed == "ц" && ghost == "ар"
        ));
        let a = key(&mut core, Key::Tab);
        eprintln!("owned singleword accept: {a:?}");
        assert_eq!(commits(&a), vec!["цар"]);
        assert_eq!(replacements(&a), vec![(3, "car"), (3, "Car")]);
        let mut document = String::new();
        project(&mut document, &a, None);
        assert_eq!(document, "Car");
        assert_eq!(core.metrics().summary().accepted_predictions, 1);
        assert!(!core
            .export_personal_profile()
            .cvm
            .words
            .iter()
            .any(|w| w == "цар"));
        learned_once(&mut core, &["car"]);
    }

    #[test]
    fn external_rewrite_capability_drop_keeps_active_conversion_safe_append() {
        let mut core = fresh(true, false, false);
        core.load_word_lang("здравей", 1_000_000, LangId::Bg);
        let mut document = String::new();
        semantic(&mut core, &mut document, "zd");
        assert_eq!((core.current_word(), document.as_str()), ("зд", "зд"));
        core.set_external_rewrite_supported(false);
        for (k, glyph) in [(Key::RawCode(19), "р"), (Key::Char('a'), "а")] {
            let a = key(&mut core, k);
            assert!(replacements(&a).is_empty());
            assert_eq!(commits(&a), vec![glyph]);
            assert!(!a.iter().any(|a| matches!(a, Action::ForwardKey)));
            project(&mut document, &a, None);
        }
        assert_eq!((core.current_word(), document.as_str()), ("здра", "здра"));
        let a = key(&mut core, Key::Space);
        project(&mut document, &a, Some(' '));
        assert_eq!(document, "здра ");
        learned_once(&mut core, &["здра"]);
    }

    #[test]
    fn external_rewrite_capability_false_survives_reset_focus_and_profile_roundtrip() {
        let mut core = fresh(false, false, false);
        key(&mut core, Key::Char('x'));
        core.reset();
        assert!(!core.external_rewrite_supported);
        core.focus_lost();
        core.focus_gained();
        assert!(!core.external_rewrite_supported);
        let profile = core.export_personal_profile();
        core.import_personal_profile(&profile);
        assert!(!core.external_rewrite_supported);
        core.set_external_rewrite_supported(true);
        assert!(core.external_rewrite_supported);
    }
}

#[cfg(test)]
mod sentence_boundary_followup_tests {
    use super::*;

    const HELLO: &[u16] = &[35, 18, 38, 38, 24];
    const WORLD: &[u16] = &[17, 24, 19, 38, 32];

    fn fresh(post: bool) -> InputMethodCore {
        let mut core = InputMethodCore::new(InputConfig {
            post_commit_autocorrect: post,
            lang_detection: false,
            ..InputConfig::default()
        });
        for word in ["hello", "world", "seed"] {
            core.load_word_lang(word, 1_000, LangId::En);
        }
        core
    }

    fn key(core: &mut InputMethodCore, key: Key) -> Vec<Action> {
        core.handle_key(KeyEvent {
            key,
            modifiers: Modifiers::empty(),
        })
    }

    // The adapter forwards the original key only after executing the batch.
    // Composing/ghost actions never insert text into the client document.
    fn project(document: &mut String, actions: &[Action], original: Option<char>) {
        for action in actions {
            match action {
                Action::CommitText(text) => document.push_str(text),
                Action::ReplaceWord { replace_len, text } => {
                    for _ in 0..*replace_len {
                        assert!(document.pop().is_some());
                    }
                    document.push_str(text);
                }
                _ => {}
            }
        }
        if actions.iter().any(|a| matches!(a, Action::ForwardKey)) {
            document.push(original.expect("modeled printable boundary"));
        }
    }

    fn raw(core: &mut InputMethodCore, codes: &[u16]) {
        for code in codes {
            key(core, Key::RawCode(*code));
        }
    }

    fn sentence(core: &mut InputMethodCore) {
        raw(core, HELLO);
        key(core, Key::RawCode(52));
        key(core, Key::RawCode(57));
    }

    fn prediction(core: &mut InputMethodCore) -> String {
        key(core, Key::RawCode(17));
        core.predictions()
            .first()
            .expect("trusted world completion")
            .word
            .clone()
    }

    #[test]
    fn sentence_boundary_raw_dot_owned_postcommit_caps() {
        let mut core = fresh(true);
        let mut client = String::new();
        raw(&mut core, HELLO);
        project(&mut client, &key(&mut core, Key::RawCode(52)), Some('.'));
        project(&mut client, &key(&mut core, Key::Space), Some(' '));
        raw(&mut core, WORLD);
        let actions = key(&mut core, Key::Space);
        eprintln!("raw-dot world boundary: {actions:?}");
        project(&mut client, &actions, Some(' '));
        assert_eq!(client, "Hello. World ");
        assert_eq!(core.context_words().0, Some("world"));
        let profile = core.export_personal_profile();
        assert!(profile.cvm.words.iter().any(|w| w == "hello"));
        assert!(!profile
            .cvm
            .words
            .iter()
            .any(|w| w == "." || w == "hello." || w.is_empty()));
        assert!(profile
            .markov_bigrams
            .iter()
            .any(|(p, w, n)| p == "hello" && w == "world" && *n == 1));
        assert!(
            profile.confirmed_words.is_empty(),
            "caps supplies no language proof"
        );
    }

    #[test]
    fn sentence_boundary_raw_dot_default_prediction_caps() {
        let mut core = fresh(false);
        sentence(&mut core);
        assert_eq!(prediction(&mut core), "World");
    }

    #[test]
    fn sentence_boundary_default_tab_accepts_actual_display_not_hidden_caps() {
        let mut core = fresh(false);
        sentence(&mut core);
        let offer = key(&mut core, Key::RawCode(17));
        assert!(offer.iter().any(|a| matches!(a, Action::ShowComposing { typed, ghost } if typed == "w" && ghost == "orld")));
        let actions = key(&mut core, Key::Tab);
        assert_eq!(
            actions
                .iter()
                .filter_map(|a| if let Action::CommitText(t) = a {
                    Some(t.as_str())
                } else {
                    None
                })
                .collect::<Vec<_>>(),
            vec!["world"]
        );
        assert!(!actions
            .iter()
            .any(|a| matches!(a, Action::ReplaceWord { .. })));
        assert_eq!(core.metrics().summary().accepted_predictions, 1);
        assert!(core.current_word().is_empty());
    }

    fn boundary_prediction(boundary: Key, modifiers: Modifiers, whitespace: bool) {
        let mut core = fresh(false);
        raw(&mut core, HELLO);
        let actions = core.handle_key(KeyEvent {
            key: boundary.clone(),
            modifiers,
        });
        assert!(actions.iter().any(|a| matches!(a, Action::ForwardKey)));
        assert!(core.current_word().is_empty());
        if whitespace {
            key(&mut core, Key::Space);
        }
        assert_eq!(prediction(&mut core), "World", "boundary {boundary:?}");
    }

    #[test]
    fn sentence_boundary_raw_bang_prediction() {
        boundary_prediction(Key::RawCode(2), Modifiers::SHIFT, true);
    }

    #[test]
    fn sentence_boundary_raw_question_prediction() {
        boundary_prediction(Key::RawCode(53), Modifiers::SHIFT, true);
    }

    #[test]
    fn sentence_boundary_return_prediction() {
        boundary_prediction(Key::Return, Modifiers::empty(), false);
    }

    #[test]
    fn sentence_boundary_local_backspace_empty_retype_and_escape_keep_hint() {
        for erase_all in [false, true] {
            let mut core = fresh(false);
            sentence(&mut core);
            raw(&mut core, &[17, 24]);
            key(&mut core, Key::Backspace);
            if erase_all {
                key(&mut core, Key::Backspace);
            }
            let next = if erase_all { 17 } else { 24 };
            key(&mut core, Key::RawCode(next));
            assert_eq!(core.predictions()[0].word, "World");
            assert!(core.has_ghost());
            key(&mut core, Key::Escape); // Existing dismiss-only path retains preedit.
            assert!(!core.current_word().is_empty());
            key(&mut core, Key::RawCode(19));
            // Escape rejected the previous candidate; preserve sentence casing,
            // without requiring its now-rejected word to remain top-ranked.
            assert!(core.predictions()[0]
                .word
                .chars()
                .next()
                .is_some_and(char::is_uppercase));
        }
    }

    #[test]
    fn sentence_boundary_repeated_spaces_and_release_preserve_hint() {
        let mut core = fresh(false);
        sentence(&mut core);
        key(&mut core, Key::Space);
        core.handle_key(KeyEvent {
            key: Key::Char('v'),
            modifiers: Modifiers::CTRL | Modifiers::RELEASE,
        });
        assert_eq!(prediction(&mut core), "World");
    }

    #[test]
    fn sentence_boundary_forwarded_edits_reset_focus_and_shortcuts_withdraw_hint() {
        for kind in 0..7 {
            let mut core = fresh(false);
            sentence(&mut core);
            match kind {
                0 => {
                    key(&mut core, Key::Backspace);
                }
                1 => {
                    raw(&mut core, &[17]);
                    key(&mut core, Key::Backspace);
                    key(&mut core, Key::Backspace);
                }
                2 => {
                    core.reset();
                }
                3 => {
                    core.focus_lost();
                    core.focus_gained();
                }
                4 | 5 => {
                    core.handle_key(KeyEvent {
                        key: Key::Char('v'),
                        modifiers: if kind == 4 {
                            Modifiers::CTRL
                        } else {
                            Modifiers::ALT
                        },
                    });
                }
                _ => {
                    key(&mut core, Key::Left);
                    // A real public external-context update distinguishes invalidation
                    // from the existing no-context start-of-input capitalization.
                    core.apply_hints(&crate::master_loop::Hints {
                        context_words: vec!["seed".into()],
                        ..Default::default()
                    });
                }
            }
            assert_eq!(prediction(&mut core), "world", "withdrawal {kind}");
        }
    }

    #[test]
    fn sentence_boundary_completed_numeric_technical_and_mixed_tokens_consume_hint() {
        for token in ["12", "foo.bar", "aБ"] {
            let mut core = fresh(false);
            sentence(&mut core);
            for ch in token.chars() {
                key(&mut core, Key::Char(ch));
            }
            key(&mut core, Key::Space);
            assert_eq!(prediction(&mut core), "world", "completed {token}");
        }
    }

    #[test]
    fn sentence_boundary_multiword_accept_does_not_capitalize_phrase_final_word() {
        let mut core = fresh(true);
        sentence(&mut core);
        raw(&mut core, &[17]);
        // Existing public multiword producer, distinct from the natural Tab guard.
        assert!(core.apply_prediction_override("world seed").is_some());
        let actions = key(&mut core, Key::Tab);
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, Action::ReplaceWord { .. })),
            "{actions:?}"
        );
        assert_eq!(prediction(&mut core), "world");
    }

    #[test]
    fn sentence_boundary_plain_space_adjacent_dot_and_decimal_keep_existing_casing() {
        for sequence in [0, 1, 2] {
            let mut core = fresh(false);
            if sequence == 2 {
                for ch in "1.2".chars() {
                    key(&mut core, Key::Char(ch));
                }
                key(&mut core, Key::Space);
            } else {
                raw(&mut core, HELLO);
                key(
                    &mut core,
                    if sequence == 0 {
                        Key::Space
                    } else {
                        Key::RawCode(52)
                    },
                );
            }
            assert_eq!(prediction(&mut core), "world", "control {sequence}");
        }
    }

    // Review follow-up: these hints describe provisional key delivery, never
    // acknowledged client deletion, lexical punctuation, or language evidence.
    fn review_dot(core: &mut InputMethodCore, client: &mut String, spaces: usize) {
        raw(core, HELLO);
        project(client, &key(core, Key::RawCode(52)), Some('.'));
        for _ in 0..spaces {
            project(client, &key(core, Key::Space), Some(' '));
        }
        assert_eq!(client.as_str(), format!("hello.{}", " ".repeat(spaces)));
    }

    fn review_forwarded_backspace(core: &mut InputMethodCore, client: &mut String) {
        let actions = key(core, Key::Backspace);
        assert!(actions.iter().any(|a| matches!(a, Action::ForwardKey)));
        assert!(!actions
            .iter()
            .any(|a| matches!(a, Action::CommitText(_) | Action::ReplaceWord { .. })));
        assert!(client.pop().is_some());
    }

    fn review_only_hello_learned(core: &InputMethodCore) {
        let profile = core.export_personal_profile();
        assert_eq!(profile.cvm.words, vec!["hello"]);
        assert!(profile.markov_bigrams.is_empty());
        assert!(profile.markov_trigrams.is_empty());
        assert!(profile.confirmed_words.is_empty());
    }

    #[test]
    fn sentence_boundary_review_escape_cancels_local_prefix_not_client_boundary() {
        let mut core = InputMethodCore::new(InputConfig {
            ghost_text: false,
            lang_detection: false,
            ..InputConfig::default()
        });
        for word in ["hello", "world"] {
            core.load_word_lang(word, 1_000, LangId::En);
        }
        let mut client = String::new();
        review_dot(&mut core, &mut client, 1);
        let actions = key(&mut core, Key::RawCode(17));
        assert!(actions.iter().any(|a| matches!(a, Action::ShowComposing { typed, ghost } if typed == "w" && ghost.is_empty())));
        assert!(!core.has_ghost());
        let cancelled = key(&mut core, Key::Escape);
        assert!(matches!(cancelled.as_slice(), [Action::HideGhost]));
        assert!(core.current_word().is_empty());
        assert_eq!(client, "hello. ");
        review_only_hello_learned(&core);
        assert_eq!(prediction(&mut core), "World");
    }

    #[test]
    fn sentence_boundary_review_empty_tab_confirms_forwarded_terminator() {
        let mut core = fresh(false);
        let mut client = String::new();
        review_dot(&mut core, &mut client, 0);
        let actions = key(&mut core, Key::Tab);
        assert!(matches!(actions.as_slice(), [Action::ForwardKey]));
        project(&mut client, &actions, Some('\t'));
        assert_eq!(client, "hello.\t");
        assert!(core.current_word().is_empty());
        review_only_hello_learned(&core);
        assert_eq!(prediction(&mut core), "World");
    }

    #[test]
    fn sentence_boundary_review_trimming_extra_space_retains_known_separator() {
        let mut core = fresh(false);
        let mut client = String::new();
        review_dot(&mut core, &mut client, 2);
        review_forwarded_backspace(&mut core, &mut client);
        assert_eq!(client, "hello. ");
        review_only_hello_learned(&core);
        assert_eq!(prediction(&mut core), "World");
    }

    #[test]
    fn sentence_boundary_review_last_space_delete_and_reinsert_restores_hint() {
        let mut core = fresh(false);
        let mut client = String::new();
        review_dot(&mut core, &mut client, 1);
        review_forwarded_backspace(&mut core, &mut client);
        assert_eq!(client, "hello.");
        project(&mut client, &key(&mut core, Key::Space), Some(' '));
        assert_eq!(client, "hello. ");
        review_only_hello_learned(&core);
        assert_eq!(prediction(&mut core), "World");
    }

    #[test]
    fn sentence_boundary_review_repeated_separator_and_punctuation_deletions() {
        // Exercise every arm even on RED; only capitalize with a surviving or
        // reinserted separator after a terminator the model still tracks.
        let mut mismatches = Vec::new();
        for (spaces, deleted, reinsert, expected) in [
            (3, 1, false, "World"),
            (3, 2, false, "World"),
            (3, 3, false, "world"),
            (3, 3, true, "World"),
            (3, 4, false, "world"),
            (3, 4, true, "world"),
        ] {
            let mut core = fresh(false);
            let mut client = String::new();
            review_dot(&mut core, &mut client, spaces);
            for _ in 0..deleted {
                review_forwarded_backspace(&mut core, &mut client);
            }
            if reinsert {
                project(&mut client, &key(&mut core, Key::Space), Some(' '));
            }
            review_only_hello_learned(&core);
            let actual = prediction(&mut core);
            eprintln!("separator spaces={spaces} deleted={deleted} reinsert={reinsert} client={client:?} prediction={actual}");
            if actual != expected {
                mismatches.push((spaces, deleted, reinsert, actual, expected));
            }
        }
        assert!(mismatches.is_empty(), "{mismatches:?}");
    }

    #[test]
    fn sentence_boundary_review_opaque_tab_return_then_backspace_withdraws_hint() {
        for (spaces, boundary, extra_space) in [
            (0, Key::Tab, false),
            (0, Key::Tab, true),
            (1, Key::Tab, false),
            (1, Key::Tab, true),
            (0, Key::Return, false),
            (1, Key::Return, true),
        ] {
            let mut core = fresh(false);
            let mut client = String::new();
            review_dot(&mut core, &mut client, spaces);
            let glyph = if boundary == Key::Tab { '\t' } else { '\n' };
            let actions = key(&mut core, boundary);
            assert_eq!(
                actions
                    .iter()
                    .filter(|a| matches!(a, Action::ForwardKey))
                    .count(),
                1
            );
            assert!(!actions
                .iter()
                .any(|a| matches!(a, Action::CommitText(_) | Action::ReplaceWord { .. })));
            project(&mut client, &actions, Some(glyph));
            if extra_space {
                project(&mut client, &key(&mut core, Key::Space), Some(' '));
            }
            review_forwarded_backspace(&mut core, &mut client);
            review_only_hello_learned(&core);
            assert_eq!(prediction(&mut core), "world", "opaque client {client:?}");
        }
    }

    #[test]
    fn sentence_boundary_review_space_release_cannot_inflate_separator_count() {
        let mut core = fresh(false);
        let mut client = String::new();
        review_dot(&mut core, &mut client, 1);
        let actions = core.handle_key(KeyEvent {
            key: Key::Space,
            modifiers: Modifiers::RELEASE,
        });
        assert!(matches!(actions.as_slice(), [Action::ForwardKey]));
        // A release does not insert its key glyph into the client projection.
        review_forwarded_backspace(&mut core, &mut client);
        assert_eq!(client, "hello.");
        review_only_hello_learned(&core);
        assert_eq!(prediction(&mut core), "world");
    }

    #[test]
    fn sentence_boundary_review_forwarded_word_backspace_withdraws_hint() {
        for handed_off in [false, true] {
            let mut core = fresh(false);
            let mut client = String::new();
            review_dot(&mut core, &mut client, 2);
            if handed_off {
                raw(&mut core, &[17]);
            }
            let actions = key(&mut core, Key::Char('a'));
            project(&mut client, &actions, Some('a'));
            assert_eq!(core.current_word(), if handed_off { "wa" } else { "a" });
            assert_eq!(
                client,
                if handed_off {
                    "hello.  wa"
                } else {
                    "hello.  a"
                }
            );
            review_forwarded_backspace(&mut core, &mut client);
            review_only_hello_learned(&core);
            if handed_off {
                assert_eq!(core.current_word(), "w");
                assert_eq!(core.predictions()[0].word, "world");
                assert_eq!(client, "hello.  w");
            } else {
                assert!(core.current_word().is_empty());
                assert_eq!(prediction(&mut core), "world");
                assert_eq!(client, "hello.  ");
            }
        }
    }

    const REVIEW_CLOSERS: &[char] = &[
        '"', '\'', ')', ']', '}', '‘', '’', '“', '”', '«', '»', '‹', '›',
    ];

    fn tail_event(core: &mut InputMethodCore, event: KeyEvent, glyph: char, post: bool) -> bool {
        let mut client = String::new();
        raw(core, HELLO);
        project(&mut client, &key(core, Key::RawCode(52)), Some('.'));
        let tail = core.handle_key(event);
        let tail_word = core.current_word().to_string();
        project(&mut client, &tail, Some(glyph));
        project(&mut client, &key(core, Key::Space), Some(' '));
        if post {
            raw(core, WORLD);
            project(&mut client, &key(core, Key::Space), Some(' '));
            let expected = format!("Hello.{glyph} World ");
            eprintln!("tail={glyph:?} tailword={tail_word:?} actions={tail:?} client={client:?}");
            // Snapshot words follow HashMap iteration, not commit order.
            let mut learned = core.export_personal_profile().cvm.words;
            learned.sort();
            eprintln!("sorted learned words={learned:?}");
            client == expected
                && tail_word.is_empty()
                && tail == vec![Action::ForwardKey]
                && learned == vec!["hello", "world"]
        } else {
            let top = prediction(core);
            eprintln!("tail={glyph:?} tailword={tail_word:?} actions={tail:?} prediction={top} client={client:?}");
            let profile = core.export_personal_profile();
            top == "World"
                && tail_word.is_empty()
                && tail == vec![Action::ForwardKey]
                && profile.cvm.words == vec!["hello"]
                && profile.markov_bigrams.is_empty()
                && profile.markov_trigrams.is_empty()
                && profile.confirmed_words.is_empty()
        }
    }

    #[test]
    fn sentence_boundary_tail_semantic_closers_keep_pending_without_tokens() {
        let mut failed = Vec::new();
        for &ch in REVIEW_CLOSERS {
            let mut core = fresh(false);
            if !tail_event(
                &mut core,
                KeyEvent {
                    key: Key::Char(ch),
                    modifiers: Modifiers::empty(),
                },
                ch,
                false,
            ) {
                failed.push(ch);
            }
        }
        assert!(failed.is_empty(), "semantic tails: {failed:?}");
    }

    #[test]
    fn sentence_boundary_tail_shared_raw_closers_keep_pending_without_tokens() {
        let mut failed = Vec::new();
        for (code, modifiers, glyph) in [
            (40, Modifiers::empty(), '\''),
            (40, Modifiers::SHIFT, '"'),
            (11, Modifiers::SHIFT, ')'),
        ] {
            assert_eq!(
                keymap::scancode_to_both_with_caps(
                    code,
                    modifiers.contains(Modifiers::SHIFT),
                    false
                ),
                Some((glyph, glyph))
            );
            let mut core = fresh(false);
            if !tail_event(
                &mut core,
                KeyEvent {
                    key: Key::RawCode(code),
                    modifiers,
                },
                glyph,
                false,
            ) {
                failed.push(glyph);
            }
        }
        assert!(failed.is_empty(), "shared raw tails: {failed:?}");
    }

    #[test]
    fn sentence_boundary_tail_preserves_owned_postcommit_caps() {
        let mut failed = Vec::new();
        for (key, modifiers, glyph) in [
            (Key::Char('”'), Modifiers::empty(), '”'),
            (Key::RawCode(40), Modifiers::SHIFT, '"'),
        ] {
            let mut core = fresh(true);
            if !tail_event(&mut core, KeyEvent { key, modifiers }, glyph, true) {
                failed.push(glyph);
            }
        }
        assert!(failed.is_empty(), "postcommit tails: {failed:?}");
    }

    #[test]
    fn sentence_boundary_tail_repeated_terminators_do_not_create_tokens() {
        let mut core = fresh(false);
        let mut client = String::new();
        review_dot(&mut core, &mut client, 0);
        let mut clean = true;
        for (key, modifiers, glyph) in [
            (Key::RawCode(52), Modifiers::empty(), '.'),
            (Key::RawCode(2), Modifiers::SHIFT, '!'),
            (Key::RawCode(53), Modifiers::SHIFT, '?'),
            (Key::Char('”'), Modifiers::empty(), '”'),
        ] {
            let a = core.handle_key(KeyEvent { key, modifiers });
            clean &= a == vec![Action::ForwardKey] && core.current_word().is_empty();
            project(&mut client, &a, Some(glyph));
        }
        project(&mut client, &key(&mut core, Key::Space), Some(' '));
        assert_eq!(client, "hello..!?” ");
        let top = prediction(&mut core);
        eprintln!("repeated tails prediction={top} clean={clean}");
        assert!(clean && top == "World");
        review_only_hello_learned(&core);
    }

    #[test]
    fn sentence_boundary_tail_adjacent_word_or_digit_withdraws_pending() {
        let mut failed = Vec::new();
        for digit in [false, true] {
            let mut core = fresh(false);
            raw(&mut core, HELLO);
            key(&mut core, Key::RawCode(52));
            key(&mut core, Key::Char('"'));
            if digit {
                key(&mut core, Key::RawCode(2));
                key(&mut core, Key::Space);
            }
            key(&mut core, Key::RawCode(17));
            let actual = core.predictions().first().map(|p| p.word.clone());
            eprintln!(
                "adjacent digit={digit} word={:?} prediction={actual:?}",
                core.current_word()
            );
            if actual.as_deref() != Some("world") {
                failed.push(digit);
            }
        }
        assert!(failed.is_empty(), "adjacent controls: {failed:?}");
    }

    #[test]
    fn sentence_boundary_tail_ambiguous_raw_bracket_keeps_bg_composition() {
        for modifiers in [Modifiers::empty(), Modifiers::SHIFT] {
            let mut core = fresh(false);
            core.load_word_lang("щука", 1_000_000, LangId::Bg);
            raw(&mut core, HELLO);
            key(&mut core, Key::RawCode(52));
            let a = core.handle_key(KeyEvent {
                key: Key::RawCode(27),
                modifiers,
            });
            assert!(core.debug_state().0);
            assert_eq!(
                core.current_word(),
                if modifiers.is_empty() { "щ" } else { "Щ" }
            );
            assert!(a.iter().any(|a| matches!(a, Action::ShowComposing { .. })));
            assert!(!a.iter().any(|a| matches!(
                a,
                Action::ForwardKey | Action::CommitText(_) | Action::ReplaceWord { .. }
            )));
            review_only_hello_learned(&core);
        }
    }

    fn unknown_withdraws(event: KeyEvent) {
        let mut core = fresh(false);
        sentence(&mut core);
        let before = core.detected_language();
        let a = core.handle_key(event);
        assert_eq!(a, vec![Action::ForwardKey]);
        assert!(core.current_word().is_empty());
        assert_eq!(core.context_words().0, Some("hello"));
        assert_eq!(core.detected_language().lang, before.lang);
        assert_eq!(core.detected_language().confidence, before.confidence);
        // Public refreshed surrounding context after the forwarded operation.
        core.apply_hints(&crate::master_loop::Hints {
            context_words: vec!["seed".into()],
            ..Default::default()
        });
        review_only_hello_learned(&core);
        assert_eq!(prediction(&mut core), "world");
    }

    #[test]
    fn sentence_boundary_unknown_shift_insert_raw_withdraws_hint() {
        unknown_withdraws(KeyEvent {
            key: Key::RawCode(110),
            modifiers: Modifiers::SHIFT,
        });
    }

    #[test]
    fn sentence_boundary_unknown_shift_insert_keysym_withdraws_hint() {
        unknown_withdraws(KeyEvent {
            key: Key::Other(0xff63),
            modifiers: Modifiers::SHIFT,
        });
    }

    #[test]
    fn sentence_boundary_unknown_keys_preserve_word_detector_and_context() {
        let mut failed = Vec::new();
        for key_id in [Key::Other(0xffbe), Key::RawCode(u16::MAX)] {
            let mut core = fresh(false);
            sentence(&mut core);
            raw(&mut core, &[17]);
            let before = core.detected_language();
            let a = key(&mut core, key_id.clone());
            assert_eq!(a, vec![Action::ForwardKey]);
            assert_eq!(core.current_word(), "w");
            assert!(core.debug_state().0);
            assert_eq!(core.context_words().0, Some("hello"));
            assert_eq!(core.detected_language().lang, before.lang);
            assert_eq!(core.detected_language().confidence, before.confidence);
            raw(&mut core, &[24]);
            if core.predictions()[0].word != "world" {
                failed.push(key_id);
            }
            review_only_hello_learned(&core);
        }
        assert!(failed.is_empty(), "unknown forwarded keys: {failed:?}");
    }

    #[test]
    fn sentence_boundary_pure_modifier_identity_preserves_pending_and_confirmed() {
        let mut failed = Vec::new();
        for key_id in [
            Key::RawCode(42),
            Key::RawCode(54),
            Key::RawCode(58),
            Key::Other(0xffe1),
            Key::Other(0xffe2),
            Key::Other(0xffe5),
        ] {
            for confirmed in [false, true] {
                for modifiers in [Modifiers::empty(), Modifiers::SHIFT] {
                    let mut core = fresh(false);
                    let mut client = String::new();
                    review_dot(&mut core, &mut client, usize::from(confirmed));
                    assert_eq!(
                        core.handle_key(KeyEvent {
                            key: key_id.clone(),
                            modifiers
                        }),
                        vec![Action::ForwardKey]
                    );
                    if !confirmed {
                        key(&mut core, Key::Space);
                    }
                    let actual = prediction(&mut core);
                    if actual != "World" {
                        failed.push((key_id.clone(), confirmed, modifiers, actual));
                    }
                    review_only_hello_learned(&core);
                }
            }
        }
        assert!(failed.is_empty(), "pure modifier controls: {failed:?}");
    }

    #[test]
    fn sentence_boundary_unknown_key_releases_preserve_pending_hint() {
        for key_id in [
            Key::RawCode(110),
            Key::Other(0xff63),
            Key::RawCode(42),
            Key::Other(0xffe1),
        ] {
            let mut core = fresh(false);
            raw(&mut core, HELLO);
            key(&mut core, Key::RawCode(52));
            assert_eq!(
                core.handle_key(KeyEvent {
                    key: key_id,
                    modifiers: Modifiers::SHIFT | Modifiers::RELEASE
                }),
                vec![Action::ForwardKey]
            );
            key(&mut core, Key::Space);
            assert_eq!(prediction(&mut core), "World");
            review_only_hello_learned(&core);
        }
    }

    #[test]
    fn sentence_boundary_stored_semantic_quote_context_prediction_caps() {
        let mut failed = Vec::new();
        for quote in ['"', '”', '“'] {
            let mut core = fresh(false);
            let stored = format!("hello.{quote}");
            for ch in stored.chars() {
                key(&mut core, Key::Char(ch));
            }
            key(&mut core, Key::Space);
            assert_eq!(core.context_words().0, Some(stored.as_str()));
            assert_eq!(core.export_personal_profile().cvm.words, vec![stored]);
            let actual = prediction(&mut core);
            eprintln!("stored quote={quote:?} prediction={actual}");
            if actual != "World" {
                failed.push(quote);
            }
        }
        assert!(failed.is_empty(), "stored semantic context: {failed:?}");
    }

    #[test]
    fn sentence_boundary_stored_semantic_quote_context_owned_postcommit_caps() {
        for quote in ['"', '”'] {
            let mut core = fresh(true);
            let mut client = String::new();
            for ch in format!("hello.{quote}").chars() {
                project(&mut client, &key(&mut core, Key::Char(ch)), Some(ch));
            }
            project(&mut client, &key(&mut core, Key::Space), Some(' '));
            raw(&mut core, WORLD);
            project(&mut client, &key(&mut core, Key::Space), Some(' '));
            assert_eq!(client, format!("Hello.{quote} World "));
        }
    }

    fn deletion_tail(core: &mut InputMethodCore, client: &mut String, shared_raw: bool) {
        for (key, modifiers, glyph) in if shared_raw {
            vec![
                (Key::RawCode(40), Modifiers::SHIFT, '"'),
                (Key::RawCode(11), Modifiers::SHIFT, ')'),
            ]
        } else {
            vec![
                (Key::Char('”'), Modifiers::empty(), '”'),
                (Key::Char(')'), Modifiers::empty(), ')'),
            ]
        } {
            let actions = core.handle_key(KeyEvent { key, modifiers });
            assert_eq!(actions, vec![Action::ForwardKey]);
            assert!(core.current_word().is_empty());
            project(client, &actions, Some(glyph));
        }
    }

    #[test]
    fn sentence_boundary_closer_deletion_preserves_surviving_terminator() {
        let mut failed = Vec::new();
        for shared_raw in [false, true] {
            for (deleted, expected) in [(1, "World"), (2, "World"), (3, "world")] {
                let mut core = fresh(false);
                let mut client = String::new();
                review_dot(&mut core, &mut client, 0);
                deletion_tail(&mut core, &mut client, shared_raw);
                for _ in 0..deleted {
                    review_forwarded_backspace(&mut core, &mut client);
                }
                project(&mut client, &key(&mut core, Key::Space), Some(' '));
                let actual = prediction(&mut core);
                eprintln!("closer sharedraw={shared_raw} deleted={deleted} client={client:?} prediction={actual}");
                if actual != expected {
                    failed.push((shared_raw, deleted, actual, expected));
                }
                review_only_hello_learned(&core);
            }
        }
        assert!(failed.is_empty(), "closer deletion: {failed:?}");
    }

    #[test]
    fn sentence_boundary_closer_deletion_after_spaces_retains_full_tail() {
        let mut failed = Vec::new();
        for deleted in 1..=5 {
            let mut core = fresh(false);
            let mut client = String::new();
            review_dot(&mut core, &mut client, 0);
            deletion_tail(&mut core, &mut client, false);
            for _ in 0..2 {
                project(&mut client, &key(&mut core, Key::Space), Some(' '));
            }
            for _ in 0..deleted {
                review_forwarded_backspace(&mut core, &mut client);
            }
            project(&mut client, &key(&mut core, Key::Space), Some(' '));
            let expected = if deleted == 5 { "world" } else { "World" };
            let actual = prediction(&mut core);
            eprintln!("closer spaces deleted={deleted} client={client:?} prediction={actual}");
            if actual != expected {
                failed.push((deleted, actual, expected));
            }
            review_only_hello_learned(&core);
        }
        assert!(failed.is_empty(), "spaced closer deletion: {failed:?}");
    }

    #[test]
    fn sentence_boundary_closer_deletion_repeated_terminators_are_counted() {
        let mut failed = Vec::new();
        for deleted in 1..=4 {
            let mut core = fresh(false);
            let mut client = String::new();
            review_dot(&mut core, &mut client, 0);
            for ch in ['!', '?', '”'] {
                let actions = key(&mut core, Key::Char(ch));
                assert_eq!(actions, vec![Action::ForwardKey]);
                project(&mut client, &actions, Some(ch));
            }
            for _ in 0..deleted {
                review_forwarded_backspace(&mut core, &mut client);
            }
            project(&mut client, &key(&mut core, Key::Space), Some(' '));
            let expected = if deleted == 4 { "world" } else { "World" };
            let actual = prediction(&mut core);
            if actual != expected {
                failed.push((deleted, client, actual, expected));
            }
            review_only_hello_learned(&core);
        }
        assert!(
            failed.is_empty(),
            "repeated terminator deletion: {failed:?}"
        );
    }

    #[test]
    fn sentence_boundary_closer_deletion_preserves_owned_postcommit_caps() {
        let mut core = fresh(true);
        let mut client = String::new();
        raw(&mut core, HELLO);
        project(&mut client, &key(&mut core, Key::RawCode(52)), Some('.'));
        deletion_tail(&mut core, &mut client, false);
        review_forwarded_backspace(&mut core, &mut client);
        review_forwarded_backspace(&mut core, &mut client);
        project(&mut client, &key(&mut core, Key::Space), Some(' '));
        raw(&mut core, WORLD);
        project(&mut client, &key(&mut core, Key::Space), Some(' '));
        assert_eq!(client, "Hello. World ");
        let profile = core.export_personal_profile();
        let mut words = profile.cvm.words;
        words.sort();
        assert_eq!(words, vec!["hello", "world"]);
        assert_eq!(
            profile.markov_bigrams,
            vec![("hello".into(), "world".into(), 1)]
        );
        assert!(profile.markov_trigrams.is_empty());
        assert!(profile.confirmed_words.is_empty());
    }

    #[test]
    fn sentence_boundary_closer_deletion_local_backspace_escape_preserve_payload() {
        let mut failed = Vec::new();
        for escape in [false, true] {
            let mut core = InputMethodCore::new(InputConfig {
                ghost_text: false,
                lang_detection: false,
                ..InputConfig::default()
            });
            for word in ["hello", "world"] {
                core.load_word_lang(word, 1_000, LangId::En);
            }
            let mut client = String::new();
            review_dot(&mut core, &mut client, 0);
            deletion_tail(&mut core, &mut client, false);
            project(&mut client, &key(&mut core, Key::Space), Some(' '));
            let typing = key(&mut core, Key::RawCode(17));
            assert!(typing.iter().any(|a| matches!(a, Action::ShowComposing { typed, ghost } if typed == "w" && ghost.is_empty())));
            let local = key(&mut core, if escape { Key::Escape } else { Key::Backspace });
            assert_eq!(local, vec![Action::HideGhost]);
            assert!(core.current_word().is_empty());
            assert_eq!(client, "hello.”) ");
            // Remove the known Space and both closers, leaving the terminator.
            for _ in 0..3 {
                review_forwarded_backspace(&mut core, &mut client);
            }
            assert_eq!(client, "hello.");
            project(&mut client, &key(&mut core, Key::Space), Some(' '));
            let actual = prediction(&mut core);
            if actual != "World" {
                failed.push((escape, actual));
            }
            review_only_hello_learned(&core);
        }
        assert!(failed.is_empty(), "local cancellation payload: {failed:?}");
    }

    #[test]
    fn sentence_boundary_closer_deletion_unknown_navigation_focus_withdraw() {
        for invalidation in 0..4 {
            let mut core = fresh(false);
            let mut client = String::new();
            review_dot(&mut core, &mut client, 0);
            deletion_tail(&mut core, &mut client, false);
            match invalidation {
                0 => assert_eq!(key(&mut core, Key::Other(0xff63)), vec![Action::ForwardKey]),
                1 => {
                    key(&mut core, Key::Left);
                }
                2 => core.focus_gained(),
                _ => {
                    core.focus_lost();
                }
            }
            // An actual surrounding-context producer may refresh after the
            // operation. This is not a claimed observation of our projection.
            core.apply_hints(&crate::master_loop::Hints {
                context_words: vec!["seed".into()],
                ..Default::default()
            });
            review_forwarded_backspace(&mut core, &mut client);
            project(&mut client, &key(&mut core, Key::Space), Some(' '));
            assert_eq!(
                prediction(&mut core),
                "world",
                "invalidation={invalidation}"
            );
            review_only_hello_learned(&core);
        }
    }

    #[test]
    fn sentence_boundary_closer_deletion_ambiguous_raw_is_not_a_literal_tail() {
        let mut core = fresh(false);
        core.load_word_lang("щука", 1_000_000, LangId::Bg);
        let mut client = String::new();
        review_dot(&mut core, &mut client, 0);
        deletion_tail(&mut core, &mut client, false);
        let composing = key(&mut core, Key::RawCode(27));
        assert_eq!(core.current_word(), "щ");
        assert!(composing
            .iter()
            .any(|a| matches!(a, Action::ShowComposing { .. })));
        assert!(!composing.iter().any(|a| matches!(
            a,
            Action::ForwardKey | Action::CommitText(_) | Action::ReplaceWord { .. }
        )));
        assert_eq!(key(&mut core, Key::Backspace), vec![Action::HideGhost]);
        project(&mut client, &key(&mut core, Key::Space), Some(' '));
        assert_eq!(client, "hello.”) ");
        assert_eq!(prediction(&mut core), "world");
        review_only_hello_learned(&core);
    }

    #[test]
    fn sentence_boundary_modifier_all_identities_preserve_pending_and_confirmed() {
        let mut identities = vec![
            (Key::RawCode(29), Modifiers::CTRL),
            (Key::RawCode(97), Modifiers::CTRL),
            (Key::RawCode(56), Modifiers::ALT),
            (Key::RawCode(100), Modifiers::ALT),
            (Key::RawCode(125), Modifiers::SUPER),
            (Key::RawCode(126), Modifiers::SUPER),
        ];
        identities.extend((0xffe1..=0xffee).map(|code| {
            let held = match code {
                0xffe1 | 0xffe2 => Modifiers::SHIFT,
                0xffe3 | 0xffe4 => Modifiers::CTRL,
                0xffe5 | 0xffe6 => Modifiers::CAPS_LOCK,
                0xffe9 | 0xffea => Modifiers::ALT,
                _ => Modifiers::SUPER,
            };
            (Key::Other(code), held)
        }));
        let mut failed = Vec::new();
        for (identity, held) in identities {
            for confirmed in [false, true] {
                for modifiers in [Modifiers::empty(), held] {
                    let mut core = fresh(false);
                    let mut client = String::new();
                    review_dot(&mut core, &mut client, usize::from(confirmed));
                    let before = core.detected_language();
                    let press = core.handle_key(KeyEvent {
                        key: identity.clone(),
                        modifiers,
                    });
                    let release = core.handle_key(KeyEvent {
                        key: identity.clone(),
                        modifiers: modifiers | Modifiers::RELEASE,
                    });
                    assert_eq!(press, vec![Action::ForwardKey]);
                    assert_eq!(release, vec![Action::ForwardKey]);
                    assert!(core.current_word().is_empty());
                    assert_eq!(core.context_words().0, Some("hello"));
                    assert_eq!(core.detected_language().lang, before.lang);
                    assert_eq!(core.detected_language().confidence, before.confidence);
                    if !confirmed {
                        key(&mut core, Key::Space);
                    }
                    let actual = prediction(&mut core);
                    if actual != "World" {
                        failed.push((identity.clone(), confirmed, modifiers, actual));
                    }
                    review_only_hello_learned(&core);
                }
            }
        }
        assert!(failed.is_empty(), "modifier-only presses: {failed:?}");
    }

    #[test]
    fn sentence_boundary_modifier_actual_shortcuts_and_unknown_keys_withdraw() {
        for (identity, modifiers) in [
            (Key::RawCode(47), Modifiers::CTRL), // Ctrl+V, not Control itself.
            (Key::Char('a'), Modifiers::ALT),
            (Key::Other(0xffe0), Modifiers::empty()),
            (Key::Other(0xffef), Modifiers::empty()),
            (Key::RawCode(110), Modifiers::SHIFT),
        ] {
            for confirmed in [false, true] {
                let mut core = fresh(false);
                let mut client = String::new();
                review_dot(&mut core, &mut client, usize::from(confirmed));
                let before = core.detected_language();
                assert_eq!(
                    core.handle_key(KeyEvent {
                        key: identity.clone(),
                        modifiers,
                    }),
                    vec![Action::ForwardKey]
                );
                assert!(core.current_word().is_empty());
                assert_eq!(core.context_words().0, Some("hello"));
                assert_eq!(core.detected_language().lang, before.lang);
                assert_eq!(core.detected_language().confidence, before.confidence);
                key(&mut core, Key::Space);
                assert_eq!(prediction(&mut core), "world", "{identity:?}");
                review_only_hello_learned(&core);
            }
        }
    }

    #[test]
    fn sentence_boundary_modifier_releases_preserve_hint_and_enabled_state() {
        for (identity, held) in [
            (Key::RawCode(29), Modifiers::CTRL),
            (Key::Other(0xffe9), Modifiers::ALT),
            (Key::Char('v'), Modifiers::CTRL),
            (Key::Escape, Modifiers::SUPER),
        ] {
            let mut core = fresh(false);
            let mut client = String::new();
            review_dot(&mut core, &mut client, 0);
            assert_eq!(
                core.handle_key(KeyEvent {
                    key: identity,
                    modifiers: held | Modifiers::RELEASE,
                }),
                vec![Action::ForwardKey]
            );
            assert!(core.is_enabled());
            key(&mut core, Key::Space);
            assert_eq!(prediction(&mut core), "World");
            review_only_hello_learned(&core);
        }
    }

    #[test]
    fn sentence_boundary_modifier_super_escape_kill_switch_still_precedes_guard() {
        let mut core = fresh(false);
        sentence(&mut core);
        let kill = KeyEvent {
            key: Key::Escape,
            modifiers: Modifiers::SUPER,
        };
        assert_eq!(core.handle_key(kill.clone()), vec![Action::HideGhost]);
        assert!(!core.is_enabled());
        assert_eq!(
            core.handle_key(KeyEvent {
                key: Key::Escape,
                modifiers: Modifiers::SUPER | Modifiers::RELEASE,
            }),
            vec![Action::ForwardKey]
        );
        assert!(!core.is_enabled());
        assert_eq!(
            core.handle_key(KeyEvent {
                key: Key::RawCode(125),
                modifiers: Modifiers::SUPER,
            }),
            vec![Action::ForwardKey]
        );
        assert!(!core.is_enabled());
        assert_eq!(core.handle_key(kill), vec![Action::HideGhost]);
        assert!(core.is_enabled());
        review_only_hello_learned(&core);
    }

    #[test]
    fn sentence_boundary_reconciled_veto_keeps_explicit_uppercase_prefix() {
        let mut core = fresh(false);
        sentence(&mut core);
        core.withdraw_sentence_boundary_hint();
        let actions = core.handle_key(KeyEvent {
            key: Key::RawCode(17),
            modifiers: Modifiers::SHIFT,
        });
        assert_eq!(core.current_word(), "W");
        assert!(actions
            .iter()
            .any(|a| matches!(a, Action::ShowComposing { typed, .. } if typed == "W")));
        assert_eq!(core.predictions()[0].word, "World");
        review_only_hello_learned(&core);
    }

    #[test]
    fn sentence_boundary_reconciled_veto_keeps_registered_proper_noun() {
        let mut core = fresh(true);
        let dir = std::env::temp_dir().join(format!(
            "smartkey-sentence-reconciliation-noun-{}",
            std::process::id()
        ));
        std::fs::create_dir(&dir).unwrap();
        let corpus = dir.join("proper.json");
        std::fs::write(
            &corpus,
            r#"{"unigrams":{"london":1000},"proper_nouns":["London"]}"#,
        )
        .unwrap();
        core.load_corpus_file(&corpus).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        sentence(&mut core);
        core.withdraw_sentence_boundary_hint();
        raw(&mut core, &[38]);
        assert_eq!(core.current_word(), "l");
        assert_eq!(core.predictions()[0].word, "London");
        raw(&mut core, &[24, 49, 32, 24, 49]);
        assert_eq!(core.current_word(), "london");
        review_only_hello_learned(&core);
        let actions = key(&mut core, Key::Space);
        assert_eq!(
            actions,
            vec![
                Action::HideGhost,
                Action::CommitText("london".into()),
                Action::ForwardKey,
                Action::ReplaceWord {
                    replace_len: 6,
                    text: "London".into(),
                },
            ]
        );
        assert!(core.current_word().is_empty());
        let profile = core.export_personal_profile();
        let mut learned = profile.cvm.words;
        learned.sort();
        assert_eq!(learned, vec!["hello", "london"]);
        assert_eq!(
            profile.markov_bigrams,
            vec![("hello".into(), "london".into(), 1)]
        );
        assert!(profile.markov_trigrams.is_empty());
        assert!(profile.confirmed_words.is_empty());
    }

    #[test]
    fn sentence_boundary_reconciled_positive_withdrawal_keeps_false_floor() {
        for event in [
            None,
            Some(KeyEvent {
                key: Key::Other(0xff63),
                modifiers: Modifiers::SHIFT,
            }),
            Some(KeyEvent {
                key: Key::RawCode(47),
                modifiers: Modifiers::CTRL,
            }),
            Some(KeyEvent {
                key: Key::RawCode(u16::MAX),
                modifiers: Modifiers::empty(),
            }),
            Some(KeyEvent {
                key: Key::Backspace,
                modifiers: Modifiers::empty(),
            }),
        ] {
            let mut core = fresh(false);
            sentence(&mut core);
            // This stale lexical hint is independently overridden by the
            // current-position decision, including after its positive expires.
            core.apply_hints(&crate::master_loop::Hints {
                context_words: vec!["hello.".into()],
                ..Default::default()
            });
            core.reconcile_sentence_boundary_hint("hello. seed. ");
            let expected = if let Some(event) = event {
                let actions = core.handle_key(event);
                assert_eq!(
                    actions
                        .iter()
                        .filter(|a| matches!(a, Action::ForwardKey))
                        .count(),
                    1
                );
                assert!(!actions
                    .iter()
                    .any(|a| matches!(a, Action::CommitText(_) | Action::ReplaceWord { .. })));
                assert!(core.current_word().is_empty());
                "world"
            } else {
                "World"
            };
            assert_eq!(prediction(&mut core), expected);
            review_only_hello_learned(&core);
        }
    }
}

#[cfg(test)]
mod layout_handoff_tests {
    use super::*;

    fn press(key: Key) -> KeyEvent {
        KeyEvent {
            key,
            modifiers: Modifiers::empty(),
        }
    }

    fn raw(core: &mut InputMethodCore, text: &str) {
        for ch in text.chars() {
            let code = (2..=53)
                .find(|code| {
                    keymap::scancode_to_both(*code, false).is_some_and(|pair| pair.0 == ch)
                })
                .unwrap();
            core.handle_key(press(Key::RawCode(code)));
        }
    }

    fn seeded_core() -> InputMethodCore {
        let mut core = InputMethodCore::new(InputConfig {
            ghost_text_separation_margin: 0.0,
            ..InputConfig::default()
        });
        core.load_word_lang("hello", 1_000_000, LangId::En);
        // Real committed history populates CVM, Markov (including higher
        // n-grams), OOV vocabulary and session cache before the handoff.
        for word in ["one", "two", "three", "four", "five"] {
            for ch in word.chars() {
                core.handle_key(press(Key::Char(ch)));
            }
            core.handle_key(press(Key::Space));
        }
        core
    }

    fn metrics(core: &InputMethodCore) -> serde_json::Value {
        serde_json::to_value(core.metrics().summary()).unwrap()
    }

    #[test]
    fn layout_handoff_delivers_only_owned_prefix_without_learning_any_word() {
        let mut core = seeded_core();
        raw(&mut core, "hel");
        assert_eq!(core.current_word(), "hel");
        assert_eq!(core.ghost_text(), "lo");
        let learned = core.engine.learned_state_for_test();
        let metrics = metrics(&core);
        assert_eq!(
            core.flush_preedit_for_layout(),
            [Action::CommitText("hel".into()), Action::HideGhost]
        );
        assert_eq!(
            core.engine.learned_state_for_test(),
            learned,
            "delivery alone must not train profile, session, n-grams or OOV words"
        );
        assert_eq!(self::metrics(&core), metrics);
        assert!(core.current_word().is_empty() && !core.has_ghost());
        assert!(core.predictions().is_empty());
        assert_eq!(core.context_words(), (None, None));
        assert!(core.take_commit_learning().words.is_empty());
        assert_eq!(core.flush_preedit_for_layout(), [Action::HideGhost]);
        assert_eq!(core.engine.learned_state_for_test(), learned);
        core.handle_key(press(Key::Space));
        assert_eq!(
            core.engine.learned_state_for_test(),
            learned,
            "next empty boundary must not lazily learn the flushed prefix"
        );
    }

    #[test]
    fn layout_handoff_keeps_explicitly_accepted_character_but_not_the_rest() {
        let mut core = seeded_core();
        raw(&mut core, "hel");
        let right = core.handle_key(press(Key::Right));
        assert_eq!(
            right,
            [Action::ShowComposing {
                typed: "hell".into(),
                ghost: "o".into()
            }]
        );
        let learned = core.engine.learned_state_for_test();
        let metrics = metrics(&core);
        assert_eq!(
            core.flush_preedit_for_layout(),
            [Action::CommitText("hell".into()), Action::HideGhost]
        );
        assert_eq!(core.engine.learned_state_for_test(), learned);
        assert_eq!(self::metrics(&core), metrics);
        assert_eq!(core.flush_preedit_for_layout(), [Action::HideGhost]);
    }

    #[test]
    fn layout_handoff_does_not_redeliver_semantic_input_already_in_client() {
        let mut core = seeded_core();
        for ch in "hel".chars() {
            assert!(core
                .handle_key(press(Key::Char(ch)))
                .contains(&Action::ForwardKey));
        }
        assert_eq!(core.current_word(), "hel");
        let learned = core.engine.learned_state_for_test();
        assert_eq!(core.flush_preedit_for_layout(), [Action::HideGhost]);
        assert_eq!(core.engine.learned_state_for_test(), learned);
        assert!(core.current_word().is_empty());
    }

    #[test]
    fn layout_handoff_after_full_accept_does_not_recommit_or_relearn() {
        let mut core = seeded_core();
        raw(&mut core, "hel");
        assert_eq!(
            core.handle_key(press(Key::Tab)),
            [Action::HideGhost, Action::CommitText("hello".into())]
        );
        let learned = core.engine.learned_state_for_test();
        let metrics = metrics(&core);
        assert_eq!(core.flush_preedit_for_layout(), [Action::HideGhost]);
        assert_eq!(core.flush_preedit_for_layout(), [Action::HideGhost]);
        assert_eq!(core.engine.learned_state_for_test(), learned);
        assert_eq!(self::metrics(&core), metrics);
    }
}

#[cfg(test)]
mod backspace_recovered_detector_tests {
    use super::*;

    fn raw(core: &mut InputMethodCore, code: u16) {
        let actions = core.handle_key(KeyEvent {
            key: Key::RawCode(code),
            modifiers: Modifiers::empty(),
        });
        assert!(actions.iter().any(|action| matches!(
            action, Action::ShowComposing { typed, .. } if typed == core.current_word()
        )));
        assert!(!actions.iter().any(|action| matches!(
            action,
            Action::CommitText(_) | Action::ReplaceWord { .. } | Action::ForwardKey
        )));
        assert_eq!(core.metrics().summary().total_commits, 0);
    }

    fn fixture(prior: LangId) -> InputMethodCore {
        let mut core = InputMethodCore::new(InputConfig::default());
        if prior == LangId::En {
            core.load_word_lang("list", 190_546, LangId::En);
            core.load_word_lang("листа", 47_863, LangId::Bg);
            core.load_word_lang("листовете", 95, LangId::Bg);
        }
        core.load_word_lang("liston", 457, LangId::En);
        core.load_word_lang("листове", 4_466, LangId::Bg);
        core.apply_hints(&crate::master_loop::Hints {
            lang_prior: Some(prior),
            context_words: vec![match prior {
                LangId::En => "thank".into(),
                _ => "благодаря".into(),
            }],
            ..Default::default()
        });
        core
    }

    #[test]
    fn deleting_recovered_bg_suffix_restores_english_detector_and_ghost() {
        let mut fresh = fixture(LangId::En);
        for code in [38, 23, 31] {
            raw(&mut fresh, code);
        }
        assert_eq!(fresh.current_word(), "lis");
        assert_eq!(fresh.ghost_text(), "t");
        assert_eq!(fresh.detected_language().lang, LangId::En);

        let mut edited = fixture(LangId::En);
        for code in [38, 23, 31, 20, 24, 17] {
            raw(&mut edited, code);
        }
        assert_eq!(edited.current_word(), "листов");
        assert_eq!(edited.detected_language().lang, LangId::Bg);
        assert!(edited.detected_language().confidence >= 0.6);
        for _ in 0..3 {
            raw(&mut edited, 14);
        }
        assert_eq!(edited.current_word(), "lis");
        assert_eq!(
            (edited.detected_language().lang, edited.ghost_text()),
            (LangId::En, fresh.ghost_text())
        );
        assert!(edited.predictions().iter().any(|p| p.word == "list"));
    }

    #[test]
    fn deleting_recovered_en_suffix_restores_bulgarian_detector_and_ghost() {
        let mut fresh = fixture(LangId::Bg);
        for code in [38, 23, 31] {
            raw(&mut fresh, code);
        }
        assert_eq!(fresh.current_word(), "лис");
        assert_eq!(fresh.ghost_text(), "тове");
        assert_eq!(fresh.detected_language().lang, LangId::Bg);

        let mut edited = fixture(LangId::Bg);
        for code in [38, 23, 31, 20, 24, 49] {
            raw(&mut edited, code);
        }
        assert_eq!(edited.current_word(), "liston");
        assert_eq!(edited.detected_language().lang, LangId::En);
        assert!(edited.detected_language().confidence >= 0.6);
        for _ in 0..3 {
            raw(&mut edited, 14);
        }
        assert_eq!(edited.current_word(), "лис");
        assert_eq!(
            (edited.detected_language().lang, edited.ghost_text()),
            (LangId::Bg, fresh.ghost_text())
        );
        assert!(edited.predictions().iter().any(|p| p.word == "листове"));
    }
}
