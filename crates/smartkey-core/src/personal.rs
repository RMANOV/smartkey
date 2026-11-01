// Personal profile persistence — versioned envelope for CVM + Markov + weights.
//
// v1: bare CvmSnapshot (backward compat — old personal.json files)
// v2: PersonalProfile { cvm, markov bigrams/trigrams, adaptive weights }

use serde::{Deserialize, Serialize};

use crate::correction_memory::CorrectionSnapshot;
use crate::cvm::CvmSnapshot;
use crate::lang_cvm::LangCvmSnapshot;
use crate::lang_detect::{LangId, LanguageDetector};
use crate::light_profile::LightProfile;

/// Language explicitly attested for a complete personal word. Unsupported
/// future strings retain the rest of the profile without creating evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConfirmedWordLanguage {
    Bg,
    En,
    #[serde(other)]
    Unsupported,
}

impl ConfirmedWordLanguage {
    pub(crate) fn lang(self) -> Option<LangId> {
        match self {
            Self::Bg => Some(LangId::Bg),
            Self::En => Some(LangId::En),
            Self::Unsupported => None,
        }
    }
}

/// Positive full-word confirmation, independent of sampled generic vocabulary.
/// Frequency is deliberately absent: the engine supplies its fixed value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfirmedWord {
    pub word: String,
    pub language: ConfirmedWordLanguage,
}

impl ConfirmedWord {
    pub(crate) fn new(word: &str, lang: LangId) -> Option<Self> {
        let language = match lang {
            LangId::Bg => ConfirmedWordLanguage::Bg,
            LangId::En => ConfirmedWordLanguage::En,
            LangId::Tech => return None,
        };
        if word.chars().count() < 3 || word.chars().any(|ch| ch.is_whitespace() || ch.is_control())
        {
            return None;
        }
        let mut observed_letter = false;
        for ch in word.chars().filter(|ch| ch.is_alphabetic()) {
            if LanguageDetector::instant_classify(ch) != Some(lang) {
                return None;
            }
            observed_letter = true;
        }
        observed_letter.then(|| Self {
            word: word.to_lowercase(),
            language,
        })
    }
}

/// Versioned personal profile containing all learned state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonalProfile {
    /// Schema version. Current = 4.
    pub version: u32,
    /// CVM streaming counter snapshot (v2 compat fallback).
    pub cvm: CvmSnapshot,
    /// Personal Markov bigrams: (context, word, count).
    pub markov_bigrams: Vec<(String, String, u32)>,
    /// Personal Markov trigrams: (w1, w2, word, count).
    pub markov_trigrams: Vec<(String, String, String, u32)>,
    /// Adaptive ensemble weights snapshot.
    pub weights: Option<AdaptiveWeightsSnapshot>,
    /// Per-language CVM tracks (v3, optional for backward compat).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang_cvm: Option<LangCvmSnapshot>,
    /// Correction memory snapshot (v4, optional for backward compat).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corrections: Option<CorrectionSnapshot>,
    /// Light profile snapshot (v4, optional for backward compat).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light_profile: Option<LightProfile>,
    /// Independently confirmed personal words (optional v4 extension).
    /// Absence never implies confirmation from historical CVM/prior data.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub confirmed_words: Vec<ConfirmedWord>,
}

/// Snapshot of adaptive ensemble weights for persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdaptiveWeightsSnapshot {
    /// Corpus weight (α).
    pub alpha: f64,
    /// Markov weight (β).
    pub beta: f64,
    /// Personal weight (γ).
    pub gamma: f64,
    /// Personal Markov blend delta (δ).
    pub personal_markov_delta: f64,
    /// Number of word commits since last weight reset.
    pub commit_count: u32,
}

/// Snapshot of personal Markov chain for serialization.
#[derive(Debug, Clone)]
pub struct PersonalMarkovSnapshot {
    pub bigrams: Vec<(String, String, u32)>,
    pub trigrams: Vec<(String, String, String, u32)>,
}

impl PersonalProfile {
    /// Create a v4 profile from components.
    pub fn new(
        cvm: CvmSnapshot,
        markov: PersonalMarkovSnapshot,
        weights: Option<AdaptiveWeightsSnapshot>,
    ) -> Self {
        Self {
            version: 4,
            cvm,
            markov_bigrams: markov.bigrams,
            markov_trigrams: markov.trigrams,
            weights,
            lang_cvm: None,
            corrections: None,
            light_profile: None,
            confirmed_words: Vec::new(),
        }
    }

    /// Create a v4 profile with per-language CVM tracks.
    pub fn with_lang_cvm(
        cvm: CvmSnapshot,
        markov: PersonalMarkovSnapshot,
        weights: Option<AdaptiveWeightsSnapshot>,
        lang_cvm: LangCvmSnapshot,
    ) -> Self {
        Self {
            version: 4,
            cvm,
            markov_bigrams: markov.bigrams,
            markov_trigrams: markov.trigrams,
            weights,
            lang_cvm: Some(lang_cvm),
            corrections: None,
            light_profile: None,
            confirmed_words: Vec::new(),
        }
    }

    /// Extract the Markov snapshot from this profile.
    pub fn markov_snapshot(&self) -> PersonalMarkovSnapshot {
        PersonalMarkovSnapshot {
            bigrams: self.markov_bigrams.clone(),
            trigrams: self.markov_trigrams.clone(),
        }
    }
}

/// Try to deserialize a personal profile JSON string.
///
/// Supports both formats:
/// - v2: `PersonalProfile` (new format with Markov + weights)
/// - v1: bare `CvmSnapshot` (old format — wrapped in a default profile)
pub fn load_personal_json(json_str: &str) -> Result<PersonalProfile, String> {
    // A malformed versioned envelope must not fall back to unrelated bare-CVM
    // fields. Unsupported optional confirmation languages deserialize safely.
    match serde_json::from_str::<PersonalProfile>(json_str) {
        Ok(profile) => return Ok(profile),
        Err(error) => {
            if serde_json::from_str::<serde_json::Value>(json_str).is_ok_and(|value| {
                value.get("cvm").is_some() || value.get("confirmed_words").is_some()
            }) {
                return Err(format!("invalid personal profile: {error}"));
            }
        }
    }

    // Fall back to v1 (bare CvmSnapshot).
    let cvm: CvmSnapshot =
        serde_json::from_str(json_str).map_err(|e| format!("invalid personal profile: {e}"))?;

    Ok(PersonalProfile {
        version: 1,
        cvm,
        markov_bigrams: Vec::new(),
        markov_trigrams: Vec::new(),
        weights: None,
        lang_cvm: None,
        corrections: None,
        light_profile: None,
        confirmed_words: Vec::new(),
    })
}

/// Extract personal Markov data from a MarkovChain for snapshot.
pub fn extract_markov_snapshot(
    bigrams: &crate::markov::BigramData,
    trigrams: &crate::markov::TrigramData,
) -> PersonalMarkovSnapshot {
    let mut bi = Vec::new();
    for (ctx, (followers, _)) in bigrams {
        for (word, &count) in followers {
            bi.push((ctx.clone(), word.clone(), count));
        }
    }

    let mut tri = Vec::new();
    for (w1, level2) in trigrams {
        for (w2, (followers, _)) in level2 {
            for (word, &count) in followers {
                tri.push((w1.clone(), w2.clone(), word.clone(), count));
            }
        }
    }

    PersonalMarkovSnapshot {
        bigrams: bi,
        trigrams: tri,
    }
}

// ======================================================================
// Tests
// ======================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_cvm_snapshot() -> CvmSnapshot {
        CvmSnapshot {
            version: 1,
            saved_at_unix: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs_f64(),
            capacity: 500,
            max_capacity: 5000,
            round: 0,
            decay_lambda: 0.001,
            words: vec!["hello".into()],
            age_secs: [("hello".into(), 0.0)].into(),
        }
    }

    #[test]
    fn review_confirmation_validates_same_script_whole_tokens_with_punctuation_and_digits() {
        for (word, lang, folded) in [
            ("Qx-name2", LangId::En, "qx-name2"),
            ("Зор-иван2", LangId::Bg, "зор-иван2"),
        ] {
            let confirmed = ConfirmedWord::new(word, lang).unwrap();
            assert_eq!(confirmed.word, folded);
            assert_eq!(confirmed.language.lang(), Some(lang));
        }
        for (word, lang) in [
            ("", LangId::En),
            ("qx", LangId::En),
            ("   ", LangId::En),
            ("qx word", LangId::En),
            ("qx\nword", LangId::En),
            ("qx\u{0000}word", LangId::En),
            ("qxслово", LangId::En),
            ("qxé", LangId::En),
            ("qx漢", LangId::En),
            ("123", LangId::En),
            ("___", LangId::En),
            ("qxword", LangId::Bg),
            ("зориван", LangId::En),
            ("qxword", LangId::Tech),
        ] {
            assert!(
                ConfirmedWord::new(word, lang).is_none(),
                "invalid confirmation: {word:?} {lang:?}"
            );
        }
    }

    #[test]
    fn review_confirmation_malformed_envelope_cannot_fall_back_to_bare_cvm() {
        let profile = PersonalProfile::new(
            test_cvm_snapshot(),
            PersonalMarkovSnapshot {
                bigrams: Vec::new(),
                trigrams: Vec::new(),
            },
            None,
        );
        let mut value = serde_json::to_value(&profile).unwrap();
        // Valid bare-CVM fields must not rescue a malformed explicit envelope.
        for (key, entry) in serde_json::to_value(test_cvm_snapshot())
            .unwrap()
            .as_object()
            .unwrap()
        {
            value
                .as_object_mut()
                .unwrap()
                .entry(key.clone())
                .or_insert(entry.clone());
        }
        value["confirmed_words"] =
            serde_json::json!([{ "word": "qxproof", "language": { "bad": "en" } }]);
        assert!(load_personal_json(&serde_json::to_string(&value).unwrap()).is_err());
    }

    #[test]
    fn test_personal_profile_round_trip() {
        let profile = PersonalProfile::new(
            test_cvm_snapshot(),
            PersonalMarkovSnapshot {
                bigrams: vec![("i".into(), "love".into(), 3)],
                trigrams: vec![("i".into(), "love".into(), "rust".into(), 2)],
            },
            Some(AdaptiveWeightsSnapshot {
                alpha: 0.35,
                beta: 0.40,
                gamma: 0.25,
                personal_markov_delta: 0.2,
                commit_count: 42,
            }),
        );

        let json = serde_json::to_string_pretty(&profile).unwrap();
        let restored = load_personal_json(&json).unwrap();

        assert_eq!(restored.version, 4);
        assert_eq!(restored.markov_bigrams.len(), 1);
        assert_eq!(restored.markov_trigrams.len(), 1);
        assert!(restored.weights.is_some());
        let w = restored.weights.unwrap();
        assert!((w.alpha - 0.35).abs() < 1e-9);
        assert_eq!(w.commit_count, 42);
    }

    #[test]
    fn test_backward_compat_v1_bare_cvm() {
        let cvm = test_cvm_snapshot();
        let json = serde_json::to_string_pretty(&cvm).unwrap();

        // Load as personal profile — should succeed via v1 fallback.
        let profile = load_personal_json(&json).unwrap();
        assert_eq!(profile.version, 1);
        assert!(profile.markov_bigrams.is_empty());
        assert!(profile.markov_trigrams.is_empty());
        assert!(profile.weights.is_none());
        assert_eq!(profile.cvm.words, vec!["hello".to_string()]);
    }

    #[test]
    fn test_invalid_json_returns_error() {
        let result = load_personal_json("not json");
        assert!(result.is_err());
    }

    #[test]
    fn test_personal_markov_persists_round_trip() {
        let markov = PersonalMarkovSnapshot {
            bigrams: vec![
                ("hello".into(), "world".into(), 5),
                ("i".into(), "love".into(), 3),
            ],
            trigrams: vec![("i".into(), "love".into(), "rust".into(), 2)],
        };

        let profile = PersonalProfile::new(test_cvm_snapshot(), markov, None);
        let json = serde_json::to_string(&profile).unwrap();
        let restored = load_personal_json(&json).unwrap();

        assert_eq!(restored.markov_bigrams.len(), 2);
        assert_eq!(restored.markov_trigrams.len(), 1);
        assert!(restored
            .markov_bigrams
            .iter()
            .any(|(c, w, n)| c == "hello" && w == "world" && *n == 5));
    }

    #[test]
    fn personal_profile_float_roundtrip_preserves_f64_bits() {
        let values =
            std::iter::once(0.48475499919573045_f64).chain((1..=16).map(|n| f64::from(n) / 19.0));
        for value in values {
            let mut profile = PersonalProfile::new(
                test_cvm_snapshot(),
                PersonalMarkovSnapshot {
                    bigrams: Vec::new(),
                    trigrams: Vec::new(),
                },
                None,
            );
            profile.light_profile = Some(LightProfile {
                lang_prior: [value, (1.0 - value) * 0.6, (1.0 - value) * 0.4],
                ghost_accept_rate: value,
                suppress_after_reject: 1,
                confidence_floor: (0.35 - 0.25 * value).max(0.12),
            });
            let bits = |light: &LightProfile| {
                (
                    light.lang_prior.map(f64::to_bits),
                    light.ghost_accept_rate.to_bits(),
                    light.suppress_after_reject,
                    light.confidence_floor.to_bits(),
                )
            };
            let expected = bits(profile.light_profile.as_ref().unwrap());
            for generation in 0..3 {
                let json = serde_json::to_string_pretty(&profile).unwrap();
                profile = load_personal_json(&json).unwrap();
                assert_eq!(
                    bits(profile.light_profile.as_ref().unwrap()),
                    expected,
                    "value={value:?}, generation={generation}"
                );
            }
        }
    }
}
