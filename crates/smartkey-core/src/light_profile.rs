// Light user profile — tracks language priors and ghost acceptance rates.
//
// Auto-tunes confidence floor based on rolling accept/reject ratio:
//   high accept_rate → low floor (show more ghosts)
//   low accept_rate  → high floor (less noise)

use serde::{Deserialize, Serialize};

use crate::lang_detect::LangId;

/// Lightweight user profile for adaptive ghost text behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LightProfile {
    /// Language prior probabilities: [Bg, En, Tech].
    /// Sum ≈ 1.0. Updated incrementally on each committed word.
    pub lang_prior: [f64; 3],
    /// Rolling accept/(accept+reject) ratio for ghost text.
    pub ghost_accept_rate: f64,
    /// Words to suppress ghost after a rejection (0 = no suppression).
    pub suppress_after_reject: u8,
    /// Auto-tuned minimum confidence for ghost display.
    /// Formula: max(0.12, 0.35 - 0.25 * accept_rate)
    pub confidence_floor: f64,
}

impl Default for LightProfile {
    fn default() -> Self {
        Self {
            lang_prior: [0.33, 0.34, 0.33],
            ghost_accept_rate: 0.50,
            suppress_after_reject: 0,
            confidence_floor: 0.225, // max(0.12, 0.35 - 0.25*0.50)
        }
    }
}

impl LightProfile {
    /// Record a ghost text acceptance. Updates accept_rate and confidence_floor.
    pub fn record_accept(&mut self, lang: LangId) {
        // EMA update for accept_rate (α = 0.05 for smooth decay).
        self.ghost_accept_rate = 0.95 * self.ghost_accept_rate + 0.05;
        self.suppress_after_reject = 0;
        self.update_confidence_floor();
        self.observe_lang(lang);
    }

    /// Record a ghost text rejection. Temporarily suppresses ghosts.
    pub fn record_reject(&mut self) {
        self.ghost_accept_rate *= 0.95;
        self.suppress_after_reject = 1; // suppress for next 1 word
        self.update_confidence_floor();
    }

    /// Record observing a language (from committed word). Incremental prior update.
    pub fn observe_lang(&mut self, lang: LangId) {
        let idx = match lang {
            LangId::Bg => 0,
            LangId::En => 1,
            LangId::Tech => 2,
        };
        // EMA-like prior update: α = 0.02 for slow adaptation.
        let alpha = 0.02;
        for (i, prior) in self.lang_prior.iter_mut().enumerate() {
            if i == idx {
                *prior = (1.0 - alpha) * *prior + alpha;
            } else {
                *prior *= 1.0 - alpha;
            }
        }
        // Re-normalize to prevent drift.
        let sum: f64 = self.lang_prior.iter().sum();
        if sum > 0.0 {
            for p in &mut self.lang_prior {
                *p /= sum;
            }
        }
    }

    /// Decrement suppression counter. Returns true if ghost is still suppressed.
    pub fn tick_suppression(&mut self) -> bool {
        if self.suppress_after_reject > 0 {
            self.suppress_after_reject -= 1;
            true
        } else {
            false
        }
    }

    fn update_confidence_floor(&mut self) {
        self.confidence_floor = (0.35 - 0.25 * self.ghost_accept_rate).max(0.12);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_values_are_sane() {
        let p = LightProfile::default();
        // lang_prior sums to ~1.0
        let sum: f64 = p.lang_prior.iter().sum();
        assert!((sum - 1.0).abs() < 1e-9);
        // accept rate starts at 0.5
        assert!((p.ghost_accept_rate - 0.50).abs() < 1e-9);
        // no suppression initially
        assert_eq!(p.suppress_after_reject, 0);
        // confidence floor at default
        assert!((p.confidence_floor - 0.225).abs() < 1e-6);
    }

    #[test]
    fn observe_lang_shifts_prior_toward_observed() {
        let mut p = LightProfile::default();
        let initial_bg = p.lang_prior[0];
        // Observe Bg multiple times
        for _ in 0..20 {
            p.observe_lang(LangId::Bg);
        }
        assert!(p.lang_prior[0] > initial_bg, "Bg prior should increase");
    }

    #[test]
    fn observe_lang_prior_sums_to_one() {
        let mut p = LightProfile::default();
        for _ in 0..10 {
            p.observe_lang(LangId::En);
        }
        let sum: f64 = p.lang_prior.iter().sum();
        assert!((sum - 1.0).abs() < 1e-9);
    }

    #[test]
    fn record_reject_lowers_accept_rate() {
        let mut p = LightProfile::default();
        let initial = p.ghost_accept_rate;
        p.record_reject();
        assert!(p.ghost_accept_rate < initial);
    }

    #[test]
    fn record_reject_sets_suppression() {
        let mut p = LightProfile::default();
        p.record_reject();
        assert_eq!(p.suppress_after_reject, 1);
    }

    #[test]
    fn record_accept_raises_accept_rate() {
        let mut p = LightProfile::default();
        p.record_reject(); // first lower it
        let after_reject = p.ghost_accept_rate;
        p.record_accept(LangId::En);
        assert!(p.ghost_accept_rate > after_reject);
    }

    #[test]
    fn record_accept_clears_suppression() {
        let mut p = LightProfile::default();
        p.record_reject();
        assert!(p.suppress_after_reject > 0);
        p.record_accept(LangId::En);
        assert_eq!(p.suppress_after_reject, 0);
    }

    #[test]
    fn tick_suppression_decrements_and_returns_true() {
        let mut p = LightProfile::default();
        p.record_reject(); // sets suppress_after_reject = 1
        let suppressed = p.tick_suppression();
        assert!(suppressed);
        assert_eq!(p.suppress_after_reject, 0);
    }

    #[test]
    fn tick_suppression_returns_false_when_zero() {
        let mut p = LightProfile::default();
        assert_eq!(p.suppress_after_reject, 0);
        let suppressed = p.tick_suppression();
        assert!(!suppressed);
    }

    #[test]
    fn tick_suppression_exhausts_counter() {
        let mut p = LightProfile::default();
        p.record_reject(); // suppress_after_reject = 1
        assert!(p.tick_suppression()); // → 0, returns true
        assert!(!p.tick_suppression()); // → 0, returns false
    }

    #[test]
    fn confidence_floor_adapts_with_accept_rate() {
        let mut p = LightProfile::default();
        // Drive accept rate high with many accepts
        for _ in 0..50 {
            p.record_accept(LangId::En);
        }
        let high_accept_floor = p.confidence_floor;
        // Reset and drive accept rate low
        let mut p2 = LightProfile::default();
        for _ in 0..50 {
            p2.record_reject();
        }
        let low_accept_floor = p2.confidence_floor;
        // Higher accept rate → lower confidence floor (show more ghosts)
        assert!(high_accept_floor < low_accept_floor);
        // Floor is always >= 0.12
        assert!(high_accept_floor >= 0.12);
        assert!(low_accept_floor >= 0.12);
    }
}
