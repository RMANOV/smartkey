// Ensemble scorer — combines corpus frequency, Markov context, and CVM personal boost.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use crate::bpe::BpeTokenizer;
use crate::cache::PredictionCache;
use crate::cvm::{CvmCounter, CvmSnapshot};
use crate::hedge::HedgeMixer;
use crate::input::InputConfig;
use crate::kneser_ney::KneserNeyScorer;
use crate::lang_cvm::LangCvmTracks;
use crate::lang_detect::LangId;
use crate::markov::MarkovChain;
use crate::ngram::{NgramTrie, WordEntry};
use crate::personal::{extract_markov_snapshot, AdaptiveWeightsSnapshot, PersonalProfile};
use crate::ppm::PpmModel;
use crate::session_cache::SessionCacheLM;

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
const SESSION_BLEND: f64 = 0.4;

/// Boost factor for per-language CVM track scores (v0.4.1).
const LANG_CVM_BOOST: f64 = 1.5;

/// Unified prediction engine that blends three scoring signals:
///
/// * **Corpus frequency** (α) — how common the word is in the language model trie.
/// * **Markov context** (β) — bigram/trigram probability given recent words.
/// * **Personal boost** (γ) — CVM-tracked frequency of words *this* user types.
///
/// The final score is `α·corpus + β·markov + γ·personal`.
pub struct SmartKeyEngine {
    trie: NgramTrie,
    markov: MarkovChain,
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
    /// Prediction cache (Mutex: predict takes &self but cache needs mutation;
    /// Mutex instead of RefCell for Send+Sync required by PyO3).
    cache: Mutex<PredictionCache>,
    /// PPM character-level model (v0.4.0, behind flag).
    ppm: Option<PpmModel>,
    /// BPE tokenizer for OOV fallback (v0.4.0).
    bpe: Option<BpeTokenizer>,
    /// Session cache LM for burstiness tracking (v0.4.0).
    session_cache: SessionCacheLM,
    /// Interpolated Kneser-Ney scorer (experimental, behind flag).
    kn_scorer: Option<KneserNeyScorer>,
    /// Hedge/Exp3 adaptive weight mixer (experimental, behind flag).
    hedge: Option<HedgeMixer>,
    /// Feature flags snapshot from config.
    use_session_cache_lm: bool,
    bpe_enabled: bool,
    use_kneser_ney: bool,
    use_hedge: bool,
}

impl SmartKeyEngine {
    /// Create a new engine with default weights `(0.4, 0.4, 0.2)` and a
    /// CVM counter sized `(500, 5000)`.
    pub fn new() -> Self {
        Self::from_config(&InputConfig::default())
    }

    /// Create a new engine from an `InputConfig`, respecting all tuning parameters.
    pub fn from_config(config: &InputConfig) -> Self {
        Self {
            trie: NgramTrie::new(),
            markov: MarkovChain::with_lambdas(config.markov_lambdas),
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
            cache: Mutex::new(PredictionCache::new()),
            ppm: if config.use_ppm {
                Some(PpmModel::default_order())
            } else {
                None
            },
            bpe: None, // Loaded from corpus if bpe_enabled
            session_cache: SessionCacheLM::new(),
            kn_scorer: None, // Built lazily after corpus load when flag is set
            hedge: if config.use_hedge {
                Some(HedgeMixer::with_defaults(&[
                    config.weights.0,
                    config.weights.1,
                    config.weights.2,
                ]))
            } else {
                None
            },
            use_session_cache_lm: config.use_session_cache_lm,
            bpe_enabled: config.bpe_enabled,
            use_kneser_ney: config.use_kneser_ney,
            use_hedge: config.use_hedge,
        }
    }

    /// Insert a word with its corpus frequency into the trie.
    pub fn load_word(&mut self, word: &str, frequency: u32) {
        self.trie.insert(word, frequency);
        // Train PPM model if enabled.
        if let Some(ref mut ppm) = self.ppm {
            ppm.train_word(word);
        }
    }

    /// Train a bigram transition `context → word` with the given count.
    pub fn load_bigram(&mut self, context: &str, word: &str, count: u32) {
        self.markov.train_bigram(context, word, count);
    }

    /// Train a trigram transition `(w1, w2) → word` with the given count.
    pub fn load_trigram(&mut self, w1: &str, w2: &str, word: &str, count: u32) {
        self.markov.train_trigram(w1, w2, word, count);
    }

    /// Feed a word into the personal CVM layer (call when the user types a word).
    pub fn learn(&mut self, word: &str) {
        self.personal.process(word);
        self.cache.lock().unwrap().invalidate();
    }

    /// Feed a word into the per-language CVM track.
    pub fn learn_with_lang(&mut self, word: &str, lang: LangId) {
        self.personal_tracks.learn(word, lang);
        self.cache.lock().unwrap().invalidate();
    }

    /// Set the BPE tokenizer (loaded from corpus).
    pub fn set_bpe(&mut self, bpe: BpeTokenizer) {
        self.bpe = Some(bpe);
    }

    /// Record a word commit in the session cache for burstiness tracking.
    pub fn observe_session(&mut self, word: &str) {
        self.session_cache.observe(word);
    }

    /// Clear the session cache (e.g. on session restart).
    pub fn clear_session_cache(&mut self) {
        self.session_cache.clear();
    }

    /// Build the Kneser-Ney scorer from the currently loaded Markov data.
    ///
    /// Call after corpus loading is complete. Only effective when `use_kneser_ney`
    /// flag is set; otherwise a no-op.
    pub fn build_kneser_ney(&mut self) {
        if self.use_kneser_ney {
            self.kn_scorer = Some(KneserNeyScorer::from_markov_data(
                self.markov.bigrams_raw(),
                self.markov.trigrams_raw(),
            ));
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
        self.cache.lock().unwrap().invalidate();
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

        // Recompute raw scores for the accepted word.
        let corpus_score = if let Some(entry) = self.trie.prefix_search(word, 1).first() {
            entry.frequency as f64
        } else {
            0.0
        };
        let markov_score = self.markov.score_with_backoff(word, prev1, prev2);
        let personal_score = self.personal.frequency_score(word);

        if self.use_hedge {
            // Hedge/Exp3 multiplicative weights update.
            // Normalize raw scores to [0,1] rewards.
            let max_s = corpus_score.max(markov_score).max(personal_score).max(1e-6);
            let rewards = [
                corpus_score / max_s,
                markov_score / max_s,
                personal_score / max_s,
            ];
            if let Some(ref mut hedge) = self.hedge {
                hedge.update(&rewards);
                let (a, b, g) = hedge.weights_triple();
                self.alpha = a;
                self.beta = b;
                self.gamma = g;
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

        self.cache.lock().unwrap().invalidate();
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

        self.cache.lock().unwrap().invalidate();
    }

    /// Export the personal CVM layer as a portable snapshot (backward compat).
    pub fn export_personal(&self) -> CvmSnapshot {
        self.personal.to_snapshot()
    }

    /// Replace the personal CVM layer from a snapshot (backward compat).
    pub fn import_personal(&mut self, snapshot: &CvmSnapshot) {
        self.personal = CvmCounter::from_snapshot(snapshot);
        self.cache.lock().unwrap().invalidate();
    }

    /// Fast candidate count for a prefix (without full scoring).
    ///
    /// Used by transliteration mode to compare prefix viability across layouts.
    pub fn candidate_count(&self, prefix: &str, limit: usize) -> usize {
        self.trie.prefix_search(prefix, limit).len()
    }

    /// Return the corpus frequency of the top trie candidate for `prefix`.
    ///
    /// Used by wrong-layout detection: comparing max frequencies across
    /// languages is far more discriminative than comparing candidate counts,
    /// especially with 100K+ word corpora where virtually every 2-letter
    /// prefix has 3+ candidates in both languages.
    pub fn max_prefix_frequency(&self, prefix: &str) -> u32 {
        self.trie
            .prefix_search(prefix, 1)
            .first()
            .map(|e| e.frequency)
            .unwrap_or(0)
    }

    /// Score two prefixes (EN and BG interpretations) for dual-buffer comparison.
    ///
    /// Returns raw corpus frequencies as `(en_freq, bg_freq)`. The dual buffer
    /// normalises these to determine the winner. Uses `max_prefix_frequency`
    /// for speed — a single trie lookup per language, no full prediction pass.
    pub fn score_both(&self, en_prefix: &str, bg_prefix: &str) -> (f64, f64) {
        let en = self.max_prefix_frequency(en_prefix) as f64;
        let bg = self.max_prefix_frequency(bg_prefix) as f64;
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
        if let Some(cached) = self.cache.lock().unwrap().get(prefix, context) {
            return cached;
        }

        let candidate_pool = limit * 3;

        // Step 1: candidate generation via trie prefix search.
        // When PPM is enabled and prefix is short (≤ 1 char), use PPM to guide
        // candidate generation — rank_next_chars() identifies the most likely
        // next characters, then we search the trie with each extended prefix.
        let candidates = if let Some(ppm) = self
            .ppm
            .as_ref()
            .filter(|_| !prefix.is_empty() && prefix.len() <= 1)
        {
            let next_chars = ppm.rank_next_chars(prefix, 5);
            let per_char_pool = candidate_pool / next_chars.len().max(1);
            let mut guided: Vec<WordEntry> = Vec::new();
            let mut seen = HashSet::new();
            for (ch, _) in &next_chars {
                let extended = format!("{}{}", prefix, ch);
                for entry in self.trie.prefix_search(&extended, per_char_pool) {
                    if seen.insert(entry.word.clone()) {
                        guided.push(entry);
                    }
                }
            }
            // Also include regular prefix search results for coverage.
            for entry in self.trie.prefix_search(prefix, candidate_pool) {
                if seen.insert(entry.word.clone()) {
                    guided.push(entry);
                }
            }
            guided
        } else {
            self.trie.prefix_search(prefix, candidate_pool)
        };

        // Step 1b: fuzzy fallback — if exact prefix returned fewer than `limit`
        // candidates, fill the gap with fuzzy matches (edit distance ≤ 2).
        let (fuzzy_candidates, discount_map) = if candidates.len() < limit && !prefix.is_empty() {
            let fuzzy_slots = candidate_pool.saturating_sub(candidates.len());
            let fuzzy_matches = self
                .trie
                .fuzzy_search(prefix, self.fuzzy_max_edits, fuzzy_slots);

            // Build a set of words already found by exact prefix search to
            // avoid scoring the same word twice.
            let exact_words: HashSet<&str> = candidates.iter().map(|c| c.word.as_str()).collect();

            let mut discount_map: HashMap<String, f64> =
                HashMap::with_capacity(fuzzy_matches.len());
            let mut merged: Vec<WordEntry> = Vec::with_capacity(fuzzy_matches.len());
            for fm in &fuzzy_matches {
                if exact_words.contains(fm.word.as_str()) {
                    continue;
                }
                let discount = self.fuzzy_discounts[fm.edit_distance.min(2) as usize];
                discount_map.insert(fm.word.clone(), discount);
                merged.push(WordEntry {
                    word: fm.word.clone(),
                    frequency: fm.frequency,
                });
            }
            (merged, discount_map)
        } else {
            (Vec::new(), HashMap::new())
        };

        // If both exact and fuzzy are empty, try BPE fallback.
        if candidates.is_empty() && fuzzy_candidates.is_empty() {
            if self.bpe_enabled {
                if let Some(ref bpe) = self.bpe {
                    let bpe_suggestions = bpe.suggest_completions(prefix, limit);
                    if !bpe_suggestions.is_empty() {
                        let max_score = bpe_suggestions[0].1.max(1e-6);
                        let mut preds: Vec<Prediction> = bpe_suggestions
                            .into_iter()
                            .map(|(word, score)| Prediction {
                                word,
                                score,
                                confidence: score / max_score,
                            })
                            .collect();
                        preds.truncate(limit);
                        self.cache
                            .lock()
                            .unwrap()
                            .put(prefix, context, preds.clone());
                        return preds;
                    }
                }
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

        // Single pass: compute raw scores and track maxima simultaneously.
        // This avoids calling markov.score_with_backoff() and personal.frequency_score()
        // twice per candidate (was: once for max, once for scoring).
        struct RawScores {
            corpus: f64,
            markov: f64,
            personal: f64,
            ppm: f64,
        }

        // Stack-allocate for the common case (≤30 candidates; default limit*3=15).
        // Falls back to Vec for unusually large candidate pools.
        let mut raw_small = arrayvec::ArrayVec::<RawScores, 30>::new();
        let mut raw_large = Vec::new();
        let mut max_markov = 1e-6_f64;
        let mut max_personal = 1.0_f64;
        let mut max_ppm = 1e-6_f64;
        let use_stack = all_candidates.len() <= 30;

        for c in &all_candidates {
            // Blend corpus Markov and personal Markov.
            // When KN is enabled, use interpolated Kneser-Ney instead of Katz backoff.
            let corpus_m = if let Some(ref kn) = self.kn_scorer {
                kn.score(&c.word, prev1, prev2)
            } else {
                self.markov.score_with_backoff(&c.word, prev1, prev2)
            };
            let personal_m = self
                .personal_markov
                .score_with_backoff(&c.word, prev1, prev2);
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
            // PPM scoring (if enabled).
            let ppm_score = if let Some(ref ppm) = self.ppm {
                ppm.score_candidate(prefix, &c.word)
            } else {
                0.0
            };
            max_markov = max_markov.max(m);
            max_personal = max_personal.max(p);
            max_ppm = max_ppm.max(ppm_score);
            let scores = RawScores {
                corpus: c.frequency as f64,
                markov: m,
                personal: p,
                ppm: ppm_score,
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
        // When PPM is active, borrow δ weight from β (markov) for short prefixes.
        let (eff_alpha, eff_beta, eff_gamma, eff_delta) = if self.ppm.is_some() && prefix.len() <= 1
        {
            // Short prefix: PPM gets 0.15 borrowed from markov.
            let delta = 0.15_f64.min(self.beta * 0.5);
            (self.alpha, self.beta - delta, self.gamma, delta)
        } else if self.ppm.is_some() {
            // Longer prefix: PPM gets a smaller share.
            let delta = 0.05_f64.min(self.beta * 0.2);
            (self.alpha, self.beta - delta, self.gamma, delta)
        } else {
            (self.alpha, self.beta, self.gamma, 0.0)
        };

        let mut scored: Vec<Prediction> = all_candidates
            .iter()
            .zip(raw.iter())
            .map(|(entry, r)| {
                let corpus_score = r.corpus / max_freq;
                let markov_score = r.markov / max_markov;
                let personal_score = r.personal / max_personal;
                let ppm_score = if max_ppm > 1e-6 { r.ppm / max_ppm } else { 0.0 };

                let mut score = eff_alpha * corpus_score
                    + eff_beta * markov_score
                    + eff_gamma * personal_score
                    + eff_delta * ppm_score;

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

        // Step 5: normalise confidence so the top candidate is ~1.0.
        if let Some(top_score) = scored.first().map(|p| p.score) {
            if top_score > 0.0 {
                for p in &mut scored {
                    p.confidence = p.score / top_score;
                }
            }
        }

        // Store in cache before returning.
        self.cache
            .lock()
            .unwrap()
            .put(prefix, context, scored.clone());

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

        // Top candidate should have confidence ~1.0.
        assert!(
            (preds[0].confidence - 1.0).abs() < 1e-9,
            "top prediction confidence should be ~1.0, got {}",
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
        // Top by corpus frequency: hello(100), world(90), help(80).
        assert_eq!(preds[0].word, "hello");
        assert_eq!(preds[1].word, "world");
        assert_eq!(preds[2].word, "help");
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
}
