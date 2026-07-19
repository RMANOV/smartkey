//! Pure n-gram-component calibration snapshot.
//!
//! Semantics mirror `phase_a/freqmodel.py` exactly:
//!
//! ```text
//! top3(ctx) = 3 highest-raw-count followers of ctx, ties lexicographic;
//!             global unigram top-3 back-off when ctx has no bigram mass.
//! p_word    = count(ctx, w) / total(ctx); unigram u/U back-off; else 0.
//! p_top3    = deduplicated top-3 raw-frequency mass, clamped to [0, 1].
//! ```
//!
//! The lab `FreqModel` merges every corpus file into one table; the live
//! engine splits tables per language, so these functions aggregate raw counts
//! across ALL language models before ranking/normalising. Only raw corpus
//! count tables are read — no ensemble / reranker / calibrator / PPM / BPE
//! code is reachable from this module (`phase_a/constants.py`
//! `PREDICTION_SCOPE = "ngram_component"`).

use std::collections::HashMap;
use std::time::Instant;

use crate::lang_model::LanguageModel;

/// FFI-bound numbers for one prediction snapshot. The candidate words never
/// cross the FFI — the caller holds them transiently in core memory to
/// compute the `{0,1}` outcome bit and then discards them.
#[derive(Debug, Clone)]
pub struct O1Numbers {
    /// Number of candidates in the snapshot top-3 (0–3).
    pub n_candidates: u32,
    /// Raw normalised frequency mass of the deduplicated top-3 set.
    pub p_top3: f64,
    /// Core-internal query time in µs (diagnostics only — NOT the gated
    /// `latency_us`, which is measured adapter-side).
    pub core_latency_us: u64,
}

/// Merge raw bigram follower counts for `ctx` across all language models.
fn merged_followers<'a>(models: &[&'a LanguageModel], ctx: &str) -> (HashMap<&'a str, u64>, u64) {
    let mut merged: HashMap<&str, u64> = HashMap::new();
    let mut ctx_total: u64 = 0;
    if !ctx.is_empty() {
        for model in models {
            if let Some((followers, total)) = model.markov.bigrams_raw().get(ctx) {
                for (word, count) in followers {
                    *merged.entry(word.as_str()).or_insert(0) += u64::from(*count);
                }
                ctx_total += u64::from(*total);
            }
        }
    }
    (merged, ctx_total)
}

/// Merged raw unigram count of `word` and the merged total across models.
fn merged_unigram(models: &[&LanguageModel], word: &str) -> (u64, u64) {
    let mut count: u64 = 0;
    let mut total: u64 = 0;
    for model in models {
        count += model
            .markov
            .unigram_counts_raw()
            .get(word)
            .copied()
            .map_or(0, u64::from);
        total += model.markov.total_unigram_count();
    }
    (count, total)
}

/// Global unigram top-3 back-off candidates (highest merged raw count, ties
/// lexicographic) — the `FreqModel._unigram_top3` mirror. Full-vocabulary
/// scan: callers compute this ONCE and cache it (`FreqModel` precomputes at
/// load time; the cache-once semantics are part of the declared parity).
pub fn global_unigram_top3(models: &[&LanguageModel]) -> Vec<String> {
    let mut merged: HashMap<&str, u64> = HashMap::new();
    for model in models {
        for (word, count) in model.markov.unigram_counts_raw() {
            *merged.entry(word.as_str()).or_insert(0) += u64::from(*count);
        }
    }
    let mut ranked: Vec<(&str, u64)> = merged.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    ranked
        .into_iter()
        .take(3)
        .map(|(w, _)| w.to_string())
        .collect()
}

/// Raw normalised frequency `P_raw(word | ctx)` with unigram back-off —
/// the exact `FreqModel.p_word` semantics over merged tables.
fn p_word(
    models: &[&LanguageModel],
    merged: &HashMap<&str, u64>,
    ctx_total: u64,
    word: &str,
) -> f64 {
    if ctx_total > 0 {
        let c = merged.get(word).copied().unwrap_or(0);
        if c > 0 {
            return c as f64 / ctx_total as f64;
        }
    }
    let (u, u_total) = merged_unigram(models, word);
    if u_total > 0 && u > 0 {
        return u as f64 / u_total as f64;
    }
    0.0
}

/// One ngram-component snapshot at a word boundary: `(top3, numbers)`.
/// `uni_top3_cache` is the precomputed [`global_unigram_top3`] back-off list.
pub fn snapshot(
    models: &[&LanguageModel],
    ctx: &str,
    uni_top3_cache: &[String],
) -> (Vec<String>, O1Numbers) {
    let t0 = Instant::now();
    let (merged, ctx_total) = merged_followers(models, ctx);
    let top3: Vec<String> = if merged.is_empty() {
        uni_top3_cache.to_vec()
    } else {
        let mut ranked: Vec<(&str, u64)> = merged.iter().map(|(w, c)| (*w, *c)).collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        ranked
            .into_iter()
            .take(3)
            .map(|(w, _)| w.to_string())
            .collect()
    };
    let mut seen: Vec<&str> = Vec::with_capacity(3);
    let mut mass = 0.0_f64;
    for word in top3.iter().take(3) {
        if seen.contains(&word.as_str()) {
            continue;
        }
        seen.push(word.as_str());
        mass += p_word(models, &merged, ctx_total, word);
    }
    let p_top3 = mass.clamp(0.0, 1.0);
    let numbers = O1Numbers {
        n_candidates: top3.len().min(3) as u32,
        p_top3,
        core_latency_us: t0.elapsed().as_micros() as u64,
    };
    (top3, numbers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang_detect::LangId;

    fn model(lang: LangId) -> LanguageModel {
        LanguageModel::new(lang, [0.6, 0.3, 0.1], false)
    }

    #[test]
    fn top3_ranks_by_count_then_lexicographic() {
        let mut m = model(LangId::En);
        m.markov.train_bigram("the", "cat", 5);
        m.markov.train_bigram("the", "dog", 5);
        m.markov.train_bigram("the", "ant", 3);
        m.markov.train_bigram("the", "zebra", 9);
        let (top3, nums) = snapshot(&[&m], "the", &[]);
        assert_eq!(top3, vec!["zebra", "cat", "dog"]); // 9, then 5-tie lexicographic
        assert_eq!(nums.n_candidates, 3);
        // p = (9 + 5 + 5) / 22
        assert!((nums.p_top3 - 19.0 / 22.0).abs() < 1e-12);
    }

    #[test]
    fn merges_counts_across_language_models() {
        let mut en = model(LangId::En);
        let mut bg = model(LangId::Bg);
        en.markov.train_bigram("na", "masa", 2);
        bg.markov.train_bigram("na", "masa", 3);
        bg.markov.train_bigram("na", "stol", 4);
        let (top3, nums) = snapshot(&[&en, &bg], "na", &[]);
        assert_eq!(top3, vec!["masa", "stol"]); // merged masa=5 beats stol=4
        assert_eq!(nums.n_candidates, 2);
        assert!((nums.p_top3 - 1.0).abs() < 1e-12); // full mass, clamped semantics
    }

    #[test]
    fn backoff_to_unigram_top3_when_no_bigram_mass() {
        let mut m = model(LangId::En);
        m.markov.train_unigram("the", 100);
        m.markov.train_unigram("and", 80);
        m.markov.train_unigram("for", 60);
        m.markov.train_unigram("rare", 1);
        let cache = global_unigram_top3(&[&m]);
        assert_eq!(cache, vec!["the", "and", "for"]);
        let (top3, nums) = snapshot(&[&m], "unseen-context", &cache);
        assert_eq!(top3, cache);
        // p from unigram u/U: (100+80+60)/241
        assert!((nums.p_top3 - 240.0 / 241.0).abs() < 1e-12);
    }

    #[test]
    fn empty_context_uses_backoff_and_partial_followers_do_not_fill() {
        let mut m = model(LangId::En);
        m.markov.train_unigram("the", 10);
        m.markov.train_bigram("ctx", "only-one", 2);
        let cache = global_unigram_top3(&[&m]);
        let (top3_empty, _) = snapshot(&[&m], "", &cache);
        assert_eq!(top3_empty, vec!["the"]); // empty ctx → unigram back-off
        let (top3_partial, nums) = snapshot(&[&m], "ctx", &cache);
        assert_eq!(top3_partial, vec!["only-one"]); // <3 followers stay as-is (FreqModel parity)
        assert_eq!(nums.n_candidates, 1);
    }

    #[test]
    fn follower_without_count_falls_back_to_unigram_p() {
        // A top-3 word can carry unigram mass even when absent from ctx
        // followers — p_word backs off per-word exactly like FreqModel.
        let mut m = model(LangId::En);
        m.markov.train_bigram("ctx", "a", 1);
        m.markov.train_unigram("a", 4);
        let (_, nums) = snapshot(&[&m], "ctx", &[]);
        // top3 = [a]; p = 1/1 (bigram path wins over unigram)
        assert!((nums.p_top3 - 1.0).abs() < 1e-12);
        assert_eq!(nums.n_candidates, 1);
    }

    #[test]
    fn no_mass_anywhere_gives_zero_p_and_empty_candidates() {
        let m = model(LangId::En);
        let (top3, nums) = snapshot(&[&m], "anything", &[]);
        assert!(top3.is_empty());
        assert_eq!(nums.n_candidates, 0);
        assert_eq!(nums.p_top3, 0.0);
    }
}
