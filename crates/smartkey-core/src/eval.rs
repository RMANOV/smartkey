// Evaluation harness — prediction quality metrics for A/B testing.
//
// Tracks Top-1 Accept Rate, MRR@5, Keystroke Savings, and p50/p99 latency.
// Every subsequent module (lang detection, session cache, PPM, BPE) is evaluated
// against these metrics to prove improvement before shipping as default.

use std::collections::VecDeque;
use std::time::Duration;

/// Rolling window size for latency samples.
const LATENCY_WINDOW: usize = 1000;

/// Prediction quality and performance metrics.
///
/// Counters are monotonic (`u64`); latency samples use a bounded rolling window
/// to avoid unbounded memory growth in long-running sessions.
#[derive(Debug, Clone)]
pub struct PredictionMetrics {
    /// Total words committed (Space, Return, focus_lost).
    total_commits: u64,
    /// Words committed via Tab (ghost accepted).
    accepted_predictions: u64,
    /// Running sum of reciprocal ranks: `Σ 1/rank` for accepted predictions.
    mrr_sum: f64,
    /// Total characters saved by ghost acceptance.
    keystroke_chars_saved: u64,
    /// Total characters the user actually typed.
    keystroke_chars_typed: u64,
    /// Rolling window of recent `predict()` call durations.
    latency_samples: VecDeque<Duration>,
}

/// Snapshot of metrics for logging / export.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MetricsSummary {
    pub total_commits: u64,
    pub accepted_predictions: u64,
    /// `accepted_predictions / total_commits` (0.0 if no commits).
    pub top1_accept_rate: f64,
    /// `mrr_sum / accepted_predictions` (0.0 if none accepted).
    pub mrr_at_5: f64,
    /// `keystroke_chars_saved / (keystroke_chars_saved + keystroke_chars_typed)`.
    pub keystroke_savings: f64,
    /// Median latency (p50) of recent `predict()` calls.
    pub p50_latency: Duration,
    /// 99th percentile latency of recent `predict()` calls.
    pub p99_latency: Duration,
}

impl PredictionMetrics {
    /// Create a fresh metrics instance with all counters at zero.
    pub fn new() -> Self {
        Self {
            total_commits: 0,
            accepted_predictions: 0,
            mrr_sum: 0.0,
            keystroke_chars_saved: 0,
            keystroke_chars_typed: 0,
            latency_samples: VecDeque::with_capacity(LATENCY_WINDOW),
        }
    }

    /// Record a word commit (any commit path: Space, Return, focus_lost).
    pub fn record_commit(&mut self, word_len: usize) {
        self.total_commits += 1;
        self.keystroke_chars_typed += word_len as u64;
    }

    /// Record a ghost-text acceptance via Tab.
    ///
    /// * `rank` — 1-based position of the accepted word in `last_predictions`.
    /// * `ghost_suffix_len` — characters saved (ghost text length).
    /// * `full_word_len` — total length of the committed word.
    pub fn record_acceptance(
        &mut self,
        rank: usize,
        ghost_suffix_len: usize,
        full_word_len: usize,
    ) {
        self.accepted_predictions += 1;
        if rank > 0 {
            self.mrr_sum += 1.0 / rank as f64;
        }
        self.keystroke_chars_saved += ghost_suffix_len as u64;
        // The typed portion is the prefix (full_word_len - ghost_suffix_len).
        let typed = full_word_len.saturating_sub(ghost_suffix_len);
        self.keystroke_chars_typed += typed as u64;
        self.total_commits += 1;
    }

    /// Record a `predict()` call latency sample.
    /// Total number of word commits recorded.
    pub fn commit_count(&self) -> usize {
        self.total_commits as usize
    }

    pub fn record_latency(&mut self, duration: Duration) {
        if self.latency_samples.len() >= LATENCY_WINDOW {
            self.latency_samples.pop_front();
        }
        self.latency_samples.push_back(duration);
    }

    /// Compute a summary snapshot of all metrics.
    pub fn summary(&self) -> MetricsSummary {
        let top1_accept_rate = if self.total_commits > 0 {
            self.accepted_predictions as f64 / self.total_commits as f64
        } else {
            0.0
        };

        let mrr_at_5 = if self.accepted_predictions > 0 {
            self.mrr_sum / self.accepted_predictions as f64
        } else {
            0.0
        };

        let total_chars = self.keystroke_chars_saved + self.keystroke_chars_typed;
        let keystroke_savings = if total_chars > 0 {
            self.keystroke_chars_saved as f64 / total_chars as f64
        } else {
            0.0
        };

        let (p50, p99) = self.compute_percentiles();

        MetricsSummary {
            total_commits: self.total_commits,
            accepted_predictions: self.accepted_predictions,
            top1_accept_rate,
            mrr_at_5,
            keystroke_savings,
            p50_latency: p50,
            p99_latency: p99,
        }
    }

    /// Reset all counters (e.g. for A/B test phase transitions).
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    fn compute_percentiles(&self) -> (Duration, Duration) {
        if self.latency_samples.is_empty() {
            return (Duration::ZERO, Duration::ZERO);
        }

        let mut sorted: Vec<Duration> = self.latency_samples.iter().copied().collect();
        sorted.sort();

        let p50_idx = (sorted.len() as f64 * 0.50) as usize;
        let p99_idx = (sorted.len() as f64 * 0.99) as usize;

        let p50 = sorted[p50_idx.min(sorted.len() - 1)];
        let p99 = sorted[p99_idx.min(sorted.len() - 1)];

        (p50, p99)
    }
}

impl Default for PredictionMetrics {
    fn default() -> Self {
        Self::new()
    }
}

// ======================================================================
// Tests
// ======================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_metrics_summary() {
        let m = PredictionMetrics::new();
        let s = m.summary();
        assert_eq!(s.total_commits, 0);
        assert_eq!(s.accepted_predictions, 0);
        assert_eq!(s.top1_accept_rate, 0.0);
        assert_eq!(s.mrr_at_5, 0.0);
        assert_eq!(s.keystroke_savings, 0.0);
        assert_eq!(s.p50_latency, Duration::ZERO);
        assert_eq!(s.p99_latency, Duration::ZERO);
    }

    #[test]
    fn test_commit_increments_counter() {
        let mut m = PredictionMetrics::new();
        m.record_commit(5);
        m.record_commit(3);
        let s = m.summary();
        assert_eq!(s.total_commits, 2);
        assert_eq!(s.top1_accept_rate, 0.0); // no acceptances
    }

    #[test]
    fn test_acceptance_metrics() {
        let mut m = PredictionMetrics::new();
        // Accept "hello" (5 chars) with ghost suffix "llo" (3 chars) at rank 1.
        m.record_acceptance(1, 3, 5);
        // Accept "world" (5 chars) with ghost suffix "ld" (2 chars) at rank 2.
        m.record_acceptance(2, 2, 5);

        let s = m.summary();
        assert_eq!(s.total_commits, 2);
        assert_eq!(s.accepted_predictions, 2);
        assert_eq!(s.top1_accept_rate, 1.0); // 2/2
                                             // MRR = (1/1 + 1/2) / 2 = 0.75
        assert!((s.mrr_at_5 - 0.75).abs() < 1e-9);
        // Keystroke savings = 5 / (5 + 5) = 0.5
        assert!((s.keystroke_savings - 0.5).abs() < 1e-9);
    }

    #[test]
    fn test_mixed_commits_and_acceptances() {
        let mut m = PredictionMetrics::new();
        m.record_commit(4); // manual commit "test"
        m.record_acceptance(1, 3, 5); // Tab accept "hello" with ghost "llo"
        m.record_commit(6); // manual commit "foobar"

        let s = m.summary();
        assert_eq!(s.total_commits, 3);
        assert_eq!(s.accepted_predictions, 1);
        // Top-1 accept rate: 1/3 ≈ 0.333...
        assert!((s.top1_accept_rate - 1.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn test_latency_percentiles() {
        let mut m = PredictionMetrics::new();
        // Insert 100 samples: 1us, 2us, ..., 100us
        for i in 1..=100 {
            m.record_latency(Duration::from_micros(i));
        }
        let s = m.summary();
        // p50 ≈ 50us, p99 ≈ 99us
        assert!(s.p50_latency >= Duration::from_micros(49));
        assert!(s.p50_latency <= Duration::from_micros(51));
        assert!(s.p99_latency >= Duration::from_micros(98));
    }

    #[test]
    fn test_latency_window_bounded() {
        let mut m = PredictionMetrics::new();
        // Insert more than LATENCY_WINDOW samples.
        for i in 0..LATENCY_WINDOW + 500 {
            m.record_latency(Duration::from_micros(i as u64));
        }
        assert_eq!(m.latency_samples.len(), LATENCY_WINDOW);
    }

    #[test]
    fn test_reset_clears_all() {
        let mut m = PredictionMetrics::new();
        m.record_commit(5);
        m.record_acceptance(1, 3, 5);
        m.record_latency(Duration::from_micros(100));
        m.reset();
        let s = m.summary();
        assert_eq!(s.total_commits, 0);
        assert_eq!(s.accepted_predictions, 0);
        assert!(m.latency_samples.is_empty());
    }
}
