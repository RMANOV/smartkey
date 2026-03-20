// Master Loop — proactive orchestrator wrapping InputMethodCore.
//
// State machine: ANTICIPATING → TRACKING → CORRECTING → LEARNING
//
// Before each word:  reads context, predicts lang/caps, checks corrections.
// During each word:  delegates to core, monitors frustration signals.
// After each word:   records corrections, updates profile, loops.
//
// Implements the Decorator pattern: mirrors InputMethodCore's public API 1:1.

use std::path::Path;
use std::time::Instant;

use crate::context_sampler::{self, ContextSampler, NullContextSampler};
use crate::correction_memory::CorrectionMemory;
use crate::ensemble::Prediction;
use crate::eval::PredictionMetrics;
use crate::frustration::{FrustrationDetector, FrustrationSignal};
use crate::input::{Action, InputConfig, InputMethodCore, KeyEvent};
use crate::lang_detect::{DetectedLanguage, LangId};
use crate::light_profile::LightProfile;

/// State machine phases for the master loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Word boundary — reading context, predicting language/caps.
    Anticipating,
    /// Mid-word — delegating to core, monitoring frustration.
    Tracking,
    /// Frustration detected — adjusting ghost behavior.
    Correcting,
    /// Word committed — recording corrections, updating profile.
    Learning,
}

/// Hints passed from MasterLoop to InputMethodCore to bias predictions.
#[derive(Debug, Clone)]
pub struct Hints {
    /// Predicted language for the upcoming word.
    pub lang_prior: Option<LangId>,
    /// Whether to suppress ghost text entirely (after frustration).
    pub suppress_ghost: bool,
    /// Extra confidence added to ghost threshold (positive = more lenient).
    pub confidence_boost: f64,
    /// Adaptive confidence floor from LightProfile (overrides config min_confidence).
    /// `None` = use the static config value.
    pub confidence_floor: Option<f64>,
}

impl Default for Hints {
    fn default() -> Self {
        Self {
            lang_prior: None,
            suppress_ghost: false,
            confidence_boost: 0.0,
            confidence_floor: None,
        }
    }
}

/// Proactive orchestrator wrapping InputMethodCore.
///
/// Adds anticipation (predict before typing), frustration detection,
/// correction memory, and a light user profile. Mirrors the core's
/// public API as a transparent decorator.
pub struct MasterLoop {
    core: InputMethodCore,
    phase: Phase,
    frustration: FrustrationDetector,
    corrections: CorrectionMemory,
    light_profile: LightProfile,
    context_sampler: Box<dyn ContextSampler>,
    /// Words remaining for ghost suppression after frustration.
    suppress_countdown: u8,
    /// When the last Tab-accept occurred (for REJECT detection).
    last_tab_accept: Option<Instant>,
    /// The prediction that was last accepted via Tab.
    last_accepted_prediction: Option<String>,
    /// Last known commit count from metrics (for change detection).
    last_known_commits: usize,
    /// Whether anticipation was already run for the current word.
    anticipated: bool,
}

impl MasterLoop {
    pub fn new(config: InputConfig) -> Self {
        Self {
            core: InputMethodCore::new(config),
            phase: Phase::Anticipating,
            frustration: FrustrationDetector::new(),
            corrections: CorrectionMemory::new(),
            light_profile: LightProfile::default(),
            context_sampler: Box::new(NullContextSampler),
            suppress_countdown: 0,
            last_tab_accept: None,
            last_accepted_prediction: None,
            last_known_commits: 0,
            anticipated: false,
        }
    }

    /// Create with a custom context sampler (platform-specific).
    pub fn with_context_sampler(config: InputConfig, sampler: Box<dyn ContextSampler>) -> Self {
        Self {
            context_sampler: sampler,
            ..Self::new(config)
        }
    }

    // ======================================================================
    // Main event handler (Decorator)
    // ======================================================================

    /// Process a key event through the master loop state machine.
    pub fn handle_key(&mut self, event: KeyEvent) -> Vec<Action> {
        // ── Phase 1: ANTICIPATE (on word start) ──────────────────────
        if !self.anticipated && self.core.current_word().is_empty() {
            self.phase = Phase::Anticipating;
            let hints = self.anticipate();
            self.core.apply_hints(&hints);
            self.anticipated = true;
            self.phase = Phase::Tracking;
        }

        // ── Phase 2: DELEGATE to core ────────────────────────────────
        let is_tab = matches!(event.key, crate::input::Key::Tab);
        // Capture prediction BEFORE delegation — handle_key(Tab) clears
        // last_predictions via reset_word(), so reading after is always None.
        let pre_tab_prediction = if is_tab {
            self.core.predictions().first().map(|p| p.word.clone())
        } else {
            None
        };
        let actions = self.core.handle_key(event.clone());

        // Track Tab acceptance time for REJECT detection.
        if is_tab && self.core.current_word().is_empty() {
            self.last_tab_accept = Some(Instant::now());
            self.last_accepted_prediction = pre_tab_prediction;
        }

        // ── Phase 3: EVALUATE ────────────────────────────────────────
        let signal =
            self.frustration
                .feed(&event.key, self.core.current_word(), self.last_tab_accept);

        if let Some(ref signal) = signal {
            self.phase = Phase::Correcting;
            self.handle_frustration(signal);
        }

        // Word committed? → LEARNING phase.
        if self.core.current_word().is_empty() && self.session_commits_changed() {
            self.phase = Phase::Learning;
            self.learn_from_commit();
            self.phase = Phase::Anticipating;
            self.anticipated = false;
            self.core.clear_hints();
            self.frustration.reset_word();
        }

        actions
    }

    // ======================================================================
    // Delegated API (1:1 mirror of InputMethodCore)
    // ======================================================================

    pub fn focus_lost(&mut self) -> Vec<Action> {
        self.anticipated = false;
        self.core.clear_hints();
        self.frustration.reset_word();
        self.core.focus_lost()
    }

    pub fn focus_gained(&mut self) {
        self.core.focus_gained();
    }

    pub fn reset(&mut self) -> Vec<Action> {
        self.anticipated = false;
        self.core.clear_hints();
        self.frustration.reset_word();
        self.core.reset()
    }

    pub fn predictions(&self) -> &[Prediction] {
        self.core.predictions()
    }

    pub fn is_enabled(&self) -> bool {
        self.core.is_enabled()
    }

    pub fn current_word(&self) -> &str {
        self.core.current_word()
    }

    pub fn has_ghost(&self) -> bool {
        self.core.has_ghost()
    }

    pub fn ghost_text(&self) -> &str {
        self.core.ghost_text()
    }

    pub fn save_personal(&self, path: &Path) -> Result<(), String> {
        self.core.save_personal(path)
    }

    pub fn load_personal(&mut self, path: &Path) -> Result<(), String> {
        self.core.load_personal(path)
    }

    pub fn save_personal_default(&self) -> Result<(), String> {
        self.core.save_personal_default()
    }

    pub fn load_personal_default(&mut self) -> Result<(), String> {
        self.core.load_personal_default()
    }

    pub fn metrics(&self) -> &PredictionMetrics {
        self.core.metrics()
    }

    pub fn detected_language(&self) -> DetectedLanguage {
        self.core.detected_language()
    }

    // Corpus loading (delegated).
    pub fn load_word(&mut self, word: &str, freq: u32) {
        self.core.load_word(word, freq);
    }

    pub fn load_bigram(&mut self, ctx: &str, word: &str, count: u32) {
        self.core.load_bigram(ctx, word, count);
    }

    pub fn load_trigram(&mut self, w1: &str, w2: &str, word: &str, count: u32) {
        self.core.load_trigram(w1, w2, word, count);
    }

    pub fn load_word_lang(&mut self, word: &str, freq: u32, lang: LangId) {
        self.core.load_word_lang(word, freq, lang);
    }

    pub fn load_bigram_lang(&mut self, ctx: &str, word: &str, count: u32, lang: LangId) {
        self.core.load_bigram_lang(ctx, word, count, lang);
    }

    pub fn load_trigram_lang(&mut self, w1: &str, w2: &str, word: &str, count: u32, lang: LangId) {
        self.core.load_trigram_lang(w1, w2, word, count, lang);
    }

    pub fn load_corpus_file(&mut self, path: &Path) -> Result<(), String> {
        self.core.load_corpus_file(path)
    }

    /// Current phase of the master loop state machine.
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Access the light profile (read-only).
    pub fn light_profile(&self) -> &LightProfile {
        &self.light_profile
    }

    /// Access the correction memory (read-only).
    pub fn correction_count(&self) -> usize {
        self.corrections.to_snapshot().entries.len()
    }

    // ======================================================================
    // Internal: Anticipation
    // ======================================================================

    fn anticipate(&mut self) -> Hints {
        let mut hints = Hints::default();

        // 1. Sample surrounding text for context.
        if let Some(text) = self.context_sampler.get_surrounding_text(100) {
            let analysis = context_sampler::analyze_surrounding(&text);
            if let Some(lang) = analysis.lang {
                hints.lang_prior = Some(lang);
            }
        }

        // 2. Fall back to light profile's language prior.
        if hints.lang_prior.is_none() {
            let priors = &self.light_profile.lang_prior;
            let max_idx = priors
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(i, _)| i)
                .unwrap_or(1);
            // Only set prior if it's meaningfully dominant (>40%).
            if priors[max_idx] > 0.40 {
                hints.lang_prior = Some(match max_idx {
                    0 => LangId::Bg,
                    1 => LangId::En,
                    _ => LangId::Tech,
                });
            }
        }

        // 3. Ghost suppression from light profile or frustration cooldown.
        if self.suppress_countdown > 0 {
            hints.suppress_ghost = true;
            self.suppress_countdown -= 1;
        } else if self.light_profile.tick_suppression() {
            hints.suppress_ghost = true;
        }

        // 4. Confidence boost from accept_rate.
        // High accept rate → positive boost (show more ghosts).
        // Low accept rate → negative boost (be stricter).
        hints.confidence_boost = (self.light_profile.ghost_accept_rate - 0.50) * 0.10;

        // 5. Adaptive confidence floor from LightProfile (F14).
        // Overrides the static config `ghost_text_min_confidence` with a
        // value that auto-tunes based on rolling accept/reject ratio.
        hints.confidence_floor = Some(self.light_profile.confidence_floor);

        hints
    }

    // ======================================================================
    // Internal: Frustration handling
    // ======================================================================

    fn handle_frustration(&mut self, signal: &FrustrationSignal) {
        match signal {
            FrustrationSignal::Reject { severity } => {
                self.light_profile.record_reject();
                // Suppress ghost for 1-3 words based on severity.
                self.suppress_countdown = (severity * 3.0).ceil() as u8;
                // Record in correction memory if we know what was accepted.
                if let Some(accepted) = &self.last_accepted_prediction {
                    let ctx_hash = self.current_context_hash();
                    let current = self.core.current_word();
                    if !current.is_empty() {
                        self.corrections.record(ctx_hash, accepted, current);
                    }
                }
            }
            FrustrationSignal::RapidDelete { severity, .. } => {
                self.light_profile.record_reject();
                // Suppress ghost temporarily.
                self.suppress_countdown = (severity * 2.0).ceil() as u8;
                // Record negative example: if ghost was showing, the top prediction
                // was wrong for this context — teach correction memory.
                self.record_ghost_as_negative();
            }
            FrustrationSignal::Retype { severity, prefix } => {
                // Wrong language detected — boost opposite language prior.
                let was_latin = prefix.chars().all(|c| c.is_ascii_alphabetic());
                let correct_lang = if was_latin { LangId::Bg } else { LangId::En };
                // Observe the correct language multiple times to shift prior.
                let observations = (severity * 5.0).ceil() as u8;
                for _ in 0..observations {
                    self.light_profile.observe_lang(correct_lang);
                }
                // Record negative example for the wrong-language prediction.
                self.record_ghost_as_negative();
            }
            FrustrationSignal::Abandon { .. } => {
                self.light_profile.record_reject();
                self.suppress_countdown = 1;
                // Escape + manual typing = explicit rejection of the ghost.
                self.record_ghost_as_negative();
            }
        }
    }

    /// Record the current top prediction as a negative example in correction memory.
    ///
    /// Called when frustration signals indicate the ghost was unwanted. The user's
    /// current (manually typed) word becomes the "correct" replacement, teaching
    /// the system to suppress that prediction in similar contexts.
    fn record_ghost_as_negative(&mut self) {
        let current = self.core.current_word();
        if current.is_empty() {
            return;
        }
        // Use the top prediction as the "predicted_prefix" to suppress.
        if let Some(top) = self.core.predictions().first() {
            let ctx_hash = self.current_context_hash();
            self.corrections.record(ctx_hash, &top.word, current);
        }
    }

    // ======================================================================
    // Internal: Learning
    // ======================================================================

    fn learn_from_commit(&mut self) {
        self.last_known_commits = self.core.metrics().commit_count();

        // Update language prior from detection.
        let lang = self.core.detected_language().lang;
        self.light_profile.observe_lang(lang);

        // Clear the accepted prediction tracker.
        self.last_accepted_prediction = None;
    }

    fn session_commits_changed(&self) -> bool {
        self.core.metrics().commit_count() != self.last_known_commits
    }

    fn current_context_hash(&self) -> u64 {
        let (prev1, prev2) = self.core.context_words();
        CorrectionMemory::context_hash(prev1, prev2)
    }
}
