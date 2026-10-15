// Master Loop — proactive orchestrator wrapping InputMethodCore.
//
// State machine: ANTICIPATING → TRACKING → CORRECTING → LEARNING
//
// Before each word:  reads context, predicts lang/caps, checks corrections.
// During each word:  delegates to core, monitors frustration signals.
// After each word:   records corrections, updates profile, loops.
//
// Implements the Decorator pattern: mirrors InputMethodCore's public API 1:1.

use std::borrow::Cow;
use std::path::Path;
use std::time::Instant;

use crate::context_sampler::{self, ContextAnalysis, ContextSampler, NullContextSampler};
use crate::correction_memory::CorrectionMemory;
use crate::ensemble::Prediction;
use crate::eval::PredictionMetrics;
use crate::frustration::{FrustrationDetector, FrustrationSignal};
use crate::input::{Action, InputConfig, InputMethodCore, Key, KeyEvent, Modifiers};
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

const MAX_SENTENCE_ECHOES: usize = 64;
const MAX_SENTENCE_ECHO_BYTES: usize = 64 * 1024;

/// A local replacement ending at the original cursor, sharing the document.
/// Preedit never enters this projection; cursor positions count Unicode chars.
#[derive(Clone, PartialEq, Eq)]
struct SentenceEchoImage {
    left: usize,
    cursor: usize,
    inserted: String,
}

#[derive(Clone, Copy)]
enum OpaqueSeparator {
    Return,
    Tab,
}

/// Possible delivered echoes while sentence presentation remains bound to a
/// client segment. Matching acknowledges progress, never an undo log.
struct SentenceEchoes {
    base: Box<str>,
    anchor: usize,
    images: Vec<SentenceEchoImage>,
    opaque: Option<OpaqueSeparator>,
}

impl SentenceEchoes {
    fn new(text: &str, cursor: usize) -> Option<Self> {
        let anchor = text
            .char_indices()
            .nth(cursor)
            .map(|(byte, _)| byte)
            .or_else(|| (cursor == text.chars().count()).then_some(text.len()))?;
        Some(Self {
            base: text.into(),
            anchor,
            images: vec![SentenceEchoImage {
                left: anchor,
                cursor,
                inserted: String::new(),
            }],
            opaque: None,
        })
    }

    fn matches(&self, image: &SentenceEchoImage, text: &str, cursor: usize) -> bool {
        cursor == image.cursor
            && text.len() == image.left + image.inserted.len() + self.base.len() - self.anchor
            && text.starts_with(&self.base[..image.left])
            && text[image.left..].starts_with(&image.inserted)
            && text.ends_with(&self.base[self.anchor..])
    }

    fn acknowledge(&mut self, text: &str, cursor: usize) -> bool {
        if let Some(index) = self
            .images
            .iter()
            .rposition(|image| self.matches(image, text, cursor))
        {
            self.images.drain(..index);
            return true;
        }
        if let Some(kind) = self.opaque {
            let last = self.images.last().expect("an echo always has a base image");
            let gap = text
                .strip_prefix(&self.base[..last.left])
                .and_then(|rest| rest.strip_prefix(last.inserted.as_str()))
                .and_then(|rest| rest.strip_suffix(&self.base[self.anchor..]));
            if let Some(gap) = gap {
                let compatible = !gap.is_empty()
                    && gap.chars().all(char::is_whitespace)
                    && match kind {
                        OpaqueSeparator::Return => gap.contains(['\r', '\n']),
                        OpaqueSeparator::Tab => true,
                    };
                if compatible && cursor == last.cursor + gap.chars().count() {
                    // The opaque key's actual result is now known. Retire all
                    // pre-barrier images and bind subsequent edits to this ACK.
                    *self = Self::new(text, cursor).expect("validated character cursor");
                    return true;
                }
            }
        }
        false
    }

    fn push(&mut self, image: SentenceEchoImage) -> bool {
        if self.images.last() == Some(&image) {
            return true;
        }
        if self.images.len() >= MAX_SENTENCE_ECHOES
            || self.images.iter().map(|i| i.inserted.len()).sum::<usize>() + image.inserted.len()
                > MAX_SENTENCE_ECHO_BYTES
        {
            return false;
        }
        self.images.push(image);
        true
    }

    fn edit(&mut self, delete: usize, insert: &str) -> bool {
        if self.opaque.is_some() || insert.len() > MAX_SENTENCE_ECHO_BYTES {
            return false;
        }
        let mut image = self.images.last().expect("base image").clone();
        for _ in 0..delete {
            if image.inserted.pop().is_none() {
                let Some((byte, _)) = self.base[..image.left].char_indices().next_back() else {
                    return false;
                };
                image.left = byte;
            }
            image.cursor -= 1;
        }
        // Standalone surrounding replacement really exposes the deletion
        // before its commit. Coalesced same-batch replacement never does.
        if delete > 0 && !self.push(image.clone()) {
            return false;
        }
        image.inserted.push_str(insert);
        image.cursor += insert.chars().count();
        self.push(image)
    }

    fn has_known_linebreak_or_blank_prefix(&self) -> bool {
        if self.opaque.is_some() {
            return false;
        }
        let image = self.images.last().expect("base image");
        let mut linebreak = false;
        let mut only_spaces = true;
        for ch in self.base[..image.left]
            .chars()
            .chain(image.inserted.chars())
            .rev()
        {
            if !ch.is_whitespace() {
                return linebreak;
            }
            linebreak |= matches!(ch, '\r' | '\n');
            only_spaces &= ch == ' ';
        }
        linebreak || only_spaces
    }

    fn before_cursor(&self) -> Option<Cow<'_, str>> {
        if self.opaque.is_some() {
            return None;
        }
        let image = self.images.last()?;
        let left = &self.base[..image.left];
        if image.inserted.is_empty() {
            return Some(Cow::Borrowed(left));
        }
        if left.is_empty() {
            return Some(Cow::Borrowed(image.inserted.as_str()));
        }
        let bytes = left.len().checked_add(image.inserted.len())?;
        if bytes > MAX_SENTENCE_ECHO_BYTES {
            return None;
        }
        let mut prefix = String::with_capacity(bytes);
        prefix.push_str(left);
        prefix.push_str(&image.inserted);
        Some(Cow::Owned(prefix))
    }

    fn record(&mut self, event: &KeyEvent, actions: &[Action], mut composing: bool) -> bool {
        // Match the IBus adapter's exact commit+same-length-replace folding.
        // In particular Commithello+ReplaceHello has no lowercase echo.
        let mut wire: Vec<Action> = Vec::new();
        let mut pending_commit: Option<usize> = None;
        for action in actions {
            if let Action::ReplaceWord { replace_len, text } = action {
                if let Some(index) = pending_commit {
                    if let Action::CommitText(previous) = &wire[index] {
                        if *replace_len == previous.chars().count() {
                            wire[index] = Action::CommitText(text.clone());
                            continue;
                        }
                    }
                }
            }
            if matches!(action, Action::CommitText(_)) {
                pending_commit = Some(wire.len());
            }
            wire.push(action.clone());
        }
        for (index, action) in wire.iter().enumerate() {
            match action {
                Action::CommitText(text) => {
                    if !self.edit(0, text) {
                        return false;
                    }
                    composing = false;
                }
                Action::ReplaceWord { replace_len, text } => {
                    if !self.edit(if composing { 0 } else { *replace_len }, text) {
                        return false;
                    }
                    composing = false;
                }
                Action::ShowComposing { .. } => composing = true,
                Action::ShowGhost(_) => composing = false,
                Action::HideGhost => {
                    if !wire[index + 1..]
                        .iter()
                        .any(|a| matches!(a, Action::CommitText(_) | Action::ReplaceWord { .. }))
                    {
                        composing = false;
                    }
                }
                Action::ForwardKey => {}
            }
        }
        if !actions.iter().any(|a| matches!(a, Action::ForwardKey)) || event.key.is_pure_modifier()
        {
            return true;
        }
        // Forward is delivered after the entire action batch. Raw letters do
        // not reveal the OS keyval; project only layout-identical glyphs.
        match event.key {
            Key::Return | Key::Tab => {
                if self.opaque.is_none() || matches!(event.key, Key::Return) {
                    self.opaque = Some(if matches!(event.key, Key::Return) {
                        OpaqueSeparator::Return
                    } else {
                        OpaqueSeparator::Tab
                    });
                }
                true
            }
            Key::Space if self.opaque.is_some() => true,
            Key::Space => self.edit(0, " "),
            Key::Backspace => self.edit(1, ""),
            Key::Char(ch) if !ch.is_control() => self.edit(0, ch.encode_utf8(&mut [0; 4])),
            Key::RawCode(code) => {
                let pair = crate::keymap::scancode_to_both_with_caps(
                    code,
                    event.modifiers.contains(Modifiers::SHIFT),
                    event.modifiers.contains(Modifiers::CAPS_LOCK),
                );
                if let Some((en, _)) = pair.filter(|(en, bg)| en == bg) {
                    self.edit(0, en.encode_utf8(&mut [0; 4]))
                } else {
                    false
                }
            }
            _ => false,
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
    /// Whether anticipation was already run for the current word.
    anticipated: bool,
    /// Cached surrounding text from the platform adapter (Linux IBus path).
    surrounding_text: Option<String>,
    /// Cursor position inside `surrounding_text` when provided by the platform.
    surrounding_cursor_pos: Option<usize>,
    /// Bounded presentation-only acknowledgements, never model context.
    sentence_echoes: Option<SentenceEchoes>,
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
            anticipated: false,
            surrounding_text: None,
            surrounding_cursor_pos: None,
            sentence_echoes: None,
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
        let event = event.normalize_special_key();
        // Key-up must preserve the attribution from its matching press.
        if event.modifiers.contains(Modifiers::RELEASE) {
            return self.core.handle_key(event);
        }
        // Forwarded shortcuts can edit client text. Invalidate attribution,
        // but do not start anticipation or count them as adaptation events.
        if !event.key.is_pure_modifier()
            && event.modifiers.intersects(Modifiers::CTRL | Modifiers::ALT)
        {
            self.sentence_echoes = None;
            self.last_prediction_accept = None;
            self.last_shown_ghost = None;
            return self.core.handle_key(event);
        }

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
        if self.core.current_word().is_empty() {
            let cached_analysis = self.cached_surrounding_analysis();
            let terminal_lang = self.terminal_context_language(cached_analysis.as_ref());
            self.core.set_terminal_context_lang(terminal_lang);
            if !self.anticipated {
                self.phase = Phase::Anticipating;
                let hints = self.anticipate(cached_analysis);
                self.core.apply_hints(&hints);
                self.anticipated = true;
                self.phase = Phase::Tracking;
            }
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
        // Capture prediction BEFORE delegation. Terminal Tab
        // and Right clear last_predictions/current_word inside the core, while
        // a partial Right keeps both alive and must not count as acceptance.
        let pre_accept_prediction = if is_accept_key && self.core.has_ghost() {
            self.core.predictions().first().map(|p| p.word.clone())
        } else {
            None
        };
        let hint_was_live = self.core.has_sentence_boundary_hint();
        let was_composing = self.core.debug_state().0;
        let word_was_empty = self.core.current_word().is_empty();
        let mut actions = self.core.handle_key(event.clone());
        let commit_learning = self.core.take_commit_learning();
        let word_committed = !commit_learning.words.is_empty();
        self.apply_correction_override(&mut actions);
        self.enforce_rejection_suppression(&mut actions);
        if !self.core.current_word().is_empty()
            && actions.iter().any(|a| matches!(a, Action::CommitText(_)))
            && actions
                .iter()
                .any(|a| matches!(a, Action::ForwardKey | Action::HideGhost))
        {
            // Forwarded handoffs and consumed delivery can both remove the
            // displayed ghost. Retire its old identity before further edits.
            self.last_shown_ghost = None;
        }
        let terminal_accept =
            pre_accept_prediction.is_some() && commit_learning.accepted_prediction;

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
        if word_committed {
            self.phase = Phase::Learning;
            self.learn_from_commit(commit_learning);
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
        self.record_sentence_delivery(
            &event,
            &actions,
            hint_was_live,
            was_composing,
            word_was_empty,
            word_committed,
        );

        actions
    }

    // ======================================================================
    // Delegated API (1:1 mirror of InputMethodCore)
    // ======================================================================

    pub fn set_external_rewrite_supported(&mut self, supported: bool) {
        self.core.set_external_rewrite_supported(supported);
    }

    pub fn focus_lost(&mut self) -> Vec<Action> {
        self.sentence_echoes = None;
        self.anticipated = false;
        self.core.clear_hints();
        self.frustration.reset_word();
        self.surrounding_text = None;
        self.surrounding_cursor_pos = None;
        self.last_shown_ghost = None;
        let actions = self.core.focus_lost();
        let commit_learning = self.core.take_commit_learning();
        self.learn_from_commit(commit_learning);
        actions
    }

    pub fn focus_gained(&mut self) {
        self.sentence_echoes = None;
        self.core.focus_gained();
    }

    pub fn reset(&mut self) -> Vec<Action> {
        self.sentence_echoes = None;
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
        self.save_personal(&crate::paths::personal_profile_path())
    }

    pub fn load_personal_default(&mut self) -> Result<(), String> {
        self.load_personal(&crate::paths::personal_profile_path())
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
        self.surrounding_text = text;
        self.surrounding_cursor_pos = cursor_pos;
        self.reconcile_sentence_report();
        let analysis = self.cached_surrounding_analysis();
        let terminal_lang = self.terminal_context_language(analysis.as_ref());
        self.core.set_terminal_context_lang(terminal_lang);
    }

    fn reconcile_sentence_report(&mut self) {
        if self.sentence_echoes.is_none() && !self.core.has_sentence_boundary_hint() {
            return;
        }
        let Some((text, cursor)) = self
            .surrounding_text
            .as_deref()
            .zip(self.surrounding_cursor_pos)
            .filter(|(text, cursor)| *cursor <= text.chars().count())
        else {
            // Unavailable geometry supplies no authority, including no means
            // to lift an already established negative presentation decision.
            return;
        };
        let Some(echoes) = self.sentence_echoes.as_mut() else {
            // An unbound first report cannot be assumed to echo an empty
            // document. Withdraw presentation only; retain the active token.
            self.core.withdraw_sentence_boundary_hint();
            return;
        };
        if !echoes.acknowledge(text, cursor) {
            let byte = text
                .char_indices()
                .nth(cursor)
                .map(|(byte, _)| byte)
                .unwrap_or(text.len());
            self.core.reconcile_sentence_boundary_hint(&text[..byte]);
            // The lease already existed. A fresh valid report can replace
            // its anchor even when ordinary text removed the local hint.
            self.sentence_echoes = SentenceEchoes::new(text, cursor);
        }
    }

    fn record_sentence_delivery(
        &mut self,
        event: &KeyEvent,
        actions: &[Action],
        hint_was_live: bool,
        was_composing: bool,
        word_was_empty: bool,
        word_committed: bool,
    ) {
        let forwarded_backspace = matches!(event.key, Key::Backspace)
            && actions.iter().any(|a| matches!(a, Action::ForwardKey));
        if self.core.is_enabled()
            && forwarded_backspace
            && word_was_empty
            && self.core.current_word().is_empty()
            && !word_committed
            && !actions.iter().any(|a| matches!(a, Action::CommitText(_)))
            && self
                .sentence_echoes
                .as_ref()
                .is_some_and(SentenceEchoes::has_known_linebreak_or_blank_prefix)
        {
            // The generic Core hint cannot retract opaque layout whitespace.
            // This existing exact, nonopaque image can: project the final wire
            // batch once, then classify the actual remaining client prefix.
            let echoes = self.sentence_echoes.as_mut().expect("checked bound image");
            let retained = if echoes.record(event, actions, was_composing) {
                if let Some(prefix) = echoes.before_cursor() {
                    self.core.reconcile_sentence_boundary_hint(&prefix);
                    self.core.has_sentence_boundary_hint()
                } else {
                    false
                }
            } else {
                false
            };
            if !retained {
                self.core.withdraw_sentence_boundary_hint();
                self.sentence_echoes = None;
            }
            return;
        }
        let hint_is_live = self.core.has_sentence_boundary_hint();
        if !self.core.is_enabled()
            || (!hint_is_live
                && (self.sentence_echoes.is_none() || word_committed || forwarded_backspace))
        {
            self.sentence_echoes = None;
            return;
        }
        if !hint_was_live && hint_is_live && self.sentence_echoes.is_none() {
            self.sentence_echoes = self
                .surrounding_text
                .as_deref()
                .zip(self.surrounding_cursor_pos)
                .and_then(|(text, cursor)| SentenceEchoes::new(text, cursor));
        }
        if self
            .sentence_echoes
            .as_mut()
            .is_some_and(|echoes| !echoes.record(event, actions, was_composing))
        {
            self.core.withdraw_sentence_boundary_hint();
            self.sentence_echoes = None;
        }
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

    fn anticipate(&mut self, cached_analysis: Option<ContextAnalysis>) -> Hints {
        let mut hints = Hints::default();

        // 1. Sample surrounding text for context.
        if let Some(analysis) = cached_analysis.or_else(|| {
            self.context_sampler
                .get_surrounding_text(100)
                .map(|text| context_sampler::analyze_surrounding(&text))
        }) {
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

    fn cached_surrounding_analysis(&self) -> Option<ContextAnalysis> {
        self.cached_surrounding_excerpt(100)
            .map(|text| context_sampler::analyze_surrounding(&text))
    }

    fn terminal_context_language(&self, analysis: Option<&ContextAnalysis>) -> Option<LangId> {
        let text = self.surrounding_text.as_deref()?;
        let cursor = self.surrounding_cursor_pos?;
        // Legacy live hints accept absent/byte-like cursors. Terminal routing
        // requires an actual client character position and its pre-cursor text.
        if cursor > text.chars().count() {
            return None;
        }
        analysis?
            .lang
            .filter(|lang| matches!(lang, LangId::En | LangId::Bg))
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

    fn learn_from_commit(&mut self, batch: crate::input::CommitLearningBatch) {
        if batch.accepted_prediction {
            self.light_profile.record_acceptance();
        }
        for word in batch.words {
            if let Some(language) = word.language {
                self.light_profile.observe_lang(language);
            }
        }
        self.last_accepted_prediction = None;
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

#[cfg(test)]
mod review_tech_prior_tests {
    use super::*;
    use crate::input::Key;

    fn press(key: Key) -> KeyEvent {
        KeyEvent {
            key,
            modifiers: Modifiers::empty(),
        }
    }

    fn assert_tech_prior_once(keys: impl IntoIterator<Item = KeyEvent>, boundary: KeyEvent) {
        let mut ml = MasterLoop::new(InputConfig {
            ghost_text: false,
            ..InputConfig::default()
        });
        for key in keys {
            ml.handle_key(key);
        }
        assert_eq!(ml.detected_language().lang, LangId::Tech);
        assert!((ml.detected_language().confidence - 0.9).abs() < 1e-12);
        assert_eq!(ml.light_profile().lang_prior, [0.33, 0.34, 0.33]);
        ml.handle_key(boundary);
        let observed = ml.light_profile().lang_prior;
        for (actual, expected) in observed.iter().zip([0.3234, 0.3332, 0.3434]) {
            assert!(
                (actual - expected).abs() < 1e-12,
                "one direct Tech word must update its prior: {observed:?}"
            );
        }
        assert_eq!(ml.light_profile().ghost_accept_rate, 0.5);
        assert_eq!(ml.metrics().summary().accepted_predictions, 0);
        ml.handle_key(press(Key::Space));
        assert_eq!(ml.light_profile().lang_prior, observed);
    }

    #[test]
    fn review_tech_direct_symbols_update_prior_without_acceptance() {
        assert_tech_prior_once(
            "___".chars().map(|ch| press(Key::Char(ch))),
            press(Key::Space),
        );
    }

    #[test]
    fn review_tech_direct_digits_update_prior_on_return() {
        assert_tech_prior_once(
            "123".chars().map(|ch| press(Key::Char(ch))),
            press(Key::Return),
        );
    }

    #[test]
    fn review_tech_shared_shift_key_updates_prior_without_acceptance() {
        assert_tech_prior_once(
            (0..3).map(|_| KeyEvent {
                key: Key::RawCode(12),
                modifiers: Modifiers::SHIFT,
            }),
            press(Key::RawCode(57)),
        );
    }
}

#[cfg(test)]
mod retraction_prior_tests {
    use super::*;
    use crate::input::Key;

    fn press(key: Key) -> KeyEvent {
        KeyEvent {
            key,
            modifiers: Modifiers::empty(),
        }
    }

    fn assert_retracted_tech_prior(keys: impl IntoIterator<Item = Key>, boundary: Key) {
        let mut config = InputConfig {
            ghost_text: false,
            ..InputConfig::default()
        };
        config.dual_buffer.enabled = false;
        let mut ml = MasterLoop::new(config);
        for key in keys {
            ml.handle_key(press(key));
        }
        ml.handle_key(press(boundary));
        let priors = ml.light_profile().lang_prior;
        for (actual, expected) in priors.iter().zip([0.3234, 0.3332, 0.3434]) {
            assert!(
                (actual - expected).abs() < 1e-12,
                "one surviving direct Tech token must update its prior: {priors:?}"
            );
        }
        assert_eq!(ml.light_profile().ghost_accept_rate, 0.5);
        assert_eq!(ml.metrics().summary().accepted_predictions, 0);
        ml.handle_key(press(Key::Space));
        assert_eq!(ml.light_profile().lang_prior, priors);
    }

    #[test]
    fn retraction_deleted_inferred_raw_suffix_restores_tech_prior() {
        assert_retracted_tech_prior(
            [
                Key::Char('_'),
                Key::RawCode(26),
                Key::Backspace,
                Key::Char('_'),
                Key::Char('_'),
            ],
            Key::Space,
        );
    }

    #[test]
    fn retraction_deleted_script_restores_tech_prior() {
        assert_retracted_tech_prior(
            "123a".chars().map(Key::Char).chain([Key::Backspace]),
            Key::Return,
        );
    }

    #[test]
    fn retraction_deleted_entire_prefix_does_not_suppress_tech_prior() {
        assert_retracted_tech_prior(
            [
                Key::Char('a'),
                Key::Backspace,
                Key::Char('1'),
                Key::Char('2'),
                Key::Char('3'),
            ],
            Key::Space,
        );
    }
}

#[cfg(test)]
mod commit_language_receipt_contract_tests {
    use super::*;
    use crate::input::Key;

    fn press(key: Key) -> KeyEvent {
        KeyEvent {
            key,
            modifiers: Modifiers::empty(),
        }
    }

    // Break caught: focus_lost commits in core without synchronously consuming
    // its language evidence, then a later key learns the old commit.
    #[test]
    fn focus_lost_learns_language_immediately_and_only_once() {
        let mut ml = MasterLoop::new(InputConfig {
            ghost_text: false,
            ..InputConfig::default()
        });
        ml.load_word("мама", 1_000_000);
        for code in [50, 30, 50, 30] {
            ml.handle_key(press(Key::RawCode(code)));
        }
        assert_eq!(ml.current_word(), "мама");
        let mut expected = ml.light_profile.clone();
        expected.observe_lang(LangId::Bg);
        let before_commits = ml.metrics().commit_count();
        ml.focus_lost();
        assert_eq!(ml.metrics().commit_count(), before_commits + 1);
        assert_eq!(
            ml.light_profile.lang_prior, expected.lang_prior,
            "focus_lost must consume its own word evidence before returning"
        );
        ml.handle_key(press(Key::Space));
        ml.focus_lost();
        assert_eq!(
            ml.light_profile.lang_prior, expected.lang_prior,
            "empty boundaries must not relearn the old commit"
        );
    }

    // Break caught: using one stale prefix-detector language for all words in
    // one fully accepted completion; accepting once must still update UX once.
    #[test]
    fn multiword_tab_accept_observes_each_words_language_once() {
        let mut ml = MasterLoop::new(InputConfig {
            ghost_text_separation_margin: 0.0,
            ..InputConfig::default()
        });
        ml.load_word("hello", 1_000_000);
        ml.load_word("здравей", 1_000_000);
        for ch in "hel".chars() {
            ml.handle_key(press(Key::Char(ch)));
        }
        assert!(ml.core.apply_prediction_override("hello здравей").is_some());
        let mut expected = ml.light_profile.clone();
        expected.record_accept(LangId::En);
        expected.observe_lang(LangId::Bg);
        ml.handle_key(press(Key::Tab));
        assert!(ml.current_word().is_empty());
        assert_eq!(ml.metrics().summary().accepted_predictions, 1);
        assert_eq!(
            ml.light_profile.ghost_accept_rate,
            expected.ghost_accept_rate
        );
        assert_eq!(
            ml.light_profile.lang_prior, expected.lang_prior,
            "accepted words must not all inherit the prefix detector language"
        );
        ml.handle_key(press(Key::Space));
        assert_eq!(ml.light_profile.lang_prior, expected.lang_prior);
    }

    #[test]
    fn unsupported_delimiter_does_not_reinforce_inherited_language_prior() {
        let mut ml = MasterLoop::new(InputConfig {
            ghost_text: false,
            ..InputConfig::default()
        });
        ml.light_profile.lang_prior = [0.80, 0.10, 0.10];
        for code in [16, 45] {
            ml.handle_key(press(Key::RawCode(code)));
        }
        assert!(!ml.current_word().is_empty());
        let before = ml.light_profile.clone();
        ml.handle_key(press(Key::Space));
        assert_eq!(ml.metrics().commit_count(), 1);
        assert_eq!(
            ml.light_profile.lang_prior, before.lang_prior,
            "an inherited unsupported winner is not new language evidence"
        );
        assert_eq!(ml.light_profile.ghost_accept_rate, before.ghost_accept_rate);
    }

    #[test]
    fn accepted_mixed_script_has_positive_feedback_without_a_language_vote() {
        let mut ml = MasterLoop::new(InputConfig::default());
        ml.handle_key(press(Key::Char('h')));
        assert!(ml.core.apply_prediction_override("hеllo").is_some());
        let before = ml.light_profile.clone();
        ml.handle_key(press(Key::Tab));
        assert_eq!(ml.metrics().summary().accepted_predictions, 1);
        assert_eq!(ml.light_profile.lang_prior, before.lang_prior);
        assert_eq!(
            ml.light_profile.ghost_accept_rate,
            0.95 * before.ghost_accept_rate + 0.05
        );
        let accepted_prior = ml.light_profile.lang_prior;
        let accepted_rate = ml.light_profile.ghost_accept_rate;
        ml.handle_key(press(Key::Space));
        ml.focus_lost();
        assert_eq!(ml.light_profile.lang_prior, accepted_prior);
        assert_eq!(ml.light_profile.ghost_accept_rate, accepted_rate);
    }
}

#[cfg(test)]
mod shortword_surrounding_routing_tests {
    use super::*;
    use crate::input::{Action, Key};

    fn raw(code: u16) -> KeyEvent {
        KeyEvent {
            key: Key::RawCode(code),
            modifiers: Modifiers::empty(),
        }
    }

    fn reduced(context: &str) -> MasterLoop {
        let mut ml = MasterLoop::new(InputConfig::default());
        ml.core.load_word_lang("like", 2_570_395, LangId::En);
        ml.core.load_word_lang("li", 17_782, LangId::En);
        ml.core.load_word_lang("ли", 2_511_886, LangId::Bg);
        ml.set_surrounding_text(Some(context.to_string()), Some(context.chars().count()));
        ml
    }

    #[test]
    fn shortword_recorded_bg_intent_survives_surrounding_hint_to_literal_boundary() {
        // Public synthetic BG context isolates the same actual surrounding-text
        // hint path; the recorded private literal stays in the frozen packet.
        for boundary in [57, 28] {
            let mut ml = reduced("има ");
            ml.handle_key(raw(38));
            let actions = ml.handle_key(raw(23));
            assert_eq!(ml.current_word(), "ли");
            assert!(actions.iter().any(
                |action| matches!(action, Action::ShowComposing { typed, .. } if typed == "ли")
            ));
            assert_eq!(ml.core.debug_state(), (true, false, true));
            let actions = ml.handle_key(raw(boundary));
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
            assert_eq!(ml.metrics().summary().accepted_predictions, 0);
            assert_eq!(ml.light_profile().ghost_accept_rate, 0.5);
            assert!((ml.light_profile().lang_prior[0] - 0.3434).abs() < 1e-12);
            let prior = ml.light_profile().lang_prior;
            ml.handle_key(raw(57));
            assert_eq!(
                ml.light_profile().lang_prior,
                prior,
                "one committed word supplies one independent observation"
            );
        }
    }

    #[test]
    fn shortword_english_and_mixed_context_do_not_blanket_select_bg() {
        for context in ["hello ", "има hello "] {
            let mut ml = reduced(context);
            ml.handle_key(raw(38));
            ml.handle_key(raw(23));
            assert_eq!(ml.current_word(), "li");
            let actions = ml.handle_key(raw(57));
            assert!(actions
                .iter()
                .any(|action| matches!(action, Action::CommitText(word) if word == "li")));
        }
    }
}

#[cfg(test)]
mod terminal_context_contract_tests {
    use super::*;
    use crate::input::{Action, Key};

    const BG_CONTEXT: &str = "Искам да благодаря ";

    fn press(key: Key) -> KeyEvent {
        KeyEvent {
            key,
            modifiers: Modifiers::empty(),
        }
    }

    fn raw(code: u16) -> KeyEvent {
        press(Key::RawCode(code))
    }

    fn context(ml: &mut MasterLoop, text: Option<&str>) {
        ml.set_surrounding_text(text.map(str::to_string), text.map(|s| s.chars().count()));
    }

    fn reduced(text: Option<&str>) -> MasterLoop {
        let mut ml = MasterLoop::new(InputConfig::default());
        // Public reduced fixtures distinguish completed words from a longer
        // prefix candidate. They do not stand in for the copied-corpus replay.
        ml.load_word_lang("with", 7_079_457, LangId::En);
        ml.load_word_lang("wi", 12_302, LangId::En);
        ml.load_word_lang("ви", 2_137_962, LangId::Bg);
        context(&mut ml, text);
        ml
    }

    fn type_wi(ml: &mut MasterLoop) {
        ml.handle_key(raw(17));
        ml.handle_key(raw(23));
        assert_eq!(
            ml.current_word(),
            "wi",
            "prefix selection remains unchanged"
        );
        assert_eq!(ml.core.debug_state(), (true, false, true));
    }

    fn boundary(ml: &mut MasterLoop, key: Key, expected: &str) {
        let before = ml.metrics().summary();
        let actions = ml.handle_key(press(key));
        let commits: Vec<&str> = actions
            .iter()
            .filter_map(|action| match action {
                Action::CommitText(word) => Some(word.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(commits, vec![expected], "{actions:?}");
        assert_eq!(
            actions
                .iter()
                .filter(|a| matches!(a, Action::ForwardKey))
                .count(),
            1,
            "{actions:?}"
        );
        assert!(!actions
            .iter()
            .any(|a| matches!(a, Action::ReplaceWord { .. })));
        assert_eq!(ml.current_word(), "");
        assert_eq!(
            ml.metrics().summary().total_commits,
            before.total_commits + 1
        );
        assert_eq!(
            ml.metrics().summary().accepted_predictions,
            before.accepted_predictions
        );
    }

    #[test]
    fn terminal_context_bg_exact_word_wins_at_space_without_accepting_ghost() {
        let mut ml = reduced(Some(BG_CONTEXT));
        type_wi(&mut ml);
        assert_eq!(ml.ghost_text(), "th");
        boundary(&mut ml, Key::RawCode(57), "ви");
        assert_eq!(ml.light_profile().ghost_accept_rate, 0.5);
        assert!((ml.light_profile().lang_prior[0] - 0.3434).abs() < 1e-12);
        assert!(ml.core.export_personal_profile().confirmed_words.is_empty());
        let prior = ml.light_profile().lang_prior;
        let commits = ml.metrics().summary().total_commits;
        ml.handle_key(raw(57));
        ml.focus_lost();
        assert_eq!(
            ml.light_profile().lang_prior,
            prior,
            "one language observation only"
        );
        assert_eq!(ml.metrics().summary().total_commits, commits);
    }

    #[test]
    fn terminal_context_bg_exact_word_wins_at_return() {
        let mut ml = reduced(Some(BG_CONTEXT));
        type_wi(&mut ml);
        boundary(&mut ml, Key::RawCode(28), "ви");
    }

    #[test]
    fn terminal_context_bg_exact_word_wins_at_punctuation() {
        let mut ml = reduced(Some(BG_CONTEXT));
        type_wi(&mut ml);
        boundary(&mut ml, Key::Char(','), "ви");
    }

    #[test]
    fn terminal_context_unchanged_bg_setter_keeps_current_authority() {
        let mut ml = reduced(Some(BG_CONTEXT));
        type_wi(&mut ml);
        context(&mut ml, Some(BG_CONTEXT));
        boundary(&mut ml, Key::Space, "ви");
    }

    #[test]
    fn terminal_context_withdrawn_midword_cannot_use_stale_bg_authority() {
        let mut ml = reduced(Some(BG_CONTEXT));
        type_wi(&mut ml);
        context(&mut ml, None);
        boundary(&mut ml, Key::Space, "wi");
    }

    #[test]
    fn terminal_context_changed_to_en_midword_cannot_use_stale_bg_authority() {
        let mut ml = reduced(Some(BG_CONTEXT));
        type_wi(&mut ml);
        context(&mut ml, Some("I want to thank "));
        boundary(&mut ml, Key::Space, "wi");
    }

    #[test]
    fn terminal_context_erase_to_empty_withdraw_and_retype_cannot_reuse_authority() {
        let mut ml = reduced(Some(BG_CONTEXT));
        type_wi(&mut ml);
        ml.handle_key(raw(14));
        ml.handle_key(raw(14));
        assert_eq!(ml.current_word(), "");
        context(&mut ml, None);
        type_wi(&mut ml);
        boundary(&mut ml, Key::Space, "wi");
    }

    #[test]
    fn terminal_context_absent_opposed_and_tech_context_keep_display() {
        for text in [None, Some("I want to thank "), Some("_!!! ")] {
            let mut ml = reduced(text);
            type_wi(&mut ml);
            boundary(&mut ml, Key::Space, "wi");
        }
    }

    #[test]
    fn terminal_context_profile_prior_alone_is_not_surrounding_proof() {
        let mut ml = reduced(None);
        ml.light_profile.lang_prior = [0.8, 0.1, 0.1];
        type_wi(&mut ml);
        boundary(&mut ml, Key::Space, "wi");
    }

    #[test]
    fn terminal_context_committed_momentum_alone_is_not_surrounding_proof() {
        let mut ml = reduced(None);
        for ch in "име".chars() {
            ml.handle_key(press(Key::Char(ch)));
        }
        ml.handle_key(raw(57));
        type_wi(&mut ml);
        boundary(&mut ml, Key::Space, "wi");
    }

    #[test]
    fn terminal_context_exact_tie_keeps_display() {
        let mut ml = reduced(Some(BG_CONTEXT));
        ml.load_word_lang("wi", 2_137_962, LangId::En);
        type_wi(&mut ml);
        boundary(&mut ml, Key::Space, "wi");
    }

    #[test]
    fn terminal_context_symmetric_english_exact_word_can_win() {
        let mut ml = MasterLoop::new(InputConfig::default());
        ml.load_word_lang("wi", 2_137_962, LangId::En);
        ml.load_word_lang("вина", 7_079_457, LangId::Bg);
        ml.load_word_lang("ви", 12_302, LangId::Bg);
        context(&mut ml, Some("I want to thank "));
        ml.handle_key(raw(17));
        ml.handle_key(raw(23));
        assert_eq!(ml.current_word(), "ви");
        assert_eq!(ml.core.debug_state(), (true, false, true));
        boundary(&mut ml, Key::Space, "wi");
    }

    #[test]
    fn terminal_context_strong_exact_english_survives_bg_context() {
        let mut ml = MasterLoop::new(InputConfig::default());
        ml.load_word_lang("he", 1_000_000, LangId::En);
        ml.load_word_lang("хе", 1, LangId::Bg);
        context(&mut ml, Some(BG_CONTEXT));
        ml.handle_key(raw(35));
        ml.handle_key(raw(18));
        assert_eq!(ml.current_word(), "he");
        boundary(&mut ml, Key::Space, "he");
    }

    #[test]
    fn terminal_context_existing_li_absent_en_and_mixed_guards_survive() {
        for text in [None, Some("hello "), Some("има hello ")] {
            let mut ml = MasterLoop::new(InputConfig::default());
            ml.load_word_lang("like", 2_570_395, LangId::En);
            ml.load_word_lang("li", 17_782, LangId::En);
            ml.load_word_lang("ли", 2_511_886, LangId::Bg);
            context(&mut ml, text);
            ml.handle_key(raw(38));
            ml.handle_key(raw(23));
            assert_eq!(ml.current_word(), "li");
            boundary(&mut ml, Key::Space, "li");
        }
    }

    #[test]
    fn terminal_context_casing_uses_original_selected_buffer() {
        for (modifiers, expected) in [(Modifiers::SHIFT, "Ви"), (Modifiers::CAPS_LOCK, "ВИ")] {
            let mut ml = reduced(Some(BG_CONTEXT));
            ml.handle_key(KeyEvent {
                key: Key::RawCode(17),
                modifiers,
            });
            ml.handle_key(KeyEvent {
                key: Key::RawCode(23),
                modifiers: if modifiers == Modifiers::SHIFT {
                    Modifiers::empty()
                } else {
                    modifiers
                },
            });
            assert_eq!(ml.core.debug_state(), (true, false, true));
            boundary(&mut ml, Key::Space, expected);
        }
    }

    #[test]
    fn terminal_context_direct_semantic_chars_stay_literal() {
        let mut ml = reduced(Some(BG_CONTEXT));
        for ch in "wi".chars() {
            ml.handle_key(press(Key::Char(ch)));
        }
        assert_eq!(ml.current_word(), "wi");
        assert!(!ml.core.debug_state().0);
        let actions = ml.handle_key(raw(57));
        assert!(!actions
            .iter()
            .any(|a| matches!(a, Action::CommitText(_) | Action::ReplaceWord { .. })));
        assert_eq!(
            actions
                .iter()
                .filter(|a| matches!(a, Action::ForwardKey))
                .count(),
            1
        );
        assert_eq!(ml.metrics().summary().accepted_predictions, 0);
    }

    #[test]
    fn terminal_context_explicit_full_tab_accepts_visible_completion() {
        let mut ml = reduced(Some(BG_CONTEXT));
        type_wi(&mut ml);
        assert_eq!(ml.ghost_text(), "th");
        let actions = ml.handle_key(press(Key::Tab));
        assert_eq!(
            actions
                .iter()
                .filter_map(|a| match a {
                    Action::CommitText(word) => Some(word.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            vec!["with"]
        );
        assert!(!actions
            .iter()
            .any(|a| matches!(a, Action::ForwardKey | Action::ReplaceWord { .. })));
        assert_eq!(ml.metrics().summary().accepted_predictions, 1);
        let commits = ml.metrics().summary().total_commits;
        ml.handle_key(raw(57));
        assert_eq!(ml.metrics().summary().total_commits, commits);
    }

    #[test]
    fn terminal_context_partial_right_keeps_accepted_prefix_locked() {
        let mut ml = reduced(Some(BG_CONTEXT));
        ml.load_word_lang("wit", 12_302, LangId::En);
        ml.load_word_lang("вит", 2_137_962, LangId::Bg);
        type_wi(&mut ml);
        ml.handle_key(press(Key::Right));
        assert_eq!(ml.current_word(), "wit");
        assert_eq!(ml.core.debug_state(), (true, true, false));
        boundary(&mut ml, Key::Space, "wit");
        assert!(ml.core.export_personal_profile().confirmed_words.is_empty());
    }

    #[test]
    fn terminal_context_locked_both_supported_keeps_existing_decision() {
        let mut ml = MasterLoop::new(InputConfig::default());
        ml.load_word_lang("withers", 90_000_000, LangId::En);
        ml.load_word_lang("with", 1, LangId::En);
        ml.load_word_lang("витх", 2_137_962, LangId::Bg);
        context(&mut ml, Some(BG_CONTEXT));
        for code in [17, 23, 20, 35] {
            ml.handle_key(raw(code));
        }
        assert_eq!(ml.current_word(), "with");
        assert_eq!(ml.core.debug_state(), (true, true, false));
        boundary(&mut ml, Key::Space, "with");
    }
}

#[cfg(test)]
mod terminal_context_source_quality_tests {
    use super::*;
    use crate::input::{Action, Key};

    const BG_CONTEXT: &str = "Искам да благодаря ";

    struct BgSampler;

    impl ContextSampler for BgSampler {
        fn get_surrounding_text(&self, _max_chars: usize) -> Option<String> {
            Some(BG_CONTEXT.to_string())
        }
    }

    fn fixture(ml: &mut MasterLoop) {
        ml.load_word_lang("with", 7_079_457, LangId::En);
        ml.load_word_lang("wi", 12_302, LangId::En);
        ml.load_word_lang("ви", 2_137_962, LangId::Bg);
    }

    fn raw(code: u16) -> KeyEvent {
        KeyEvent {
            key: Key::RawCode(code),
            modifiers: Modifiers::empty(),
        }
    }

    fn assert_no_terminal_crossover(ml: &mut MasterLoop) {
        ml.handle_key(raw(17));
        ml.handle_key(raw(23));
        assert_eq!(ml.current_word(), "wi");
        let actions = ml.handle_key(raw(57));
        assert_eq!(
            actions
                .iter()
                .filter_map(|a| match a {
                    Action::CommitText(word) => Some(word.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            vec!["wi"]
        );
        assert_eq!(
            actions
                .iter()
                .filter(|a| matches!(a, Action::ForwardKey))
                .count(),
            1
        );
        assert!(!actions
            .iter()
            .any(|a| matches!(a, Action::ReplaceWord { .. })));
    }

    #[test]
    fn terminal_context_sampler_return_without_client_cache_is_not_authority() {
        let mut ml = MasterLoop::with_context_sampler(InputConfig::default(), Box::new(BgSampler));
        fixture(&mut ml);
        ml.handle_key(raw(17));
        assert_eq!(
            ml.current_word(),
            "в",
            "nonempty BG sampler still supplies the existing live hint"
        );
        ml.handle_key(raw(23));
        assert_eq!(ml.current_word(), "wi");
        let actions = ml.handle_key(raw(57));
        assert_eq!(
            actions
                .iter()
                .filter_map(|a| match a {
                    Action::CommitText(word) => Some(word.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            vec!["wi"]
        );
        assert_eq!(
            actions
                .iter()
                .filter(|a| matches!(a, Action::ForwardKey))
                .count(),
            1
        );
        assert!(!actions
            .iter()
            .any(|a| matches!(a, Action::ReplaceWord { .. })));
    }

    #[test]
    fn terminal_context_unknown_or_out_of_range_cursor_is_not_authority() {
        for cursor in [None, Some(BG_CONTEXT.chars().count() + 1), Some(usize::MAX)] {
            let mut ml = MasterLoop::new(InputConfig::default());
            fixture(&mut ml);
            ml.set_surrounding_text(Some(BG_CONTEXT.to_string()), cursor);
            assert_no_terminal_crossover(&mut ml);
        }
    }

    #[test]
    fn terminal_context_erase_retype_reseeds_still_valid_client_cache() {
        let mut ml = MasterLoop::new(InputConfig::default());
        fixture(&mut ml);
        ml.set_surrounding_text(
            Some(BG_CONTEXT.to_string()),
            Some(BG_CONTEXT.chars().count()),
        );
        ml.handle_key(raw(17));
        ml.handle_key(raw(23));
        ml.handle_key(raw(14));
        ml.handle_key(raw(14));
        assert_eq!(ml.current_word(), "");
        ml.handle_key(raw(17));
        ml.handle_key(raw(23));
        assert_eq!(ml.current_word(), "wi");
        let actions = ml.handle_key(raw(57));
        assert_eq!(
            actions
                .iter()
                .filter_map(|a| match a {
                    Action::CommitText(word) => Some(word.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            vec!["ви"]
        );
    }

    #[test]
    fn terminal_context_uses_only_known_pre_cursor_client_text() {
        for (text, cursor, expected) in [
            (
                format!("hello {BG_CONTEXT}"),
                "hello ".chars().count(),
                "wi",
            ),
            (
                format!("{BG_CONTEXT}hello "),
                BG_CONTEXT.chars().count(),
                "ви",
            ),
            (BG_CONTEXT.to_string(), 0, "wi"),
        ] {
            let mut ml = MasterLoop::new(InputConfig::default());
            fixture(&mut ml);
            ml.set_surrounding_text(Some(text), Some(cursor));
            ml.handle_key(raw(17));
            ml.handle_key(raw(23));
            assert_eq!(ml.current_word(), "wi");
            let actions = ml.handle_key(raw(57));
            assert_eq!(
                actions
                    .iter()
                    .filter_map(|a| match a {
                        Action::CommitText(word) => Some(word.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
                vec![expected]
            );
        }
    }
}

#[cfg(test)]
mod terminal_alignment_contract_tests {
    use super::*;
    use crate::input::{Action, Key};

    fn raw(code: u16) -> KeyEvent {
        KeyEvent {
            key: Key::RawCode(code),
            modifiers: Modifiers::empty(),
        }
    }

    fn step(ml: &mut MasterLoop, key: Key, text: &mut String, preedit: &mut String) {
        let actions = ml.handle_key(KeyEvent {
            key: key.clone(),
            modifiers: Modifiers::empty(),
        });
        super::hybrid_delivery_contract_tests::project(&actions, &key, text, preedit);
    }

    fn assert_full_token(
        ml: &mut MasterLoop,
        expected: &str,
        text: &mut String,
        preedit: &mut String,
    ) {
        assert_eq!(ml.current_word(), expected);
        assert_eq!(
            text, expected,
            "handoff delivered the prefix and semantic suffix once"
        );
        assert!(preedit.is_empty());
        assert_eq!(ml.metrics().summary().total_commits, 0);
        let actions = ml.handle_key(raw(57));
        super::hybrid_delivery_contract_tests::project(&actions, &Key::RawCode(57), text, preedit);
        assert_eq!(text, &format!("{expected} "));
        assert_eq!(
            actions
                .iter()
                .filter_map(|a| match a {
                    Action::CommitText(word) => Some(word.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            Vec::<&str>::new(),
            "{actions:?}"
        );
        assert_eq!(
            actions
                .iter()
                .filter(|a| matches!(a, Action::ForwardKey))
                .count(),
            1
        );
        assert!(!actions
            .iter()
            .any(|a| matches!(a, Action::ReplaceWord { .. })));
        assert_eq!(ml.current_word(), "");
        assert_eq!(ml.metrics().summary().accepted_predictions, 0);
        assert_eq!(ml.metrics().summary().total_commits, 1);
        assert!(ml
            .core
            .export_personal_profile()
            .cvm
            .words
            .iter()
            .any(|word| word == expected));
    }

    #[test]
    fn terminal_alignment_both_supported_cannot_discard_direct_semantic_suffix() {
        let mut ml = MasterLoop::new(InputConfig::default());
        ml.load_word_lang("with", 7_079_457, LangId::En);
        ml.load_word_lang("wi", 12_302, LangId::En);
        ml.load_word_lang("ви", 2_137_962, LangId::Bg);
        let text = "Искам да благодаря ";
        ml.set_surrounding_text(Some(text.to_string()), Some(text.chars().count()));
        let (mut text, mut preedit) = (String::new(), String::new());
        step(&mut ml, Key::RawCode(17), &mut text, &mut preedit);
        step(&mut ml, Key::RawCode(23), &mut text, &mut preedit);
        assert_eq!(ml.current_word(), "wi");
        step(&mut ml, Key::Char('x'), &mut text, &mut preedit);
        assert_eq!(ml.current_word(), "wix");
        assert_full_token(&mut ml, "wix", &mut text, &mut preedit);
    }

    #[test]
    fn terminal_alignment_one_sided_cannot_discard_direct_semantic_suffix() {
        let mut ml = MasterLoop::new(InputConfig::default());
        for (word, frequency) in [
            ("status", 900_000),
            ("static", 600_000),
            ("station", 500_000),
        ] {
            ml.load_word_lang(word, frequency, LangId::En);
        }
        ml.load_word_lang("статия", 50_000, LangId::Bg);
        let (mut text, mut preedit) = (String::new(), String::new());
        for code in [31, 20, 30, 20, 23, 16] {
            step(&mut ml, Key::RawCode(code), &mut text, &mut preedit);
        }
        assert_eq!(ml.current_word(), "statiq");
        assert_eq!(ml.core.debug_state(), (true, true, false));
        step(&mut ml, Key::Char('x'), &mut text, &mut preedit);
        assert_eq!(ml.current_word(), "statiqx");
        assert_full_token(&mut ml, "statiqx", &mut text, &mut preedit);
    }
}

#[cfg(test)]
mod no_regression_contract_tests {
    use super::*;
    use crate::input::{Action, Key};

    fn check(context: Option<&str>, expected: &str) {
        let mut ml = MasterLoop::new(InputConfig::default());
        // Verified current JSON frequencies form a reduced fixture; the sealed
        // copied-binary replay is separate evidence, not a historical replay.
        ml.load_word_lang("no", 2_238_721, LangId::En);
        ml.load_word_lang("not", 4_897_788, LangId::En);
        ml.load_word_lang("но", 3_630_780, LangId::Bg);
        if let Some(text) = context {
            ml.set_surrounding_text(Some(text.to_string()), Some(text.chars().count()));
        }
        let mut readings = Vec::new();
        let raw = |code| KeyEvent {
            key: Key::RawCode(code),
            modifiers: Modifiers::empty(),
        };
        for code in [49, 24] {
            let actions = ml.handle_key(raw(code));
            let word = ml.current_word().to_string();
            assert!(actions
                .iter()
                .any(|a| matches!(a, Action::ShowComposing { typed, .. } if typed == &word)));
            assert!(!actions.iter().any(|a| matches!(
                a,
                Action::CommitText(_) | Action::ForwardKey | Action::ReplaceWord { .. }
            )));
            readings.push(word);
        }
        let actions = ml.handle_key(raw(57));
        let commits: Vec<&str> = actions
            .iter()
            .filter_map(|a| match a {
                Action::CommitText(word) => Some(word.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            commits,
            vec![expected],
            "preedit={readings:?}; actions={actions:?}"
        );
        assert_eq!(readings.last().map(String::as_str), Some(expected));
        if context.is_some() {
            assert_eq!(readings[0], "н");
        }
        assert_eq!(
            actions
                .iter()
                .filter(|a| matches!(a, Action::ForwardKey))
                .count(),
            1
        );
        assert!(!actions
            .iter()
            .any(|a| matches!(a, Action::ReplaceWord { .. })));
        assert_eq!(ml.current_word(), "");
        assert_eq!(ml.metrics().summary().total_commits, 1);
        assert_eq!(ml.metrics().summary().accepted_predictions, 0);
    }

    #[test]
    fn no_regression_known_bg_context_commits_particle_once() {
        check(Some("Искам да благодаря "), "но");
    }

    #[test]
    fn no_regression_absent_context_keeps_observational_latin_control() {
        check(None, "no");
    }
}

#[cfg(test)]
mod hybrid_delivery_contract_tests {
    use super::*;
    use crate::input::{Action, Key};

    fn scenario(name: &str) -> (Vec<Key>, &'static str) {
        use Key::{Backspace as Bs, Char as Ch, RawCode as Raw};
        match name {
            "raw_suffix" => (vec![Raw(16), Ch('a'), Raw(45)], "qax"),
            "one_raw_backspace" => (vec![Raw(16), Ch('a'), Bs], "q"),
            "two_raw_backspace" => (vec![Raw(16), Raw(45), Ch('a'), Bs], "qx"),
            "direct_prefix" => (vec![Ch('a'), Raw(16)], "aq"),
            "immediate_space" => (vec![Raw(16), Ch('a')], "qa"),
            "raw_only" => (vec![Raw(16), Raw(45), Bs], "q"),
            "direct_only" => (vec![Ch('q'), Ch('a')], "qa"),
            "unicode_mixed" => (vec![Ch('é'), Ch('д'), Bs, Ch('я')], "éя"),
            _ => panic!("unknown test scenario"),
        }
    }

    // Projection of the bounded IBus dispatcher contract: composing replaces
    // preedit, hide discards it, safe_commit clears it before insertion, and a
    // ForwardKey delivers the ORIGINAL event keyval after the action batch.
    // Raw fixture keyvals below are explicitly Latin q/x, not inferred winners.
    pub(super) fn project(actions: &[Action], key: &Key, text: &mut String, preedit: &mut String) {
        for action in actions {
            match action {
                Action::ShowComposing { typed, .. } => preedit.clone_from(typed),
                Action::HideGhost | Action::ShowGhost(_) => preedit.clear(),
                Action::CommitText(word) => {
                    preedit.clear();
                    text.push_str(word);
                }
                Action::ReplaceWord { .. } => panic!("unexpected replacement: {actions:?}"),
                Action::ForwardKey => {}
            }
        }
        if actions
            .iter()
            .any(|action| matches!(action, Action::ForwardKey))
        {
            match key {
                Key::Char(ch) => text.push(*ch),
                Key::RawCode(16) => text.push('q'),
                Key::RawCode(45) => text.push('x'),
                Key::RawCode(57) | Key::Space => text.push(' '),
                Key::Backspace => {
                    text.pop();
                }
                _ => panic!("unmodelled ORIGINAL forwarded keyval: {key:?}"),
            }
        }
    }

    fn run(name: &str, canonical: bool) {
        let (keys, expected) = scenario(name);
        let mut ml = MasterLoop::new(InputConfig {
            ghost_text: false,
            ..InputConfig::default()
        });
        let (mut delivered, mut preedit, mut trace) = (String::new(), String::new(), Vec::new());
        for key in keys.into_iter().chain([Key::RawCode(57)]) {
            let word_before = ml.current_word().to_string();
            let actions = ml.handle_key(KeyEvent {
                key: key.clone(),
                modifiers: Modifiers::empty(),
            });
            project(&actions, &key, &mut delivered, &mut preedit);
            trace.push(format!("key={key:?}; word={:?}; actions={actions:?}; committed={delivered:?}; preedit={preedit:?}", ml.current_word()));
            if key != Key::RawCode(57) {
                assert_eq!(ml.metrics().summary().total_commits, 0, "{trace:#?}");
            }
            if key == Key::RawCode(57) {
                eprintln!("scenario={name}; trace={trace:#?}");
                assert_eq!(ml.current_word(), "");
                assert!(preedit.is_empty());
                assert_eq!(
                    actions
                        .iter()
                        .filter(|a| matches!(a, Action::ForwardKey))
                        .count(),
                    1
                );
                assert_eq!(ml.metrics().summary().accepted_predictions, 0);
                if canonical {
                    assert_eq!(
                        word_before, expected,
                        "canonical token conservation; {trace:#?}"
                    );
                } else {
                    assert_eq!(
                        delivered,
                        format!("{expected} "),
                        "client delivery projection; {trace:#?}"
                    );
                }
            }
        }
    }

    macro_rules! contract {
        ($test:ident, $scenario:literal, $canonical:literal) => {
            #[test]
            fn $test() {
                run($scenario, $canonical);
            }
        };
    }
    contract!(hybrid_canonical_raw_suffix, "raw_suffix", true);
    contract!(
        hybrid_canonical_one_raw_backspace,
        "one_raw_backspace",
        true
    );
    contract!(
        hybrid_canonical_two_raw_backspace,
        "two_raw_backspace",
        true
    );
    contract!(hybrid_canonical_direct_prefix, "direct_prefix", true);
    contract!(hybrid_client_raw_suffix, "raw_suffix", false);
    contract!(hybrid_client_one_raw_backspace, "one_raw_backspace", false);
    contract!(hybrid_client_two_raw_backspace, "two_raw_backspace", false);
    contract!(hybrid_client_direct_prefix, "direct_prefix", false);
    contract!(hybrid_client_immediate_space, "immediate_space", false);
    contract!(hybrid_control_raw_only, "raw_only", false);
    contract!(hybrid_control_direct_only, "direct_only", false);
    contract!(hybrid_control_unicode_mixed, "unicode_mixed", false);
}

#[cfg(test)]
mod hybrid_handoff_control_tests {
    use super::*;
    use crate::input::{Action, Key};

    fn key(ml: &mut MasterLoop, key: Key) -> Vec<Action> {
        ml.handle_key(KeyEvent {
            key,
            modifiers: Modifiers::empty(),
        })
    }

    fn core(ghost: bool) -> MasterLoop {
        MasterLoop::new(InputConfig {
            ghost_text: ghost,
            ..InputConfig::default()
        })
    }

    fn handoff(ml: &mut MasterLoop) {
        key(ml, Key::RawCode(16));
        let prefix = ml.current_word().to_string();
        let actions = key(ml, Key::Char('a'));
        assert_eq!(
            actions
                .iter()
                .filter_map(|a| match a {
                    Action::CommitText(s) => Some(s.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            [prefix.as_str()]
        );
        assert!(actions.contains(&Action::ForwardKey));
        assert!(!actions.iter().any(|a| matches!(
            a,
            Action::ReplaceWord { .. } | Action::ShowGhost(_) | Action::ShowComposing { .. }
        )));
        assert_eq!(ml.metrics().summary().total_commits, 0);
        assert!(ml.core.export_personal_profile().cvm.words.is_empty());
        assert!(ml.core.export_personal_profile().confirmed_words.is_empty());
    }

    #[test]
    fn hybrid_control_layout_snapshot_preserves_caps_and_physical_punctuation() {
        for bg in [false, true] {
            for modifiers in [
                Modifiers::empty(),
                Modifiers::CAPS_LOCK,
                Modifiers::SHIFT,
                Modifiers::SHIFT | Modifiers::CAPS_LOCK,
            ] {
                for (code, en, shifted_en, bg_lower, bg_upper) in [
                    (26, '[', '{', 'ш', 'Ш'),
                    (27, ']', '}', 'щ', 'Щ'),
                    (41, '`', '~', 'ч', 'Ч'),
                    (43, '\\', '|', 'ю', 'Ю'),
                ] {
                    let mut ml = core(false);
                    ml.load_word_lang(
                        if bg { "яяя" } else { "question" },
                        1000,
                        if bg { LangId::Bg } else { LangId::En },
                    );
                    handoff(&mut ml);
                    let before = ml.core.detected_language();
                    let prior = ml.light_profile().lang_prior;
                    let actions = ml.handle_key(KeyEvent {
                        key: Key::RawCode(code),
                        modifiers,
                    });
                    let selected = if bg {
                        if modifiers.contains(Modifiers::SHIFT)
                            ^ modifiers.contains(Modifiers::CAPS_LOCK)
                        {
                            bg_upper
                        } else {
                            bg_lower
                        }
                    } else if modifiers.contains(Modifiers::SHIFT) {
                        shifted_en
                    } else {
                        en
                    };
                    assert!(
                        actions.contains(&Action::CommitText(selected.to_string())),
                        "{actions:?}"
                    );
                    assert!(!actions
                        .iter()
                        .any(|a| matches!(a, Action::ForwardKey | Action::ReplaceWord { .. })));
                    assert_eq!(
                        ml.current_word(),
                        format!("{}a{selected}", if bg { "я" } else { "q" })
                    );
                    let after = ml.core.detected_language();
                    assert_eq!(
                        (after.lang, after.confidence),
                        (before.lang, before.confidence)
                    );
                    assert_eq!(ml.metrics().summary().total_commits, 0);
                    assert_eq!(ml.light_profile().lang_prior, prior);
                    assert!(ml.core.export_personal_profile().confirmed_words.is_empty());
                    let boundary = key(&mut ml, Key::Space);
                    assert!(!boundary
                        .iter()
                        .any(|a| matches!(a, Action::CommitText(_) | Action::ReplaceWord { .. })));
                    assert_eq!(ml.metrics().summary().total_commits, 1);
                    assert_eq!(
                        ml.light_profile().lang_prior,
                        prior,
                        "mixed/inferred word did not become direct proof"
                    );
                }
            }
        }
    }

    #[test]
    fn hybrid_control_semantic_prefix_has_explicit_en_raw_default() {
        let mut ml = core(false);
        let bg = "Искам да благодаря ";
        ml.set_surrounding_text(Some(bg.to_string()), Some(bg.chars().count()));
        key(&mut ml, Key::Char('а'));
        let before = ml.core.detected_language();
        let actions = key(&mut ml, Key::RawCode(26));
        assert_eq!(ml.current_word(), "а[");
        assert!(actions.contains(&Action::CommitText("[".to_string())));
        assert!(!actions.contains(&Action::ForwardKey));
        let after = ml.core.detected_language();
        assert_eq!(
            (after.lang, after.confidence),
            (before.lang, before.confidence)
        );
        assert_eq!(ml.metrics().summary().total_commits, 0);
    }

    #[test]
    fn hybrid_control_literal_handoff_resists_legacy_transliteration() {
        let setup = || {
            let mut ml = core(false);
            ml.load_word_lang("question", 2_000_000, LangId::En);
            ml.load_word_lang("яа", 1_000_000, LangId::Bg);
            ml
        };
        let mut ml = setup();
        handoff(&mut ml);
        assert_eq!(ml.current_word(), "qa");
        let actions = key(&mut ml, Key::Char('a'));
        assert_eq!(ml.current_word(), "qaa");
        assert!(actions.contains(&Action::ForwardKey));
        assert!(!actions
            .iter()
            .any(|a| matches!(a, Action::ReplaceWord { .. })));
        let mut direct = setup();
        key(&mut direct, Key::Char('q'));
        let actions = key(&mut direct, Key::Char('a'));
        assert_eq!(
            direct.current_word(),
            "яа",
            "semantic-only legacy policy is unchanged"
        );
        assert!(actions
            .iter()
            .any(|a| matches!(a, Action::ReplaceWord { text, .. } if text == "яа")));
    }

    #[test]
    fn hybrid_control_resets_and_empty_backspace_restore_raw_composition() {
        for exit in [0, 1, 2] {
            let mut ml = core(false);
            handoff(&mut ml);
            match exit {
                0 => {
                    ml.reset();
                }
                1 => {
                    ml.focus_lost();
                }
                _ => {
                    key(&mut ml, Key::Left);
                }
            }
            assert_eq!(ml.current_word(), "");
            let actions = key(&mut ml, Key::RawCode(16));
            assert!(
                actions
                    .iter()
                    .any(|a| matches!(a, Action::ShowComposing { .. })),
                "exit={exit}: {actions:?}"
            );
            assert!(!actions.iter().any(|a| matches!(a, Action::CommitText(_))));
        }
        let mut escaped = core(false);
        handoff(&mut escaped);
        key(&mut escaped, Key::Escape);
        assert_eq!(
            escaped.current_word(),
            "qa",
            "Escape dismisses, not cancels, a delivered token"
        );
        assert!(key(&mut escaped, Key::RawCode(45)).contains(&Action::CommitText("x".to_string())));
        assert_eq!(escaped.current_word(), "qax");
        let mut ml = core(false);
        handoff(&mut ml);
        key(&mut ml, Key::Backspace);
        key(&mut ml, Key::Backspace);
        assert_eq!(ml.current_word(), "");
        for ch in "123".chars() {
            key(&mut ml, Key::Char(ch));
        }
        key(&mut ml, Key::Space);
        for (actual, expected) in ml
            .light_profile()
            .lang_prior
            .iter()
            .zip([0.3234, 0.3332, 0.3434])
        {
            assert!(
                (actual - expected).abs() < 1e-12,
                "editing baseline survives handoff and deletion"
            );
        }
        let prior = ml.light_profile().lang_prior;
        key(&mut ml, Key::Space);
        assert_eq!(ml.light_profile().lang_prior, prior);
        assert_eq!(ml.metrics().summary().total_commits, 1);
        assert!(key(&mut ml, Key::RawCode(16))
            .iter()
            .any(|a| matches!(a, Action::ShowComposing { .. })));
    }

    #[test]
    fn hybrid_control_hidden_handoff_prediction_cannot_be_accepted() {
        let mut ml = MasterLoop::new(InputConfig {
            ghost_text: true,
            space_accept: true,
            ..InputConfig::default()
        });
        ml.load_word_lang("qabc", 1_000_000, LangId::En);
        handoff(&mut ml);
        assert_eq!(ml.current_word(), "qa");
        assert!(!ml.has_ghost() && ml.predictions().is_empty());
        assert!(ml.last_shown_ghost.is_none());
        let tab = key(&mut ml, Key::Tab);
        assert_eq!(tab, vec![Action::HideGhost, Action::ForwardKey]);
        assert!(ml.current_word().is_empty());
        assert_eq!(ml.metrics().summary().total_commits, 1);
        let boundary = key(&mut ml, Key::Space);
        assert!(boundary.contains(&Action::ForwardKey));
        assert!(!boundary.iter().any(|a| matches!(a, Action::CommitText(_))));
        assert_eq!(ml.metrics().summary().accepted_predictions, 0);
        assert_eq!(ml.metrics().summary().total_commits, 1);
    }

    #[test]
    fn hybrid_control_next_raw_prediction_partial_right_and_full_accept_once() {
        for partial in [false, true] {
            let mut ml = core(true);
            ml.load_word_lang("qaxyz", 1_000_000, LangId::En);
            handoff(&mut ml);
            key(&mut ml, Key::RawCode(45));
            assert_eq!(ml.current_word(), "qax");
            assert!(ml.has_ghost());
            if partial {
                let actions = key(&mut ml, Key::Right);
                assert_eq!(ml.current_word(), "qaxy");
                assert!(actions.contains(&Action::CommitText("y".to_string())));
                assert_eq!(ml.metrics().summary().total_commits, 0);
                assert_eq!(ml.metrics().summary().accepted_predictions, 0);
            }
            let actions = key(&mut ml, if partial { Key::Right } else { Key::Tab });
            assert!(actions.contains(&Action::CommitText(
                if partial { "z" } else { "yz" }.to_string()
            )));
            assert_eq!(ml.current_word(), "");
            assert_eq!(ml.metrics().summary().total_commits, 1);
            assert_eq!(ml.metrics().summary().accepted_predictions, 1);
            assert_eq!(
                key(&mut ml, Key::Space)
                    .iter()
                    .filter(|a| matches!(a, Action::CommitText(_)))
                    .count(),
                0
            );
            assert_eq!(ml.metrics().summary().total_commits, 1);
        }
    }
}

#[cfg(test)]
mod hybrid_punctuation_parity_tests {
    use super::*;
    use crate::input::{Action, Key};

    #[test]
    fn hybrid_punctuation_matches_existing_nondual_semantic_token_policy() {
        for raw in [false, true] {
            let mut ml = MasterLoop::new(InputConfig {
                ghost_text: false,
                ..InputConfig::default()
            });
            for key in [
                if raw {
                    Key::RawCode(16)
                } else {
                    Key::Char('q')
                },
                Key::Char('a'),
                if raw {
                    Key::RawCode(26)
                } else {
                    Key::Char('[')
                },
            ] {
                ml.handle_key(KeyEvent {
                    key,
                    modifiers: Modifiers::empty(),
                });
            }
            assert_eq!(ml.current_word(), "qa[");
            assert_eq!(ml.metrics().summary().total_commits, 0);
            let actions = ml.handle_key(KeyEvent {
                key: Key::Space,
                modifiers: Modifiers::empty(),
            });
            assert_eq!(
                actions
                    .iter()
                    .filter(|a| matches!(a, Action::CommitText(_)))
                    .count(),
                0
            );
            assert_eq!(ml.metrics().summary().total_commits, 1);
        }
    }
}

#[cfg(test)]
mod hybrid_space_regained_ghost_tests {
    use super::*;
    use crate::input::{Action, Key};

    fn run(space: Key) {
        for hybrid in [true, false] {
            let mut ml = MasterLoop::new(InputConfig {
                ghost_text: true,
                space_accept: true,
                ..InputConfig::default()
            });
            ml.load_word_lang("qaxyz", 1_000_000, LangId::En);
            let keys = [
                Key::RawCode(16),
                if hybrid {
                    Key::Char('a')
                } else {
                    Key::RawCode(30)
                },
                Key::RawCode(45),
            ];
            let (mut delivered, mut preedit, mut trace) =
                (String::new(), String::new(), Vec::new());
            for key in keys {
                let actions = ml.handle_key(KeyEvent {
                    key: key.clone(),
                    modifiers: Modifiers::empty(),
                });
                super::hybrid_delivery_contract_tests::project(
                    &actions,
                    &key,
                    &mut delivered,
                    &mut preedit,
                );
                trace.push(format!(
                    "{key:?}: {actions:?}; word={:?}; delivered={delivered:?}",
                    ml.current_word()
                ));
                assert_eq!(ml.metrics().summary().total_commits, 0);
            }
            assert_eq!(ml.current_word(), "qax");
            assert_eq!(ml.ghost_text(), "yz", "visible regained ghost is required");
            assert!(
                ml.space_display_identity_ok(),
                "display attribution must actually match"
            );
            assert_eq!(ml.debug_state().0, !hybrid);
            assert_eq!(ml.core.space_accept_armed(), !hybrid);
            assert_eq!(delivered, if hybrid { "qax" } else { "" });
            let actions = ml.handle_key(KeyEvent {
                key: space.clone(),
                modifiers: Modifiers::empty(),
            });
            super::hybrid_delivery_contract_tests::project(
                &actions,
                &space,
                &mut delivered,
                &mut preedit,
            );
            trace.push(format!("{space:?}: {actions:?}; delivered={delivered:?}"));
            eprintln!("hybrid={hybrid}; trace={trace:#?}");
            let commits = actions
                .iter()
                .filter_map(|a| match a {
                    Action::CommitText(s) => Some(s.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                commits,
                if hybrid { vec![] } else { vec!["qaxyz "] },
                "{trace:#?}"
            );
            assert_eq!(
                actions
                    .iter()
                    .filter(|a| matches!(a, Action::ForwardKey))
                    .count(),
                usize::from(hybrid)
            );
            assert!(!actions
                .iter()
                .any(|a| matches!(a, Action::ReplaceWord { .. })));
            assert_eq!(delivered, if hybrid { "qax " } else { "qaxyz " });
            assert!(
                ml.current_word().is_empty() && ml.ghost_text().is_empty() && preedit.is_empty()
            );
            assert_eq!(ml.metrics().summary().total_commits, 1);
            assert_eq!(
                ml.metrics().summary().accepted_predictions,
                u64::from(!hybrid)
            );
            assert!(!ml
                .handle_key(KeyEvent {
                    key: space.clone(),
                    modifiers: Modifiers::empty()
                })
                .iter()
                .any(|a| matches!(a, Action::CommitText(_))));
            assert_eq!(ml.metrics().summary().total_commits, 1);
            assert_eq!(
                ml.metrics().summary().accepted_predictions,
                u64::from(!hybrid)
            );
        }
    }

    #[test]
    fn hybrid_space_regained_ghost_raw_boundary_is_literal() {
        run(Key::RawCode(57));
    }

    #[test]
    fn hybrid_space_regained_ghost_semantic_boundary_is_literal() {
        run(Key::Space);
    }
}

#[cfg(test)]
mod delivered_tab_boundary_tests {
    use super::*;
    use crate::input::Key;

    fn press(ml: &mut MasterLoop, key: Key) -> Vec<Action> {
        ml.handle_key(KeyEvent {
            key,
            modifiers: Modifiers::empty(),
        })
    }

    fn run(prefix: &[Key], composed: bool) {
        let mut ml = MasterLoop::new(InputConfig {
            ghost_text: false,
            ..InputConfig::default()
        });
        let (mut client, mut preedit) = (String::new(), String::new());
        for key in prefix {
            let actions = press(&mut ml, key.clone());
            super::hybrid_delivery_contract_tests::project(
                &actions,
                key,
                &mut client,
                &mut preedit,
            );
        }
        assert_eq!(ml.current_word(), "qa");
        assert_eq!(ml.metrics().summary().total_commits, 0);
        let tab = press(&mut ml, Key::Tab);
        // Bounded client projection: this text client inserts a forwarded Tab.
        super::hybrid_delivery_contract_tests::project(
            &tab,
            &Key::Char('\t'),
            &mut client,
            &mut preedit,
        );
        let commits = tab
            .iter()
            .filter_map(|action| match action {
                Action::CommitText(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(commits, if composed { vec!["qa"] } else { vec![] });
        assert_eq!(
            tab.iter()
                .filter(|a| matches!(a, Action::ForwardKey))
                .count(),
            1
        );
        assert_eq!(
            client, "qa\t",
            "Tab must never redeliver an owned prefix: {tab:?}"
        );
        assert!(preedit.is_empty());
        assert_eq!(
            ml.current_word(),
            "",
            "Tab ended the delivered token: {tab:?}"
        );
        assert_eq!(ml.metrics().summary().total_commits, 1);
        assert_eq!(ml.core.context_words().0, Some("qa"));
        assert_eq!(ml.core.export_personal_profile().cvm.words, ["qa"]);

        let next = press(&mut ml, Key::RawCode(45));
        super::hybrid_delivery_contract_tests::project(
            &next,
            &Key::RawCode(45),
            &mut client,
            &mut preedit,
        );
        assert_eq!(
            ml.current_word(),
            "x",
            "new raw input must start a fresh token"
        );
        assert_eq!(format!("{client}{preedit}"), "qa\tx");
        assert_eq!(ml.metrics().summary().total_commits, 1);
        let boundary = press(&mut ml, Key::Space);
        super::hybrid_delivery_contract_tests::project(
            &boundary,
            &Key::Space,
            &mut client,
            &mut preedit,
        );
        assert_eq!(client, "qa\tx ");
        assert!(preedit.is_empty() && ml.current_word().is_empty());
        assert_eq!(ml.metrics().summary().total_commits, 2);
        assert_eq!(ml.metrics().summary().accepted_predictions, 0);
        let words = ml.core.export_personal_profile().cvm.words;
        assert_eq!(words.iter().filter(|w| w.as_str() == "qa").count(), 1);
        assert_eq!(words.iter().filter(|w| w.as_str() == "x").count(), 1);
        eprintln!("prefix={prefix:?}; tab={tab:?}; next={next:?}; boundary={boundary:?}; client={client:?}");
    }

    #[test]
    fn delivered_tab_boundary_hybrid_finishes_without_recommit() {
        run(&[Key::RawCode(16), Key::Char('a')], false);
    }

    #[test]
    fn delivered_tab_boundary_semantic_finishes_without_recommit() {
        run(&[Key::Char('q'), Key::Char('a')], false);
    }

    #[test]
    fn delivered_tab_boundary_raw_preedit_commits_once() {
        run(&[Key::RawCode(16), Key::RawCode(30)], true);
    }

    #[test]
    fn delivered_tab_boundary_empty_forwards_without_learning() {
        let mut ml = MasterLoop::new(InputConfig::default());
        assert_eq!(press(&mut ml, Key::Tab), [Action::ForwardKey]);
        assert_eq!(ml.metrics().summary().total_commits, 0);
        assert!(ml.current_word().is_empty());
    }
}

#[cfg(test)]
mod hybrid_active_transliteration_tests {
    use super::*;
    use crate::input::Key;

    fn press(ml: &mut MasterLoop, key: Key) -> Vec<Action> {
        ml.handle_key(KeyEvent {
            key,
            modifiers: Modifiers::empty(),
        })
    }

    pub(super) fn legacy_prefix() -> (MasterLoop, String, String) {
        let mut ml = MasterLoop::new(InputConfig {
            ghost_text: false,
            ..InputConfig::default()
        });
        ml.load_word_lang("здравей", 1_000_000, LangId::Bg);
        let (mut client, mut preedit) = (String::new(), String::new());
        let first = press(&mut ml, Key::Char('z'));
        super::hybrid_delivery_contract_tests::project(
            &first,
            &Key::Char('z'),
            &mut client,
            &mut preedit,
        );
        assert_eq!(client, "z");
        let second = press(&mut ml, Key::Char('d'));
        assert!(
            second.contains(&Action::ReplaceWord {
                replace_len: 1,
                text: "зд".to_string(),
            }),
            "must exercise the actual legacy transliteration producer: {second:?}"
        );
        assert!(!second
            .iter()
            .any(|a| matches!(a, Action::ForwardKey | Action::CommitText(_))));
        // The real replacement consumes d and replaces the earlier forwarded z.
        client.pop();
        client.push_str("зд");
        preedit.clear();
        assert_eq!(ml.current_word(), "зд");
        assert!(!ml.debug_state().0);
        assert_eq!(ml.metrics().summary().total_commits, 0);
        eprintln!(
            "legacy producer={second:?}; client={client:?}; word={:?}",
            ml.current_word()
        );
        (ml, client, preedit)
    }

    #[test]
    fn hybrid_active_transliteration_raw_continuation_preserves_layout_without_new_evidence() {
        let (mut ml, mut client, mut preedit) = legacy_prefix();
        let detector = ml.detected_language();
        let prior = ml.light_profile().lang_prior;
        for (code, glyph, word) in [(19, "р", "здр"), (30, "а", "здра")] {
            let actions = press(&mut ml, Key::RawCode(code));
            assert!(!actions
                .iter()
                .any(|a| matches!(a, Action::ForwardKey | Action::ReplaceWord { .. })));
            super::hybrid_delivery_contract_tests::project(
                &actions,
                &Key::RawCode(code),
                &mut client,
                &mut preedit,
            );
            let after = ml.detected_language();
            eprintln!("raw={code}; actions={actions:?}; word={:?}; client={client:?}; detector={:?}->{:?}; prior={prior:?}->{:?}", ml.current_word(), (detector.lang, detector.confidence), (after.lang, after.confidence), ml.light_profile().lang_prior);
            assert!(
                actions.contains(&Action::CommitText(glyph.to_string())),
                "{actions:?}"
            );
            assert_eq!(ml.current_word(), word);
            assert_eq!(client, word);
            assert!(!ml.debug_state().0 && preedit.is_empty());
            assert_eq!(
                (after.lang, after.confidence),
                (detector.lang, detector.confidence)
            );
            assert_eq!(ml.light_profile().lang_prior, prior);
            assert_eq!(ml.metrics().summary().total_commits, 0);
            assert!(ml.core.export_personal_profile().cvm.words.is_empty());
            assert!(ml.core.export_personal_profile().confirmed_words.is_empty());
        }
        let backspace = press(&mut ml, Key::Backspace);
        super::hybrid_delivery_contract_tests::project(
            &backspace,
            &Key::Backspace,
            &mut client,
            &mut preedit,
        );
        assert_eq!(client, "здр");
        assert_eq!(ml.current_word(), "здр");
        let next = press(&mut ml, Key::RawCode(18));
        assert!(next.contains(&Action::CommitText("е".to_string())));
        super::hybrid_delivery_contract_tests::project(
            &next,
            &Key::RawCode(18),
            &mut client,
            &mut preedit,
        );
        assert_eq!(ml.current_word(), "здре");
        assert_eq!(client, "здре");
        let boundary = press(&mut ml, Key::Space);
        super::hybrid_delivery_contract_tests::project(
            &boundary,
            &Key::Space,
            &mut client,
            &mut preedit,
        );
        assert_eq!(boundary, [Action::HideGhost, Action::ForwardKey]);
        assert_eq!(client, "здре ");
        assert!(ml.current_word().is_empty());
        assert_eq!(ml.metrics().summary().total_commits, 1);
        assert_eq!(ml.metrics().summary().accepted_predictions, 0);
        assert!(ml.core.export_personal_profile().confirmed_words.is_empty());
    }

    #[test]
    fn hybrid_active_transliteration_empty_backspace_or_reset_releases_delivery_snapshot() {
        for reset in [false, true] {
            let (mut ml, mut client, mut preedit) = legacy_prefix();
            let raw = press(&mut ml, Key::RawCode(19));
            assert!(raw.contains(&Action::CommitText("р".to_string())));
            super::hybrid_delivery_contract_tests::project(
                &raw,
                &Key::RawCode(19),
                &mut client,
                &mut preedit,
            );
            if reset {
                assert_eq!(ml.reset(), [Action::HideGhost]);
            } else {
                for _ in 0..3 {
                    let actions = press(&mut ml, Key::Backspace);
                    super::hybrid_delivery_contract_tests::project(
                        &actions,
                        &Key::Backspace,
                        &mut client,
                        &mut preedit,
                    );
                }
            }
            assert!(ml.current_word().is_empty());
            if !reset {
                assert!(client.is_empty());
            }
            let fresh = press(&mut ml, Key::RawCode(26));
            assert!(
                ml.debug_state().0,
                "first raw after empty must start a fresh dual buffer"
            );
            assert!(fresh
                .iter()
                .any(|a| matches!(a, Action::ShowComposing { .. })));
            assert!(!fresh
                .iter()
                .any(|a| matches!(a, Action::CommitText(_) | Action::ForwardKey)));
            assert_eq!(ml.metrics().summary().total_commits, 0);
        }
    }
}

#[cfg(test)]
mod hybrid_followup_tests {
    use super::*;
    use crate::input::Key;

    fn step(
        ml: &mut MasterLoop,
        client: &mut String,
        preedit: &mut String,
        key: Key,
    ) -> Vec<Action> {
        let actions = ml.handle_key(KeyEvent {
            key: key.clone(),
            modifiers: Modifiers::empty(),
        });
        super::hybrid_delivery_contract_tests::project(&actions, &key, client, preedit);
        actions
    }

    fn visible_ghost() -> (MasterLoop, String, String) {
        let mut ml = MasterLoop::new(InputConfig {
            ghost_text: true,
            ..InputConfig::default()
        });
        ml.load_word_lang("qaxyz", 1_000_000, LangId::En);
        let (mut client, mut preedit) = (String::new(), String::new());
        for key in [
            Key::RawCode(16),
            Key::Char('a'),
            Key::RawCode(45),
            Key::RawCode(21),
        ] {
            step(&mut ml, &mut client, &mut preedit, key);
        }
        assert_eq!(ml.current_word(), "qaxy");
        assert_eq!(client, "qaxy");
        assert_eq!(ml.ghost_text(), "z");
        assert!(ml.last_shown_ghost.is_some());
        (ml, client, preedit)
    }

    #[test]
    fn hybrid_followup_consumed_hide_retires_attribution_before_backspace() {
        let (mut ml, mut client, mut preedit) = visible_ghost();
        let finished = step(&mut ml, &mut client, &mut preedit, Key::RawCode(44));
        assert_eq!(
            finished,
            [Action::CommitText("z".to_string()), Action::HideGhost]
        );
        assert_eq!(ml.current_word(), "qaxyz");
        assert_eq!(client, "qaxyz");
        let hidden_attribution = ml.last_shown_ghost.clone();
        let backspace = step(&mut ml, &mut client, &mut preedit, Key::Backspace);
        eprintln!("finished={finished:?}; hidden={hidden_attribution:?}; backspace={backspace:?}; client={client:?}; rejections={}", ml.rejection_prefix_count());
        assert_eq!(ml.current_word(), "qaxy");
        assert_eq!(client, "qaxy");
        assert_eq!(
            ml.rejection_prefix_count(),
            0,
            "editing a manually finished word must not reject an invisible ghost"
        );
        assert!(hidden_attribution.is_none());
        assert_eq!(ml.ghost_text(), "z");
        assert_eq!(ml.metrics().summary().total_commits, 0);
    }

    #[test]
    fn hybrid_followup_manual_completion_edits_never_suppress_the_valid_ghost() {
        let (mut ml, mut client, mut preedit) = visible_ghost();
        for _ in 0..3 {
            let finished = step(&mut ml, &mut client, &mut preedit, Key::RawCode(44));
            let edited = step(&mut ml, &mut client, &mut preedit, Key::Backspace);
            eprintln!("finished={finished:?}; edited={edited:?}; client={client:?}; ghost={:?}; suppressed={}", ml.ghost_text(), ml.is_ghost_suppressed("qaxy", "qaxyz"));
            assert_eq!(client, "qaxy");
            assert_eq!(ml.current_word(), "qaxy");
        }
        assert_eq!(
            ml.ghost_text(),
            "z",
            "manual edits must not train suppression"
        );
        assert!(!ml.is_ghost_suppressed("qaxy", "qaxyz"));
        assert_eq!(ml.rejection_prefix_count(), 0);
        assert!(ml.core.export_personal_profile().confirmed_words.is_empty());
    }

    #[test]
    fn hybrid_followup_backspace_over_a_visible_ghost_still_records_rejection() {
        let (mut ml, mut client, mut preedit) = visible_ghost();
        step(&mut ml, &mut client, &mut preedit, Key::Backspace);
        assert_eq!(ml.rejection_prefix_count(), 1);
        assert!(!ml.is_ghost_suppressed("qaxy", "qaxyz"));
        assert_eq!(ml.current_word(), "qax");
        assert_eq!(client, "qax");
    }

    #[test]
    fn hybrid_followup_active_transliteration_survives_raw_then_semantic() {
        let (mut ml, mut client, mut preedit) =
            super::hybrid_active_transliteration_tests::legacy_prefix();
        step(&mut ml, &mut client, &mut preedit, Key::RawCode(19));
        assert_eq!(client, "здр");
        let semantic = step(&mut ml, &mut client, &mut preedit, Key::Char('a'));
        eprintln!(
            "semantic={semantic:?}; current={:?}; client={client:?}",
            ml.current_word()
        );
        assert_eq!(ml.current_word(), "здра");
        assert_eq!(client, "здра");
        assert_eq!(
            semantic,
            [Action::CommitText("а".to_string()), Action::HideGhost]
        );
        assert_eq!(ml.metrics().summary().total_commits, 0);
        assert!(ml.core.export_personal_profile().cvm.words.is_empty());
        step(&mut ml, &mut client, &mut preedit, Key::Space);
        assert_eq!(client, "здра ");
        assert_eq!(ml.metrics().summary().total_commits, 1);
        assert_eq!(ml.metrics().summary().accepted_predictions, 0);
        assert!(ml.core.export_personal_profile().confirmed_words.is_empty());
    }

    #[test]
    fn hybrid_followup_raw_handoff_retires_stale_transliteration_latch() {
        let (mut ml, mut client, mut preedit) =
            super::hybrid_active_transliteration_tests::legacy_prefix();
        for _ in 0..2 {
            step(&mut ml, &mut client, &mut preedit, Key::Backspace);
        }
        assert!(ml.current_word().is_empty() && client.is_empty());
        step(&mut ml, &mut client, &mut preedit, Key::RawCode(44));
        assert_eq!(ml.current_word(), "з");
        assert!(ml.debug_state().0 && client.is_empty());
        let handoff = step(&mut ml, &mut client, &mut preedit, Key::Char('a'));
        eprintln!(
            "handoff={handoff:?}; current={:?}; client={client:?}",
            ml.current_word()
        );
        assert_eq!(
            ml.current_word(),
            "зa",
            "fresh raw handoff owns its literal semantic key"
        );
        assert_eq!(client, "зa");
        assert_eq!(
            handoff
                .iter()
                .filter_map(|a| match a {
                    Action::CommitText(s) => Some(s.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            ["з"]
        );
        assert_eq!(
            handoff
                .iter()
                .filter(|a| matches!(a, Action::ForwardKey))
                .count(),
            1
        );
        assert_eq!(ml.metrics().summary().total_commits, 0);
        step(&mut ml, &mut client, &mut preedit, Key::Space);
        assert_eq!(client, "зa ");
        assert_eq!(ml.metrics().summary().total_commits, 1);
    }
}

#[cfg(test)]
mod external_rewrite_delegation_tests {
    use super::*;
    use crate::input::{Key, KeyEvent, Modifiers};

    fn press(ml: &mut MasterLoop, ch: char) -> Vec<Action> {
        ml.handle_key(KeyEvent {
            key: Key::Char(ch),
            modifiers: Modifiers::empty(),
        })
    }

    #[test]
    fn external_rewrite_master_loop_false_delegates_before_legacy_mutation() {
        let mut ml = MasterLoop::new(InputConfig {
            ghost_text: false,
            ..InputConfig::default()
        });
        ml.load_word_lang("здравей", 1_000_000, LangId::Bg);
        ml.set_external_rewrite_supported(false);
        press(&mut ml, 'z');
        let a = press(&mut ml, 'd');
        eprintln!(
            "MasterLoop unsupported legacy: {a:?}; word={:?}",
            ml.current_word()
        );
        assert!(!a.iter().any(|a| matches!(a, Action::ReplaceWord { .. })));
        assert_eq!(ml.current_word(), "zd");
    }

    #[test]
    fn external_rewrite_master_loop_true_preserves_legacy_producer() {
        let mut ml = MasterLoop::new(InputConfig {
            ghost_text: false,
            ..InputConfig::default()
        });
        ml.load_word_lang("здравей", 1_000_000, LangId::Bg);
        ml.set_external_rewrite_supported(true);
        press(&mut ml, 'z');
        let a = press(&mut ml, 'd');
        assert!(a.contains(&Action::ReplaceWord {
            replace_len: 1,
            text: "зд".into()
        }));
        assert_eq!(ml.current_word(), "зд");
    }
}

#[cfg(test)]
mod sentence_modifier_review_tests {
    use super::*;
    use crate::input::Key;

    fn send(ml: &mut MasterLoop, key: Key, modifiers: Modifiers) -> Vec<Action> {
        ml.handle_key(KeyEvent { key, modifiers })
    }

    fn plain(ml: &mut MasterLoop, key: Key) -> Vec<Action> {
        send(ml, key, Modifiers::empty())
    }

    // Core/native tests cover every identity. These exercise the ML paths.
    fn modifiers() -> [(Key, Modifiers); 3] {
        [
            (Key::RawCode(29), Modifiers::CTRL),
            (Key::Other(0xffea), Modifiers::ALT),
            (Key::RawCode(126), Modifiers::SUPER),
        ]
    }

    fn sentence(confirmed: bool, space_accept: bool) -> MasterLoop {
        let mut ml = MasterLoop::new(InputConfig {
            ghost_text_min_confidence: 0.0,
            ghost_text_separation_margin: 0.0,
            space_accept,
            ..InputConfig::default()
        });
        ml.load_word_lang("hello", 1_000_000, LangId::En);
        ml.load_word_lang("world", 1_000_000, LangId::En);
        for code in [35, 18, 38, 38, 24, 52] {
            plain(&mut ml, Key::RawCode(code));
        }
        if confirmed {
            plain(&mut ml, Key::Space);
        }
        assert!(ml.current_word().is_empty());
        assert_eq!(ml.metrics().summary().total_commits, 1);
        ml
    }

    fn prediction(ml: &mut MasterLoop) -> String {
        plain(ml, Key::RawCode(17));
        ml.predictions()[0].word.clone()
    }

    #[test]
    fn sentence_boundary_modifier_taps_preserve_pending_and_confirmed_hints() {
        for (key, held) in modifiers() {
            for confirmed in [false, true] {
                let mut ml = sentence(confirmed, false);
                let prior = ml.light_profile.lang_prior;
                assert_eq!(send(&mut ml, key.clone(), held), [Action::ForwardKey]);
                assert_eq!(
                    send(&mut ml, key.clone(), held | Modifiers::RELEASE),
                    [Action::ForwardKey]
                );
                assert_eq!(ml.light_profile.lang_prior, prior);
                assert_eq!(ml.metrics().summary().total_commits, 1);
                assert_eq!(ml.metrics().summary().accepted_predictions, 0);
                if !confirmed {
                    plain(&mut ml, Key::Space);
                }
                assert_eq!(
                    prediction(&mut ml),
                    "World",
                    "{key:?}, confirmed={confirmed}"
                );
            }
        }
    }

    #[test]
    fn sentence_boundary_modifier_shortcuts_withdraw_but_releases_preserve() {
        for confirmed in [false, true] {
            for (key, held) in [
                (Key::RawCode(46), Modifiers::CTRL),
                (Key::Char('c'), Modifiers::ALT),
            ] {
                let mut ml = sentence(confirmed, false);
                assert_eq!(send(&mut ml, key, held), [Action::ForwardKey]);
                if !confirmed {
                    plain(&mut ml, Key::Space);
                }
                assert_eq!(prediction(&mut ml), "world");
                assert_eq!(ml.metrics().summary().total_commits, 1);
            }
            let mut ml = sentence(confirmed, false);
            assert_eq!(
                send(
                    &mut ml,
                    Key::RawCode(29),
                    Modifiers::CTRL | Modifiers::RELEASE
                ),
                [Action::ForwardKey]
            );
            if !confirmed {
                plain(&mut ml, Key::Space);
            }
            assert_eq!(prediction(&mut ml), "World");
        }
    }

    #[test]
    fn sentence_boundary_modifier_super_space_and_kill_keep_existing_contracts() {
        let mut ml = sentence(true, true);
        assert_eq!(prediction(&mut ml), "World");
        let actions = send(&mut ml, Key::RawCode(57), Modifiers::SUPER);
        assert_eq!(
            actions,
            [
                Action::HideGhost,
                Action::CommitText("w".into()),
                Action::ForwardKey
            ]
        );
        assert_eq!(ml.metrics().summary().accepted_predictions, 0);
        assert_eq!(ml.metrics().summary().total_commits, 2);
        let mut ml = sentence(true, false);
        assert_eq!(
            send(&mut ml, Key::Escape, Modifiers::SUPER),
            [Action::HideGhost]
        );
        assert!(!ml.is_enabled());
        assert_eq!(
            send(&mut ml, Key::Escape, Modifiers::SUPER | Modifiers::RELEASE),
            [Action::ForwardKey]
        );
        assert!(!ml.is_enabled());
        send(&mut ml, Key::Escape, Modifiers::SUPER);
        assert!(ml.is_enabled());
        assert_eq!(prediction(&mut ml), "world");
    }

    #[test]
    fn sentence_boundary_modifier_tap_keeps_visible_space_acceptance_attributable() {
        let mut control = sentence(true, true);
        assert_eq!(prediction(&mut control), "World");
        assert!(control.core.space_accept_armed());
        assert_eq!(
            plain(&mut control, Key::Space),
            [Action::HideGhost, Action::CommitText("world ".into())]
        );
        assert_eq!(control.metrics().summary().accepted_predictions, 1);
        let mut ml = sentence(true, true);
        assert_eq!(prediction(&mut ml), "World");
        assert_eq!(ml.ghost_text(), "orld");
        assert!(ml.last_shown_ghost.is_some());
        send(&mut ml, Key::RawCode(29), Modifiers::CTRL);
        send(
            &mut ml,
            Key::RawCode(29),
            Modifiers::CTRL | Modifiers::RELEASE,
        );
        assert_eq!(
            plain(&mut ml, Key::Space),
            [Action::HideGhost, Action::CommitText("world ".into())]
        );
        assert_eq!(ml.metrics().summary().accepted_predictions, 1);
        assert_eq!(ml.metrics().summary().total_commits, 2);
    }

    #[test]
    fn sentence_boundary_modifier_after_tab_retires_immediate_backspace_attribution() {
        for (key, held) in modifiers() {
            let mut ml = sentence(true, false);
            assert_eq!(prediction(&mut ml), "World");
            assert!(plain(&mut ml, Key::Tab).contains(&Action::CommitText("world".into())));
            assert!(ml.last_prediction_accept.is_some());
            assert_eq!(send(&mut ml, key.clone(), held), [Action::ForwardKey]);
            assert!(ml.last_prediction_accept.is_none(), "{key:?}");
            assert!(ml.last_shown_ghost.is_none(), "{key:?}");
            send(&mut ml, key, held | Modifiers::RELEASE);
            let deleted = plain(&mut ml, Key::Backspace);
            assert!(!deleted
                .iter()
                .any(|a| matches!(a, Action::CommitText(_) | Action::ReplaceWord { .. })));
            assert_eq!(ml.rejection_prefix_count(), 0);
            assert_eq!(ml.metrics().summary().accepted_predictions, 1);
            assert_eq!(ml.metrics().summary().total_commits, 2);
        }
    }

    mod surrounding_refresh_review_tests {
        use super::*;

        fn fresh(post: bool) -> MasterLoop {
            let mut ml = MasterLoop::new(InputConfig {
                ghost_text_min_confidence: 0.0,
                ghost_text_separation_margin: 0.0,
                post_commit_autocorrect: post,
                ..InputConfig::default()
            });
            ml.load_word_lang("hello", 1_000_000, LangId::En);
            ml.load_word_lang("world", 1_000_000, LangId::En);
            ml
        }

        fn report(ml: &mut MasterLoop, text: &str, cursor: usize) {
            ml.set_surrounding_text(Some(text.into()), Some(cursor));
        }

        // Synthetic document projection, not a GTK/GUI claim. These default-mode
        // controls project only actual CommitText and the supplied original key on
        // ForwardKey. Preedit is absent from document text; Replace is not guessed.
        fn own_sentence(
            prefix: &str,
            tail: &str,
            space: bool,
        ) -> (MasterLoop, String, usize, Vec<(String, usize)>) {
            let mut ml = fresh(false);
            let mut text = format!("{prefix}{tail}");
            let mut cursor = prefix.chars().count();
            report(&mut ml, &text, cursor);
            let mut echoes = vec![(text.clone(), cursor)];
            let mut keys = vec![
                (35, 'h'),
                (18, 'e'),
                (38, 'l'),
                (38, 'l'),
                (24, 'o'),
                (52, '.'),
            ];
            if space {
                keys.push((57, ' '));
            }
            for (raw, original) in keys {
                for action in plain(&mut ml, Key::RawCode(raw)) {
                    let inserted = match action {
                        Action::CommitText(text) => Some(text),
                        Action::ForwardKey => Some(original.to_string()),
                        Action::ReplaceWord { .. } => {
                            panic!("unexpected rewrite in default-mode projection")
                        }
                        _ => None,
                    };
                    if let Some(inserted) = inserted {
                        let byte = text
                            .char_indices()
                            .nth(cursor)
                            .map(|(i, _)| i)
                            .unwrap_or(text.len());
                        text.insert_str(byte, &inserted);
                        cursor += inserted.chars().count();
                        echoes.push((text.clone(), cursor));
                    }
                }
            }
            assert_eq!(ml.metrics().summary().total_commits, 1);
            (ml, text, cursor, echoes)
        }

        #[test]
        fn sentence_boundary_surrounding_external_word_reconciles_prediction() {
            let mut ml = sentence(false, false);
            // The adapter refreshes before Space: anticipation can cache the
            // punctuation-bearing old token, independently of the local hint.
            report(&mut ml, "hello.", 6);
            plain(&mut ml, Key::Space);
            let prior = ml.light_profile.lang_prior;
            report(&mut ml, "hello. seed ", 12);
            assert_eq!(ml.light_profile.lang_prior, prior);
            assert_eq!(prediction(&mut ml), "world");
            assert_eq!(ml.current_word(), "w");
            assert_eq!(ml.metrics().summary().total_commits, 1);
            assert!(!ml
                .core
                .export_personal_profile()
                .cvm
                .words
                .iter()
                .any(|word| word == "seed"));
        }

        #[test]
        fn sentence_boundary_surrounding_external_word_prevents_optin_postcap() {
            let mut ml = fresh(true);
            for code in [35, 18, 38, 38, 24, 52, 57] {
                plain(&mut ml, Key::RawCode(code));
            }
            report(&mut ml, "hello. seed ", 12);
            for code in [17, 24, 19, 38, 32] {
                plain(&mut ml, Key::RawCode(code));
            }
            let actions = plain(&mut ml, Key::Space);
            assert_eq!(
                actions,
                [
                    Action::HideGhost,
                    Action::CommitText("world".into()),
                    Action::ForwardKey
                ]
            );
            assert_eq!(ml.metrics().summary().total_commits, 2);
            assert_eq!(ml.metrics().summary().accepted_predictions, 0);
            assert!(!ml
                .core
                .export_personal_profile()
                .cvm
                .words
                .iter()
                .any(|word| word == "seed"));
        }

        #[test]
        fn sentence_boundary_surrounding_identical_delayed_and_final_own_echoes_preserve_hint() {
            // Four actual projected images: baseline, Commithello, Forwarddot,
            // ForwardSpace. Each fixture observes only one forward prefix of them.
            for count in [1, 2, 4] {
                let (mut ml, text, cursor, echoes) = own_sentence("prior seed ", "", true);
                assert_eq!(echoes.len(), 4);
                assert_eq!(text, "prior seed hello. ");
                for (observed, position) in echoes.iter().take(count) {
                    report(&mut ml, observed, *position);
                }
                if count == 4 {
                    report(&mut ml, &text, cursor);
                }
                assert_eq!(prediction(&mut ml), "World", "own echo prefix {count}");
                assert_eq!(ml.metrics().summary().total_commits, 1);
            }
        }

        #[test]
        fn sentence_boundary_surrounding_external_edit_preserves_active_preedit_and_learning() {
            let mut ml = sentence(true, false);
            plain(&mut ml, Key::RawCode(17));
            plain(&mut ml, Key::RawCode(24));
            assert_eq!(ml.current_word(), "wo");
            assert!(ml.debug_state().0);
            report(&mut ml, "hello. seed ", 12);
            assert_eq!(ml.current_word(), "wo");
            assert!(ml.debug_state().0);
            assert_eq!(ml.metrics().summary().total_commits, 1);
            plain(&mut ml, Key::RawCode(19));
            assert_eq!(ml.current_word(), "wor");
            assert_eq!(ml.predictions()[0].word, "world");
            assert!(!ml
                .core
                .export_personal_profile()
                .cvm
                .words
                .iter()
                .any(|word| word == "seed"));
        }

        #[test]
        fn sentence_boundary_surrounding_cursor_only_move_uses_character_positions() {
            // Emoji is English-compatible in this sampler, while its UTF-8 byte
            // length differs from the client's Unicode character cursor.
            for prefix in ["", "😀 "] {
                let (mut ml, text, cursor, _) = own_sentence(prefix, "seed ", true);
                assert_eq!(cursor, prefix.chars().count() + 7);
                report(&mut ml, &text, cursor);
                plain(&mut ml, Key::RawCode(17));
                assert_eq!(ml.current_word(), "w", "fixture layout, prefix={prefix:?}");
                assert_eq!(
                    ml.predictions().first().map(|p| p.word.as_str()),
                    Some("World"),
                    "fixture prediction, prefix={prefix:?}"
                );
                plain(&mut ml, Key::Backspace); // Local preedit only, not client text.
                assert!(ml.current_word().is_empty());
                report(&mut ml, &text, text.chars().count());
                plain(&mut ml, Key::RawCode(17));
                assert_eq!(ml.current_word(), "w", "fixture layout, prefix={prefix:?}");
                assert_eq!(
                    ml.predictions().first().map(|p| p.word.as_str()),
                    Some("world"),
                    "cursor moved past seed, prefix={prefix:?}"
                );
                assert_eq!(ml.metrics().summary().total_commits, 1);
            }
        }

        #[test]
        fn sentence_boundary_surrounding_observed_return_and_tab_keep_opaque_start() {
            let mut ml = fresh(false);
            report(&mut ml, "prior seed ", 11);
            for code in [35, 18, 38, 38, 24] {
                plain(&mut ml, Key::RawCode(code));
            }
            assert_eq!(
                plain(&mut ml, Key::Return),
                [
                    Action::HideGhost,
                    Action::CommitText("hello".into()),
                    Action::ForwardKey
                ]
            );
            // Actual commit-only delayed echo.
            report(&mut ml, "prior seed hello", 16);
            // Explicit client report, not a guessed Return glyph.
            report(&mut ml, "prior seed hello\n", 17);
            assert_eq!(prediction(&mut ml), "World");
            let (mut ml, text, cursor, _) = own_sentence("prior seed ", "", false);
            assert_eq!(plain(&mut ml, Key::Tab), [Action::ForwardKey]);
            let reported = format!("{text}\t");
            report(&mut ml, &reported, cursor + 1);
            assert_eq!(prediction(&mut ml), "World");
            for (separator, glyph) in [(Key::Tab, '\t'), (Key::Space, ' ')] {
                let (mut ml, text, cursor, _) = own_sentence("prior seed ", "", false);
                plain(&mut ml, separator);
                plain(&mut ml, Key::Return);
                // Actual reported whitespace, not an assumed Return glyph.
                report(&mut ml, &format!("{text}{glyph}\n"), cursor + 2);
                assert_eq!(prediction(&mut ml), "World");
            }
        }

        #[test]
        fn sentence_boundary_surrounding_empty_and_whitespace_reports_keep_raw_geometry_only() {
            for (text, cursor) in [("", 0), (" \t\n", 3)] {
                let mut ml = fresh(false);
                report(&mut ml, text, cursor);
                assert_eq!(ml.surrounding_text.as_deref(), Some(text));
                assert_eq!(ml.surrounding_cursor_pos, Some(cursor));
                let analysis = ml.cached_surrounding_analysis();
                assert!(analysis.is_none());
                assert_eq!(ml.terminal_context_language(analysis.as_ref()), None);
            }
            let mut ml = fresh(false);
            ml.set_surrounding_text(None, None);
            assert!(ml.surrounding_text.is_none());
            assert!(ml.surrounding_cursor_pos.is_none());
            assert!(ml.cached_surrounding_analysis().is_none());
            assert_eq!(ml.terminal_context_language(None), None);
        }

        #[test]
        fn sentence_boundary_surrounding_coalesced_optin_echoes_exclude_internal_states() {
            for (observed, cursor, expected) in [
                ("prior seed Hello. ", 18, "World"),
                ("prior seed hello", 16, "world"),
                ("prior seed ", 11, "world"),
            ] {
                let mut ml = fresh(true);
                report(&mut ml, "prior seed ", 11);
                for code in [35, 18, 38, 38, 24] {
                    plain(&mut ml, Key::RawCode(code));
                }
                let actions = plain(&mut ml, Key::RawCode(52));
                assert!(actions.contains(&Action::CommitText("hello".into())));
                assert!(actions.contains(&Action::ReplaceWord {
                    replace_len: 5,
                    text: "Hello".into(),
                }));
                plain(&mut ml, Key::Space);
                // A baseline report is legitimate until final delivery is
                // acknowledged. Lowercase hello was never a wire image.
                if observed == "prior seed " {
                    report(&mut ml, "prior seed Hello. ", 18);
                }
                report(&mut ml, observed, cursor);
                assert_eq!(prediction(&mut ml), expected, "{observed:?}");
                assert_eq!(ml.metrics().summary().total_commits, 1);
            }

            let mut echoes = SentenceEchoes::new("prior seed hello", 16).unwrap();
            assert!(echoes.record(
                &KeyEvent {
                    key: Key::RawCode(52),
                    modifiers: Modifiers::empty(),
                },
                &[
                    Action::ReplaceWord {
                        replace_len: 5,
                        text: "Hello".into(),
                    },
                    Action::ForwardKey,
                ],
                false,
            ));
            // Standalone SURROUNDING replacement really can report deletion.
            assert!(echoes.acknowledge("prior seed ", 11));
            assert!(echoes.acknowledge("prior seed Hello.", 17));
        }

        #[test]
        fn sentence_boundary_surrounding_acknowledged_images_cannot_rewind() {
            let (mut ml, text, cursor, echoes) = own_sentence("prior seed ", "", true);
            report(&mut ml, &text, cursor);
            assert_eq!(prediction(&mut ml), "World");
            plain(&mut ml, Key::Backspace);
            report(&mut ml, &echoes[1].0, echoes[1].1);
            assert_eq!(prediction(&mut ml), "world");
            assert_eq!(ml.metrics().summary().total_commits, 1);
        }

        #[test]
        fn sentence_boundary_surrounding_unavailable_reports_cannot_lift_negative() {
            for report_kind in [0, 1, 2] {
                let mut ml = sentence(false, false);
                report(&mut ml, "hello.", 6);
                plain(&mut ml, Key::Space);
                report(&mut ml, "hello. seed ", 12);
                match report_kind {
                    0 => ml.set_surrounding_text(None, None),
                    1 => ml.set_surrounding_text(Some("hello.".into()), None),
                    _ => report(&mut ml, "hello.", 99),
                }
                assert_eq!(prediction(&mut ml), "world");
            }
            let mut ml = sentence(true, false);
            // No pre-edit report was available: a first valid report is not
            // proof that the unknown base was empty, even if it looks familiar.
            report(&mut ml, "hello. ", 7);
            assert_eq!(prediction(&mut ml), "world");
        }

        #[test]
        fn sentence_boundary_surrounding_return_requires_observed_linebreak() {
            let mut ml = fresh(false);
            report(&mut ml, "prior seed ", 11);
            for code in [35, 18, 38, 38, 24] {
                plain(&mut ml, Key::RawCode(code));
            }
            plain(&mut ml, Key::Return);
            report(&mut ml, "prior seed hello ", 17);
            assert_eq!(prediction(&mut ml), "world");
        }

        #[test]
        fn sentence_boundary_surrounding_overflow_preserves_canonical_word() {
            let (mut ml, _, _, _) = own_sentence("prior seed ", "", true);
            assert!(ml.core.has_sentence_boundary_hint());
            let mut typed = String::new();
            for _ in 0..MAX_SENTENCE_ECHOES {
                plain(&mut ml, Key::Char('a'));
                typed.push('a');
                assert_eq!(ml.current_word(), typed);
                assert_eq!(ml.metrics().summary().total_commits, 1);
            }
            assert!(ml.sentence_echoes.is_none());
            assert!(!ml.core.has_sentence_boundary_hint());
            assert!(ml.core.take_commit_learning().words.is_empty());
        }

        #[test]
        fn sentence_boundary_surrounding_lifecycle_does_not_restore_old_hint() {
            let (mut ml, text, cursor, _) = own_sentence("prior seed ", "", true);
            plain(&mut ml, Key::RawCode(17));
            assert_eq!(
                plain(&mut ml, Key::Escape),
                [Action::ShowComposing {
                    typed: "w".into(),
                    ghost: String::new(),
                }]
            );
            assert_eq!(ml.current_word(), "w");
            report(&mut ml, &text, cursor);
            assert!(ml.core.has_sentence_boundary_hint());
            assert!(ml.sentence_echoes.is_some());
            assert_eq!(ml.metrics().summary().total_commits, 1);

            for transition in [0, 1, 2, 3, 4] {
                let (mut ml, text, cursor, _) = own_sentence("prior seed ", "", true);
                match transition {
                    0 => {
                        ml.reset();
                    }
                    1 => {
                        ml.focus_lost();
                    }
                    2 => {
                        send(&mut ml, Key::Char('c'), Modifiers::CTRL);
                    }
                    3 => {
                        ml.focus_gained();
                    }
                    _ => {
                        assert_eq!(plain(&mut ml, Key::Escape), [Action::ForwardKey]);
                    }
                }
                assert!(ml.sentence_echoes.is_none());
                assert!(!ml.core.has_sentence_boundary_hint());
                report(&mut ml, &text, cursor);
                assert!(ml.sentence_echoes.is_none());
                assert!(!ml.core.has_sentence_boundary_hint());
            }
        }

        fn fresh_snapshot_after_sentence(post: bool, text: &str) -> MasterLoop {
            let mut ml = fresh(post);
            // Explicit empty report is a real anchor, never an inferred base.
            report(&mut ml, "", 0);
            for code in [35, 18, 38, 38, 24, 52, 57] {
                plain(&mut ml, Key::RawCode(code));
            }
            let prior = ml.light_profile.lang_prior;
            report(&mut ml, text, text.chars().count());
            assert_eq!(ml.light_profile.lang_prior, prior);
            assert_eq!(ml.metrics().summary().total_commits, 1);
            ml
        }

        #[test]
        fn sentence_boundary_surrounding_fresh_terminal_prediction_matches_current_report() {
            for (text, expected) in [
                ("hello. seed ", "world"),
                ("hello. seed.", "world"), // Adjacent input has no separator.
                ("hello. seed. ", "World"),
            ] {
                let mut ml = fresh_snapshot_after_sentence(false, text);
                plain(&mut ml, Key::RawCode(17));
                assert_eq!(ml.current_word(), "w");
                assert_eq!(
                    ml.predictions().first().map(|p| p.word.as_str()),
                    Some(expected),
                    "fresh report {text:?}"
                );
                assert_eq!(ml.metrics().summary().total_commits, 1);
            }
        }

        #[test]
        fn sentence_boundary_surrounding_fresh_pending_terminator_requires_real_space() {
            let mut ml = fresh_snapshot_after_sentence(false, "hello. seed.");
            assert_eq!(
                plain(&mut ml, Key::Space),
                [Action::HideGhost, Action::ForwardKey]
            );
            plain(&mut ml, Key::RawCode(17));
            assert_eq!(ml.current_word(), "w");
            assert_eq!(
                ml.predictions().first().map(|p| p.word.as_str()),
                Some("World")
            );
            assert_eq!(ml.metrics().summary().total_commits, 1);
            assert_eq!(ml.metrics().summary().accepted_predictions, 0);
        }

        #[test]
        fn sentence_boundary_surrounding_fresh_terminal_controls_optin_postcaps() {
            for (text, capitalize) in [("hello. seed ", false), ("hello. seed. ", true)] {
                let mut ml = fresh_snapshot_after_sentence(true, text);
                for code in [17, 24, 19, 38, 32] {
                    plain(&mut ml, Key::RawCode(code));
                }
                let actions = plain(&mut ml, Key::Space);
                let mut expected = vec![
                    Action::HideGhost,
                    Action::CommitText("world".into()),
                    Action::ForwardKey,
                ];
                if capitalize {
                    expected.push(Action::ReplaceWord {
                        replace_len: 5,
                        text: "World".into(),
                    });
                }
                assert_eq!(actions, expected, "fresh report {text:?}");
                assert!(ml.current_word().is_empty());
                assert_eq!(ml.metrics().summary().total_commits, 2);
                assert_eq!(ml.metrics().summary().accepted_predictions, 0);
            }
        }

        #[test]
        fn sentence_boundary_surrounding_fresh_reports_reconcile_both_directions() {
            for (first, second, expected) in [
                ("hello. seed. ", "hello. seed. other ", "world"),
                ("hello. seed ", "hello. seed. ", "World"),
            ] {
                let mut ml = fresh_snapshot_after_sentence(false, first);
                let prior = ml.light_profile.lang_prior;
                report(&mut ml, second, second.chars().count());
                assert_eq!(ml.light_profile.lang_prior, prior);
                plain(&mut ml, Key::RawCode(17));
                assert_eq!(ml.current_word(), "w");
                assert_eq!(
                    ml.predictions().first().map(|p| p.word.as_str()),
                    Some(expected),
                    "{first:?} -> {second:?}"
                );
                assert_eq!(ml.metrics().summary().total_commits, 1);
            }
        }

        #[test]
        fn sentence_boundary_surrounding_fresh_reports_preserve_preedit_and_user_case() {
            for (upper, text, word) in
                [(true, "hello. seed ", "Wo"), (false, "hello. seed. ", "wo")]
            {
                let mut ml = fresh_snapshot_after_sentence(false, "hello. ");
                send(
                    &mut ml,
                    Key::RawCode(17),
                    if upper {
                        Modifiers::SHIFT
                    } else {
                        Modifiers::empty()
                    },
                );
                plain(&mut ml, Key::RawCode(24));
                assert_eq!(ml.current_word(), word);
                let prior = ml.light_profile.lang_prior;
                report(&mut ml, text, text.chars().count());
                assert_eq!(ml.current_word(), word);
                assert!(ml.debug_state().0);
                assert_eq!(ml.light_profile.lang_prior, prior);
                plain(&mut ml, Key::RawCode(19));
                assert_eq!(ml.current_word(), if upper { "Wor" } else { "wor" });
                assert_eq!(
                    ml.predictions().first().map(|p| p.word.as_str()),
                    Some("World")
                );
                assert_eq!(ml.metrics().summary().total_commits, 1);
            }
        }

        #[test]
        fn sentence_boundary_surrounding_fresh_observed_linebreak_with_indent_starts_sentence() {
            let mut ml = fresh_snapshot_after_sentence(false, "hello. seed\n  ");
            plain(&mut ml, Key::RawCode(17));
            assert_eq!(ml.current_word(), "w");
            assert_eq!(
                ml.predictions().first().map(|p| p.word.as_str()),
                Some("World")
            );
            assert_eq!(ml.metrics().summary().total_commits, 1);
        }

        #[test]
        fn sentence_boundary_surrounding_own_forwarded_word_echo_keeps_token_start_decision() {
            for (base, expected) in [("hello. ", "World"), ("hello. seed ", "world")] {
                let mut ml = fresh_snapshot_after_sentence(false, base);
                let mut document = base.to_string();
                for ch in ['w', 'o'] {
                    let actions = plain(&mut ml, Key::Char(ch));
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
                    // Actual semantic Forward delivers this original glyph;
                    // ShowGhost is preedit, not document text.
                    document.push(ch);
                    report(&mut ml, &document, document.chars().count());
                }
                assert_eq!(ml.current_word(), "wo");
                plain(&mut ml, Key::RawCode(19));
                assert_eq!(ml.current_word(), "wor");
                assert_eq!(
                    ml.predictions().first().map(|p| p.word.as_str()),
                    Some(expected)
                );
                assert_eq!(ml.metrics().summary().total_commits, 1);
            }
        }

        #[test]
        fn sentence_boundary_surrounding_fresh_lease_retires_on_client_edits_and_word_completion() {
            for text in ["hello. seed. ", "hello. seed "] {
                for transition in [0, 1, 2, 3] {
                    let mut ml = fresh_snapshot_after_sentence(false, text);
                    assert!(ml.sentence_echoes.is_some());
                    match transition {
                        0 => {
                            plain(&mut ml, Key::Other(0xff63));
                        }
                        1 => {
                            send(&mut ml, Key::Char('v'), Modifiers::CTRL);
                        }
                        2 => {
                            assert_eq!(
                                plain(&mut ml, Key::Backspace),
                                [Action::HideGhost, Action::ForwardKey]
                            );
                            assert!(ml.current_word().is_empty());
                            assert_eq!(ml.metrics().summary().total_commits, 1);
                            assert_eq!(ml.metrics().summary().accepted_predictions, 0);
                            if text == "hello. seed. " {
                                // The observed plain Space was removed, leaving
                                // the original dot pending rather than opaque.
                                assert!(ml.sentence_echoes.is_some());
                                assert!(ml.core.has_sentence_boundary_hint());
                                report(&mut ml, "hello. seed.", 12);
                                assert!(ml.sentence_echoes.is_some());
                                assert!(ml.core.has_sentence_boundary_hint());
                                // Public behavior distinguishes pending from a
                                // confirmed start without a new Core test API.
                                let mut adjacent = fresh_snapshot_after_sentence(false, text);
                                plain(&mut adjacent, Key::Backspace);
                                plain(&mut adjacent, Key::RawCode(17));
                                assert_eq!(adjacent.current_word(), "w");
                                assert_eq!(
                                    adjacent.predictions().first().map(|p| p.word.as_str()),
                                    Some("world")
                                );
                                assert_eq!(
                                    plain(&mut ml, Key::Backspace),
                                    [Action::HideGhost, Action::ForwardKey]
                                );
                            }
                        }
                        _ => {
                            for code in [17, 24, 19, 38, 32] {
                                plain(&mut ml, Key::RawCode(code));
                            }
                            plain(&mut ml, Key::Space);
                            assert_eq!(ml.metrics().summary().total_commits, 2);
                        }
                    }
                    assert!(ml.current_word().is_empty());
                    assert!(ml.sentence_echoes.is_none());
                    assert!(!ml.core.has_sentence_boundary_hint());
                    // A discarded lease cannot be recreated just because an
                    // old report has a sentence-looking suffix or a false veto.
                    report(&mut ml, "hello. seed. ", 13);
                    assert!(ml.sentence_echoes.is_none());
                    assert!(!ml.core.has_sentence_boundary_hint());
                    if transition != 3 {
                        plain(&mut ml, Key::RawCode(17));
                        assert_eq!(ml.current_word(), "w");
                        assert_eq!(
                            ml.predictions().first().map(|p| p.word.as_str()),
                            Some("world")
                        );
                        assert_eq!(ml.metrics().summary().total_commits, 1);
                    }
                }
            }
        }

        #[test]
        fn sentence_boundary_surrounding_fresh_lease_ignores_unavailable_geometry() {
            for (text, expected) in [("hello. seed. ", "World"), ("hello. seed ", "world")] {
                for unavailable in [0, 1] {
                    let mut ml = fresh_snapshot_after_sentence(false, text);
                    assert!(ml.sentence_echoes.is_some());
                    if unavailable == 0 {
                        ml.set_surrounding_text(None, None);
                    } else {
                        report(&mut ml, "ordinary ", 999);
                    }
                    assert!(ml.sentence_echoes.is_some());
                    plain(&mut ml, Key::RawCode(17));
                    assert_eq!(ml.current_word(), "w");
                    assert_eq!(
                        ml.predictions().first().map(|p| p.word.as_str()),
                        Some(expected)
                    );
                    assert_eq!(ml.metrics().summary().total_commits, 1);
                }
            }
        }

        #[test]
        fn sentence_boundary_surrounding_fresh_lease_kill_and_reenable_retire_authority() {
            for text in ["hello. seed. ", "hello. seed "] {
                let mut ml = fresh_snapshot_after_sentence(false, text);
                assert!(ml.sentence_echoes.is_some());
                assert!(ml.is_enabled());
                assert_eq!(
                    send(&mut ml, Key::Escape, Modifiers::SUPER),
                    [Action::HideGhost]
                );
                assert!(!ml.is_enabled());
                assert!(ml.current_word().is_empty());
                assert!(ml.sentence_echoes.is_none());
                assert!(!ml.core.has_sentence_boundary_hint());
                report(&mut ml, "hello. seed. other. ", 20);
                assert!(ml.sentence_echoes.is_none());
                assert!(!ml.core.has_sentence_boundary_hint());
                assert_eq!(
                    send(&mut ml, Key::Escape, Modifiers::SUPER),
                    [Action::HideGhost]
                );
                assert!(ml.is_enabled());
                assert!(ml.sentence_echoes.is_none());
                assert!(!ml.core.has_sentence_boundary_hint());
                report(&mut ml, text, text.chars().count());
                assert!(ml.sentence_echoes.is_none());
                assert!(!ml.core.has_sentence_boundary_hint());
                assert_eq!(ml.metrics().summary().total_commits, 1);
                assert_eq!(ml.metrics().summary().accepted_predictions, 0);
            }
        }

        #[test]
        fn sentence_boundary_surrounding_observed_two_spaces_one_delete_keeps_prediction() {
            for own_echo in [false, true] {
                let mut document = "hello. seed.  ".to_string();
                let mut ml = fresh_snapshot_after_sentence(false, &document);
                assert_eq!(
                    plain(&mut ml, Key::Backspace),
                    [Action::HideGhost, Action::ForwardKey]
                );
                assert!(ml.current_word().is_empty());
                document.pop(); // Explicit synthetic client applies the forwarded Backspace.
                if own_echo {
                    report(&mut ml, &document, document.chars().count());
                }
                plain(&mut ml, Key::RawCode(17));
                assert_eq!(ml.current_word(), "w");
                assert_eq!(
                    ml.predictions().first().map(|p| p.word.as_str()),
                    Some("World"),
                    "own delete echo={own_echo}"
                );
                assert_eq!(ml.metrics().summary().total_commits, 1);
                assert_eq!(ml.metrics().summary().accepted_predictions, 0);
            }
        }

        #[test]
        fn sentence_boundary_surrounding_observed_space_delete_keeps_owned_postcaps() {
            let mut ml = fresh_snapshot_after_sentence(true, "hello. seed.  ");
            assert_eq!(
                plain(&mut ml, Key::Backspace),
                [Action::HideGhost, Action::ForwardKey]
            );
            for code in [17, 24, 19, 38, 32] {
                plain(&mut ml, Key::RawCode(code));
            }
            assert_eq!(
                plain(&mut ml, Key::Space),
                [
                    Action::HideGhost,
                    Action::CommitText("world".into()),
                    Action::ForwardKey,
                    Action::ReplaceWord {
                        replace_len: 5,
                        text: "World".into()
                    },
                ]
            );
            assert!(ml.current_word().is_empty());
            assert_eq!(ml.metrics().summary().total_commits, 2);
            assert_eq!(ml.metrics().summary().accepted_predictions, 0);
        }

        #[test]
        fn sentence_boundary_surrounding_observed_space_removal_and_reinsert_track_boundary() {
            for (deletions, separator, expected) in [
                (2, false, "world"), // No separator: adjacent input stays lowercase.
                (3, true, "world"),  // The original dot was removed too.
                (2, true, "World"),  // Surviving dot becomes a start on real Space.
            ] {
                let mut ml = fresh_snapshot_after_sentence(false, "hello. seed.  ");
                for _ in 0..deletions {
                    assert_eq!(
                        plain(&mut ml, Key::Backspace),
                        [Action::HideGhost, Action::ForwardKey]
                    );
                }
                if separator {
                    assert_eq!(
                        plain(&mut ml, Key::Space),
                        [Action::HideGhost, Action::ForwardKey]
                    );
                }
                plain(&mut ml, Key::RawCode(17));
                assert_eq!(ml.current_word(), "w");
                assert_eq!(
                    ml.predictions().first().map(|p| p.word.as_str()),
                    Some(expected),
                    "deleted={deletions}, separator={separator}"
                );
                assert_eq!(ml.metrics().summary().total_commits, 1);
                assert_eq!(ml.metrics().summary().accepted_predictions, 0);
            }
        }

        #[test]
        fn sentence_boundary_surrounding_observed_closers_and_repeated_terminators_retract() {
            for (text, tail_count) in [
                ("hello. seed.!)]  ", 3),
                ("hello. seed.!)]”  ", 4), // Count Unicode characters, not bytes.
                ("hello. seed)!  ", 0),    // The closer precedes the first terminator.
            ] {
                for (remove_first, expected) in [(true, "world"), (false, "World")] {
                    let mut ml = fresh_snapshot_after_sentence(false, text);
                    let deletions = 2 + tail_count + usize::from(remove_first);
                    // Remove both Spaces and all characters AFTER the first
                    // terminator; only remove that terminator in the negative arm.
                    for _ in 0..deletions {
                        assert_eq!(
                            plain(&mut ml, Key::Backspace),
                            [Action::HideGhost, Action::ForwardKey]
                        );
                    }
                    assert_eq!(
                        plain(&mut ml, Key::Space),
                        [Action::HideGhost, Action::ForwardKey]
                    );
                    plain(&mut ml, Key::RawCode(17));
                    assert_eq!(ml.current_word(), "w");
                    assert_eq!(
                        ml.predictions().first().map(|p| p.word.as_str()),
                        Some(expected),
                        "text={text:?}, deleted={deletions}"
                    );
                    assert_eq!(ml.metrics().summary().total_commits, 1);
                    assert_eq!(ml.metrics().summary().accepted_predictions, 0);
                }
            }
        }

        #[test]
        fn sentence_boundary_surrounding_mixed_observed_whitespace_requires_known_linebreak() {
            for (whitespace, expected) in [
                ("\t  ", "world"),
                ("\u{00a0}  ", "world"),
                ("\r\n  ", "World"), // A bound observed linebreak survives this deletion.
            ] {
                let mut ml =
                    fresh_snapshot_after_sentence(false, &format!("hello. seed.{whitespace}"));
                assert_eq!(
                    plain(&mut ml, Key::Backspace),
                    [Action::HideGhost, Action::ForwardKey]
                );
                assert_eq!(
                    plain(&mut ml, Key::Space),
                    [Action::HideGhost, Action::ForwardKey]
                );
                plain(&mut ml, Key::RawCode(17));
                assert_eq!(ml.current_word(), "w");
                assert_eq!(
                    ml.predictions().first().map(|p| p.word.as_str()),
                    Some(expected),
                    "opaque whitespace={whitespace:?}"
                );
                assert_eq!(ml.metrics().summary().total_commits, 1);
                assert_eq!(ml.metrics().summary().accepted_predictions, 0);
            }
        }

        #[test]
        fn sentence_boundary_surrounding_empty_space_keeps_fallback_veto_after_shortcut() {
            for shortcut in [false, true] {
                let mut ml = fresh(false);
                report(&mut ml, "", 0);
                for code in [35, 18, 38, 38, 24, 52] {
                    plain(&mut ml, Key::RawCode(code));
                }
                // Adapter refresh before Space makes anticipation cache the
                // punctuation-bearing old token, separate from the local hint.
                report(&mut ml, "hello.", 6);
                plain(&mut ml, Key::Space);
                report(&mut ml, "hello. seed.  ", 14);
                assert_eq!(
                    plain(&mut ml, Key::Space),
                    [Action::HideGhost, Action::ForwardKey]
                );
                if shortcut {
                    assert_eq!(
                        send(&mut ml, Key::Char('v'), Modifiers::CTRL),
                        [Action::ForwardKey]
                    );
                }
                plain(&mut ml, Key::RawCode(17));
                assert_eq!(ml.current_word(), "w");
                let expected = if shortcut { "world" } else { "World" };
                assert_eq!(
                    ml.predictions().first().map(|p| p.word.as_str()),
                    Some(expected),
                    "shortcut={shortcut}"
                );
                assert_eq!(ml.metrics().summary().total_commits, 1);
                assert_eq!(ml.metrics().summary().accepted_predictions, 0);
            }
        }

        #[test]
        fn sentence_boundary_surrounding_known_linebreak_indent_deletion_keeps_prediction() {
            for (prefix, linebreak, suffix) in [
                ("", "\n", ""),
                ("", "\r\n", ""),
                ("", "\n\n", ""),
                ("😀 ", "\n", " unchanged"),
            ] {
                for deletions in [1, 2] {
                    for own_echo in [false, true] {
                        let before = format!("{prefix}hello. seed.{linebreak}  ");
                        let mut text = format!("{before}{suffix}");
                        let mut cursor = before.chars().count();
                        let mut ml = fresh_snapshot_after_sentence(false, &text);
                        report(&mut ml, &text, cursor);
                        assert!(ml.core.has_sentence_boundary_hint());
                        assert!(ml.sentence_echoes.is_some());
                        for _ in 0..deletions {
                            assert_eq!(
                                plain(&mut ml, Key::Backspace),
                                [Action::HideGhost, Action::ForwardKey]
                            );
                            let byte = text.char_indices().nth(cursor - 1).unwrap().0;
                            text.remove(byte); // Explicit client applies one Unicode-char deletion.
                            cursor -= 1;
                            assert!(text.ends_with(suffix));
                            if own_echo {
                                report(&mut ml, &text, cursor);
                            }
                        }
                        plain(&mut ml, Key::RawCode(17));
                        assert_eq!(ml.current_word(), "w");
                        assert_eq!(
                            ml.predictions().first().map(|p| p.word.as_str()),
                            Some("World"),
                            "linebreak={linebreak:?}, deleted={deletions}, own_echo={own_echo}, prefix={prefix:?}"
                        );
                        assert_eq!(ml.metrics().summary().total_commits, 1);
                        assert_eq!(ml.metrics().summary().accepted_predictions, 0);
                    }
                }
            }
        }

        #[test]
        fn sentence_boundary_surrounding_known_linebreak_removal_reveals_literal_dot() {
            for linebreak in ["\n", "\r\n", "\n\n"] {
                for (remove_dot, separator, expected) in [
                    (false, false, "world"), // The surviving dot still needs whitespace.
                    (false, true, "World"),
                    (true, true, "world"),
                ] {
                    let mut text = format!("hello. seed.{linebreak}  ");
                    let mut ml = fresh_snapshot_after_sentence(false, &text);
                    for _ in 0..2 + linebreak.chars().count() + usize::from(remove_dot) {
                        assert_eq!(
                            plain(&mut ml, Key::Backspace),
                            [Action::HideGhost, Action::ForwardKey]
                        );
                        text.pop();
                        report(&mut ml, &text, text.chars().count());
                    }
                    assert_eq!(
                        text,
                        if remove_dot {
                            "hello. seed"
                        } else {
                            "hello. seed."
                        }
                    );
                    assert_eq!(ml.core.has_sentence_boundary_hint(), !remove_dot);
                    if separator {
                        assert_eq!(
                            plain(&mut ml, Key::Space),
                            [Action::HideGhost, Action::ForwardKey]
                        );
                    }
                    plain(&mut ml, Key::RawCode(17));
                    assert_eq!(ml.current_word(), "w");
                    assert_eq!(
                        ml.predictions().first().map(|p| p.word.as_str()),
                        Some(expected),
                        "linebreak={linebreak:?}, remove_dot={remove_dot}, separator={separator}"
                    );
                    assert_eq!(ml.metrics().summary().total_commits, 1);
                    assert_eq!(ml.metrics().summary().accepted_predictions, 0);
                }
            }
        }

        #[test]
        fn sentence_boundary_surrounding_known_linebreak_delete_keeps_owned_postcaps_once() {
            for own_echo in [false, true] {
                let mut ml = fresh_snapshot_after_sentence(true, "hello. seed.\n  ");
                assert_eq!(
                    plain(&mut ml, Key::Backspace),
                    [Action::HideGhost, Action::ForwardKey]
                );
                if own_echo {
                    report(&mut ml, "hello. seed.\n ", 14);
                }
                for code in [17, 24, 19, 38, 32] {
                    plain(&mut ml, Key::RawCode(code));
                }
                assert_eq!(ml.current_word(), "world");
                assert_eq!(
                    plain(&mut ml, Key::Space),
                    [
                        Action::HideGhost,
                        Action::CommitText("world".into()),
                        Action::ForwardKey,
                        Action::ReplaceWord {
                            replace_len: 5,
                            text: "World".into()
                        },
                    ],
                    "own_echo={own_echo}"
                );
                assert!(ml.current_word().is_empty());
                assert_eq!(ml.metrics().summary().total_commits, 2);
                assert_eq!(ml.metrics().summary().accepted_predictions, 0);
                assert!(ml.core.take_commit_learning().words.is_empty());
            }
        }

        #[test]
        fn sentence_boundary_surrounding_known_blank_deletion_and_revalidation_fences() {
            for fence in [0, 1, 2, 3, 4, 5] {
                let mut ml = match fence {
                    0 => fresh_snapshot_after_sentence(false, "hello\nseed "),
                    1 => {
                        let mut unbound = sentence(true, false);
                        report(&mut unbound, "hello. seed.\n  ", 15);
                        assert!(unbound.sentence_echoes.is_none());
                        unbound
                    }
                    2 => {
                        let mut retired = fresh_snapshot_after_sentence(false, "hello. seed.\n  ");
                        send(&mut retired, Key::Char('v'), Modifiers::CTRL);
                        report(&mut retired, "hello. seed.\n  ", 15);
                        assert!(retired.sentence_echoes.is_none());
                        retired
                    }
                    3 => {
                        let mut invalid = fresh_snapshot_after_sentence(false, "ordinary ");
                        report(&mut invalid, "hello. seed.\n  ", 999);
                        invalid
                    }
                    _ => {
                        let mut opaque = fresh_snapshot_after_sentence(false, "hello. seed.\n  ");
                        plain(&mut opaque, if fence == 4 { Key::Tab } else { Key::Return });
                        opaque // No client ACK makes either separator a known literal image.
                    }
                };
                assert_eq!(
                    plain(&mut ml, Key::Backspace),
                    [Action::HideGhost, Action::ForwardKey]
                );
                assert!(ml.current_word().is_empty());
                assert!(ml.sentence_echoes.is_none(), "fence={fence}");
                plain(&mut ml, Key::RawCode(17));
                assert_eq!(ml.current_word(), "w");
                assert_eq!(
                    ml.predictions().first().map(|p| p.word.as_str()),
                    Some("world"),
                    "fence={fence}"
                );
                assert_eq!(ml.metrics().summary().total_commits, 1);
                assert_eq!(ml.metrics().summary().accepted_predictions, 0);
            }

            // Deleting an already delivered semantic token is not an empty-token edit.
            let mut delivered = fresh_snapshot_after_sentence(false, "hello. seed.\n  ");
            let typed = plain(&mut delivered, Key::Char('a'));
            assert!(typed.contains(&Action::ForwardKey));
            assert_eq!(delivered.current_word(), "a");
            plain(&mut delivered, Key::Backspace);
            assert!(delivered.current_word().is_empty());
            assert!(delivered.sentence_echoes.is_none());
            plain(&mut delivered, Key::RawCode(17));
            assert_eq!(delivered.current_word(), "w");
            assert_eq!(
                delivered.predictions().first().map(|p| p.word.as_str()),
                Some("world")
            );
            assert_eq!(delivered.metrics().summary().total_commits, 1);
            assert_eq!(delivered.metrics().summary().accepted_predictions, 0);

            // Local preedit deletion never forwards a client Backspace or consumes the linebreak.
            let mut preedit = fresh_snapshot_after_sentence(false, "hello. seed.\n  ");
            plain(&mut preedit, Key::RawCode(17));
            assert_eq!(preedit.current_word(), "w");
            assert_eq!(
                preedit.predictions().first().map(|p| p.word.as_str()),
                Some("World")
            );
            assert_eq!(plain(&mut preedit, Key::Backspace), [Action::HideGhost]);
            assert!(preedit.current_word().is_empty());
            assert!(preedit.sentence_echoes.is_some());
            plain(&mut preedit, Key::RawCode(17));
            assert_eq!(preedit.current_word(), "w");
            assert_eq!(
                preedit.predictions().first().map(|p| p.word.as_str()),
                Some("World")
            );
            assert_eq!(preedit.metrics().summary().total_commits, 1);
            assert_eq!(preedit.metrics().summary().accepted_predictions, 0);

            for deletions in [1, 2, 3] {
                let mut ml = fresh_snapshot_after_sentence(false, "  ");
                for _ in 0..deletions {
                    assert_eq!(
                        plain(&mut ml, Key::Backspace),
                        [Action::HideGhost, Action::ForwardKey]
                    );
                }
                if deletions == 3 {
                    assert!(ml.sentence_echoes.is_none(), "past known document start");
                }
                plain(&mut ml, Key::RawCode(17));
                assert_eq!(ml.current_word(), "w");
                assert_eq!(
                    ml.predictions().first().map(|p| p.word.as_str()),
                    Some(if deletions < 3 { "World" } else { "world" })
                );
                assert_eq!(ml.metrics().summary().total_commits, 1);
                assert_eq!(ml.metrics().summary().accepted_predictions, 0);
            }
        }
    }
}
