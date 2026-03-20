// Hedge/Exp3 adaptive weight mixer — multiplicative weights update.
//
// Experimental (behind `use_hedge` flag). Replaces EMA-based adaptive
// weight learning with the Hedge algorithm (multiplicative weights).
// Better handles non-stationary weight landscapes and provides
// O(sqrt(T log K)) regret bound.

/// Default learning rate (η) for multiplicative weight updates.
///
/// Controls how aggressively weights shift toward the dominant signal.
/// Lower values = more stable but slower adaptation.
const DEFAULT_ETA: f64 = 0.1;

/// Minimum weight floor to prevent any signal from being zeroed out.
const MIN_WEIGHT: f64 = 0.01;

/// Multiplicative Weights Update (Hedge/Exp3) mixer for ensemble signals.
///
/// Maintains mixing weights for K signals. On each prediction acceptance,
/// rewards the dominant signal and applies multiplicative update:
/// `w[i] *= exp(η * reward[i])`, then renormalize.
pub struct HedgeMixer {
    /// Current mixing weights (sum to 1.0).
    weights: Vec<f64>,
    /// Default weights to decay toward.
    defaults: Vec<f64>,
    /// Learning rate η.
    eta: f64,
    /// Number of updates (for decay scheduling).
    update_count: u32,
    /// Decay interval: every N updates, nudge toward defaults.
    decay_interval: u32,
    /// Decay rate toward defaults (0.0 = no decay, 1.0 = snap to defaults).
    decay_rate: f64,
}

impl HedgeMixer {
    /// Create a new mixer with K signals and uniform initial weights.
    pub fn new(k: usize) -> Self {
        let w = 1.0 / k as f64;
        Self {
            weights: vec![w; k],
            defaults: vec![w; k],
            eta: DEFAULT_ETA,
            update_count: 0,
            decay_interval: 20,
            decay_rate: 0.1,
        }
    }

    /// Create with explicit default weights.
    pub fn with_defaults(defaults: &[f64]) -> Self {
        let mut weights = defaults.to_vec();
        let sum: f64 = weights.iter().sum();
        if sum > 0.0 {
            for w in &mut weights {
                *w /= sum;
            }
        }
        Self {
            weights: weights.clone(),
            defaults: weights,
            eta: DEFAULT_ETA,
            update_count: 0,
            decay_interval: 20,
            decay_rate: 0.1,
        }
    }

    /// Get current mixing weights.
    pub fn weights(&self) -> &[f64] {
        &self.weights
    }

    /// Get weights as a tuple (for 3-signal compatibility with ensemble).
    pub fn weights_triple(&self) -> (f64, f64, f64) {
        (
            self.weights.first().copied().unwrap_or(0.0),
            self.weights.get(1).copied().unwrap_or(0.0),
            self.weights.get(2).copied().unwrap_or(0.0),
        )
    }

    /// Get weights as a 5-tuple (F16: extended signals — corpus, markov, personal, PPM, tech).
    pub fn weights_five(&self) -> (f64, f64, f64, f64, f64) {
        (
            self.weights.first().copied().unwrap_or(0.0),
            self.weights.get(1).copied().unwrap_or(0.0),
            self.weights.get(2).copied().unwrap_or(0.0),
            self.weights.get(3).copied().unwrap_or(0.0),
            self.weights.get(4).copied().unwrap_or(0.0),
        )
    }

    /// Number of signals this mixer tracks.
    pub fn signal_count(&self) -> usize {
        self.weights.len()
    }

    /// Update weights based on per-signal rewards.
    ///
    /// `rewards[i]` should be higher for signals that contributed to
    /// a correct prediction. Typical: raw score of each signal for the
    /// accepted word, normalized to [0, 1].
    pub fn update(&mut self, rewards: &[f64]) {
        if rewards.len() != self.weights.len() {
            return;
        }

        // Multiplicative update: w[i] *= exp(η * reward[i])
        for (w, &r) in self.weights.iter_mut().zip(rewards.iter()) {
            *w *= (self.eta * r).exp();
            *w = w.max(MIN_WEIGHT);
        }

        // Renormalize
        self.normalize();

        self.update_count += 1;

        // Periodic decay toward defaults
        if self.update_count.is_multiple_of(self.decay_interval) {
            for (w, d) in self.weights.iter_mut().zip(self.defaults.iter()) {
                *w += self.decay_rate * (d - *w);
            }
            self.normalize();
        }
    }

    /// Set the learning rate η.
    pub fn set_eta(&mut self, eta: f64) {
        self.eta = eta.max(0.001);
    }

    /// Reset weights to defaults.
    pub fn reset(&mut self) {
        self.weights = self.defaults.clone();
        self.update_count = 0;
    }

    fn normalize(&mut self) {
        let sum: f64 = self.weights.iter().sum();
        if sum > 0.0 {
            for w in &mut self.weights {
                *w /= sum;
            }
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
    fn test_hedge_uniform_init() {
        let m = HedgeMixer::new(3);
        let w = m.weights();
        assert_eq!(w.len(), 3);
        for &wi in w {
            assert!((wi - 1.0 / 3.0).abs() < 1e-9);
        }
    }

    #[test]
    fn test_hedge_update_shifts_weights() {
        let mut m = HedgeMixer::new(3);
        // Reward signal 0 heavily
        m.update(&[1.0, 0.0, 0.0]);
        let w = m.weights();
        assert!(
            w[0] > w[1],
            "rewarded signal should have higher weight: {:?}",
            w
        );
        assert!(
            w[0] > w[2],
            "rewarded signal should have higher weight: {:?}",
            w
        );
    }

    #[test]
    fn test_hedge_weights_sum_to_one() {
        let mut m = HedgeMixer::new(3);
        m.update(&[0.8, 0.1, 0.1]);
        m.update(&[0.2, 0.7, 0.1]);
        m.update(&[0.1, 0.1, 0.8]);
        let sum: f64 = m.weights().iter().sum();
        assert!(
            (sum - 1.0).abs() < 1e-9,
            "weights should sum to 1.0, got {sum}"
        );
    }

    #[test]
    fn test_hedge_with_defaults() {
        let m = HedgeMixer::with_defaults(&[0.4, 0.4, 0.2]);
        let w = m.weights();
        assert!((w[0] - 0.4).abs() < 1e-9);
        assert!((w[1] - 0.4).abs() < 1e-9);
        assert!((w[2] - 0.2).abs() < 1e-9);
    }

    #[test]
    fn test_hedge_decay_toward_defaults() {
        let mut m = HedgeMixer::with_defaults(&[0.4, 0.4, 0.2]);
        // Heavily reward signal 0 many times
        for _ in 0..20 {
            m.update(&[1.0, 0.0, 0.0]);
        }
        // After decay interval (20), weights should have nudged back toward defaults
        let w = m.weights();
        // Signal 0 should still be dominant but not as extreme
        assert!(
            w[1] > MIN_WEIGHT,
            "signal 1 should not be zeroed out: {:?}",
            w
        );
        assert!(
            w[2] > MIN_WEIGHT,
            "signal 2 should not be zeroed out: {:?}",
            w
        );
    }

    #[test]
    fn test_hedge_min_weight_floor() {
        let mut m = HedgeMixer::new(3);
        // Extremely lopsided rewards
        for _ in 0..100 {
            m.update(&[1.0, 0.0, 0.0]);
        }
        let w = m.weights();
        assert!(
            w[1] >= MIN_WEIGHT,
            "weight should not go below floor: {:?}",
            w
        );
        assert!(
            w[2] >= MIN_WEIGHT,
            "weight should not go below floor: {:?}",
            w
        );
    }

    #[test]
    fn test_hedge_reset() {
        let mut m = HedgeMixer::with_defaults(&[0.4, 0.4, 0.2]);
        m.update(&[1.0, 0.0, 0.0]);
        m.reset();
        let w = m.weights();
        assert!((w[0] - 0.4).abs() < 1e-9);
    }

    #[test]
    fn test_hedge_wrong_size_noop() {
        let mut m = HedgeMixer::new(3);
        let before = m.weights().to_vec();
        m.update(&[1.0, 0.0]); // wrong size
        assert_eq!(m.weights(), &before);
    }
}
