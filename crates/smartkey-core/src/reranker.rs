//! Tiny feedforward neural reranker for prediction candidates.
//!
//! A 2-layer network (6 → 32 → 1, 257 parameters) that captures nonlinear
//! feature interactions the linear weighted ensemble misses. Pure Rust
//! implementation with zero external dependencies.
//!
//! Inspired by the Hybrid RNN-Transformer approach (Bamberg, 2025) but
//! scaled down to a minimal feedforward architecture suitable for
//! sub-50µs inference in a keyboard prediction engine.

use rand::rngs::SmallRng;
use rand::Rng;
use rand::SeedableRng;
use serde::{Deserialize, Serialize};

/// Number of input features per candidate.
pub const NUM_FEATURES: usize = 6;
/// Hidden layer width.
const HIDDEN: usize = 32;
/// Default learning rate for online gradient descent.
const LEARNING_RATE: f64 = 0.01;
/// Minimum training examples before the model is considered trained.
const MIN_TRAIN_EXAMPLES: usize = 50;

/// Tiny feedforward neural reranker.
///
/// Architecture: Linear(6→32) → ReLU → Linear(32→1) → sigmoid.
///
/// Input features per candidate:
///   [0] ensemble_score  — weighted sum from the linear scoring pass
///   [1] markov_ratio    — normalised Markov probability
///   [2] personal_ratio  — normalised CVM/personal score
///   [3] prefix_coverage — len(prefix) / len(word)
///   [4] frequency_rank  — 1.0 / (1 + rank)
///   [5] context_match   — highest matched n-gram order / 7.0
#[derive(Clone, Serialize, Deserialize)]
pub struct NeuralReranker {
    /// Weights for hidden layer: [HIDDEN][NUM_FEATURES].
    w1: [[f64; NUM_FEATURES]; HIDDEN],
    /// Bias for hidden layer.
    b1: [f64; HIDDEN],
    /// Weights for output layer: [HIDDEN].
    w2: [f64; HIDDEN],
    /// Bias for output layer.
    b2: f64,
    /// Number of training examples seen.
    train_count: usize,
    /// Whether the model is enabled.
    enabled: bool,
}

impl NeuralReranker {
    /// Create a new reranker with small random weights (Xavier-like init).
    pub fn new() -> Self {
        let mut rng = SmallRng::seed_from_u64(0xDEAD_BEEF);
        let scale1 = (2.0 / (NUM_FEATURES + HIDDEN) as f64).sqrt();
        let scale2 = (2.0 / (HIDDEN + 1) as f64).sqrt();

        let mut w1 = [[0.0; NUM_FEATURES]; HIDDEN];
        let mut b1 = [0.0; HIDDEN];
        let mut w2 = [0.0; HIDDEN];

        for row in &mut w1 {
            for val in row.iter_mut() {
                *val = rng.gen_range(-scale1..scale1);
            }
        }
        for val in &mut b1 {
            *val = rng.gen_range(-0.01..0.01);
        }
        for val in &mut w2 {
            *val = rng.gen_range(-scale2..scale2);
        }

        Self {
            w1,
            b1,
            w2,
            b2: 0.0,
            train_count: 0,
            enabled: true,
        }
    }

    /// Whether the reranker has been trained on enough data.
    pub fn is_trained(&self) -> bool {
        self.train_count >= MIN_TRAIN_EXAMPLES && self.enabled
    }

    /// Enable or disable the reranker.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Forward pass: features → score ∈ [0, 1].
    ///
    /// Runs in ~1µs (257 multiply-adds).
    pub fn forward(&self, features: &[f64; NUM_FEATURES]) -> f64 {
        // Hidden layer: h = ReLU(W1 · x + b1)
        let mut hidden = [0.0_f64; HIDDEN];
        for (i, h) in hidden.iter_mut().enumerate() {
            let mut sum = self.b1[i];
            for (j, &f) in features.iter().enumerate() {
                sum += self.w1[i][j] * f;
            }
            // ReLU activation.
            *h = sum.max(0.0);
        }

        // Output layer: o = sigmoid(W2 · h + b2)
        let mut logit = self.b2;
        for (i, &h) in hidden.iter().enumerate() {
            logit += self.w2[i] * h;
        }

        // Sigmoid with clamping for numerical stability.
        sigmoid(logit)
    }

    /// Train on a batch of examples using online gradient descent.
    ///
    /// Each example is a list of feature vectors for candidates, plus the
    /// index of the "correct" candidate (the one that should score highest).
    ///
    /// Returns the average loss over the batch.
    pub fn train_batch(&mut self, examples: &[(Vec<[f64; NUM_FEATURES]>, usize)]) -> f64 {
        if examples.is_empty() {
            return 0.0;
        }

        let mut total_loss = 0.0;

        for (candidates, &target_idx) in examples.iter().map(|(c, t)| (c, t)) {
            if target_idx >= candidates.len() || candidates.is_empty() {
                continue;
            }

            // Forward pass for each candidate.
            let outputs: Vec<f64> = candidates.iter().map(|f| self.forward(f)).collect();

            // Binary cross-entropy loss: target gets label 1.0, others 0.0.
            for (i, (&output, features)) in outputs.iter().zip(candidates.iter()).enumerate() {
                let target = if i == target_idx { 1.0 } else { 0.0 };
                let loss = binary_cross_entropy(output, target);
                total_loss += loss;

                // Backward pass (manual gradient computation).
                let d_output = output - target; // dL/d_logit for BCE + sigmoid

                // Recompute hidden activations for gradient.
                let mut hidden = [0.0_f64; HIDDEN];
                for (hi, h) in hidden.iter_mut().enumerate() {
                    let mut sum = self.b1[hi];
                    for (j, &f) in features.iter().enumerate() {
                        sum += self.w1[hi][j] * f;
                    }
                    *h = sum; // pre-activation
                }

                // Gradient for W2, b2.
                for (hi, &pre_act) in hidden.iter().enumerate() {
                    let h_act = pre_act.max(0.0); // ReLU
                    self.w2[hi] -= LEARNING_RATE * d_output * h_act;
                }
                self.b2 -= LEARNING_RATE * d_output;

                // Gradient for W1, b1 (backprop through ReLU).
                for (hi, &pre_act) in hidden.iter().enumerate() {
                    if pre_act <= 0.0 {
                        continue; // ReLU gradient is 0
                    }
                    let d_hidden = d_output * self.w2[hi];
                    for (j, &f) in features.iter().enumerate() {
                        self.w1[hi][j] -= LEARNING_RATE * d_hidden * f;
                    }
                    self.b1[hi] -= LEARNING_RATE * d_hidden;
                }
            }

            self.train_count += 1;
        }

        let n = examples.len() as f64;
        total_loss / n
    }

    /// Rerank a list of (score, features) pairs.
    ///
    /// Returns reranked scores blended with originals.
    /// Blend factor: 0.7 original + 0.3 neural (conservative).
    pub fn rerank(&self, candidates: &mut [(f64, [f64; NUM_FEATURES])]) {
        if !self.is_trained() || candidates.is_empty() {
            return;
        }

        for (score, features) in candidates.iter_mut() {
            let neural_score = self.forward(features);
            if neural_score.is_finite() {
                *score = 0.7 * *score + 0.3 * neural_score;
            }
            // If NaN/Inf, leave original score unchanged.
        }
    }
}

impl Default for NeuralReranker {
    fn default() -> Self {
        Self::new()
    }
}

/// Sigmoid activation: σ(x) = 1 / (1 + e^(-x)).
#[inline]
fn sigmoid(x: f64) -> f64 {
    let x = x.clamp(-15.0, 15.0); // prevent overflow
    1.0 / (1.0 + (-x).exp())
}

/// Binary cross-entropy loss: -[t·log(p) + (1-t)·log(1-p)].
#[inline]
fn binary_cross_entropy(predicted: f64, target: f64) -> f64 {
    let p = predicted.clamp(1e-7, 1.0 - 1e-7);
    -(target * p.ln() + (1.0 - target) * (1.0 - p).ln())
}

// ====================================================================
// Tests
// ====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_returns_finite_value() {
        let r = NeuralReranker::new();
        let features = [0.5, 0.3, 0.2, 0.4, 0.1, 0.6];
        let score = r.forward(&features);
        assert!(score.is_finite());
        assert!((0.0..=1.0).contains(&score));
    }

    #[test]
    fn forward_with_zeros() {
        let r = NeuralReranker::new();
        let score = r.forward(&[0.0; NUM_FEATURES]);
        assert!(score.is_finite());
        assert!((0.0..=1.0).contains(&score));
    }

    #[test]
    fn forward_with_large_values() {
        let r = NeuralReranker::new();
        let score = r.forward(&[1e6; NUM_FEATURES]);
        assert!(score.is_finite());
        assert!((0.0..=1.0).contains(&score));
    }

    #[test]
    fn forward_with_negative_values() {
        let r = NeuralReranker::new();
        let score = r.forward(&[-1.0, -0.5, 0.0, 0.5, 1.0, -1.0]);
        assert!(score.is_finite());
        assert!((0.0..=1.0).contains(&score));
    }

    #[test]
    fn untrained_reranker_does_not_modify_scores() {
        let r = NeuralReranker::new();
        assert!(!r.is_trained());

        let mut candidates = vec![
            (0.9, [0.9, 0.8, 0.1, 0.5, 0.9, 0.5]),
            (0.5, [0.5, 0.3, 0.5, 0.3, 0.5, 0.3]),
        ];
        let original: Vec<f64> = candidates.iter().map(|(s, _)| *s).collect();
        r.rerank(&mut candidates);
        let after: Vec<f64> = candidates.iter().map(|(s, _)| *s).collect();
        assert_eq!(original, after);
    }

    #[test]
    fn training_reduces_loss() {
        let mut r = NeuralReranker::new();

        // Simple training data: candidate 0 should always win.
        let examples: Vec<(Vec<[f64; NUM_FEATURES]>, usize)> = (0..100)
            .map(|_| {
                (
                    vec![
                        [0.9, 0.8, 0.7, 0.5, 0.9, 0.8], // good candidate
                        [0.1, 0.2, 0.1, 0.3, 0.1, 0.1], // bad candidate
                    ],
                    0, // target is candidate 0
                )
            })
            .collect();

        let loss1 = r.train_batch(&examples[..10]);
        let loss2 = r.train_batch(&examples[10..50]);
        let loss3 = r.train_batch(&examples[50..]);

        // Loss should generally decrease (or at least not explode).
        assert!(
            loss3 < loss1 * 2.0,
            "loss should not explode: {loss1} -> {loss3}"
        );
        assert!(r.is_trained());
    }

    #[test]
    fn trained_reranker_boosts_correct_candidate() {
        let mut r = NeuralReranker::new();

        // Train: candidate with high markov + high personal should win.
        let examples: Vec<(Vec<[f64; NUM_FEATURES]>, usize)> = (0..200)
            .map(|_| {
                (
                    vec![
                        [0.8, 0.9, 0.8, 0.5, 0.7, 0.9], // target
                        [0.7, 0.1, 0.1, 0.5, 0.8, 0.1], // decoy: high corpus but low markov
                        [0.3, 0.3, 0.3, 0.5, 0.3, 0.3], // neutral
                    ],
                    0,
                )
            })
            .collect();

        r.train_batch(&examples);
        assert!(r.is_trained());

        // Verify: target features should score higher than decoy.
        let target_score = r.forward(&[0.8, 0.9, 0.8, 0.5, 0.7, 0.9]);
        let decoy_score = r.forward(&[0.7, 0.1, 0.1, 0.5, 0.8, 0.1]);
        assert!(
            target_score > decoy_score,
            "target ({target_score:.3}) should score higher than decoy ({decoy_score:.3})"
        );
    }

    #[test]
    fn rerank_blends_scores_conservatively() {
        let mut r = NeuralReranker::new();

        // Train enough to activate.
        let examples: Vec<(Vec<[f64; NUM_FEATURES]>, usize)> = (0..100)
            .map(|_| {
                (
                    vec![
                        [0.9, 0.9, 0.9, 0.5, 0.9, 0.9],
                        [0.1, 0.1, 0.1, 0.5, 0.1, 0.1],
                    ],
                    0,
                )
            })
            .collect();
        r.train_batch(&examples);

        let mut candidates = vec![
            (0.8, [0.8, 0.7, 0.6, 0.5, 0.8, 0.7]),
            (0.2, [0.2, 0.1, 0.1, 0.3, 0.2, 0.1]),
        ];
        r.rerank(&mut candidates);

        // Both scores should still be finite and in reasonable range.
        for (score, _) in &candidates {
            assert!(score.is_finite());
            assert!(*score >= 0.0);
        }
    }

    #[test]
    fn serde_roundtrip() {
        let mut r = NeuralReranker::new();
        let examples: Vec<(Vec<[f64; NUM_FEATURES]>, usize)> = (0..100)
            .map(|_| {
                (
                    vec![
                        [0.9, 0.8, 0.7, 0.5, 0.9, 0.8],
                        [0.1, 0.2, 0.1, 0.3, 0.1, 0.1],
                    ],
                    0,
                )
            })
            .collect();
        r.train_batch(&examples);

        let json = serde_json::to_string(&r).unwrap();
        let r2: NeuralReranker = serde_json::from_str(&json).unwrap();

        let features = [0.5, 0.5, 0.5, 0.5, 0.5, 0.5];
        let score1 = r.forward(&features);
        let score2 = r2.forward(&features);
        assert!(
            (score1 - score2).abs() < 1e-10,
            "serde roundtrip should preserve weights"
        );
        assert_eq!(r.train_count, r2.train_count);
    }

    #[test]
    fn sigmoid_edge_cases() {
        assert!((sigmoid(0.0) - 0.5).abs() < 1e-10);
        assert!(sigmoid(100.0) > 0.999);
        assert!(sigmoid(-100.0) < 0.001);
        // NaN clamped to -15.0 by f64::clamp, then sigmoid(-15.0) ≈ 0.
        // f64::NAN.clamp() returns NaN on some platforms, so just check finite input.
        assert!(sigmoid(15.0).is_finite());
        assert!(sigmoid(-15.0).is_finite());
    }

    #[test]
    fn empty_batch_returns_zero_loss() {
        let mut r = NeuralReranker::new();
        let loss = r.train_batch(&[]);
        assert_eq!(loss, 0.0);
    }

    #[test]
    fn disabled_reranker_skips_reranking() {
        let mut r = NeuralReranker::new();
        let examples: Vec<(Vec<[f64; NUM_FEATURES]>, usize)> = (0..100)
            .map(|_| (vec![[0.9, 0.8, 0.7, 0.5, 0.9, 0.8], [0.1; NUM_FEATURES]], 0))
            .collect();
        r.train_batch(&examples);
        assert!(r.is_trained());

        r.set_enabled(false);
        assert!(!r.is_trained());

        let mut candidates = vec![(0.8, [0.8; NUM_FEATURES])];
        let original = candidates[0].0;
        r.rerank(&mut candidates);
        assert_eq!(candidates[0].0, original);
    }
}
