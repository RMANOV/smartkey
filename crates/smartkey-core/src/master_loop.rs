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
use crate::rejection_memory::RejectionMemory;

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
    /// Recent surrounding words from the active text field (oldest → newest).
    pub context_words: Vec<String>,
}

impl Default for Hints {
    fn default() -> Self {
        Self {
            lang_prior: None,
            suppress_ghost: false,
            confidence_boost: 0.0,
            confidence_floor: None,
            context_words: Vec::new(),
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
    /// Session-scoped suppression of ghosts the user keeps rejecting.
    /// Never persisted — decays when the engine process exits.
    rejections: RejectionMemory,
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
    /// Cached surrounding text from the platform adapter (Linux IBus path).
    surrounding_text: Option<String>,
    /// Cursor position inside `surrounding_text` when provided by the platform.
    surrounding_cursor_pos: Option<usize>,
    /// The `(typed_prefix, completion)` of the ghost most recently shown, so a
    /// later frustration signal (REJECT/ABANDON) can record it as a rejection.
    last_shown_ghost: Option<(String, String)>,
}

impl MasterLoop {
    pub fn new(config: InputConfig) -> Self {
        Self {
            core: InputMethodCore::new(config),
            phase: Phase::Anticipating,
            frustration: FrustrationDetector::new(),
            corrections: CorrectionMemory::new(),
            rejections: RejectionMemory::new(),
            light_profile: LightProfile::default(),
            context_sampler: Box::new(NullContextSampler),
            suppress_countdown: 0,
            last_tab_accept: None,
            last_accepted_prediction: None,
            last_known_commits: 0,
            anticipated: false,
            surrounding_text: None,
            surrounding_cursor_pos: None,
            last_shown_ghost: None,
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
        let resets_context = matches!(
            event.key,
            crate::input::Key::Left
                | crate::input::Key::Up
                | crate::input::Key::Down
                | crate::input::Key::Home
                | crate::input::Key::End
                | crate::input::Key::PageUp
                | crate::input::Key::PageDown
        ) || (matches!(event.key, crate::input::Key::Right)
            && !self.core.has_ghost());

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
        let mut actions = self.core.handle_key(event.clone());
        self.apply_correction_override(&mut actions);
        // Session-scoped rejection memory: drop a ghost the user has already
        // rejected K times for this exact prefix. Runs *after* the correction
        // override so both the normal ghost and the replacement are covered.
        self.enforce_rejection_suppression(&mut actions);

        if resets_context {
            self.anticipated = false;
            self.phase = Phase::Anticipating;
            self.core.clear_hints();
            self.frustration.reset_word();
            self.last_tab_accept = None;
            self.last_accepted_prediction = None;
            self.last_shown_ghost = None;
        }

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
            // A typed-through boundary (Space/Return/punctuation) resolves the
            // ghost's fate; the adapter records that rejection, so the cache
            // must not linger and be re-attributed to a later ABANDON (that is
            // the double-count path). Tab-accept keeps it so a following
            // REJECT (accept-then-delete) can still attribute the completion.
            if !is_tab {
                self.last_shown_ghost = None;
            }
        }

        // Remember the ghost currently on screen (post-suppression) so a later
        // REJECT/ABANDON can attribute the rejection to the right completion.
        self.note_shown_ghost(&actions);

        actions
    }

    // ======================================================================
    // Delegated API (1:1 mirror of InputMethodCore)
    // ======================================================================

    pub fn focus_lost(&mut self) -> Vec<Action> {
        self.anticipated = false;
        self.core.clear_hints();
        self.frustration.reset_word();
        self.surrounding_text = None;
        self.surrounding_cursor_pos = None;
        self.last_shown_ghost = None;
        self.core.focus_lost()
    }

    pub fn focus_gained(&mut self) {
        self.core.focus_gained();
    }

    pub fn reset(&mut self) -> Vec<Action> {
        self.anticipated = false;
        self.core.clear_hints();
        self.frustration.reset_word();
        self.surrounding_text = None;
        self.surrounding_cursor_pos = None;
        self.last_shown_ghost = None;
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

    /// Diagnostic snapshot for the adapter's keystroke trace:
    /// `(dual_buffer_present, locked, hypothesis_phase)`.
    pub fn debug_state(&self) -> (bool, bool, bool) {
        self.core.debug_state()
    }

    pub fn ghost_text(&self) -> &str {
        self.core.ghost_text()
    }

    pub fn save_personal(&self, path: &Path) -> Result<(), String> {
        let mut profile = self.core.export_personal_profile();
        profile.corrections = Some(self.corrections.to_snapshot());
        profile.light_profile = Some(self.light_profile.clone());

        let json = serde_json::to_string_pretty(&profile).map_err(|e| e.to_string())?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(path, json).map_err(|e| e.to_string())
    }

    pub fn load_personal(&mut self, path: &Path) -> Result<(), String> {
        if !path.is_file() {
            return Ok(());
        }

        let data = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let profile = crate::personal::load_personal_json(&data)?;

        self.core.import_personal_profile(&profile);
        self.corrections = profile
            .corrections
            .as_ref()
            .map(CorrectionMemory::from_snapshot)
            .unwrap_or_default();
        self.light_profile = profile.light_profile.clone().unwrap_or_default();

        Ok(())
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

    /// Update the cached surrounding text from the active text field.
    ///
    /// Linux IBus adapters can push live surrounding text here so the
    /// anticipation phase uses real document context instead of the null sampler.
    pub fn set_surrounding_text(&mut self, text: Option<String>, cursor_pos: Option<usize>) {
        self.surrounding_text = text.filter(|value| !value.trim().is_empty());
        self.surrounding_cursor_pos = cursor_pos;
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

    /// Record that the user rejected `completion` for the typed `prefix`.
    ///
    /// Session-scoped: after K=2 rejections of the same (prefix, completion)
    /// the ghost is suppressed for the rest of the session. Called by the
    /// platform adapter for "typed through" / word-boundary rejections that
    /// never surface as a Rust-side frustration signal.
    ///
    /// This resolves the current ghost's fate, so the frustration cache is
    /// invalidated: the same outcome cannot also be recorded by a later
    /// REJECT/ABANDON (one user rejection → at most one record).
    pub fn record_ghost_rejection(&mut self, prefix: &str, completion: &str) {
        self.rejections.record(prefix, completion);
        self.last_shown_ghost = None;
    }

    /// Whether `completion` is currently suppressed for the typed `prefix`
    /// (K=2 rejections reached this session). Read-only; for diagnostics/tests.
    pub fn is_ghost_suppressed(&self, prefix: &str, completion: &str) -> bool {
        self.rejections.is_suppressed(prefix, completion)
    }

    /// Number of distinct prefixes tracked by the session rejection memory.
    pub fn rejection_prefix_count(&self) -> usize {
        self.rejections.len()
    }

    // ======================================================================
    // Internal: Anticipation
    // ======================================================================

    fn anticipate(&mut self) -> Hints {
        let mut hints = Hints::default();

        // 1. Sample surrounding text for context.
        if let Some(text) = self
            .cached_surrounding_excerpt(100)
            .or_else(|| self.context_sampler.get_surrounding_text(100))
        {
            let analysis = context_sampler::analyze_surrounding(&text);
            if let Some(lang) = analysis.lang {
                hints.lang_prior = Some(lang);
            }
            if !analysis.recent_words.is_empty() {
                // `analyze_surrounding()` returns newest → oldest; reverse to
                // keep chronology for Markov-style context consumers.
                hints.context_words = analysis.recent_words.into_iter().rev().collect();
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
        hints.confidence_boost = (self.light_profile.ghost_accept_rate - 0.50) * 0.20;

        // 5. Adaptive confidence floor from LightProfile (F14).
        // Overrides the static config `ghost_text_min_confidence` with a
        // value that auto-tunes based on rolling accept/reject ratio.
        hints.confidence_floor = Some(self.light_profile.confidence_floor);

        hints
    }

    fn cached_surrounding_excerpt(&self, max_chars: usize) -> Option<String> {
        let text = self.surrounding_text.as_deref()?;
        let before_cursor = match self.surrounding_cursor_pos {
            Some(cursor_pos) => Self::slice_before_cursor(text, cursor_pos),
            None => text,
        };
        let trimmed = before_cursor.trim_end();
        if trimmed.is_empty() {
            return None;
        }

        let skip = trimmed.chars().count().saturating_sub(max_chars);
        Some(trimmed.chars().skip(skip).collect())
    }

    fn slice_before_cursor(text: &str, cursor_pos: usize) -> &str {
        let char_count = text.chars().count();
        if cursor_pos <= char_count {
            if cursor_pos == 0 {
                return "";
            }
            let byte_idx = text
                .char_indices()
                .nth(cursor_pos)
                .map(|(idx, _)| idx)
                .unwrap_or(text.len());
            &text[..byte_idx]
        } else {
            let mut byte_idx = cursor_pos.min(text.len());
            while byte_idx > 0 && !text.is_char_boundary(byte_idx) {
                byte_idx -= 1;
            }
            &text[..byte_idx]
        }
    }

    // ======================================================================
    // Internal: Frustration handling
    // ======================================================================

    fn handle_frustration(&mut self, signal: &FrustrationSignal) {
        match signal {
            FrustrationSignal::Reject { severity } => {
                self.light_profile.record_reject();
                // Suppress ghost for 1-3 words based on severity.
                self.suppress_countdown = (severity * 1.0).min(3.0).ceil() as u8;
                // Record in correction memory if we know what was accepted.
                if let Some(accepted) = &self.last_accepted_prediction {
                    let ctx_hash = self.current_context_hash();
                    let current = self.core.current_word();
                    if !current.is_empty() {
                        self.corrections.record(ctx_hash, accepted, current);
                    }
                }
                // Accepted-then-deleted is an explicit rejection of that ghost.
                self.record_last_ghost_rejection();
            }
            FrustrationSignal::RapidDelete { severity, .. } => {
                self.light_profile.record_reject();
                // Suppress ghost temporarily.
                self.suppress_countdown = (severity * 2.0).min(255.0).ceil() as u8;
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
                self.record_last_ghost_rejection();
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

    fn apply_correction_override(&mut self, actions: &mut Vec<Action>) {
        let Some(top_word) = self.core.predictions().first().map(|p| p.word.clone()) else {
            return;
        };

        let ctx_hash = self.current_context_hash();
        let Some(replacement) = self.corrections.check(ctx_hash, &top_word) else {
            return;
        };

        let Some(override_action) = self.core.apply_prediction_override(&replacement) else {
            return;
        };

        if let Some(idx) = actions.iter().position(|action| {
            matches!(
                action,
                Action::ShowGhost(_) | Action::ShowComposing { .. } | Action::HideGhost
            )
        }) {
            actions[idx] = override_action;
        } else {
            actions.push(override_action);
        }
    }

    /// Suppress the about-to-be-shown ghost if the user has already rejected
    /// this exact completion for the current typed prefix K times this session.
    ///
    /// Rewrites the outgoing ghost action so nothing is offered:
    ///   * `ShowGhost` → `HideGhost`
    ///   * `ShowComposing { typed, .. }` → `ShowComposing { typed, ghost: "" }`
    ///     (keeps the typed preedit intact — only the completion is dropped).
    fn enforce_rejection_suppression(&mut self, actions: &mut [Action]) {
        let Some(completion) = self.core.predictions().first().map(|p| p.word.clone()) else {
            return;
        };
        let prefix = self.core.current_word().to_string();
        if prefix.is_empty() || !self.rejections.is_suppressed(&prefix, &completion) {
            return;
        }

        self.core.clear_ghost();
        for action in actions.iter_mut() {
            match action {
                Action::ShowGhost(_) => *action = Action::HideGhost,
                Action::ShowComposing { typed, ghost } => {
                    if !ghost.is_empty() {
                        *action = Action::ShowComposing {
                            typed: std::mem::take(typed),
                            ghost: String::new(),
                        };
                    }
                }
                _ => {}
            }
        }
    }

    /// Cache the ghost currently on screen as `(typed_prefix, completion)` so a
    /// subsequent frustration signal can record which completion was rejected.
    /// Left unchanged when the turn shows no ghost, so it survives an
    /// intervening Escape/commit until the context is reset.
    fn note_shown_ghost(&mut self, actions: &[Action]) {
        let shows_ghost = actions
            .iter()
            .any(|action| matches!(action, Action::ShowGhost(_) | Action::ShowComposing { .. }));
        if !shows_ghost {
            return;
        }
        let prefix = self.core.current_word();
        if prefix.is_empty() {
            return;
        }
        if let Some(completion) = self.core.predictions().first().map(|p| p.word.clone()) {
            self.last_shown_ghost = Some((prefix.to_string(), completion));
        }
    }

    /// Record the last displayed ghost into the session rejection memory.
    /// No-op when no ghost is currently attributed.
    ///
    /// Consume-once: the cache is TAKEN, not cloned, so a single shown ghost
    /// can seed at most one frustration-derived record — a later signal cannot
    /// double-count the same (prefix, completion).
    fn record_last_ghost_rejection(&mut self) {
        if let Some((prefix, completion)) = self.last_shown_ghost.take() {
            self.rejections.record(&prefix, &completion);
        }
    }

    // ======================================================================
    // Internal: Learning
    // ======================================================================

    fn learn_from_commit(&mut self) {
        self.last_known_commits = self.core.metrics().commit_count();

        // Update language prior from detection.
        let lang = self.core.detected_language().lang;
        if self.last_accepted_prediction.is_some() {
            self.light_profile.record_accept(lang);
        } else {
            self.light_profile.observe_lang(lang);
        }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::correction_memory::CorrectionMemory;
    use crate::cvm::CvmSnapshot;
    use crate::input::{Action, InputConfig, Key, KeyEvent, Modifiers};
    use crate::personal::{PersonalMarkovSnapshot, PersonalProfile};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn make_key(key: Key) -> KeyEvent {
        KeyEvent {
            key,
            modifiers: Modifiers::empty(),
        }
    }

    fn default_loop() -> MasterLoop {
        MasterLoop::new(InputConfig::default())
    }

    fn test_cvm_snapshot() -> CvmSnapshot {
        CvmSnapshot {
            version: 1,
            saved_at_unix: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be after unix epoch")
                .as_secs_f64(),
            capacity: 500,
            max_capacity: 5000,
            round: 0,
            decay_lambda: 0.001,
            words: vec![],
            age_secs: Default::default(),
        }
    }

    fn ghost_text(actions: &[Action]) -> Option<String> {
        actions.iter().find_map(|action| match action {
            Action::ShowGhost(text) => Some(text.clone()),
            _ => None,
        })
    }

    #[test]
    fn new_starts_in_anticipating_phase() {
        let ml = default_loop();
        assert_eq!(ml.phase(), Phase::Anticipating);
    }

    #[test]
    fn initial_state_is_clean() {
        let ml = default_loop();
        assert!(ml.current_word().is_empty());
        assert!(ml.is_enabled());
        assert!(!ml.has_ghost());
    }

    #[test]
    fn handle_key_char_enters_tracking_phase() {
        let mut ml = default_loop();
        ml.handle_key(make_key(Key::Char('h')));
        // After first char, we should be in Tracking or later
        assert_ne!(ml.phase(), Phase::Anticipating);
    }

    #[test]
    fn handle_key_returns_actions_vec() {
        let mut ml = default_loop();
        let actions = ml.handle_key(make_key(Key::Char('a')));
        // Actions should be a Vec — not necessarily non-empty (depends on corpus)
        let _ = actions;
    }

    #[test]
    fn focus_lost_resets_anticipated_flag() {
        let mut ml = default_loop();
        ml.handle_key(make_key(Key::Char('h')));
        ml.focus_lost();
        // After focus_lost, next char should re-trigger anticipation
        // Phase may not be directly testable, but focus_lost should not panic
        assert!(ml.current_word().is_empty());
    }

    #[test]
    fn reset_clears_current_word() {
        let mut ml = default_loop();
        ml.handle_key(make_key(Key::Char('h')));
        ml.handle_key(make_key(Key::Char('e')));
        ml.reset();
        assert!(ml.current_word().is_empty());
    }

    #[test]
    fn suppress_countdown_no_overflow_with_extreme_severity() {
        // FrustrationSignal::Reject with severity=1.0 → suppress_countdown = ceil(1.0*1) = 1
        // severity is clamped to [0,1], so countdown max = 1, no overflow
        let mut ml = default_loop();
        // Drive a rejection scenario
        // suppress_countdown is u8, so verify it stays in bounds
        ml.handle_key(make_key(Key::Escape));
        ml.handle_key(make_key(Key::Char('a')));
        // If we got here without panic/overflow, the test passes
        assert!(ml.suppress_countdown <= 3);
    }

    #[test]
    fn phase_is_readable() {
        let ml = default_loop();
        let p = ml.phase();
        // Just verify we can read phase without panic
        let _ = format!("{:?}", p);
    }

    #[test]
    fn light_profile_accessible() {
        let ml = default_loop();
        let profile = ml.light_profile();
        let sum: f64 = profile.lang_prior.iter().sum();
        assert!((sum - 1.0).abs() < 1e-9);
    }

    #[test]
    fn correction_count_starts_at_zero() {
        let ml = default_loop();
        assert_eq!(ml.correction_count(), 0);
    }

    #[test]
    fn with_context_sampler_creates_instance() {
        let config = InputConfig::default();
        let ml = MasterLoop::with_context_sampler(config, Box::new(NullContextSampler));
        assert_eq!(ml.phase(), Phase::Anticipating);
    }

    #[test]
    fn anticipate_returns_hints_with_confidence_floor() {
        let mut ml = default_loop();
        // anticipate() is called internally on handle_key when word is empty
        // We can observe its effect: after focus_lost, next char should not panic
        ml.focus_lost();
        let actions = ml.handle_key(make_key(Key::Char('t')));
        let _ = actions;
        // No panic = anticipate() ran without error
    }

    #[test]
    fn phase_debug_format() {
        assert_eq!(format!("{:?}", Phase::Anticipating), "Anticipating");
        assert_eq!(format!("{:?}", Phase::Tracking), "Tracking");
        assert_eq!(format!("{:?}", Phase::Correcting), "Correcting");
        assert_eq!(format!("{:?}", Phase::Learning), "Learning");
    }

    // ==================================================================
    // Overflow guard tests for suppress_countdown
    // ==================================================================

    #[test]
    fn suppress_countdown_clamped_from_max_f64_severity() {
        // handle_frustration uses: (severity * 1.0).min(3.0).ceil() as u8
        // If severity = f64::MAX, the .min(3.0) clamp must prevent overflow.
        let mut ml = default_loop();
        let signal = crate::frustration::FrustrationSignal::Reject { severity: f64::MAX };
        ml.handle_frustration(&signal);
        // severity=f64::MAX → (f64::MAX * 1.0).min(3.0) = 3.0 → ceil = 3 → u8 = 3
        assert_eq!(
            ml.suppress_countdown, 3,
            "severity=f64::MAX should clamp to 3, got {}",
            ml.suppress_countdown
        );
    }

    #[test]
    fn suppress_countdown_zero_severity_produces_zero_or_one() {
        // severity = 0.0 → (0.0 * 1.0).min(3.0).ceil() = 0.0 as u8 = 0
        let mut ml = default_loop();
        let signal = crate::frustration::FrustrationSignal::Reject { severity: 0.0 };
        ml.handle_frustration(&signal);
        // ceil(0.0) = 0, so suppress_countdown = 0
        assert!(
            ml.suppress_countdown <= 1,
            "severity=0.0 should produce suppress_countdown 0 or 1, got {}",
            ml.suppress_countdown
        );
    }

    #[test]
    fn suppress_countdown_nan_severity_does_not_panic() {
        // NaN severity: (NaN * 1.0) = NaN; NaN.min(3.0) = NaN on some platforms.
        // NaN.ceil() = NaN; NaN as u8 = 0 in Rust (saturating cast).
        // Must not panic.
        let mut ml = default_loop();
        let signal = crate::frustration::FrustrationSignal::Reject { severity: f64::NAN };
        ml.handle_frustration(&signal);
        // No specific value assertion — just verify no panic and u8 stays valid.
        let _ = ml.suppress_countdown;
    }

    #[test]
    fn suppress_countdown_rapid_delete_nan_severity_does_not_panic() {
        // RapidDelete path: (severity * 2.0).min(255.0).ceil() as u8
        let mut ml = default_loop();
        let signal = crate::frustration::FrustrationSignal::RapidDelete {
            severity: f64::NAN,
            count: 5,
        };
        ml.handle_frustration(&signal);
        let _ = ml.suppress_countdown;
    }

    #[test]
    fn persisted_correction_overrides_ghost_prediction() {
        let dir = tempfile::tempdir().expect("temp dir should be created");
        let profile_path = dir.path().join("master_loop_profile.json");

        let mut corrections = CorrectionMemory::new();
        let ctx_hash = CorrectionMemory::context_hash(None, None);
        corrections.record(ctx_hash, "hello", "help");
        corrections.record(ctx_hash, "hello", "help");
        corrections.record(ctx_hash, "hello", "help");

        let mut profile = PersonalProfile::new(
            test_cvm_snapshot(),
            PersonalMarkovSnapshot {
                bigrams: vec![],
                trigrams: vec![],
            },
            None,
        );
        profile.corrections = Some(corrections.to_snapshot());

        std::fs::write(
            &profile_path,
            serde_json::to_string_pretty(&profile).expect("profile should serialize"),
        )
        .expect("profile should write");

        let mut ml = MasterLoop::new(InputConfig {
            ghost_text_separation_margin: 0.0,
            ..InputConfig::default()
        });
        ml.load_word("hello", 100);
        ml.load_word("help", 80);
        ml.load_personal(&profile_path)
            .expect("profile should load into master loop");

        ml.handle_key(make_key(Key::Char('h')));
        ml.handle_key(make_key(Key::Char('e')));
        let actions = ml.handle_key(make_key(Key::Char('l')));

        assert_eq!(ghost_text(&actions).as_deref(), Some("p"));
        assert_eq!(ml.ghost_text(), "p");
        assert_eq!(
            ml.predictions().first().map(|p| p.word.as_str()),
            Some("help")
        );
    }

    /// Pre-registered scope pin (defect B, suppression_threshold 3 -> 2): a
    /// persisted CorrectionMemory entry with EXACTLY count=2 must now trigger
    /// the override — it would NOT have under the old threshold of 3.
    #[test]
    fn persisted_correction_count_two_triggers_override() {
        let dir = tempfile::tempdir().expect("temp dir should be created");
        let profile_path = dir.path().join("master_loop_profile.json");

        let mut corrections = CorrectionMemory::new();
        let ctx_hash = CorrectionMemory::context_hash(None, None);
        corrections.record(ctx_hash, "hello", "help");
        corrections.record(ctx_hash, "hello", "help"); // exactly 2 (== K)

        let mut profile = PersonalProfile::new(
            test_cvm_snapshot(),
            PersonalMarkovSnapshot {
                bigrams: vec![],
                trigrams: vec![],
            },
            None,
        );
        profile.corrections = Some(corrections.to_snapshot());

        std::fs::write(
            &profile_path,
            serde_json::to_string_pretty(&profile).expect("profile should serialize"),
        )
        .expect("profile should write");

        let mut ml = MasterLoop::new(InputConfig {
            ghost_text_separation_margin: 0.0,
            ..InputConfig::default()
        });
        ml.load_word("hello", 100);
        ml.load_word("help", 80);
        ml.load_personal(&profile_path)
            .expect("profile should load into master loop");

        ml.handle_key(make_key(Key::Char('h')));
        ml.handle_key(make_key(Key::Char('e')));
        let actions = ml.handle_key(make_key(Key::Char('l')));

        assert_eq!(
            ml.predictions().first().map(|p| p.word.as_str()),
            Some("help"),
            "count=2 persisted correction must override under threshold=2"
        );
        assert_eq!(ghost_text(&actions).as_deref(), Some("p"));
    }

    #[test]
    fn surrounding_text_biases_predictions() {
        let mut ml = MasterLoop::new(InputConfig {
            use_ppm: false,
            ghost_text_separation_margin: 0.0,
            ..InputConfig::default()
        });
        ml.load_word("world", 50);
        ml.load_word("worry", 50);
        ml.load_bigram("hello", "world", 10);
        ml.set_surrounding_text(Some("hello world".into()), Some(6));

        ml.handle_key(make_key(Key::Char('w')));
        ml.handle_key(make_key(Key::Char('o')));
        ml.handle_key(make_key(Key::Char('r')));

        let world_pos = ml.predictions().iter().position(|p| p.word == "world");
        let worry_pos = ml.predictions().iter().position(|p| p.word == "worry");
        assert!(
            world_pos.is_some() && worry_pos.is_some(),
            "both surrounding-context candidates should appear"
        );
        assert!(
            world_pos.unwrap() < worry_pos.unwrap(),
            "surrounding text should rank 'world' above 'worry'"
        );
    }

    #[test]
    fn rejection_memory_suppresses_ghost_after_two_rejections() {
        let mut ml = MasterLoop::new(InputConfig {
            ghost_text_separation_margin: 0.0,
            ..InputConfig::default()
        });
        ml.load_word("hello", 100);

        // Baseline: "hel" offers the "hello" completion as a ghost.
        ml.handle_key(make_key(Key::Char('h')));
        ml.handle_key(make_key(Key::Char('e')));
        let baseline = ml.handle_key(make_key(Key::Char('l')));
        assert_eq!(ghost_text(&baseline).as_deref(), Some("lo"));
        // Smart-caps capitalizes the sentence-initial completion; rejection
        // memory normalizes case, so recording lowercase still matches.
        assert_eq!(
            ml.predictions().first().map(|p| p.word.as_str()),
            Some("Hello")
        );

        // The user rejects "Hello" for prefix "hel" twice (K=2).
        ml.record_ghost_rejection("hel", "hello");
        ml.record_ghost_rejection("hel", "hello");

        // Fresh word, same prefix → the ghost must now be suppressed.
        ml.focus_lost();
        ml.handle_key(make_key(Key::Char('h')));
        ml.handle_key(make_key(Key::Char('e')));
        let suppressed = ml.handle_key(make_key(Key::Char('l')));
        assert!(
            ghost_text(&suppressed).is_none(),
            "ghost should be suppressed after two rejections, got {suppressed:?}"
        );
        assert!(suppressed
            .iter()
            .any(|action| matches!(action, Action::HideGhost)));
        assert!(!ml.has_ghost());
        assert_eq!(ml.ghost_text(), "");
    }

    #[test]
    fn single_rejection_does_not_suppress_ghost() {
        let mut ml = MasterLoop::new(InputConfig {
            ghost_text_separation_margin: 0.0,
            ..InputConfig::default()
        });
        ml.load_word("hello", 100);

        // Only one rejection — below the K=2 threshold.
        ml.record_ghost_rejection("hel", "hello");

        ml.handle_key(make_key(Key::Char('h')));
        ml.handle_key(make_key(Key::Char('e')));
        let actions = ml.handle_key(make_key(Key::Char('l')));
        assert_eq!(
            ghost_text(&actions).as_deref(),
            Some("lo"),
            "a single rejection must not suppress the ghost"
        );
    }

    /// Regression (double-count lifecycle): a SINGLE user rejection recorded at
    /// a word boundary must not be re-counted by a later ABANDON that inherits
    /// a stale ghost cache — otherwise one rejection reaches K=2 and suppression
    /// would fire on the 2nd attempt instead of the 3rd.
    #[test]
    fn word_boundary_record_then_abandon_counts_once() {
        let mut ml = MasterLoop::new(InputConfig {
            ghost_text_separation_margin: 0.0,
            ..InputConfig::default()
        });
        ml.load_word("hello", 100);

        // Ghost shown for "hel", then the user types through the boundary
        // (Space). handle_key commits; the adapter then records the
        // word_boundary rejection exactly once.
        ml.handle_key(make_key(Key::Char('h')));
        ml.handle_key(make_key(Key::Char('e')));
        ml.handle_key(make_key(Key::Char('l')));
        ml.handle_key(make_key(Key::Space));
        ml.record_ghost_rejection("hel", "hello");

        // A later, unrelated ABANDON (Escape on the anticipatory ghost, then a
        // keystroke) must NOT re-record the same (prefix, completion).
        ml.handle_key(make_key(Key::Escape));
        ml.handle_key(make_key(Key::Char('q')));

        assert!(
            !ml.is_ghost_suppressed("hel", "hello"),
            "one word-boundary rejection must stay at count 1, not double to K=2"
        );
    }

    #[test]
    fn abandon_signal_feeds_rejection_memory() {
        let mut ml = MasterLoop::new(InputConfig {
            ghost_text_separation_margin: 0.0,
            ..InputConfig::default()
        });
        ml.load_word("hello", 100);

        // Show a ghost, then Escape + type manually twice (ABANDON) to reject
        // the "hello" completion at prefix "hel" the required K=2 times.
        for _ in 0..2 {
            ml.handle_key(make_key(Key::Char('h')));
            ml.handle_key(make_key(Key::Char('e')));
            ml.handle_key(make_key(Key::Char('l')));
            ml.handle_key(make_key(Key::Escape));
            ml.handle_key(make_key(Key::Char('x')));
            ml.focus_lost();
        }

        // The abandon signals should have recorded the rejection.
        assert!(ml.rejection_prefix_count() >= 1);
        ml.handle_key(make_key(Key::Char('h')));
        ml.handle_key(make_key(Key::Char('e')));
        let actions = ml.handle_key(make_key(Key::Char('l')));
        assert!(
            ghost_text(&actions).is_none(),
            "abandon-recorded rejections should suppress the ghost, got {actions:?}"
        );
    }

    #[test]
    fn navigation_resets_anticipation_cycle() {
        let mut ml = default_loop();
        ml.handle_key(make_key(Key::Char('h')));
        assert_eq!(ml.phase(), Phase::Tracking);

        let actions = ml.handle_key(make_key(Key::Left));
        assert!(actions
            .iter()
            .any(|action| matches!(action, Action::HideGhost)));
        assert_eq!(ml.phase(), Phase::Anticipating);

        let next_actions = ml.handle_key(make_key(Key::Char('e')));
        assert!(next_actions
            .iter()
            .any(|action| matches!(action, Action::ForwardKey)));
        assert_eq!(ml.phase(), Phase::Tracking);
    }
}
