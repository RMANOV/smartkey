//! Binary corpus format for fast startup.
//!
//! Provides a `Corpus` structure that can be loaded from JSON or bincode,
//! and applied to a `SmartKeyEngine`.

use crate::bpe::{parse_merge_rules, BpeTokenizer};
use crate::ensemble::SmartKeyEngine;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A single word with its corpus frequency.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusWord {
    pub word: String,
    pub frequency: u32,
}

/// A bigram entry: context -> word with count.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusBigram {
    pub ctx: String,
    pub word: String,
    pub count: u32,
}

/// A trigram entry: (w1, w2) -> word with count.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusTrigram {
    pub w1: String,
    pub w2: String,
    pub word: String,
    pub count: u32,
}

/// A BPE merge rule entry for corpus serialization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusBpeMerge {
    pub a: String,
    pub b: String,
    pub merged: String,
}

/// Versioned corpus container.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Corpus {
    pub version: u32,
    pub words: Vec<CorpusWord>,
    pub bigrams: Vec<CorpusBigram>,
    pub trigrams: Vec<CorpusTrigram>,
    /// Optional BPE merge rules for OOV fallback (v0.4.0).
    #[serde(default)]
    pub bpe_merges: Vec<CorpusBpeMerge>,
}

/// Warnings emitted during corpus parsing.
#[derive(Debug, Default, Clone)]
pub struct CorpusWarnings {
    pub dropped_bigrams: usize,
    pub dropped_trigrams: usize,
}

impl Corpus {
    /// Parse corpus from JSON, reporting dropped entries.
    pub fn from_json_with_warnings(json_str: &str) -> Result<(Self, CorpusWarnings), String> {
        let v: serde_json::Value = serde_json::from_str(json_str).map_err(|e| e.to_string())?;

        let words = if let Some(obj) = v.get("unigrams").and_then(|v| v.as_object()) {
            obj.iter()
                .map(|(word, freq)| CorpusWord {
                    word: word.clone(),
                    frequency: freq.as_u64().unwrap_or(0).min(u32::MAX as u64) as u32,
                })
                .collect()
        } else {
            Vec::new()
        };

        let mut warnings = CorpusWarnings::default();

        let bigrams = if let Some(arr) = v.get("bigrams").and_then(|v| v.as_array()) {
            let total = arr.len();
            let parsed: Vec<_> = arr
                .iter()
                .filter_map(|entry| {
                    Some(CorpusBigram {
                        ctx: entry.get("ctx")?.as_str()?.to_owned(),
                        word: entry.get("word")?.as_str()?.to_owned(),
                        count: entry.get("count")?.as_u64()?.min(u32::MAX as u64) as u32,
                    })
                })
                .collect();
            warnings.dropped_bigrams = total - parsed.len();
            parsed
        } else {
            Vec::new()
        };

        let trigrams = if let Some(arr) = v.get("trigrams").and_then(|v| v.as_array()) {
            let total = arr.len();
            let parsed: Vec<_> = arr
                .iter()
                .filter_map(|entry| {
                    Some(CorpusTrigram {
                        w1: entry.get("w1")?.as_str()?.to_owned(),
                        w2: entry.get("w2")?.as_str()?.to_owned(),
                        word: entry.get("word")?.as_str()?.to_owned(),
                        count: entry.get("count")?.as_u64()?.min(u32::MAX as u64) as u32,
                    })
                })
                .collect();
            warnings.dropped_trigrams = total - parsed.len();
            parsed
        } else {
            Vec::new()
        };

        // Parse optional BPE merge rules.
        let bpe_merges = if let Some(arr) = v.get("bpe_merges").and_then(|v| v.as_array()) {
            arr.iter()
                .filter_map(|entry| {
                    Some(CorpusBpeMerge {
                        a: entry.get("a")?.as_str()?.to_owned(),
                        b: entry.get("b")?.as_str()?.to_owned(),
                        merged: entry.get("merged")?.as_str()?.to_owned(),
                    })
                })
                .collect()
        } else {
            Vec::new()
        };

        Ok((
            Self {
                version: 1,
                words,
                bigrams,
                trigrams,
                bpe_merges,
            },
            warnings,
        ))
    }

    /// Parse a corpus from JSON string.
    ///
    /// Expected format:
    /// ```json
    /// { "unigrams": {"word": freq, ...},
    ///   "bigrams": [{"ctx": "...", "word": "...", "count": N}, ...],
    ///   "trigrams": [{"w1": "...", "w2": "...", "word": "...", "count": N}, ...] }
    /// ```
    pub fn from_json(json_str: &str) -> Result<Self, String> {
        Self::from_json_with_warnings(json_str).map(|(c, _)| c)
    }

    /// Serialize to bincode.
    pub fn to_bincode(&self) -> Result<Vec<u8>, String> {
        bincode::serialize(self).map_err(|e| e.to_string())
    }

    /// Deserialize from bincode.
    pub fn from_bincode(bytes: &[u8]) -> Result<Self, String> {
        bincode::deserialize(bytes).map_err(|e| e.to_string())
    }

    /// Load this corpus into a SmartKeyEngine.
    ///
    /// Also loads BPE merge rules (if present) and builds the Kneser-Ney
    /// scorer (if the flag is enabled).
    pub fn load_into_engine(&self, engine: &mut SmartKeyEngine) {
        for w in &self.words {
            engine.load_word(&w.word, w.frequency);
        }
        for b in &self.bigrams {
            engine.load_bigram(&b.ctx, &b.word, b.count);
        }
        for t in &self.trigrams {
            engine.load_trigram(&t.w1, &t.w2, &t.word, t.count);
        }

        // Load BPE merge rules if present in corpus.
        if !self.bpe_merges.is_empty() {
            let raw: Vec<(String, String, String)> = self
                .bpe_merges
                .iter()
                .map(|m| (m.a.clone(), m.b.clone(), m.merged.clone()))
                .collect();
            let merge_rules = parse_merge_rules(&raw);
            // Build token frequencies from unigrams (subword tokens that appear as words).
            let token_freqs: HashMap<String, u32> = self
                .words
                .iter()
                .map(|w| (w.word.clone(), w.frequency))
                .collect();
            engine.set_bpe(BpeTokenizer::new(merge_rules, token_freqs));
        }

        // Build Kneser-Ney scorer from loaded Markov data (if flag is set).
        engine.build_kneser_ney();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_round_trip() {
        let json = r#"{
            "unigrams": {"hello": 100, "world": 80},
            "bigrams": [{"ctx": "say", "word": "hello", "count": 5}],
            "trigrams": [{"w1": "i", "w2": "say", "word": "hello", "count": 3}]
        }"#;

        let corpus = Corpus::from_json(json).expect("parse JSON");
        assert_eq!(corpus.version, 1);
        assert_eq!(corpus.words.len(), 2);
        assert_eq!(corpus.bigrams.len(), 1);
        assert_eq!(corpus.trigrams.len(), 1);

        // Round-trip through bincode.
        let bytes = corpus.to_bincode().expect("serialize to bincode");
        let restored = Corpus::from_bincode(&bytes).expect("parse bincode");

        assert_eq!(restored.version, corpus.version);
        assert_eq!(restored.words.len(), corpus.words.len());
        assert_eq!(restored.bigrams.len(), corpus.bigrams.len());
        assert_eq!(restored.trigrams.len(), corpus.trigrams.len());

        // Verify bigram content.
        assert_eq!(restored.bigrams[0].ctx, "say");
        assert_eq!(restored.bigrams[0].word, "hello");
        assert_eq!(restored.bigrams[0].count, 5);

        // Verify trigram content.
        assert_eq!(restored.trigrams[0].w1, "i");
        assert_eq!(restored.trigrams[0].w2, "say");
        assert_eq!(restored.trigrams[0].word, "hello");
        assert_eq!(restored.trigrams[0].count, 3);
    }

    #[test]
    fn from_json_reports_dropped_entries() {
        let json = r#"{
            "unigrams": {"hello": 10},
            "bigrams": [
                {"ctx": "the", "word": "hello", "count": 5},
                {"ctx": "bad"},
                {"word": "orphan"}
            ],
            "trigrams": [
                {"w1": "a", "w2": "b", "word": "c", "count": 1},
                {"w1": "missing"}
            ]
        }"#;
        let (corpus, warnings) = Corpus::from_json_with_warnings(json).unwrap();
        assert_eq!(corpus.words.len(), 1);
        assert_eq!(corpus.bigrams.len(), 1);
        assert_eq!(corpus.trigrams.len(), 1);
        assert_eq!(warnings.dropped_bigrams, 2);
        assert_eq!(warnings.dropped_trigrams, 1);
    }

    #[test]
    fn load_into_engine_produces_predictions() {
        let json = r#"{
            "unigrams": {"hello": 100, "help": 80, "world": 60},
            "bigrams": [{"ctx": "say", "word": "hello", "count": 5}],
            "trigrams": []
        }"#;

        let corpus = Corpus::from_json(json).expect("parse JSON");
        let mut engine = SmartKeyEngine::new();
        corpus.load_into_engine(&mut engine);

        let preds = engine.predict("hel", &[], 5, None);
        assert!(
            !preds.is_empty(),
            "engine should produce predictions after loading corpus"
        );
        assert!(
            preds.iter().any(|p| p.word == "hello"),
            "'hello' should appear in predictions"
        );
    }
}
