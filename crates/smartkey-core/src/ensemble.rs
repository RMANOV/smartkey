// Ensemble scorer — combines corpus frequency, Markov context, and CVM personal boost.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use crate::bpe::BpeTokenizer;
use crate::cache::PredictionCache;
use crate::calibration::ConfidenceCalibrator;
use crate::cvm::{CvmCounter, CvmSnapshot};
use crate::hedge::HedgeMixer;
use crate::input::InputConfig;
use crate::lang_cvm::LangCvmTracks;
use crate::lang_detect::{LangId, LanguageDetector};
use crate::lang_model::LanguageModelBank;
use crate::markov::MarkovChain;
use crate::ngram::WordEntry;
use crate::personal::{extract_markov_snapshot, AdaptiveWeightsSnapshot, PersonalProfile};
use crate::reranker::NeuralReranker;
use crate::session_cache::SessionCacheLM;
use crate::tech_vocab::TechVocab;
use crate::typing_regime::TypingRegime;

/// A single prediction with its ensemble score and UI confidence.
#[derive(Debug, Clone)]
pub struct Prediction {
    pub word: String,
    pub score: f64,
    /// Normalised confidence in `[0.0, 1.0]` — the top candidate is ~1.0.
    pub confidence: f64,
}

/// Default blend factor for personal Markov vs corpus Markov.
const DEFAULT_PERSONAL_MARKOV_DELTA: f64 = 0.2;

/// Default learning rate for adaptive weight updates.
const ADAPTIVE_LR: f64 = 0.05;

/// Decay toward default weights every N commits.
const DECAY_INTERVAL: u32 = 20;

/// Decay rate toward default weights.
const DECAY_RATE: f64 = 0.1;

/// Session cache blend factor: personal_score = max(cvm, session_cache * BLEND).
const SESSION_BLEND: f64 = 0.5; // Previous: 0.4 — rollback if recent words dominate too much

/// Boost factor for per-language CVM track scores (v0.4.1).
const LANG_CVM_BOOST: f64 = 1.2; // Previous: 1.5 — rollback if language-specific boost is too weak

/// Unified prediction engine that blends four scoring signals:
///
/// * **Corpus frequency** (α) — how common the word is in the language model trie.
/// * **Markov context** (β) — bigram/trigram probability given recent words.
/// * **Personal boost** (γ) — CVM-tracked frequency of words *this* user types.
/// * **Tech vocabulary** (δ) — hardcoded tech terms; active only in FastCoding regime.
///
/// The final score is `α·corpus + β·markov + γ·personal + δ·tech`.
/// When δ > 0, the other three weights are proportionally rescaled to keep the sum = 1.
pub struct SmartKeyEngine {
    /// Per-language model bank (trie + Markov + PPM + KN per language).
    lang_models: LanguageModelBank,
    personal: CvmCounter,
    /// Per-language CVM tracks (v0.4.0) — separate vocabulary per detected language.
    personal_tracks: LangCvmTracks,
    /// Personal Markov chain — learns from user input, separate from corpus Markov.
    personal_markov: MarkovChain,
    /// Blend factor: `markov_score = (1-δ) * corpus_markov + δ * personal_markov`.
    personal_markov_delta: f64,
    alpha: f64,
    beta: f64,
    gamma: f64,
    /// Default weights for decay-toward-defaults.
    default_alpha: f64,
    default_beta: f64,
    default_gamma: f64,
    /// Number of word commits (for adaptive weight decay interval).
    commit_count: u32,
    fuzzy_max_edits: u8,
    fuzzy_discounts: [f64; 3],
    /// Prediction cache (RefCell: predict takes &self but cache needs mutation;
    /// RefCell is safe here because the Linux PyO3 bridge is single-threaded
    /// under the GIL and exposes unsendable classes).
    cache: RefCell<PredictionCache>,
    /// BPE tokenizer for OOV fallback (v0.4.0).
    bpe: Option<BpeTokenizer>,
    /// Session cache LM for burstiness tracking (v0.4.0).
    session_cache: SessionCacheLM,
    /// Hedge/Exp3 adaptive weight mixer (experimental, behind flag).
    hedge: Option<HedgeMixer>,
    /// Tech vocabulary for FastCoding regime boost (v0.5+).
    tech_vocab: TechVocab,
    /// Current typing regime for tech-boost gating (v0.5+).
    /// Updated by `InputMethodCore` on every word commit via `set_regime()`.
    current_regime: Option<TypingRegime>,
    /// Feature flags snapshot from config.
    use_session_cache_lm: bool,
    bpe_enabled: bool,
    use_kneser_ney: bool,
    use_hedge: bool,
    /// Confidence calibrator (F6): maps raw score ratios to calibrated probabilities.
    /// RefCell: `predict()` takes `&self` but calibration needs mutation; safe because
    /// SmartKeyEngine is single-threaded (see `cache` field comment above).
    calibrator: RefCell<ConfidenceCalibrator>,
    /// Neural reranker (Phase 3): tiny feedforward network that captures
    /// nonlinear feature interactions between scoring signals.
    reranker: NeuralReranker,
    /// Whether the neural reranker is enabled.
    use_reranker: bool,
}

impl SmartKeyEngine {
    /// Create a new engine with default weights `(0.50, 0.30, 0.20)` and a
    /// CVM counter sized `(500, 5000)`.
    pub fn new() -> Self {
        Self::from_config(&InputConfig::default())
    }

    /// Create a new engine from an `InputConfig`, respecting all tuning parameters.
    pub fn from_config(config: &InputConfig) -> Self {
        Self {
            lang_models: LanguageModelBank::new(config.markov_lambdas, config.use_ppm),
            personal: CvmCounter::new(config.cvm_initial_size, config.cvm_max_size)
                .with_decay_lambda(config.cvm_decay_lambda),
            personal_tracks: LangCvmTracks::new(
                config.cvm_initial_size,
                config.cvm_max_size,
                config.cvm_decay_lambda,
            ),
            personal_markov: MarkovChain::new(),
            personal_markov_delta: DEFAULT_PERSONAL_MARKOV_DELTA,
            alpha: config.weights.0,
            beta: config.weights.1,
            gamma: config.weights.2,
            default_alpha: config.weights.0,
            default_beta: config.weights.1,
            default_gamma: config.weights.2,
            commit_count: 0,
            fuzzy_max_edits: config.fuzzy_max_edits,
            fuzzy_discounts: config.fuzzy_discounts,
            cache: RefCell::new(PredictionCache::new()),
            bpe: None, // Loaded from corpus if bpe_enabled
            session_cache: SessionCacheLM::new(),
            tech_vocab: TechVocab::new(),
            current_regime: None,
            hedge: if config.use_hedge {
                // F16: 5-signal Hedge mixer (corpus, markov, personal, PPM, tech).
                // PPM and tech start with small default weights.
                let ppm_default = if config.use_ppm { 0.05 } else { 0.0 };
                let tech_default = 0.03;
                let base_scale = 1.0 - ppm_default - tech_default;
                Some(HedgeMixer::with_defaults(&[
                    config.weights.0 * base_scale,
                    config.weights.1 * base_scale,
                    config.weights.2 * base_scale,
                    ppm_default,
                    tech_default,
                ]))
            } else {
                None
            },
            use_session_cache_lm: config.use_session_cache_lm,
            bpe_enabled: config.bpe_enabled,
            use_kneser_ney: config.use_kneser_ney,
            use_hedge: config.use_hedge,
            calibrator: RefCell::new(ConfidenceCalibrator::new()),
            reranker: NeuralReranker::new(),
            use_reranker: config.use_reranker,
        }
    }

    /// Insert a word with its corpus frequency, auto-detecting language from script.
    pub fn load_word(&mut self, word: &str, frequency: u32) {
        let lang = Self::detect_word_lang(word);
        self.lang_models.load_word(word, frequency, lang);
    }

    /// Insert a word into a specific language model.
    pub fn load_word_lang(&mut self, word: &str, frequency: u32, lang: LangId) {
        self.lang_models.load_word(word, frequency, lang);
    }

    /// Train a bigram, auto-detecting language from the target word's script.
    pub fn load_bigram(&mut self, context: &str, word: &str, count: u32) {
        let lang = Self::detect_word_lang(word);
        self.lang_models.load_bigram(context, word, count, lang);
    }

    /// Train a bigram into a specific language model.
    pub fn load_bigram_lang(&mut self, context: &str, word: &str, count: u32, lang: LangId) {
        self.lang_models.load_bigram(context, word, count, lang);
    }

    /// Train a trigram, auto-detecting language from the target word's script.
    pub fn load_trigram(&mut self, w1: &str, w2: &str, word: &str, count: u32) {
        let lang = Self::detect_word_lang(word);
        self.lang_models.load_trigram(w1, w2, word, count, lang);
    }

    /// Train a trigram into a specific language model.
    pub fn load_trigram_lang(&mut self, w1: &str, w2: &str, word: &str, count: u32, lang: LangId) {
        self.lang_models.load_trigram(w1, w2, word, count, lang);
    }

    /// Detect language from a word's Unicode script (Latin → En, Cyrillic → Bg).
    fn detect_word_lang(word: &str) -> LangId {
        for ch in word.chars() {
            if let Some(lang) = LanguageDetector::instant_classify(ch) {
                return lang;
            }
        }
        LangId::En // default for ambiguous (digits, symbols)
    }

    /// Compute adaptive interpolation weights for all corpus Markov chains
    /// and train the neural reranker from corpus statistics.
    ///
    /// Called after corpus loading is complete. Each per-language Markov chain
    /// auto-tunes its λ weights based on corpus statistics (coverage and
    /// discrimination power) instead of using fixed [0.6, 0.3, 0.1].
    pub fn compute_adaptive_lambdas(&mut self) {
        for model in self.lang_models.models_mut() {
            model.markov.compute_adaptive_lambda();
        }
        // Train the neural reranker from corpus bigram data.
        if self.use_reranker {
            self.train_reranker_from_corpus();
        }
    }

    /// Train the neural reranker using corpus bigram co-occurrence data.
    ///
    /// For each bigram (context → word), generates training examples where the
    /// correct word should score highest. This teaches the reranker to recognise
    /// feature patterns that correlate with correct predictions.
    fn train_reranker_from_corpus(&mut self) {
        use crate::reranker::NUM_FEATURES;

        // Collect training examples from corpus bigrams.
        let mut examples: Vec<(Vec<[f64; NUM_FEATURES]>, usize)> = Vec::new();

        for model in self.lang_models.all_models() {
            let bigram_data = model.markov.bigram_data();
            for (context, (followers, total)) in bigram_data.iter().take(200) {
                if *total == 0 || followers.len() < 2 {
                    continue;
                }
                // Sort followers by count to identify the "best" word.
                let mut sorted: Vec<(&String, &u32)> = followers.iter().collect();
                sorted.sort_by(|a, b| b.1.cmp(a.1));
                let _target_word = sorted[0].0;

                // Generate features for each follower.
                let mut features_list: Vec<[f64; NUM_FEATURES]> = Vec::new();
                let max_count = *sorted[0].1 as f64;

                for (i, (word, count)) in sorted.iter().take(5).enumerate() {
                    let markov_ratio = **count as f64 / max_count;
                    let freq_rank = 1.0 / (1.0 + i as f64);
                    let prefix_cov = if word.len() >= 2 {
                        2.0 / word.len() as f64
                    } else {
                        1.0
                    };
                    features_list.push([
                        markov_ratio,                              // ensemble_score proxy
                        markov_ratio,                              // markov_ratio
                        0.0,        // personal_ratio (no personal data at corpus load)
                        prefix_cov, // prefix_coverage
                        freq_rank,  // frequency_rank
                        if context.len() > 3 { 0.4 } else { 0.2 }, // context_match proxy
                    ]);
                }

                if !features_list.is_empty() {
                    examples.push((features_list, 0)); // target is always index 0 (highest count)
                }
            }
        }

        if !examples.is_empty() {
            // Two training passes for convergence.
            self.reranker.train_batch(&examples);
            self.reranker.train_batch(&examples);
        }
    }

    /// Access the neural reranker (for persistence).
    pub fn reranker(&self) -> &NeuralReranker {
        &self.reranker
    }

    /// Restore reranker weights (from personal profile).
    pub fn set_reranker(&mut self, reranker: NeuralReranker) {
        self.reranker = reranker;
    }

    /// Feed a word into the personal CVM layer (call when the user types a word).
    pub fn learn(&mut self, word: &str) {
        self.personal.process(word);
        self.cache.borrow_mut().invalidate();
    }

    /// Feed a word into the per-language CVM track.
    pub fn learn_with_lang(&mut self, word: &str, lang: LangId) {
        self.personal_tracks.learn(word, lang);
        self.cache.borrow_mut().invalidate();
    }

    /// Set the BPE tokenizer (loaded from corpus).
    pub fn set_bpe(&mut self, bpe: BpeTokenizer) {
        self.bpe = Some(bpe);
    }

    /// Record a word commit in the session cache for burstiness tracking.
    pub fn observe_session(&mut self, word: &str) {
        self.session_cache.observe(word);
    }

    /// Learn an OOV word by adding it to the language model trie (F11).
    ///
    /// When the user types a word not in the corpus, add it with a low
    /// initial frequency so it appears in future prefix searches. Also
    /// trains the Markov unigram for frequency-weighted backoff.
    pub fn learn_oov_word(&mut self, word: &str, lang: LangId) {
        // Use a modest initial frequency — enough to appear in candidates
        // but not dominate corpus words. Will grow via CVM learning.
        const OOV_INITIAL_FREQ: u32 = 5;
        self.lang_models.load_word(word, OOV_INITIAL_FREQ, lang);
        self.cache.borrow_mut().invalidate();
    }

    /// Clear the session cache (e.g. on session restart).
    pub fn clear_session_cache(&mut self) {
        self.session_cache.clear();
    }

    /// Build the Kneser-Ney scorer and collocation detector from loaded Markov data.
    ///
    /// Call after corpus loading is complete. Builds per-language KN scorers
    /// (when `use_kneser_ney` flag is set) and collocation detectors (F9).
    pub fn build_kneser_ney(&mut self) {
        for model in self.lang_models.models_mut() {
            // Build KN scorer if enabled.
            if self.use_kneser_ney {
                use crate::kneser_ney::KneserNeyScorer;
                model.kn_scorer = Some(KneserNeyScorer::from_markov_data(
                    model.markov.bigrams_raw(),
                    model.markov.trigrams_raw(),
                ));
            }

            // Build collocation detector (F9) from bigram + unigram data.
            let bigram_flat: std::collections::HashMap<
                String,
                std::collections::HashMap<String, u32>,
            > = model
                .markov
                .bigrams_raw()
                .iter()
                .map(|(ctx, (followers, _))| (ctx.clone(), followers.clone()))
                .collect();
            let total_bi: u64 = bigram_flat
                .values()
                .flat_map(|f| f.values())
                .map(|&c| c as u64)
                .sum();
            model.collocation = crate::collocation::CollocationDetector::build(
                &bigram_flat,
                model.markov.unigram_counts_raw(),
                total_bi,
                model.markov.total_unigram_count(),
            );

            // Build BG morphological grouping (F10) for BG models.
            if model.lang == crate::lang_detect::LangId::Bg {
                model.morphology =
                    crate::bg_morphology::BgMorphology::build(model.markov.unigram_counts_raw());
            }
        }
    }

    /// Train the personal Markov chain with online bigram/trigram data.
    ///
    /// Called from `commit_word()` with the preceding context words.
    pub fn learn_online_markov(&mut self, prev1: Option<&str>, prev2: Option<&str>, word: &str) {
        if let Some(ctx) = prev1 {
            self.personal_markov.train_bigram(ctx, word, 1);
        }
        if let (Some(w1), Some(w2)) = (prev2, prev1) {
            self.personal_markov.train_trigram(w1, w2, word, 1);
        }
        self.cache.borrow_mut().invalidate();
    }

    /// Train the personal Markov chain with higher-order n-gram data (4-gram+).
    ///
    /// Called from `commit_word()` with the full context window for ∞-gram
    /// backoff learning. Enables longer-context predictions over time.
    pub fn learn_online_ngram(&mut self, context: &[&str], word: &str) {
        self.personal_markov.train_ngram(context, word, 1);
        self.cache.borrow_mut().invalidate();
    }

    /// Record a calibration observation (F6).
    ///
    /// `confidence`: raw confidence of the top prediction at accept/reject time.
    /// `accepted`: whether the user accepted it (Tab) or rejected (Escape/typing).
    pub fn observe_calibration(&self, confidence: f64, accepted: bool) {
        self.calibrator.borrow_mut().observe(confidence, accepted);
    }

    /// Handle a prediction being accepted (Tab press) — update adaptive weights.
    ///
    /// Identifies the dominant scoring component for the accepted word and
    /// shifts weights toward it using an EMA update.
    pub fn on_prediction_accepted(&mut self, word: &str, context: &[&str]) {
        let prev1 = context.last().copied();
        let prev2 = if context.len() >= 2 {
            Some(context[context.len() - 2])
        } else {
            None
        };

        // Recompute raw scores for the accepted word (search all language models).
        let corpus_score = self
            .lang_models
            .all_models()
            .filter_map(|m| {
                m.trie
                    .prefix_search(word, 1)
                    .first()
                    .map(|e| e.frequency as f64)
            })
            .fold(0.0_f64, f64::max);
        let markov_score = self
            .lang_models
            .all_models()
            .map(|m| m.markov.score_with_backoff(word, prev1, prev2))
            .fold(1e-6_f64, f64::max);
        let personal_score = self.personal.frequency_score(word);

        if self.use_hedge {
            // F16: Hedge/Exp3 multiplicative weights update with 5 signals.
            // Compute PPM and tech scores for the accepted word.
            let ppm_score = self
                .lang_models
                .all_models()
                .filter_map(|m| m.ppm.as_ref())
                .map(|ppm| ppm.score_candidate("", word))
                .fold(0.0_f64, f64::max);
            let tech_score = self
                .tech_vocab
                .score(word, 1)
                .first()
                .map(|(_, s)| *s)
                .unwrap_or(0.0);

            // Normalize raw scores to [0,1] rewards.
            let max_s = corpus_score
                .max(markov_score)
                .max(personal_score)
                .max(ppm_score)
                .max(tech_score)
                .max(1e-6);

            if let Some(ref mut hedge) = self.hedge {
                if hedge.signal_count() == 5 {
                    let rewards = [
                        corpus_score / max_s,
                        markov_score / max_s,
                        personal_score / max_s,
                        ppm_score / max_s,
                        tech_score / max_s,
                    ];
                    hedge.update(&rewards);
                    // Extract updated weights — only use the first 3 for α/β/γ
                    // (PPM and tech are applied in the scoring pipeline directly).
                    let (a, b, g, _, _) = hedge.weights_five();
                    // Renormalize the first 3 to sum ≈ 1.0 for compatibility.
                    let sum3 = a + b + g;
                    if sum3 > 0.0 {
                        self.alpha = a / sum3;
                        self.beta = b / sum3;
                        self.gamma = g / sum3;
                    }
                } else {
                    // Backward compat: 3-signal hedge.
                    let rewards = [
                        corpus_score / max_s,
                        markov_score / max_s,
                        personal_score / max_s,
                    ];
                    hedge.update(&rewards);
                    let (a, b, g) = hedge.weights_triple();
                    self.alpha = a;
                    self.beta = b;
                    self.gamma = g;
                }
            }
        } else {
            // EMA update toward dominant component.
            let scores = [
                (0usize, corpus_score),
                (1, markov_score),
                (2, personal_score),
            ];
            let dominant = scores
                .iter()
                .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
                .map(|&(idx, _)| idx)
                .unwrap_or(0);

            let mut new_weights = [self.alpha, self.beta, self.gamma];
            new_weights[dominant] += ADAPTIVE_LR * (1.0 - new_weights[dominant]);

            // Renormalize to sum = 1.0.
            let sum: f64 = new_weights.iter().sum();
            if sum > 0.0 {
                self.alpha = new_weights[0] / sum;
                self.beta = new_weights[1] / sum;
                self.gamma = new_weights[2] / sum;
            }
        }

        self.commit_count += 1;

        // Decay toward defaults every DECAY_INTERVAL commits (EMA path only).
        if !self.use_hedge && self.commit_count.is_multiple_of(DECAY_INTERVAL) {
            self.alpha += DECAY_RATE * (self.default_alpha - self.alpha);
            self.beta += DECAY_RATE * (self.default_beta - self.beta);
            self.gamma += DECAY_RATE * (self.default_gamma - self.gamma);
            // Renormalize.
            let sum = self.alpha + self.beta + self.gamma;
            if sum > 0.0 {
                self.alpha /= sum;
                self.beta /= sum;
                self.gamma /= sum;
            }
        }

        self.cache.borrow_mut().invalidate();
    }

    /// Handle a prediction being rejected (Escape press) — penalise adaptive weights.
    ///
    /// When Hedge is active, applies a neutral update so decay-toward-defaults fires
    /// on schedule (proportional to rejection events). EMA path is a no-op since EMA
    /// only reacts to positive feedback (Tab).
    pub fn on_prediction_rejected(&mut self) {
        if self.use_hedge {
            self.commit_count += 1;
            if self.commit_count.is_multiple_of(DECAY_INTERVAL) {
                if let Some(ref mut hedge) = self.hedge {
                    // Neutral rewards: no signal gains; decay toward defaults fires.
                    hedge.update(&[0.5, 0.5, 0.5]);
                    let (a, b, g) = hedge.weights_triple();
                    self.alpha = a;
                    self.beta = b;
                    self.gamma = g;
                }
            }
            self.cache.borrow_mut().invalidate();
        }
    }

    /// Export the full personal profile (CVM + Markov + weights + lang CVM).
    pub fn export_personal_profile(&self) -> PersonalProfile {
        let cvm = self.personal.to_snapshot();
        let markov = extract_markov_snapshot(
            self.personal_markov.bigrams_raw(),
            self.personal_markov.trigrams_raw(),
        );
        let weights = Some(AdaptiveWeightsSnapshot {
            alpha: self.alpha,
            beta: self.beta,
            gamma: self.gamma,
            personal_markov_delta: self.personal_markov_delta,
            commit_count: self.commit_count,
        });
        let lang_cvm = self.personal_tracks.to_snapshot();
        PersonalProfile::with_lang_cvm(cvm, markov, weights, lang_cvm)
    }

    /// Import a full personal profile (CVM + Markov + weights + lang CVM).
    pub fn import_personal_profile(&mut self, profile: &PersonalProfile) {
        self.personal = CvmCounter::from_snapshot(&profile.cvm);

        // Restore per-language CVM tracks if present (v3).
        if let Some(ref lang_snap) = profile.lang_cvm {
            self.personal_tracks = LangCvmTracks::from_snapshot(lang_snap);
        }

        // Restore personal Markov.
        self.personal_markov = MarkovChain::new();
        for (ctx, word, count) in &profile.markov_bigrams {
            self.personal_markov.train_bigram(ctx, word, *count);
        }
        for (w1, w2, word, count) in &profile.markov_trigrams {
            self.personal_markov.train_trigram(w1, w2, word, *count);
        }

        // Restore adaptive weights.
        if let Some(w) = &profile.weights {
            self.alpha = w.alpha;
            self.beta = w.beta;
            self.gamma = w.gamma;
            self.personal_markov_delta = w.personal_markov_delta;
            self.commit_count = w.commit_count;
        }

        self.cache.borrow_mut().invalidate();
    }

    /// Export the personal CVM layer as a portable snapshot (backward compat).
    pub fn export_personal(&self) -> CvmSnapshot {
        self.personal.to_snapshot()
    }

    /// Replace the personal CVM layer from a snapshot (backward compat).
    pub fn import_personal(&mut self, snapshot: &CvmSnapshot) {
        self.personal = CvmCounter::from_snapshot(snapshot);
        self.cache.borrow_mut().invalidate();
    }

    /// Fast candidate count for a prefix (without full scoring).
    ///
    /// Searches across all language models.
    pub fn candidate_count(&self, prefix: &str, limit: usize) -> usize {
        self.lang_models
            .all_models()
            .map(|m| m.trie.prefix_search(prefix, limit).len())
            .sum()
    }

    /// Return the corpus frequency of the top trie candidate for `prefix`.
    ///
    /// Searches across all language models and returns the maximum.
    pub fn max_prefix_frequency(&self, prefix: &str) -> u32 {
        self.lang_models
            .all_models()
            .filter_map(|m| m.trie.prefix_search(prefix, 1).first().map(|e| e.frequency))
            .max()
            .unwrap_or(0)
    }

    /// Update the current typing regime (called by `InputMethodCore` after each word commit).
    ///
    /// The regime gates tech vocabulary δ weight and applies regime-specific
    /// weight adjustments in `predict()`:
    /// - **FastCoding:** δ=0.15 (tech vocab), α-0.05, β-0.05 (rescaled)
    /// - **DeliberateProse:** α+0.10 (favor corpus for careful writing)
    /// - **NewTopic:** γ+0.10 (lean on personal vocab in unknown domains)
    /// - **MixedLanguage/LanguageSwitch:** no weight change (handled by lang detection)
    pub fn set_regime(&mut self, regime: TypingRegime) {
        self.current_regime = Some(regime);
        self.cache.borrow_mut().invalidate();
    }

    /// Returns (Δα, Δβ, Δγ) weight adjustments for the current regime.
    ///
    /// Applied additively in `predict()` before normalisation.
    fn regime_weight_deltas(&self) -> (f64, f64, f64) {
        match self.current_regime {
            Some(TypingRegime::DeliberateProse) => (0.10, -0.05, -0.05),
            Some(TypingRegime::NewTopic) => (-0.05, -0.05, 0.10),
            // FastCoding is handled separately via TECH_DELTA.
            // MixedLanguage and LanguageSwitch don't need weight shifts.
            _ => (0.0, 0.0, 0.0),
        }
    }

    /// Exact word frequency in a specific language model (for auto-correction).
    ///
    /// Returns 0.0 if the word is not found.
    pub fn word_frequency(&self, word: &str, lang: LangId) -> f64 {
        self.lang_models
            .get(lang)
            .and_then(|m| {
                m.trie
                    .prefix_search(word, 1)
                    .first()
                    .filter(|e| e.word == word)
                    .map(|e| e.frequency as f64)
            })
            .unwrap_or(0.0)
    }

    /// Score two prefixes (EN and BG interpretations) for dual-buffer comparison.
    ///
    /// Uses per-language models directly for precise scoring: EN prefix against
    /// the EN model, BG prefix against the BG model. No cross-contamination.
    pub fn score_both(&self, en_prefix: &str, bg_prefix: &str) -> (f64, f64) {
        let en = self
            .lang_models
            .get(LangId::En)
            .and_then(|m| {
                m.trie
                    .prefix_search(en_prefix, 1)
                    .first()
                    .map(|e| e.frequency as f64)
            })
            .unwrap_or(0.0);
        let bg = self
            .lang_models
            .get(LangId::Bg)
            .and_then(|m| {
                m.trie
                    .prefix_search(bg_prefix, 1)
                    .first()
                    .map(|e| e.frequency as f64)
            })
            .unwrap_or(0.0);
        (en, bg)
    }

    /// Produce up to `limit` predictions for the given prefix and context.
    ///
    /// * `prefix` — the characters typed so far for the current word.
    /// * `context` — preceding words, most recent last (e.g. `&["i", "love"]`).
    /// * `limit` — maximum number of predictions to return.
    ///
    /// The pipeline:
    /// 1. Trie prefix search to gather `limit * 3` candidates.
    /// 2. Score each candidate on three axes (corpus, markov, personal).
    /// 3. Blend with `α·corpus + β·markov + γ·personal`.
    /// 4. Sort descending, truncate to `limit`, normalise confidence.
    ///
    /// When `lang` is `Some`, per-language CVM tracks are consulted for scoring
    /// boost (v0.4.1). Pass `None` for backward-compatible behavior.
    pub fn predict(
        &self,
        prefix: &str,
        context: &[&str],
        limit: usize,
        lang: Option<LangId>,
    ) -> Vec<Prediction> {
        if limit == 0 {
            return Vec::new();
        }

        // Check cache first.
        if let Some(cached) = self.cache.borrow_mut().get(prefix, context) {
            return cached;
        }

        let candidate_pool = limit * 3;
        let models = self.lang_models.get_or_all(lang);

        // Step 1: candidate generation from per-language model(s).
        // When PPM is enabled and prefix is short (≤ 1 char), use PPM to guide
        // candidate generation — rank_next_chars() identifies the most likely
        // next characters, then we search each model's trie with extended prefix.
        let ppm_guided =
            models.iter().any(|m| m.ppm.is_some()) && !prefix.is_empty() && prefix.len() <= 1;

        let candidates = {
            let mut result: Vec<WordEntry> = Vec::new();
            let mut seen = HashSet::new();

            if ppm_guided {
                for model in &models {
                    if let Some(ref ppm) = model.ppm {
                        let next_chars = ppm.rank_next_chars(prefix, 5);
                        let per_char_pool = candidate_pool / next_chars.len().max(1);
                        for (ch, _) in &next_chars {
                            let extended = format!("{}{}", prefix, ch);
                            for entry in model.trie.prefix_search(&extended, per_char_pool) {
                                if seen.insert(entry.word.clone()) {
                                    result.push(entry);
                                }
                            }
                        }
                    }
                }
            }
            // Regular prefix search from all relevant models.
            for model in &models {
                for entry in model.trie.prefix_search(prefix, candidate_pool) {
                    if seen.insert(entry.word.clone()) {
                        result.push(entry);
                    }
                }
            }
            result
        };

        // Step 1b: fuzzy fallback — if exact prefix returned fewer than `limit`
        // candidates, fill the gap with fuzzy matches from all relevant models.
        let (fuzzy_candidates, discount_map) = if candidates.len() < limit && !prefix.is_empty() {
            let fuzzy_slots = candidate_pool.saturating_sub(candidates.len());
            let exact_words: HashSet<&str> = candidates.iter().map(|c| c.word.as_str()).collect();

            let mut discount_map: HashMap<String, f64> = HashMap::new();
            let mut merged: Vec<WordEntry> = Vec::new();
            for model in &models {
                let fuzzy_matches =
                    model
                        .trie
                        .fuzzy_search(prefix, self.fuzzy_max_edits, fuzzy_slots);
                for fm in &fuzzy_matches {
                    if exact_words.contains(fm.word.as_str()) || discount_map.contains_key(&fm.word)
                    {
                        continue;
                    }
                    let discount = self.fuzzy_discounts[fm.edit_distance.min(2) as usize];
                    discount_map.insert(fm.word.clone(), discount);
                    merged.push(WordEntry {
                        word: fm.word.clone(),
                        frequency: fm.frequency,
                    });
                }
            }
            (merged, discount_map)
        } else {
            (Vec::new(), HashMap::new())
        };

        // If both exact and fuzzy are empty, try BPE fallback, then tech vocab fallback.
        if candidates.is_empty() && fuzzy_candidates.is_empty() {
            if self.bpe_enabled {
                if let Some(ref bpe) = self.bpe {
                    let bpe_suggestions = bpe.suggest_completions(prefix, limit);
                    if !bpe_suggestions.is_empty() {
                        let mut preds: Vec<Prediction> = bpe_suggestions
                            .into_iter()
                            .map(|(word, score)| Prediction {
                                word,
                                score,
                                confidence: 0.0,
                            })
                            .collect();
                        let score_sum: f64 = preds.iter().map(|p| p.score).sum();
                        if score_sum > 0.0 {
                            for p in &mut preds {
                                p.confidence = (p.score / score_sum).clamp(0.0, 1.0);
                            }
                        }
                        preds.truncate(limit);
                        self.cache.borrow_mut().put(prefix, context, preds.clone());
                        return preds;
                    }
                }
            }

            // Tech vocab fallback: when corpus has zero matches but tech vocab does,
            // return tech suggestions directly (activated regardless of regime —
            // they are the only candidates available).
            let tech_hits = self.tech_vocab.score(prefix, limit);
            if !tech_hits.is_empty() {
                let mut preds: Vec<Prediction> = tech_hits
                    .into_iter()
                    .map(|(word, score)| Prediction {
                        word,
                        score,
                        confidence: 0.0,
                    })
                    .collect();
                let score_sum: f64 = preds.iter().map(|p| p.score).sum();
                if score_sum > 0.0 {
                    for p in &mut preds {
                        p.confidence = (p.score / score_sum).clamp(0.0, 1.0);
                    }
                }
                preds.truncate(limit);
                self.cache.borrow_mut().put(prefix, context, preds.clone());
                return preds;
            }

            return Vec::new();
        }

        // Combine all candidates for scoring.
        let all_candidates: Vec<&WordEntry> =
            candidates.iter().chain(fuzzy_candidates.iter()).collect();

        // Determine the max corpus frequency among all candidates for normalisation.
        let max_freq = all_candidates
            .iter()
            .map(|c| c.frequency)
            .max()
            .unwrap_or(1)
            .max(1) as f64;

        // Derive Markov context pointers: prev1 = last word, prev2 = second-to-last.
        let prev1 = context.last().copied();
        let prev2 = if context.len() >= 2 {
            Some(context[context.len() - 2])
        } else {
            None
        };

        // Tech vocab scores: build a word→score map once per predict() call.
        // Only populated when regime is FastCoding (δ > 0.0), avoiding the
        // HashMap allocation in the common case.
        const TECH_DELTA: f64 = 0.15;
        let is_fast_coding = matches!(self.current_regime, Some(TypingRegime::FastCoding));
        let tech_score_map: HashMap<String, f64> = if is_fast_coding && !prefix.is_empty() {
            self.tech_vocab
                .score(prefix, candidate_pool)
                .into_iter()
                .collect()
        } else {
            HashMap::new()
        };

        // Single pass: compute raw scores and track maxima simultaneously.
        // This avoids calling markov.score_with_backoff() and personal.frequency_score()
        // twice per candidate (was: once for max, once for scoring).
        struct RawScores {
            corpus: f64,
            markov: f64,
            personal: f64,
            ppm: f64,
            tech: f64,
        }

        // Stack-allocate for the common case (≤30 candidates; default limit*3=15).
        // Falls back to Vec for unusually large candidate pools.
        let mut raw_small = arrayvec::ArrayVec::<RawScores, 30>::new();
        let mut raw_large = Vec::new();
        let mut max_markov = 1e-6_f64;
        let mut max_personal = 1.0_f64;
        let mut max_ppm = 1e-6_f64;
        let mut max_tech = 1e-6_f64;
        let use_stack = all_candidates.len() <= 30;

        for c in &all_candidates {
            // Blend corpus Markov and personal Markov.
            // Use the best score across relevant language models.
            // Corpus models use trigram backoff; personal model uses ∞-gram
            // extended backoff (leverages higher-order n-grams from user typing).
            let corpus_m = models
                .iter()
                .map(|model| {
                    if self.use_kneser_ney {
                        if let Some(ref kn) = model.kn_scorer {
                            return kn.score(&c.word, prev1, prev2);
                        }
                    }
                    model.markov.score_with_backoff(&c.word, prev1, prev2)
                })
                .fold(1e-6_f64, f64::max);
            let personal_m = self
                .personal_markov
                .score_extended_backoff(&c.word, context);
            let m = (1.0 - self.personal_markov_delta) * corpus_m
                + self.personal_markov_delta * personal_m;
            let cvm_score = self.personal.frequency_score(&c.word);
            // Language-conditioned CVM boost (v0.4.1): consult per-language track.
            let lang_boost = if let Some(lang) = lang {
                self.personal_tracks.frequency_score(&c.word, lang) * LANG_CVM_BOOST
            } else {
                0.0
            };
            let base_personal = cvm_score.max(lang_boost);
            // Blend session cache into personal score if enabled.
            let p = if self.use_session_cache_lm {
                base_personal.max(self.session_cache.score(&c.word) * SESSION_BLEND)
            } else {
                base_personal
            };
            // PPM scoring: best across relevant language models.
            let ppm_score = models
                .iter()
                .filter_map(|m| m.ppm.as_ref())
                .map(|ppm| ppm.score_candidate(prefix, &c.word))
                .fold(0.0_f64, f64::max);
            // Tech vocab scoring (non-zero only in FastCoding regime).
            let tech_score = tech_score_map
                .get(&c.word.to_lowercase())
                .copied()
                .unwrap_or(0.0);
            max_markov = max_markov.max(m);
            max_personal = max_personal.max(p);
            max_ppm = max_ppm.max(ppm_score);
            max_tech = max_tech.max(tech_score);
            let scores = RawScores {
                corpus: c.frequency as f64,
                markov: m,
                personal: p,
                ppm: ppm_score,
                tech: tech_score,
            };
            if use_stack {
                // SAFETY: use_stack guarantees len <= 30.
                raw_small.push(scores);
            } else {
                raw_large.push(scores);
            }
        }

        let raw: &[RawScores] = if use_stack { &raw_small } else { &raw_large };

        // Normalize and blend in a second pass (but no redundant score calls).
        //
        // Apply regime-specific weight adjustments (F4), then PPM/tech borrowing.
        let (da, db, dg) = self.regime_weight_deltas();
        let base_alpha = (self.alpha + da).max(0.05);
        let base_beta = (self.beta + db).max(0.05);
        let base_gamma = (self.gamma + dg).max(0.05);
        // Renormalize base weights to sum = 1.0 after regime deltas.
        let base_sum_raw = base_alpha + base_beta + base_gamma;
        let (base_alpha, base_beta, base_gamma) = if base_sum_raw.is_finite() && base_sum_raw > 0.0
        {
            (
                base_alpha / base_sum_raw,
                base_beta / base_sum_raw,
                base_gamma / base_sum_raw,
            )
        } else {
            (self.alpha, self.beta, self.gamma)
        };

        // When PPM is active, borrow a share from β (markov) for short prefixes.
        // When FastCoding, tech gets δ = 0.15 borrowed proportionally from α, β, γ.
        let has_ppm = models.iter().any(|m| m.ppm.is_some());
        let (eff_alpha, eff_beta, eff_gamma, eff_delta, eff_tech) = if has_ppm && prefix.len() <= 1
        {
            // Short prefix: PPM gets 0.15 borrowed from markov.
            let delta = 0.15_f64.min(base_beta * 0.5);
            let (a, b, g) = (base_alpha, base_beta - delta, base_gamma);
            if is_fast_coding {
                // Rescale (a, b, g) so that sum = 1 - TECH_DELTA.
                let base_sum = a + b + g;
                let scale = if base_sum > 0.0 {
                    (1.0 - TECH_DELTA) / base_sum
                } else {
                    1.0
                };
                (a * scale, b * scale, g * scale, delta * scale, TECH_DELTA)
            } else {
                (a, b, g, delta, 0.0)
            }
        } else if has_ppm {
            // Longer prefix: PPM gets a smaller share.
            let delta = 0.05_f64.min(base_beta * 0.2);
            let (a, b, g) = (base_alpha, base_beta - delta, base_gamma);
            if is_fast_coding {
                let base_sum = a + b + g;
                let scale = if base_sum > 0.0 {
                    (1.0 - TECH_DELTA) / base_sum
                } else {
                    1.0
                };
                (a * scale, b * scale, g * scale, delta * scale, TECH_DELTA)
            } else {
                (a, b, g, delta, 0.0)
            }
        } else if is_fast_coding {
            // No PPM, but FastCoding: tech gets TECH_DELTA, others rescaled.
            let sum3 = base_alpha + base_beta + base_gamma;
            let scale = if sum3 > 0.0 {
                (1.0 - TECH_DELTA) / sum3
            } else {
                1.0
            };
            (
                base_alpha * scale,
                base_beta * scale,
                base_gamma * scale,
                0.0,
                TECH_DELTA,
            )
        } else {
            (base_alpha, base_beta, base_gamma, 0.0, 0.0)
        };

        let mut scored: Vec<Prediction> = all_candidates
            .iter()
            .zip(raw.iter())
            .map(|(entry, r)| {
                let corpus_score = r.corpus / max_freq;
                let markov_score = r.markov / max_markov;
                let personal_score = r.personal / max_personal;
                let ppm_score = if max_ppm > 1e-6 { r.ppm / max_ppm } else { 0.0 };
                let tech_score = if max_tech > 1e-6 {
                    r.tech / max_tech
                } else {
                    0.0
                };

                let mut score = eff_alpha * corpus_score
                    + eff_beta * markov_score
                    + eff_gamma * personal_score
                    + eff_delta * ppm_score
                    + eff_tech * tech_score;

                // F9: Collocation boost — multiply score by PMI-derived factor
                // if (context, word) is a known collocation.
                let collocation_boost = models
                    .iter()
                    .map(|m| m.collocation.boost(&entry.word, prev1))
                    .fold(1.0_f64, f64::max);
                score *= collocation_boost;

                // F10: BG morphological frequency boost — rare inflections of
                // common stems get a frequency pooling boost.
                let morph_boost = models
                    .iter()
                    .map(|m| m.morphology.frequency_boost(&entry.word, entry.frequency))
                    .fold(1.0_f64, f64::max);
                score *= morph_boost;

                // Apply fuzzy discount if this came from fuzzy matching.
                if let Some(&discount) = discount_map.get(&entry.word) {
                    score *= discount;
                }

                Prediction {
                    word: entry.word.clone(),
                    score,
                    confidence: 0.0,
                }
            })
            .collect();

        // Step 4: sort descending by score, truncate.
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(limit);

        // Step 4b (Phase 3): Neural reranking pass.
        // Extract features per candidate and let the reranker blend its score.
        if self.use_reranker && self.reranker.is_trained() {
            let max_score = scored.first().map(|p| p.score).unwrap_or(1.0).max(1e-6);
            let mut rerank_data: arrayvec::ArrayVec<(f64, [f64; 6]), 30> =
                arrayvec::ArrayVec::new();

            for (i, (pred, r)) in scored.iter().zip(raw.iter()).enumerate() {
                let prefix_coverage = if pred.word.is_empty() {
                    0.0
                } else {
                    prefix.len() as f64 / pred.word.len() as f64
                };
                let frequency_rank = 1.0 / (1.0 + i as f64);
                let context_match = self
                    .personal_markov
                    .higher_ngrams_raw()
                    .keys()
                    .filter(|k| {
                        let klen = k.len();
                        klen <= context.len()
                            && k.iter()
                                .zip(context[context.len() - klen..].iter())
                                .all(|(a, b)| a == b)
                    })
                    .map(|k| k.len())
                    .max()
                    .unwrap_or(0) as f64
                    / 7.0;

                let features = [
                    pred.score / max_score,
                    r.markov / max_markov.max(1e-6),
                    r.personal / max_personal.max(1e-6),
                    prefix_coverage,
                    frequency_rank,
                    context_match,
                ];
                rerank_data.push((pred.score, features));
            }

            self.reranker.rerank(&mut rerank_data);

            // Apply reranked scores back and re-sort.
            for (pred, (new_score, _)) in scored.iter_mut().zip(rerank_data.iter()) {
                pred.score = *new_score;
            }
            scored.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }

        // Step 5: normalise confidence via score-sum (softmax-style) so confidences
        // reflect each candidate's fractional share of total score mass.
        // `.clamp(0.0, 1.0)` guards against floating-point rounding producing
        // values marginally outside [0, 1] (e.g. 1.0000000000000002).
        let score_sum: f64 = scored.iter().map(|p| p.score).sum();
        if score_sum > 0.0 {
            for p in &mut scored {
                let raw = (p.score / score_sum).clamp(0.0, 1.0);
                // Step 6 (F6): apply isotonic calibration when ready.
                // Maps raw score ratio → calibrated acceptance probability.
                p.confidence = self.calibrator.borrow_mut().calibrate(raw).unwrap_or(raw);
            }
        }

        // Store in cache before returning.
        self.cache.borrow_mut().put(prefix, context, scored.clone());

        scored
    }

    /// Current ensemble weights (for testing / inspection).
    pub fn weights(&self) -> (f64, f64, f64) {
        (self.alpha, self.beta, self.gamma)
    }
}

impl Default for SmartKeyEngine {
    fn default() -> Self {
        Self::new()
    }
}

// ======================================================================
// Tests
// ======================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build a small engine with a handful of words and bigrams.
    fn test_engine() -> SmartKeyEngine {
        let mut engine = SmartKeyEngine::new();
        engine.load_word("hello", 100);
        engine.load_word("help", 80);
        engine.load_word("hero", 60);
        engine.load_word("helicopter", 20);
        engine.load_word("world", 90);

        // Bigrams: "i" → "hello" (5), "i" → "help" (2)
        engine.load_bigram("i", "hello", 5);
        engine.load_bigram("i", "help", 2);

        // Trigram: ("say", "i") → "hello" (3)
        engine.load_trigram("say", "i", "hello", 3);

        engine
    }

    #[test]
    fn test_predict_basic() {
        let engine = test_engine();

        // Predict words starting with "hel" given context ["i"].
        let preds = engine.predict("hel", &["i"], 5, None);
        assert!(!preds.is_empty(), "should produce predictions");

        // "hello" should rank higher than "help" — it has higher corpus freq
        // AND stronger bigram support from context "i".
        let hello_pos = preds.iter().position(|p| p.word == "hello");
        let help_pos = preds.iter().position(|p| p.word == "help");
        assert!(
            hello_pos.is_some() && help_pos.is_some(),
            "both 'hello' and 'help' should appear"
        );
        assert!(
            hello_pos.unwrap() < help_pos.unwrap(),
            "'hello' should rank before 'help'"
        );

        // Top candidate should have a valid confidence in (0, 1].
        assert!(
            preds[0].confidence > 0.0 && preds[0].confidence <= 1.0,
            "confidence should be in (0, 1], got {}",
            preds[0].confidence
        );

        // All confidences should be in [0, 1].
        for p in &preds {
            assert!(
                p.confidence >= 0.0 && p.confidence <= 1.0,
                "confidence out of range: {}",
                p.confidence
            );
        }

        // Scores should be in descending order.
        for w in preds.windows(2) {
            assert!(
                w[0].score >= w[1].score,
                "predictions not sorted: {} >= {} failed",
                w[0].score,
                w[1].score
            );
        }
    }

    #[test]
    fn test_personal_boost() {
        use std::time::{SystemTime, UNIX_EPOCH};

        let mut engine = SmartKeyEngine::new();
        engine.load_word("apple", 50);
        engine.load_word("apply", 50);

        // Import a deterministic CVM snapshot with "apply" guaranteed in buffer.
        // This avoids flakiness from probabilistic CVM eviction.
        let now_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let snap = CvmSnapshot {
            version: 1,
            saved_at_unix: now_unix,
            capacity: 500,
            max_capacity: 5000,
            round: 0,
            decay_lambda: 0.001,
            words: vec!["apply".into()],
            age_secs: [("apply".into(), 0.0)].into(),
        };
        engine.import_personal(&snap);

        let preds = engine.predict("app", &[], 5, None);
        assert!(preds.len() >= 2);
        let apply_pos = preds.iter().position(|p| p.word == "apply").unwrap();
        let apple_pos = preds.iter().position(|p| p.word == "apple").unwrap();
        assert!(
            apply_pos < apple_pos,
            "personal boost should rank 'apply' before 'apple'"
        );
    }

    #[test]
    fn test_empty_context() {
        let engine = test_engine();

        // Predict with no context at all.
        let preds = engine.predict("hel", &[], 3, None);
        assert!(
            !preds.is_empty(),
            "should produce predictions even without context"
        );

        // Without Markov context, ordering should follow corpus frequency:
        // hello (100) > help (80) > helicopter (20).
        assert_eq!(preds[0].word, "hello");
        if preds.len() > 1 {
            assert_eq!(preds[1].word, "help");
        }
    }

    #[test]
    fn test_empty_prefix() {
        let engine = test_engine();
        // Empty prefix should return top words from the whole trie.
        let preds = engine.predict("", &[], 3, None);
        assert_eq!(preds.len(), 3);
        // All top-3 by frequency should appear (order may vary with PPM blending).
        let words: Vec<&str> = preds.iter().map(|p| p.word.as_str()).collect();
        assert!(
            words.contains(&"hello"),
            "hello should be in top 3: got {:?}",
            words
        );
    }

    #[test]
    fn test_unknown_prefix_returns_empty() {
        let engine = test_engine();
        // "zzz" is too far from any word — even fuzzy can't help.
        let preds = engine.predict("zzz", &[], 5, None);
        assert!(preds.is_empty());
    }

    #[test]
    fn test_limit_zero() {
        let engine = test_engine();
        let preds = engine.predict("hel", &[], 0, None);
        assert!(preds.is_empty());
    }

    // ==================================================================
    // Fuzzy fallback integration tests
    // ==================================================================

    #[test]
    fn test_fuzzy_fallback_misspelled_prefix() {
        let mut engine = SmartKeyEngine::new();
        engine.load_word("function", 100);
        engine.load_word("functional", 60);
        engine.load_word("funnel", 30);

        // "fucn" — transposition of n and c — exact prefix search finds nothing.
        // Fuzzy fallback should kick in and return "function" / "functional".
        let preds = engine.predict("fucn", &[], 5, None);
        assert!(
            !preds.is_empty(),
            "misspelled prefix should produce predictions via fuzzy fallback"
        );
        assert!(
            preds.iter().any(|p| p.word == "function"),
            "'function' should appear for misspelled prefix 'fucn': got {:?}",
            preds.iter().map(|p| &p.word).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_fuzzy_fallback_substitution() {
        let mut engine = SmartKeyEngine::new();
        engine.load_word("hello", 100);
        engine.load_word("help", 80);

        // "hallo" — substitution of 'e' → 'a'. Exact prefix "hallo" finds nothing.
        let preds = engine.predict("hallo", &[], 5, None);
        assert!(
            preds.iter().any(|p| p.word == "hello"),
            "'hello' should appear for 'hallo': got {:?}",
            preds.iter().map(|p| &p.word).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_exact_matches_rank_above_fuzzy() {
        let mut engine = SmartKeyEngine::new();
        engine.load_word("help", 80);
        engine.load_word("hello", 100);
        engine.load_word("held", 40);
        // "helm" — only "hel" words match exact prefix. But "help" is also
        // fuzzy-matchable from "helm" (distance 1: m→p substitution).
        // Let's use a different setup: words starting with "hel" (exact match)
        // plus words reachable only by fuzzy.
        engine.load_word("hero", 90);

        // "hel" matches hello, help, held exactly. "hero" is not a prefix match.
        let preds = engine.predict("hel", &[], 5, None);
        // All three exact matches should appear.
        assert!(preds.iter().any(|p| p.word == "hello"));
        assert!(preds.iter().any(|p| p.word == "help"));
        assert!(preds.iter().any(|p| p.word == "held"));
        // "hero" could appear via fuzzy (distance 1: l→o), but since we already
        // have 3 exact matches and limit=5, fuzzy also kicks in.
        // The exact matches should rank higher than fuzzy.
        let hello_pos = preds.iter().position(|p| p.word == "hello").unwrap();
        if let Some(hero_pos) = preds.iter().position(|p| p.word == "hero") {
            assert!(
                hello_pos < hero_pos,
                "exact match 'hello' should rank above fuzzy 'hero'"
            );
        }
    }

    #[test]
    fn test_export_import_personal() {
        let mut engine = SmartKeyEngine::new();
        for _ in 0..50 {
            engine.learn("persisted");
        }
        let snap = engine.export_personal();
        assert!(snap.words.contains(&"persisted".to_string()) || snap.words.is_empty());

        let mut engine2 = SmartKeyEngine::new();
        engine2.import_personal(&snap);
        let snap2 = engine2.export_personal();
        assert_eq!(snap.words.len(), snap2.words.len());
    }

    #[test]
    fn test_fuzzy_discount_applied() {
        let mut engine = SmartKeyEngine::new();
        // Two words with same frequency. One matches exactly, one via fuzzy.
        engine.load_word("test", 100);
        engine.load_word("best", 100);

        // "tes" — exact prefix matches "test" only. "best" is distance 1.
        let preds = engine.predict("tes", &[], 5, None);
        let test_pred = preds.iter().find(|p| p.word == "test");
        let best_pred = preds.iter().find(|p| p.word == "best");
        assert!(test_pred.is_some(), "'test' should appear");
        if let (Some(tp), Some(bp)) = (test_pred, best_pred) {
            assert!(
                tp.score > bp.score,
                "exact match 'test' ({}) should score higher than fuzzy 'best' ({})",
                tp.score,
                bp.score
            );
        }
    }

    #[test]
    fn test_markov_normalization_affects_ranking() {
        // Two candidates with equal corpus frequency but different Markov support.
        // After normalization, the one with stronger bigram context should rank higher.
        let mut engine = SmartKeyEngine::new();
        engine.load_word("world", 50);
        engine.load_word("worry", 50);

        // Strong bigram: "hello" → "world" (10)
        engine.load_bigram("hello", "world", 10);
        // No bigram support for "worry" from "hello".

        let preds = engine.predict("wor", &["hello"], 5, None);
        let world_pos = preds.iter().position(|p| p.word == "world");
        let worry_pos = preds.iter().position(|p| p.word == "worry");
        assert!(
            world_pos.is_some() && worry_pos.is_some(),
            "both 'world' and 'worry' should appear"
        );
        assert!(
            world_pos.unwrap() < worry_pos.unwrap(),
            "'world' should rank above 'worry' due to Markov context from 'hello'"
        );
    }

    #[test]
    fn test_from_config_respects_fuzzy_discount() {
        let config = InputConfig {
            fuzzy_discounts: [1.0, 0.1, 0.05],
            ..InputConfig::default()
        };

        let mut engine = SmartKeyEngine::from_config(&config);
        engine.load_word("test", 100);
        engine.load_word("best", 100); // distance 1 from "tes" prefix

        // "tes" matches "test" exactly; "best" only via fuzzy (distance 1).
        let preds = engine.predict("tes", &[], 5, None);
        let test_pred = preds.iter().find(|p| p.word == "test");
        let best_pred = preds.iter().find(|p| p.word == "best");
        assert!(test_pred.is_some(), "'test' should appear");
        if let (Some(tp), Some(bp)) = (test_pred, best_pred) {
            // With discount 0.1 for distance-1, the gap should be very large.
            assert!(
                tp.score > bp.score * 5.0,
                "custom fuzzy discount 0.1 should heavily penalise fuzzy match: \
                 test={}, best={}",
                tp.score,
                bp.score
            );
        }
    }
    #[test]
    fn common_english_completions() {
        let mut engine = SmartKeyEngine::new();
        engine.load_word("the", 100_000);
        engine.load_word("that", 60_000);
        engine.load_word("this", 55_000);
        engine.load_word("what", 70_000);
        engine.load_word("when", 65_000);
        engine.load_word("which", 50_000);
        let th = engine.predict("th", &[], 3, None);
        let wh = engine.predict("wh", &[], 3, None);
        let the_ok = th.iter().any(|p| p.word == "the");
        let wh_ok = wh
            .iter()
            .any(|p| p.word == "what" || p.word == "when" || p.word == "which");
        if !the_ok {
            panic!("th should predict the");
        }
        if !wh_ok {
            panic!("wh should predict what/when/which");
        }
    }

    #[test]
    fn session_blend_boosts_recent_words() {
        let mut engine = SmartKeyEngine::new();
        // Keep corpus frequencies close so personal learning can tip the balance.
        // With corpus weight=0.50, a large frequency gap resists personal boost.
        engine.load_word("algorithm", 3_000);
        engine.load_word("algebra", 4_000);
        engine.load_word("although", 5_000);
        for _ in 0..30 {
            engine.learn("algorithm");
        }
        engine.observe_session("algorithm");
        let preds = engine.predict("alg", &[], 5, None);
        assert!(!preds.is_empty(), "alg should produce predictions");
        let rank = preds
            .iter()
            .position(|p| p.word == "algorithm")
            .expect("algorithm missing from alg predictions");
        // Session blend + personal learning should boost "algorithm" into top 2
        // despite not having the highest corpus frequency.
        assert!(
            rank <= 1,
            "algorithm should be in top 2 after session learning, got rank {}",
            rank
        );
    }

    // ==================================================================
    // Confidence normalization tests
    // ==================================================================

    #[test]
    fn confidence_single_candidate_is_one() {
        let mut engine = SmartKeyEngine::new();
        engine.load_word("unique", 50);
        let preds = engine.predict("uniq", &[], 5, None);
        assert_eq!(preds.len(), 1, "should produce exactly one prediction");
        assert!(
            (preds[0].confidence - 1.0).abs() < 1e-9,
            "single candidate confidence should be 1.0, got {}",
            preds[0].confidence
        );
    }

    #[test]
    fn confidence_two_equal_scores_split_evenly() {
        let mut engine = SmartKeyEngine::new();
        engine.load_word("apple", 100);
        engine.load_word("apply", 100);
        let preds = engine.predict("appl", &[], 5, None);
        assert!(preds.len() >= 2, "should produce at least two predictions");
        let c0 = preds[0].confidence;
        let c1 = preds[1].confidence;
        assert!(
            (c0 - c1).abs() < 0.05,
            "two equal-score candidates should have similar confidence: {} vs {}",
            c0,
            c1
        );
    }

    #[test]
    fn confidence_sum_equals_one() {
        let engine = test_engine();
        let preds = engine.predict("hel", &[], 5, None);
        assert!(!preds.is_empty());
        let sum: f64 = preds.iter().map(|p| p.confidence).sum();
        assert!(
            (sum - 1.0).abs() < 1e-6,
            "sum of all confidences should be 1.0, got {}",
            sum
        );
    }

    #[test]
    fn confidence_ordering_matches_score_ordering() {
        let engine = test_engine();
        let preds = engine.predict("hel", &[], 5, None);
        assert!(preds.len() >= 2);
        for w in preds.windows(2) {
            assert!(
                w[0].confidence >= w[1].confidence,
                "confidence ordering should match score ordering: {} >= {} failed \
                 (words: {:?} vs {:?})",
                w[0].confidence,
                w[1].confidence,
                w[0].word,
                w[1].word
            );
        }
    }

    #[test]
    fn confidence_all_in_unit_range() {
        let engine = test_engine();
        let preds = engine.predict("hel", &["i"], 5, None);
        for p in &preds {
            assert!(
                p.confidence >= 0.0 && p.confidence <= 1.0,
                "confidence should be in [0.0, 1.0], got {} for {:?}",
                p.confidence,
                p.word
            );
        }
    }

    #[test]
    fn confidence_no_results_no_panic() {
        let engine = test_engine();
        let preds = engine.predict("zzzzzzz", &[], 5, None);
        assert!(preds.is_empty());
    }

    #[test]
    fn confidence_three_candidates_proportional() {
        let mut engine = SmartKeyEngine::new();
        engine.load_word("xalpha", 400);
        engine.load_word("xbeta", 200);
        engine.load_word("xgamma", 100);
        let preds = engine.predict("x", &[], 3, None);
        assert_eq!(preds.len(), 3, "should produce exactly three predictions");
        assert!(
            preds[0].confidence > preds[1].confidence,
            "xalpha should have higher confidence than xbeta"
        );
        assert!(
            preds[1].confidence > preds[2].confidence,
            "xbeta should have higher confidence than xgamma"
        );
        let ratio = preds[0].confidence / preds[2].confidence;
        assert!(
            ratio > 1.5,
            "confidence ratio (top/bottom) should reflect score ratio, got {:.2}",
            ratio
        );
    }

    #[test]
    fn confidence_single_garbage_score_gets_full_confidence() {
        // REGRESSION MARKER: A single candidate with a very low score still gets
        // confidence = 1.0 because softmax normalization (score/score_sum) doesn't
        // consider absolute score quality. This is the known weakness that
        // ghost_text_min_score addresses at the input layer.
        let mut engine = SmartKeyEngine::new();
        engine.load_word("zqxjk", 1);
        let preds = engine.predict("zqxj", &[], 5, None);
        assert_eq!(preds.len(), 1);
        assert!(
            (preds[0].confidence - 1.0).abs() < 1e-9,
            "even a garbage single candidate gets confidence 1.0 \
             (expected — score floor handles it at the input layer)"
        );
    }

    #[test]
    fn confidence_limit_one_returns_full_confidence() {
        let engine = test_engine();
        let preds = engine.predict("hel", &[], 1, None);
        assert_eq!(preds.len(), 1);
        assert!(
            (preds[0].confidence - 1.0).abs() < 1e-9,
            "limit=1 should yield confidence 1.0, got {}",
            preds[0].confidence
        );
    }

    // ==================================================================
    // NaN/Inf edge-case tests
    // ==================================================================

    #[test]
    fn predict_no_panic_with_nan_weights() {
        // Set alpha/beta/gamma to NaN and verify predict() doesn't panic.
        // We can't set internal fields directly, so we use adaptive weight
        // update: load a word and call predict with corrupt alpha via
        // from_config with NaN weights — InputConfig doesn't validate weights
        // in from_config path (only try_from_json does).
        let config = crate::input::InputConfig {
            weights: (f64::NAN, f64::NAN, f64::NAN),
            ..crate::input::InputConfig::default()
        };
        let mut engine = SmartKeyEngine::from_config(&config);
        engine.load_word("hello", 100);
        // Must not panic even with NaN weights.
        let preds = engine.predict("hel", &[], 5, None);
        // Confidences must be finite (clamped or zeroed).
        for p in &preds {
            assert!(
                p.confidence.is_finite(),
                "confidence must be finite even with NaN weights, got {}",
                p.confidence
            );
        }
    }

    #[test]
    fn predict_no_panic_with_inf_weights() {
        let config = crate::input::InputConfig {
            weights: (f64::INFINITY, f64::NEG_INFINITY, 0.0),
            ..crate::input::InputConfig::default()
        };
        let mut engine = SmartKeyEngine::from_config(&config);
        engine.load_word("test", 50);
        engine.load_word("team", 40);
        // Must not panic with Inf weights.
        let preds = engine.predict("te", &[], 5, None);
        for p in &preds {
            assert!(
                p.confidence.is_finite(),
                "confidence must be finite with Inf weights, got {}",
                p.confidence
            );
        }
    }

    #[test]
    fn predict_zero_score_sum_produces_no_confidence_panic() {
        // When all candidates have score 0 (e.g., zero weights), score_sum = 0.0
        // and the normalization branch is skipped. Confidences remain 0.0.
        let config = crate::input::InputConfig {
            weights: (0.0, 0.0, 0.0),
            ..crate::input::InputConfig::default()
        };
        let mut engine = SmartKeyEngine::from_config(&config);
        engine.load_word("hello", 100);
        // Must not panic; confidences should be 0.0 (no normalization applied).
        let preds = engine.predict("hel", &[], 5, None);
        for p in &preds {
            assert!(
                p.confidence >= 0.0 && p.confidence <= 1.0,
                "confidence out of [0,1] with zero weights: {}",
                p.confidence
            );
        }
    }

    #[test]
    fn confidence_single_garbage_score_does_not_nan() {
        // Single word with very small but positive frequency.
        // score_sum = that small score; ratio = 1.0; clamp keeps it in range.
        let mut engine = SmartKeyEngine::new();
        engine.load_word("zz", 1);
        let preds = engine.predict("zz", &[], 5, None);
        assert!(!preds.is_empty());
        assert!(
            preds[0].confidence.is_finite() && preds[0].confidence >= 0.0,
            "single garbage word confidence must be finite non-negative, got {}",
            preds[0].confidence
        );
    }
}
