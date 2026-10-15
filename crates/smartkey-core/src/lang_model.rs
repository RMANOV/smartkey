//! Per-language model bank — isolates trie, Markov, and PPM per language
//! so that bigrams from one language don't contaminate another.

use std::collections::{HashMap, HashSet};

use crate::bg_morphology::BgMorphology;
use crate::collocation::CollocationDetector;
use crate::kneser_ney::KneserNeyScorer;
use crate::lang_detect::LangId;
use crate::markov::MarkovChain;
use crate::ngram::{FuzzyMatch, NgramTrie, WordEntry};
use crate::ppm::PpmModel;

/// A complete language model for one language.
pub struct LanguageModel {
    pub lang: LangId,
    /// Original corpus vocabulary, unaffected by personal profile replacement.
    pub trie: NgramTrie,
    /// Sparse positive personal word evidence, independently persisted.
    pub confirmed_trie: NgramTrie,
    /// Sparse, unconfirmed online vocabulary used only to propose predictions.
    pub prediction_only_trie: NgramTrie,
    pub markov: MarkovChain,
    pub ppm: Option<PpmModel>,
    /// Interpolated Kneser-Ney scorer (built lazily after corpus load).
    pub kn_scorer: Option<KneserNeyScorer>,
    /// Collocation detector (F9): PMI-based phrase boosting.
    pub collocation: CollocationDetector,
    /// BG morphological grouping (F10): suffix stemmer + frequency pooling.
    /// Only populated for BG language models.
    pub morphology: BgMorphology,
}

impl LanguageModel {
    pub fn new(lang: LangId, markov_lambdas: [f64; 3], use_ppm: bool) -> Self {
        Self {
            lang,
            trie: NgramTrie::new(),
            confirmed_trie: NgramTrie::new(),
            prediction_only_trie: NgramTrie::new(),
            markov: MarkovChain::with_lambdas(markov_lambdas),
            ppm: if use_ppm {
                Some(PpmModel::default_order())
            } else {
                None
            },
            kn_scorer: None,
            collocation: CollocationDetector::new(),
            morphology: BgMorphology::new(),
        }
    }

    /// Original corpus frequency takes precedence even when below confirmation5.
    pub(crate) fn trusted_exact_frequency(&self, word: &str) -> Option<u32> {
        self.trie
            .exact_frequency(word)
            .or_else(|| self.confirmed_trie.exact_frequency(word))
    }

    /// Bounded effective trusted prefix pool. Confirmed/base duplicates already
    /// have identical frequency, including after subsequent corpus loads.
    pub(crate) fn trusted_prefix_search(&self, prefix: &str, limit: usize) -> Vec<WordEntry> {
        if limit == 0 {
            return Vec::new();
        }
        let mut entries = self.trie.prefix_search(prefix, limit);
        entries.extend(self.confirmed_trie.prefix_search(prefix, limit));
        entries.sort_unstable_by(|a, b| {
            b.frequency
                .cmp(&a.frequency)
                .then_with(|| b.word.cmp(&a.word))
        });
        let mut seen = HashSet::new();
        entries.retain(|entry| seen.insert(entry.word.clone()));
        entries.truncate(limit);
        entries
    }

    pub(crate) fn trusted_fuzzy_search(
        &self,
        prefix: &str,
        max_edits: u8,
        limit: usize,
    ) -> Vec<FuzzyMatch> {
        let mut entries = self.trie.fuzzy_search(prefix, max_edits, limit);
        entries.extend(self.confirmed_trie.fuzzy_search(prefix, max_edits, limit));
        entries.sort_unstable_by(|a, b| {
            a.edit_distance
                .cmp(&b.edit_distance)
                .then_with(|| b.frequency.cmp(&a.frequency))
                .then_with(|| a.word.cmp(&b.word))
        });
        let mut seen = HashSet::new();
        entries.retain(|entry| seen.insert(entry.word.clone()));
        entries.truncate(limit);
        entries
    }
}

/// Registry of per-language models.
pub struct LanguageModelBank {
    models: HashMap<LangId, LanguageModel>,
    markov_lambdas: [f64; 3],
    use_ppm: bool,
}

impl LanguageModelBank {
    pub fn new(markov_lambdas: [f64; 3], use_ppm: bool) -> Self {
        Self {
            models: HashMap::new(),
            markov_lambdas,
            use_ppm,
        }
    }

    pub fn get(&self, lang: LangId) -> Option<&LanguageModel> {
        self.models.get(&lang)
    }

    pub fn get_mut_or_create(&mut self, lang: LangId) -> &mut LanguageModel {
        let lambdas = self.markov_lambdas;
        let use_ppm = self.use_ppm;
        self.models
            .entry(lang)
            .or_insert_with(|| LanguageModel::new(lang, lambdas, use_ppm))
    }

    /// Returns target model if lang is Some, or all models if None.
    pub fn get_or_all(&self, lang: Option<LangId>) -> Vec<&LanguageModel> {
        if let Some(l) = lang {
            self.models.get(&l).into_iter().collect()
        } else {
            self.models.values().collect()
        }
    }

    /// Mutable iterator over all models (for building KN scorers).
    pub fn models_mut(&mut self) -> impl Iterator<Item = &mut LanguageModel> {
        self.models.values_mut()
    }

    /// Iterator over all models.
    pub fn all_models(&self) -> impl Iterator<Item = &LanguageModel> {
        self.models.values()
    }

    /// Whether any models have been loaded.
    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }

    pub fn load_word(&mut self, word: &str, freq: u32, lang: LangId) {
        let model = self.get_mut_or_create(lang);
        model.trie.insert(word, freq);
        // Synchronize before top-K selection: a shadowed confirmation5 must not
        // hide a different novel confirmation5 behind a later corpus2 entry.
        if model.confirmed_trie.exact_frequency(word).is_some() {
            model.confirmed_trie.insert(word, freq);
        }
        // Feed corpus frequency into Markov unigram for frequency-weighted backoff (F1).
        model.markov.train_unigram(word, freq);
        if let Some(ref mut ppm) = model.ppm {
            ppm.train_word(word);
        }
    }

    pub fn load_bigram(&mut self, ctx: &str, word: &str, count: u32, lang: LangId) {
        self.get_mut_or_create(lang)
            .markov
            .train_bigram(ctx, word, count);
    }

    pub fn load_trigram(&mut self, w1: &str, w2: &str, word: &str, count: u32, lang: LangId) {
        self.get_mut_or_create(lang)
            .markov
            .train_trigram(w1, w2, word, count);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_language_isolation() {
        let mut bank = LanguageModelBank::new([0.6, 0.3, 0.1], false);
        bank.load_word("hello", 100, LangId::En);
        bank.load_word("здравей", 100, LangId::Bg);

        let en = bank.get(LangId::En).unwrap();
        let bg = bank.get(LangId::Bg).unwrap();

        assert_eq!(en.trie.prefix_search("hel", 5).len(), 1);
        assert!(en.trie.prefix_search("здр", 5).is_empty());
        assert_eq!(bg.trie.prefix_search("здр", 5).len(), 1);
        assert!(bg.trie.prefix_search("hel", 5).is_empty());
    }

    #[test]
    fn review_confirmation_later_base_frequency_sync_precedes_prefix_and_fuzzy_selection() {
        let mut bank = LanguageModelBank::new([0.6, 0.3, 0.1], false);
        for word in ["qxaaa", "qxaab", "qxaac", "qxzzz"] {
            bank.get_mut_or_create(LangId::En)
                .confirmed_trie
                .insert(word, 5);
        }
        for word in ["qxaaa", "qxaab", "qxaac"] {
            bank.load_word(word, 2, LangId::En);
        }
        let model = bank.get(LangId::En).unwrap();
        assert_eq!(model.confirmed_trie.exact_frequency("qxaaa"), Some(2));
        assert_eq!(model.trusted_prefix_search("qx", 1)[0].word, "qxzzz");
        assert_eq!(model.trusted_prefix_search("qx", 3)[0].word, "qxzzz");
        assert_eq!(model.trusted_fuzzy_search("qy", 1, 1)[0].word, "qxzzz");
        assert!(model.trusted_prefix_search("qx", 0).is_empty());
        assert!(model.trusted_fuzzy_search("qy", 1, 0).is_empty());
    }

    #[test]
    fn review_confirmation_equal_frequency_pools_are_bounded_deduplicated_and_ordered() {
        let mut bank = LanguageModelBank::new([0.6, 0.3, 0.1], false);
        for word in ["qxalpha", "qxbravo", "qxcharlie", "qxdelta"] {
            bank.load_word(word, 5, LangId::En);
        }
        for word in ["qxalpha", "qxecho", "qxfoxtrot", "qxgolf"] {
            bank.get_mut_or_create(LangId::En)
                .confirmed_trie
                .insert(word, 5);
        }
        let model = bank.get(LangId::En).unwrap();
        let prefix = model.trusted_prefix_search("qx", 3);
        assert_eq!(prefix.len(), 3);
        assert!(prefix.iter().all(|entry| entry.frequency == 5));
        assert!(prefix.windows(2).all(|pair| pair[0].word > pair[1].word));
        let fuzzy = model.trusted_fuzzy_search("qy", 1, 3);
        assert_eq!(fuzzy.len(), 3);
        assert!(fuzzy
            .iter()
            .all(|entry| entry.frequency == 5 && entry.edit_distance == 1));
        assert!(fuzzy.windows(2).all(|pair| pair[0].word < pair[1].word));
    }

    #[test]
    fn get_or_all_filters_by_lang() {
        let mut bank = LanguageModelBank::new([0.6, 0.3, 0.1], false);
        bank.load_word("hello", 100, LangId::En);
        bank.load_word("здравей", 100, LangId::Bg);

        assert_eq!(bank.get_or_all(Some(LangId::En)).len(), 1);
        assert_eq!(bank.get_or_all(Some(LangId::Bg)).len(), 1);
        assert_eq!(bank.get_or_all(None).len(), 2);
        assert_eq!(bank.get_or_all(Some(LangId::Tech)).len(), 0);
    }

    #[test]
    fn bigrams_per_language() {
        let mut bank = LanguageModelBank::new([0.6, 0.3, 0.1], false);
        bank.load_bigram("i", "love", 5, LangId::En);
        bank.load_bigram("аз", "обичам", 5, LangId::Bg);

        let en = bank.get(LangId::En).unwrap();
        let bg = bank.get(LangId::Bg).unwrap();

        assert!(en.markov.bigram_prob("love", "i") > 0.01);
        assert!(en.markov.bigram_prob("обичам", "аз") < 0.01);
        assert!(bg.markov.bigram_prob("обичам", "аз") > 0.01);
    }
}
