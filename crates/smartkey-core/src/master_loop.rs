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
    /// Bounded, process-local memory of repeatedly rejected ghost pairs.
    rejections: RejectionMemory,
    light_profile: LightProfile,
    context_sampler: Box<dyn ContextSampler>,
    /// Words remaining for ghost suppression after frustration.
    suppress_countdown: u8,
    /// When the last terminal prediction accept occurred (for REJECT detection).
    last_prediction_accept: Option<Instant>,
    /// The prediction most recently accepted via Tab or final Right.
    last_accepted_prediction: Option<String>,
    /// Last known commit count from metrics (for change detection).
    last_known_commits: usize,
    /// Whether anticipation was already run for the current word.
    anticipated: bool,
    /// Cached surrounding text from the platform adapter (Linux IBus path).
    surrounding_text: Option<String>,
    /// Cursor position inside `surrounding_text` when provided by the platform.
    surrounding_cursor_pos: Option<usize>,
    /// Most recently displayed `(typed prefix, full completion)` pair.
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
            last_prediction_accept: None,
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
        // Opt-in Space-accept (2026-08-22): an eligible Space is the same
        // acceptance event as Tab/final Right and must be accounted once here.
        // GREEN v4: the core's own predicate is necessary, not sufficient.
        // The master loop owns the DISPLAY attribution (`last_shown_ghost`,
        // consumed by the adapter's rejection feedback); only when the tracked
        // (prefix, completion) is exactly the composition the core holds and
        // the current top prediction does Space count as an acceptance — and
        // the core is told so for exactly this key (one-shot attestation).
        let is_space_accept = crate::input::InputMethodCore::is_space_event(&event)
            && self.core.space_accept_armed()
            && self.space_display_identity_ok();
        self.core.attest_space_accept_display(is_space_accept);
        let is_accept_key = matches!(event.key, crate::input::Key::Tab | crate::input::Key::Right)
            || is_space_accept;
        let is_backspace = matches!(event.key, crate::input::Key::Backspace);
        // An accepted ghost is attributable to one immediate Backspace only.
        // Any other next key starts a new interaction and must not let a later
        // ABANDON reject the already accepted completion.
        if !is_backspace && self.last_prediction_accept.is_some() {
            self.last_prediction_accept = None;
            self.last_shown_ghost = None;
        }
        // Capture prediction and commit count BEFORE delegation. Terminal Tab
        // and Right clear last_predictions/current_word inside the core, while
        // a partial Right keeps both alive and must not count as acceptance.
        let pre_accept_prediction = if is_accept_key && self.core.has_ghost() {
            self.core.predictions().first().map(|p| p.word.clone())
        } else {
            None
        };
        let pre_commit_count = self.core.metrics().commit_count();
        let mut actions = self.core.handle_key(event.clone());
        self.apply_correction_override(&mut actions);
        self.enforce_rejection_suppression(&mut actions);
        let terminal_accept = pre_accept_prediction.is_some()
            && self.core.current_word().is_empty()
            && self.core.metrics().commit_count() > pre_commit_count;

        if resets_context {
            self.anticipated = false;
            self.phase = Phase::Anticipating;
            self.core.clear_hints();
            self.frustration.reset_word();
            self.last_prediction_accept = None;
            self.last_accepted_prediction = None;
            self.last_shown_ghost = None;
        }

        // Track terminal Tab/final-Right acceptance for learning and immediate
        // accepted-then-Backspace rejection detection.
        if terminal_accept {
            self.last_accepted_prediction = pre_accept_prediction;
            if is_space_accept {
                // A Space acceptance already landed its delimiter: the
                // immediate Backspace removes only that space (text-native),
                // so it must never be read as "accepted, then deleted".
                // Positive learning is kept (last_accepted_prediction);
                // the rejection attribution is not armed.
                self.last_prediction_accept = None;
                self.last_shown_ghost = None;
            } else {
                self.last_prediction_accept = Some(Instant::now());
            }
        }

        // ── Phase 3: EVALUATE ────────────────────────────────────────
        let signal = self.frustration.feed(
            &event.key,
            self.core.current_word(),
            self.last_prediction_accept,
        );

        if let Some(ref signal) = signal {
            self.phase = Phase::Correcting;
            self.handle_frustration(signal);
        }
        if is_backspace {
            // Backspace over a live ghost IS the rejection gesture — the most
            // natural way a user says "not this word". Previously the
            // attribution was discarded here as stale, so deleting a suggestion
            // taught the engine nothing and the same completion came straight
            // back (live trace 2026-08-03: "лг" -> ghost "бт", three times in
            // ten seconds). Record it instead; record_last_ghost_rejection
            // takes the value, so the slot is cleared either way.
            //
            // REJECT already recorded it inside handle_frustration above.
            if !matches!(signal, Some(FrustrationSignal::Reject { .. })) {
                self.record_last_ghost_rejection();
            }
            self.last_prediction_accept = None;
        }

        // Word committed? → LEARNING phase.
        if self.core.current_word().is_empty() && self.session_commits_changed() {
            self.phase = Phase::Learning;
            self.learn_from_commit();
            self.phase = Phase::Anticipating;
            self.anticipated = false;
            self.core.clear_hints();
            self.frustration.reset_word();
            // Boundary outcomes are recorded by the adapter after this call.
            // Clear the Rust-side cache to prevent a later ABANDON from
            // double-counting the same rejection. A terminal accept keeps its
            // attribution for the immediate accepted-then-deleted path above.
            if !terminal_accept {
                self.last_shown_ghost = None;
            }
        }

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

    // -- passive calibration tap (delegates to the core) ----------------

    pub fn o1_ngram_snapshot(
        &self,
        ctx: &str,
        uni_top3_cache: &[String],
    ) -> (Vec<String>, crate::o1_shim::O1Numbers) {
        self.core.o1_ngram_snapshot(ctx, uni_top3_cache)
    }

    pub fn o1_global_unigram_top3(&self) -> Vec<String> {
        self.core.o1_global_unigram_top3()
    }

    pub fn is_enabled(&self) -> bool {
        self.core.is_enabled()
    }

    pub fn current_word(&self) -> &str {
        self.core.current_word()
    }

    /// Read-only view of the wrapped core (F4 Phase R plumbing so lane E
    /// tests can inspect the dual buffer / injected prior through the loop).
    #[cfg(test)]
    pub(crate) fn core(&self) -> &InputMethodCore {
        &self.core
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

    /// Record one confirmed rejection from a platform adapter.
    pub fn record_ghost_rejection(&mut self, prefix: &str, completion: &str) {
        self.rejections.record(prefix, completion);

        // Consume only matching attribution. A diverging key can show a new
        // ghost before the adapter reports the prior rejection; clearing that
        // newer pair would make a subsequent rejection invisible.
        let matches_cached =
            self.last_shown_ghost
                .as_ref()
                .is_some_and(|(cached_prefix, cached_completion)| {
                    Self::same_normalized(cached_prefix, prefix)
                        && Self::same_normalized(cached_completion, completion)
                });
        if matches_cached {
            self.last_shown_ghost = None;
        }
    }

    pub fn is_ghost_suppressed(&self, prefix: &str, completion: &str) -> bool {
        self.rejections.is_suppressed(prefix, completion)
    }

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
                self.record_last_ghost_rejection();
            }
            FrustrationSignal::RapidDelete { severity, .. } => {
                self.light_profile.record_reject();
                // Suppress ghost temporarily.
                self.suppress_countdown = (severity * 2.0).min(255.0).ceil() as u8;
                // NOT record_ghost_as_negative(): mid-deletion `current_word` is a
                // half-erased fragment, not a word the user wanted. Teaching it to
                // correction memory made the fragment hijack the top slot after
                // three occurrences — and correction memory is PERSISTED, so the
                // damage outlived restarts. Deleting a suggestion must make the
                // engine quieter, never make it substitute something new.
                // The rejection itself is recorded on the backspace path.
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

    fn enforce_rejection_suppression(&mut self, actions: &mut [Action]) {
        let Some(completion) = self.core.predictions().first().map(|p| p.word.clone()) else {
            return;
        };
        let prefix = self.core.current_word().to_string();
        if prefix.is_empty() || !self.rejections.is_suppressed(&prefix, &completion) {
            return;
        }

        self.core.clear_ghost();
        // Suppression hides any shorter-prefix ghost that was on screen while
        // the user reached this exact prefix. It is no longer attributable.
        self.last_shown_ghost = None;
        for action in actions {
            match action {
                Action::ShowGhost(_) => *action = Action::HideGhost,
                Action::ShowComposing { typed, ghost } if !ghost.is_empty() => {
                    *action = Action::ShowComposing {
                        typed: std::mem::take(typed),
                        ghost: String::new(),
                    };
                }
                _ => {}
            }
        }
    }

    fn note_shown_ghost(&mut self, actions: &[Action]) {
        let displays_completion = actions.iter().any(|action| match action {
            Action::ShowGhost(ghost) => !ghost.is_empty(),
            Action::ShowComposing { ghost, .. } => !ghost.is_empty(),
            _ => false,
        });
        if !displays_completion {
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

    fn record_last_ghost_rejection(&mut self) {
        if let Some((prefix, completion)) = self.last_shown_ghost.take() {
            self.rejections.record(&prefix, &completion);
        }
    }

    fn same_normalized(left: &str, right: &str) -> bool {
        left.trim().to_lowercase() == right.trim().to_lowercase()
    }

    /// Display identity for the opt-in Space-accept (GREEN v4): the tracked
    /// shown pair must match the live prefix, the live top prediction, and the
    /// composition (prefix + ghost) the user currently sees.  Any gap — no
    /// attribution (e.g. consumed by `record_ghost_rejection`), a different
    /// prefix, a different completion — means "not the displayed candidate".
    fn space_display_identity_ok(&self) -> bool {
        let Some((shown_prefix, shown_completion)) = self.last_shown_ghost.as_ref() else {
            return false;
        };
        let prefix = self.core.current_word();
        let Some(top) = self.core.predictions().first() else {
            return false;
        };
        let composition = format!("{prefix}{}", self.core.ghost_text());
        Self::same_normalized(shown_prefix, prefix)
            && Self::same_normalized(shown_completion, &top.word)
            && Self::same_normalized(&top.word, &composition)
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

    /// Deleting the same suggestion twice must silence it.
    ///
    /// This is the gesture a user actually performs to say "not this word", and
    /// until now it recorded nothing: the backspace arm discarded
    /// `last_shown_ghost` as stale attribution, so `RejectionMemory` never saw a
    /// single rejection and its threshold of 2 was unreachable by deletion.
    /// Live trace 2026-08-03 shows the consequence — "лг" -> ghost "бт" offered
    /// three times within ten seconds, deleted each time.
    #[test]
    fn deleting_a_suggestion_twice_suppresses_it() {
        let mut ml = default_loop();

        // First offer, then delete it.
        ml.last_shown_ghost = Some(("лг".to_string(), "лгбт".to_string()));
        ml.handle_key(make_key(Key::Backspace));
        assert!(
            !ml.is_ghost_suppressed("лг", "лгбт"),
            "one rejection is below the threshold of 2 — must not suppress yet"
        );

        // Same completion offered again, deleted again.
        ml.last_shown_ghost = Some(("лг".to_string(), "лгбт".to_string()));
        ml.handle_key(make_key(Key::Backspace));
        assert!(
            ml.is_ghost_suppressed("лг", "лгбт"),
            "after two deletions the completion must be suppressed"
        );

        // A different completion is untouched — suppression is per (prefix, word),
        // not a blanket mute.
        assert!(
            !ml.is_ghost_suppressed("лг", "лгота"),
            "suppression must not leak to other completions of the same prefix"
        );
    }

    /// Deleting must make the engine quieter, never make it substitute.
    ///
    /// `RapidDelete` used to call `record_ghost_as_negative`, which wrote the
    /// half-erased `current_word` into CorrectionMemory as "what the user really
    /// wanted". That store is PERSISTED and hijacks the top slot after three
    /// occurrences, so a mid-word delete burst permanently poisoned predictions.
    #[test]
    fn delete_burst_does_not_write_correction_memory() {
        let mut ml = default_loop();
        // A dictionary is mandatory here: record_ghost_as_negative writes only
        // `if let Some(top) = predictions().first()`. Without loaded words there
        // are no predictions, the write is skipped anyway, and the test passes
        // even with the bug present — verified by falsification.
        ml.load_word("работа", 1_000_000);
        ml.load_word("работи", 900_000);

        for c in "работ".chars() {
            ml.handle_key(make_key(Key::Char(c)));
        }
        assert!(
            !ml.predictions().is_empty(),
            "test precondition: predictions must exist, else nothing can be recorded"
        );

        let before = ml.correction_count();
        // Erase in a burst — RapidDelete fires on 3+ backspaces within a second.
        for _ in 0..5 {
            ml.handle_key(make_key(Key::Backspace));
        }

        assert_eq!(
            ml.correction_count(),
            before,
            "a deletion burst must not teach correction memory a half-erased fragment"
        );
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

    // ── GREEN v4 (ADVOCATE_CODEX c423f1631ad2): the core Space decision must
    // depend on the MasterLoop-authoritative DISPLAY identity, not only on the
    // core-local predicate. ─────────────────────────────────────────────────

    fn space_loop() -> MasterLoop {
        let mut ml = MasterLoop::new(InputConfig {
            space_accept: true,
            ghost_text_separation_margin: 0.0,
            ..InputConfig::default()
        });
        ml.load_word("здравей", 1_000_000);
        for code in [44, 32, 19, 30] {
            ml.handle_key(make_key(Key::RawCode(code)));
        }
        assert_eq!(ml.current_word(), "здра");
        assert_eq!(ml.ghost_text(), "вей");
        ml
    }

    fn assert_literal_space(ml: &mut MasterLoop) {
        let before = ml.core.metrics().summary().accepted_predictions;
        let actions = ml.handle_key(make_key(Key::Space));
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::CommitText(t) if t == "здра")),
            "literal Space commits the typed prefix only, got {actions:?}"
        );
        assert!(actions.iter().any(|a| matches!(a, Action::ForwardKey)));
        assert!(!actions
            .iter()
            .any(|a| matches!(a, Action::CommitText(t) if t.contains("здравей"))));
        assert_eq!(
            ml.core.metrics().summary().accepted_predictions,
            before,
            "no acceptance learning on a literal Space"
        );
    }

    /// The native counterexample: the adapter's public rejection feedback
    /// clears the display attribution while the core still holds ghost/top.
    /// Space must then be literal.
    #[test]
    fn space_accept_is_literal_after_record_ghost_rejection() {
        let mut ml = space_loop();
        ml.record_ghost_rejection("здра", "здравей");
        assert!(
            ml.last_shown_ghost.is_none(),
            "precondition: attribution consumed"
        );
        assert_eq!(ml.ghost_text(), "вей", "precondition: core ghost retained");

        assert_literal_space(&mut ml);
    }

    #[test]
    fn space_accept_is_literal_without_display_attribution() {
        let mut ml = space_loop();
        ml.last_shown_ghost = None;

        assert_literal_space(&mut ml);
    }

    #[test]
    fn space_accept_is_literal_when_tracked_prefix_differs() {
        let mut ml = space_loop();
        ml.last_shown_ghost = Some(("зд".to_string(), "здравей".to_string()));

        assert_literal_space(&mut ml);
    }

    #[test]
    fn space_accept_is_literal_when_tracked_completion_differs() {
        let mut ml = space_loop();
        ml.last_shown_ghost = Some(("здра".to_string(), "здравейте".to_string()));

        assert_literal_space(&mut ml);
    }

    /// Retained-state guards: Escape (HideGhost) and a suppressed completion
    /// both leave nothing acceptable.
    #[test]
    fn space_accept_is_literal_after_escape() {
        let mut ml = space_loop();
        ml.handle_key(make_key(Key::Escape));
        assert!(!ml.has_ghost());

        assert_literal_space(&mut ml);
    }

    #[test]
    fn space_accept_is_literal_when_completion_is_suppressed() {
        let mut ml = space_loop();
        ml.record_ghost_rejection("здра", "здравей");
        ml.record_ghost_rejection("здра", "здравей");
        assert!(ml.is_ghost_suppressed("здра", "здравей"));

        assert_literal_space(&mut ml);
    }

    /// The live adapter sends EVERY key as a raw scancode (process_keycode);
    /// a raw Space (evdev 57) must be attested exactly like `Key::Space`.
    #[test]
    fn space_accept_via_raw_scancode_is_accepted() {
        let mut ml = space_loop();

        let actions = ml.handle_key(make_key(Key::RawCode(57)));

        assert!(actions
            .iter()
            .any(|a| matches!(a, Action::CommitText(t) if t == "здравей ")));
        assert!(!actions.iter().any(|a| matches!(a, Action::ForwardKey)));
        assert_eq!(ml.core.metrics().summary().accepted_predictions, 1);
    }

    #[test]
    fn raw_space_after_record_ghost_rejection_is_literal() {
        let mut ml = space_loop();
        ml.record_ghost_rejection("здра", "здравей");
        let before = ml.core.metrics().summary().accepted_predictions;

        let actions = ml.handle_key(make_key(Key::RawCode(57)));

        assert!(actions
            .iter()
            .any(|a| matches!(a, Action::CommitText(t) if t == "здра")));
        assert!(actions.iter().any(|a| matches!(a, Action::ForwardKey)));
        assert_eq!(ml.core.metrics().summary().accepted_predictions, before);
    }

    /// Positive control: the exact matching visible pair is accepted.
    #[test]
    fn space_accept_with_matching_display_identity_is_accepted() {
        let mut ml = space_loop();
        let shown = ml
            .last_shown_ghost
            .clone()
            .expect("display attribution present");
        assert_eq!(shown.0, "здра");
        assert!(
            MasterLoop::same_normalized(&shown.1, "здравей"),
            "tracked completion is the shown word (case-normalized), got {shown:?}"
        );

        let actions = ml.handle_key(make_key(Key::Space));

        assert!(actions
            .iter()
            .any(|a| matches!(a, Action::CommitText(t) if t == "здравей ")));
        assert!(!actions.iter().any(|a| matches!(a, Action::ForwardKey)));
        assert_eq!(ml.core.metrics().summary().accepted_predictions, 1);
    }

    /// Preservation: final Right then immediate Backspace still records the
    /// rejection (the Tab/Right attribution path is untouched by Space).
    #[test]
    fn final_right_then_immediate_backspace_still_records_rejection() {
        let mut ml = space_loop();
        for _ in 0..3 {
            ml.handle_key(make_key(Key::Right));
        }
        assert!(ml.current_word().is_empty());
        assert_eq!(ml.core.metrics().summary().accepted_predictions, 1);

        ml.handle_key(make_key(Key::Backspace));

        assert_eq!(ml.rejection_prefix_count(), 1);
    }

    #[test]
    fn final_right_accept_updates_light_profile_like_tab() {
        let mut ml = MasterLoop::new(InputConfig {
            ghost_text_separation_margin: 0.0,
            ..InputConfig::default()
        });
        ml.load_word("здравей", 1_000_000);
        for code in [44, 32, 19, 30, 17] {
            ml.handle_key(make_key(Key::RawCode(code)));
        }
        assert_eq!(ml.current_word(), "здрав");
        assert_eq!(ml.ghost_text(), "ей");

        ml.handle_key(make_key(Key::Right));
        assert_eq!(ml.current_word(), "здраве");
        assert_eq!(ml.ghost_text(), "й");

        let before = ml.light_profile().ghost_accept_rate;
        ml.handle_key(make_key(Key::Right));

        assert!(ml.current_word().is_empty());
        assert_eq!(ml.core.metrics().summary().accepted_predictions, 1);
        assert!(
            ml.light_profile().ghost_accept_rate > before,
            "final Right is a prediction acceptance, not an ordinary commit"
        );
    }

    /// Opt-in Space-accept (2026-08-22, ADVOCATE_CODEX bd9219aa448c A): an
    /// eligible Space is ONE acceptance event through the master loop —
    /// light-profile learning exactly like a terminal Tab/final Right, and the
    /// emitted batch is the single delimiter-carrying commit with no forward.
    #[test]
    fn eligible_space_accept_updates_light_profile_like_tab() {
        let mut ml = MasterLoop::new(InputConfig {
            space_accept: true,
            ghost_text_separation_margin: 0.0,
            ..InputConfig::default()
        });
        ml.load_word("здравей", 1_000_000);
        for code in [44, 32, 19, 30] {
            ml.handle_key(make_key(Key::RawCode(code)));
        }
        assert_eq!(ml.current_word(), "здра");
        assert_eq!(ml.ghost_text(), "вей");

        let before = ml.light_profile().ghost_accept_rate;
        let actions = ml.handle_key(make_key(Key::Space));

        assert!(ml.current_word().is_empty());
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::CommitText(t) if t == "здравей ")),
            "one commit carrying word + delimiter, got {actions:?}"
        );
        assert!(
            !actions.iter().any(|a| matches!(a, Action::ForwardKey)),
            "the Space is consumed"
        );
        assert_eq!(ml.core.metrics().summary().accepted_predictions, 1);
        assert!(
            ml.light_profile().ghost_accept_rate > before,
            "an eligible Space is a prediction acceptance, not an ordinary commit"
        );
    }

    /// Flag off: the same keystrokes are an ordinary word boundary — typed
    /// word committed, Space forwarded, nothing counted as an acceptance.
    #[test]
    fn literal_space_with_flag_off_is_not_an_acceptance() {
        let mut ml = MasterLoop::new(InputConfig {
            ghost_text_separation_margin: 0.0,
            ..InputConfig::default()
        });
        ml.load_word("здравей", 1_000_000);
        for code in [44, 32, 19, 30] {
            ml.handle_key(make_key(Key::RawCode(code)));
        }
        assert_eq!(ml.ghost_text(), "вей");

        let before = ml.light_profile().ghost_accept_rate;
        let actions = ml.handle_key(make_key(Key::Space));

        assert!(actions
            .iter()
            .any(|a| matches!(a, Action::CommitText(t) if t == "здра")));
        assert!(actions.iter().any(|a| matches!(a, Action::ForwardKey)));
        assert_eq!(ml.core.metrics().summary().accepted_predictions, 0);
        assert_eq!(ml.light_profile().ghost_accept_rate, before);
    }

    /// Blocker 2 (ADVOCATE_CODEX 4f9970749edd): after a Space acceptance the
    /// immediate Backspace removes only the delimiter (text-native), so it is
    /// NOT a rejection of the completion. Space must record the positive
    /// acceptance once but never arm the Tab/final-Right accepted-then-Backspace
    /// rejection attribution.
    #[test]
    fn space_accept_then_immediate_backspace_is_not_a_rejection() {
        let mut ml = MasterLoop::new(InputConfig {
            space_accept: true,
            ghost_text_separation_margin: 0.0,
            ..InputConfig::default()
        });
        ml.load_word("здравей", 1_000_000);
        for code in [44, 32, 19, 30] {
            ml.handle_key(make_key(Key::RawCode(code)));
        }
        assert_eq!(ml.ghost_text(), "вей");
        let accepted = ml.handle_key(make_key(Key::Space));
        assert!(accepted
            .iter()
            .any(|a| matches!(a, Action::CommitText(t) if t == "здравей ")));
        assert_eq!(ml.core.metrics().summary().accepted_predictions, 1);
        let rate_after_accept = ml.light_profile().ghost_accept_rate;

        let actions = ml.handle_key(make_key(Key::Backspace));

        assert!(
            actions.iter().any(|a| matches!(a, Action::ForwardKey)),
            "Backspace after a Space accept is forwarded untouched, got {actions:?}"
        );
        assert_eq!(
            ml.rejection_prefix_count(),
            0,
            "deleting the delimiter must not record a rejection of the completion"
        );
        assert_eq!(
            ml.light_profile().ghost_accept_rate,
            rate_after_accept,
            "no frustration REJECT may be attributed to a Space acceptance"
        );
        assert_eq!(ml.correction_count(), 0);
    }

    /// Guard: the Tab path keeps its attribution — Tab-accept then immediate
    /// Backspace IS the rejection gesture (live trace 2026-08-03).
    #[test]
    fn tab_accept_then_immediate_backspace_still_records_rejection() {
        let mut ml = MasterLoop::new(InputConfig {
            space_accept: true,
            ghost_text_separation_margin: 0.0,
            ..InputConfig::default()
        });
        ml.load_word("здравей", 1_000_000);
        for code in [44, 32, 19, 30] {
            ml.handle_key(make_key(Key::RawCode(code)));
        }
        assert_eq!(ml.ghost_text(), "вей");
        ml.handle_key(make_key(Key::Tab));
        assert_eq!(ml.core.metrics().summary().accepted_predictions, 1);
        let rate_after_accept = ml.light_profile().ghost_accept_rate;

        ml.handle_key(make_key(Key::Backspace));

        assert_eq!(
            ml.rejection_prefix_count(),
            1,
            "Tab-accept then immediate Backspace records one rejection"
        );
        assert!(
            ml.light_profile().ghost_accept_rate < rate_after_accept,
            "the frustration REJECT still lowers the accept rate on the Tab path"
        );
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

    #[test]
    fn rejection_memory_suppresses_exact_pair_after_two_records() {
        let mut ml = MasterLoop::new(InputConfig {
            ghost_text_separation_margin: 0.0,
            ..InputConfig::default()
        });
        ml.load_word("hello", 100);

        ml.handle_key(make_key(Key::Char('h')));
        ml.handle_key(make_key(Key::Char('e')));
        let baseline = ml.handle_key(make_key(Key::Char('l')));
        assert_eq!(ghost_text(&baseline).as_deref(), Some("lo"));

        ml.record_ghost_rejection("hel", "hello");
        assert!(!ml.is_ghost_suppressed("hel", "hello"));
        ml.record_ghost_rejection("hel", "hello");
        assert!(ml.is_ghost_suppressed("hel", "hello"));

        ml.focus_lost();
        ml.handle_key(make_key(Key::Char('h')));
        ml.handle_key(make_key(Key::Char('e')));
        let suppressed = ml.handle_key(make_key(Key::Char('l')));
        assert!(ghost_text(&suppressed).is_none());
        assert!(suppressed
            .iter()
            .any(|action| matches!(action, Action::HideGhost)));
        assert!(!ml.has_ghost());
        assert!(ml.last_shown_ghost.is_none());
    }

    #[test]
    fn abandon_records_the_displayed_pair_once() {
        let mut ml = MasterLoop::new(InputConfig {
            ghost_text_separation_margin: 0.0,
            ..InputConfig::default()
        });
        ml.load_word("hello", 100);
        ml.handle_key(make_key(Key::Char('h')));
        ml.handle_key(make_key(Key::Char('e')));
        ml.handle_key(make_key(Key::Char('l')));

        ml.handle_key(make_key(Key::Escape));
        ml.handle_key(make_key(Key::Char('x')));

        assert_eq!(ml.rejection_prefix_count(), 1);
        assert!(!ml.is_ghost_suppressed("hel", "hello"));
        assert!(ml.last_shown_ghost.is_none());
    }

    #[test]
    fn adapter_record_does_not_clear_a_newer_displayed_pair() {
        let mut ml = default_loop();
        ml.last_shown_ghost = Some(("new".into(), "newer".into()));

        ml.record_ghost_rejection("old", "older");
        assert_eq!(
            ml.last_shown_ghost.as_ref(),
            Some(&("new".to_string(), "newer".to_string()))
        );

        ml.record_ghost_rejection(" NEW ", "NEWER");
        assert!(ml.last_shown_ghost.is_none());
    }

    #[test]
    fn boundary_record_cannot_be_counted_again_by_later_abandon() {
        let mut ml = MasterLoop::new(InputConfig {
            ghost_text_separation_margin: 0.0,
            ..InputConfig::default()
        });
        ml.load_word("hello", 100);
        ml.handle_key(make_key(Key::Char('h')));
        ml.handle_key(make_key(Key::Char('e')));
        ml.handle_key(make_key(Key::Char('l')));
        ml.handle_key(make_key(Key::Space));
        ml.record_ghost_rejection("hel", "hello");

        ml.handle_key(make_key(Key::Escape));
        ml.handle_key(make_key(Key::Char('q')));

        assert!(!ml.is_ghost_suppressed("hel", "hello"));
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

/// F4 Phase R plumbing smoke (ruling 221af6d0a1c8): the test-only `core()`
/// view is wired.  Lane E RED tests live in `f4_lane_cde_tests.rs`, not here.
#[cfg(test)]
mod f4_plumbing_tests {
    use super::*;
    use crate::input::{InputConfig, Key, KeyEvent, Modifiers};
    use crate::lang_detect::LangId;

    #[test]
    fn core_view_mirrors_the_decorator_state() {
        let mut ml = MasterLoop::new(InputConfig::default());
        ml.load_word_lang("статия", 12_000, LangId::Bg);
        ml.set_surrounding_text(Some("smartkey ".to_string()), Some(9));
        ml.handle_key(KeyEvent {
            key: Key::RawCode(31),
            modifiers: Modifiers::empty(),
        });

        assert_eq!(ml.core().current_word(), ml.current_word());
        assert!(ml.core().dual_buffer().is_some());
        assert_eq!(ml.core().active_lang_prior(), Some(LangId::En));
    }
}
