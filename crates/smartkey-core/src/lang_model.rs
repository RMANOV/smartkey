//! Per-language model bank — isolates trie, Markov, and PPM per language
//! so that bigrams from one language don't contaminate another.

use std::collections::HashMap;

use crate::kneser_ney::KneserNeyScorer;
use crate::lang_detect::LangId;
use crate::markov::MarkovChain;
use crate::ngram::NgramTrie;
use crate::ppm::PpmModel;

/// A complete language model for one language.
pub struct LanguageModel {
    pub lang: LangId,
    pub trie: NgramTrie,
    pub markov: MarkovChain,
    pub ppm: Option<PpmModel>,
    /// Interpolated Kneser-Ney scorer (built lazily after corpus load).
    pub kn_scorer: Option<KneserNeyScorer>,
}

impl LanguageModel {
    pub fn new(lang: LangId, markov_lambdas: [f64; 3], use_ppm: bool) -> Self {
        Self {
            lang,
            trie: NgramTrie::new(),
            markov: MarkovChain::with_lambdas(markov_lambdas),
            ppm: if use_ppm {
                Some(PpmModel::default_order())
            } else {
                None
            },
            kn_scorer: None,
        }
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
