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
    /// Formula: max(0.15, 0.50 - 0.35 * accept_rate)
    pub confidence_floor: f64,
}

impl Default for LightProfile {
    fn default() -> Self {
        Self {
            lang_prior: [0.33, 0.34, 0.33],
            ghost_accept_rate: 0.50,
            suppress_after_reject: 0,
            confidence_floor: 0.325, // max(0.15, 0.50 - 0.35*0.50)
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
        self.suppress_after_reject = 2; // suppress for next 2 words
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
        self.confidence_floor = (0.50 - 0.35 * self.ghost_accept_rate).max(0.15);
    }
}
