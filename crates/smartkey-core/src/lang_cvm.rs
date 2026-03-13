// Per-language CVM tracks — separate personal vocabulary per detected language.
//
// Wraps multiple CvmCounter instances (one per LangId) so that "hello" learned
// as En and "здравей" learned as Bg don't cross-contaminate each other's
// frequency scores.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::cvm::{CvmCounter, CvmSnapshot};
use crate::lang_detect::LangId;

/// Per-language CVM counter tracks.
pub struct LangCvmTracks {
    tracks: HashMap<LangId, CvmCounter>,
    initial_size: usize,
    max_size: usize,
    decay_lambda: f64,
}

/// Serializable snapshot of all per-language CVM tracks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LangCvmSnapshot {
    pub tracks: Vec<(String, CvmSnapshot)>,
}

impl LangCvmTracks {
    /// Create new tracks with the given CVM sizing parameters.
    pub fn new(initial_size: usize, max_size: usize, decay_lambda: f64) -> Self {
        Self {
            tracks: HashMap::new(),
            initial_size,
            max_size,
            decay_lambda,
        }
    }

    /// Learn a word in the context of a detected language.
    pub fn learn(&mut self, word: &str, lang: LangId) {
        let counter = self
            .tracks
            .entry(lang)
            .or_insert_with(|| {
                CvmCounter::new(self.initial_size, self.max_size)
                    .with_decay_lambda(self.decay_lambda)
            });
        counter.process(word);
    }

    /// Get the frequency score for a word in the given language track.
    ///
    /// Returns 0.0 if the word is not in the specified language track.
    pub fn frequency_score(&self, word: &str, lang: LangId) -> f64 {
        self.tracks
            .get(&lang)
            .map(|c| c.frequency_score(word))
            .unwrap_or(0.0)
    }

    /// Check if a word exists in a specific language track.
    pub fn contains(&self, word: &str, lang: LangId) -> bool {
        self.tracks
            .get(&lang)
            .map(|c| c.contains(word))
            .unwrap_or(false)
    }

    /// Snapshot all tracks for persistence.
    pub fn to_snapshot(&self) -> LangCvmSnapshot {
        let tracks = self
            .tracks
            .iter()
            .map(|(lang, counter)| {
                let key = match lang {
                    LangId::Bg => "bg",
                    LangId::En => "en",
                    LangId::Tech => "tech",
                };
                (key.to_string(), counter.to_snapshot())
            })
            .collect();
        LangCvmSnapshot { tracks }
    }

    /// Restore from a snapshot.
    pub fn from_snapshot(snap: &LangCvmSnapshot) -> Self {
        let mut tracks = HashMap::new();
        let mut initial_size = 500;
        let mut max_size = 5000;
        let mut decay_lambda = 0.001;

        for (key, cvm_snap) in &snap.tracks {
            let lang = match key.as_str() {
                "bg" => LangId::Bg,
                "en" => LangId::En,
                "tech" => LangId::Tech,
                _ => continue,
            };
            initial_size = cvm_snap.capacity;
            max_size = cvm_snap.max_capacity;
            decay_lambda = cvm_snap.decay_lambda;
            tracks.insert(lang, CvmCounter::from_snapshot(cvm_snap));
        }

        Self {
            tracks,
            initial_size,
            max_size,
            decay_lambda,
        }
    }
}

// ======================================================================
// Tests
// ======================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lang_cvm_isolation() {
        let mut tracks = LangCvmTracks::new(500, 5000, 0.001);
        tracks.learn("hello", LangId::En);
        tracks.learn("здравей", LangId::Bg);

        // "hello" should only appear in En track
        assert!(tracks.frequency_score("hello", LangId::En) > 0.0);
        assert_eq!(tracks.frequency_score("hello", LangId::Bg), 0.0);

        // "здравей" should only appear in Bg track
        assert!(tracks.frequency_score("здравей", LangId::Bg) > 0.0);
        assert_eq!(tracks.frequency_score("здравей", LangId::En), 0.0);
    }

    #[test]
    fn test_lang_cvm_contains() {
        let mut tracks = LangCvmTracks::new(500, 5000, 0.001);
        tracks.learn("test", LangId::En);

        assert!(tracks.contains("test", LangId::En));
        assert!(!tracks.contains("test", LangId::Bg));
        assert!(!tracks.contains("unknown", LangId::En));
    }

    #[test]
    fn test_lang_cvm_snapshot_roundtrip() {
        let mut tracks = LangCvmTracks::new(500, 5000, 0.001);
        tracks.learn("hello", LangId::En);
        tracks.learn("world", LangId::En);
        tracks.learn("здравей", LangId::Bg);

        let snap = tracks.to_snapshot();
        let restored = LangCvmTracks::from_snapshot(&snap);

        // Verify En track survived
        assert!(restored.contains("hello", LangId::En));
        assert!(restored.contains("world", LangId::En));
        // Verify Bg track survived
        assert!(restored.contains("здравей", LangId::Bg));
        // Verify isolation preserved
        assert!(!restored.contains("hello", LangId::Bg));
    }

    #[test]
    fn test_backward_compat_empty_snapshot() {
        let snap = LangCvmSnapshot {
            tracks: Vec::new(),
        };
        let restored = LangCvmTracks::from_snapshot(&snap);
        assert_eq!(restored.frequency_score("anything", LangId::En), 0.0);
    }

    #[test]
    fn test_lazy_track_creation() {
        let tracks = LangCvmTracks::new(500, 5000, 0.001);
        // No tracks created until first learn()
        assert!(tracks.tracks.is_empty());
    }
}
