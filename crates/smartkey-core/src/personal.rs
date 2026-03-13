// Personal profile persistence — versioned envelope for CVM + Markov + weights.
//
// v1: bare CvmSnapshot (backward compat — old personal.json files)
// v2: PersonalProfile { cvm, markov bigrams/trigrams, adaptive weights }

use serde::{Deserialize, Serialize};

use crate::cvm::CvmSnapshot;
use crate::lang_cvm::LangCvmSnapshot;

/// Versioned personal profile containing all learned state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonalProfile {
    /// Schema version. Current = 3.
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
    /// Create a v3 profile from components.
    pub fn new(
        cvm: CvmSnapshot,
        markov: PersonalMarkovSnapshot,
        weights: Option<AdaptiveWeightsSnapshot>,
    ) -> Self {
        Self {
            version: 3,
            cvm,
            markov_bigrams: markov.bigrams,
            markov_trigrams: markov.trigrams,
            weights,
            lang_cvm: None,
        }
    }

    /// Create a v3 profile with per-language CVM tracks.
    pub fn with_lang_cvm(
        cvm: CvmSnapshot,
        markov: PersonalMarkovSnapshot,
        weights: Option<AdaptiveWeightsSnapshot>,
        lang_cvm: LangCvmSnapshot,
    ) -> Self {
        Self {
            version: 3,
            cvm,
            markov_bigrams: markov.bigrams,
            markov_trigrams: markov.trigrams,
            weights,
            lang_cvm: Some(lang_cvm),
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
    // Try v2 first.
    if let Ok(profile) = serde_json::from_str::<PersonalProfile>(json_str) {
        return Ok(profile);
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

        assert_eq!(restored.version, 3);
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
}
