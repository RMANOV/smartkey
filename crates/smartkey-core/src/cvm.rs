//! CVM streaming cardinality estimator.
//!
//! Probabilistic counter based on the CVM algorithm (Chakraborty–Vinodchandran–Meel)
//! for estimating the number of distinct elements in a stream using O(log N) memory.
//!
//! Used as the personal vocabulary learning layer in SmartKey — tracks which words
//! the user types frequently so the prediction engine can boost them.

use rand::Rng;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

/// Default exponential decay rate.
///
/// With `λ = 0.001`, a word unseen for ~693 seconds (~11.5 minutes) decays to
/// 50% of its original score. This strikes a balance between retaining recent
/// vocabulary and fading stale high-frequency words during topic switches.
const DEFAULT_DECAY_LAMBDA: f64 = 0.001;

/// Adaptive CVM counter with configurable memory budget.
///
/// Maintains a random sample of observed elements. When the sample buffer fills,
/// a new "round" begins: half the elements are randomly evicted and the round
/// counter increments. The cardinality estimate is `|buffer| * 2^round`.

#[derive(Debug, Clone)]
pub struct CvmCounter {
    /// Current memory capacity (may grow via [`adjust_memory`]).
    capacity: usize,
    /// Hard upper bound on capacity.
    max_capacity: usize,
    /// The random sample of elements currently retained.
    buffer: HashSet<String>,
    /// How many halving rounds have occurred.
    round: u32,
    /// Timestamp of the last time each element was seen (inserted or re-processed).
    last_seen: HashMap<String, Instant>,
    /// Exponential decay rate `λ` used in `exp(-λ * Δt)`.
    decay_lambda: f64,
}

impl CvmCounter {
    /// Create a new counter.
    ///
    /// * `initial_size` — starting buffer capacity before the first round fires.
    /// * `max_size` — upper bound for adaptive memory growth.
    ///
    /// # Panics
    ///
    /// Panics if `initial_size` is 0 or `max_size < initial_size`.
    pub fn new(initial_size: usize, max_size: usize) -> Self {
        assert!(initial_size > 0, "initial_size must be > 0");
        assert!(max_size >= initial_size, "max_size must be >= initial_size");
        Self {
            capacity: initial_size,
            max_capacity: max_size,
            buffer: HashSet::with_capacity(initial_size),
            round: 0,
            last_seen: HashMap::with_capacity(initial_size),
            decay_lambda: DEFAULT_DECAY_LAMBDA,
        }
    }

    /// Set the exponential decay rate `λ` (builder-style).
    ///
    /// Higher values cause faster decay; `0.0` disables decay entirely.
    ///
    /// # Panics
    ///
    /// Panics if `lambda` is negative.
    pub fn with_decay_lambda(mut self, lambda: f64) -> Self {
        assert!(lambda >= 0.0, "decay_lambda must be >= 0.0");
        self.decay_lambda = lambda;
        self
    }

    /// Return the current decay rate `λ`.
    pub fn decay_lambda(&self) -> f64 {
        self.decay_lambda
    }

    /// Process a single element from the stream.
    ///
    /// If the element is already in the buffer, it faces `round + 1` independent
    /// coin flips — on the first "heads" (p = 0.5) it is evicted. This models
    /// geometric decay so that frequently-seen elements survive longer.
    ///
    /// If the element is new, it is inserted unconditionally.
    ///
    /// When the buffer reaches capacity a new round begins (half the elements
    /// are randomly discarded).
    pub fn process(&mut self, element: &str) {
        let mut rng = rand::thread_rng();
        let now = Instant::now();

        if self.buffer.contains(element) {
            // Probabilistic eviction: flip up to (round + 1) coins.
            let mut evicted = false;
            for _ in 0..=self.round {
                if rng.gen::<f64>() >= 0.5 {
                    self.buffer.remove(element);
                    self.last_seen.remove(element);
                    evicted = true;
                    break;
                }
            }
            if !evicted {
                // Element survived — refresh its timestamp.
                self.last_seen.insert(element.to_owned(), now);
            }
        } else {
            self.buffer.insert(element.to_owned());
            self.last_seen.insert(element.to_owned(), now);
        }

        if self.buffer.len() >= self.capacity {
            self.start_new_round();
        }
    }

    /// Estimate the number of distinct elements seen so far.
    pub fn estimate(&self) -> usize {
        self.buffer
            .len()
            .saturating_mul(1_usize << self.round.min(63))
    }

    /// Current round number (number of halvings that have occurred).
    pub fn round(&self) -> u32 {
        self.round
    }

    /// Current memory capacity (buffer slots, not bytes).
    pub fn memory_size(&self) -> usize {
        self.capacity
    }

    /// Adaptively grow memory when the observed error rate is too high.
    ///
    /// If `error_rate > 0.1` and we haven't hit `max_size`, the capacity doubles.
    pub fn adjust_memory(&mut self, error_rate: f64) {
        if error_rate > 0.1 && self.capacity < self.max_capacity {
            self.capacity = (self.capacity * 2).min(self.max_capacity);
        }
    }

    /// Probabilistic membership test.
    ///
    /// Returns `true` if the element is *currently* in the sample buffer.
    /// False negatives are possible (the element may have been evicted); false
    /// positives are not.
    pub fn contains(&self, element: &str) -> bool {
        self.buffer.contains(element)
    }

    /// Frequency score for ranking predictions, with recency decay.
    ///
    /// Returns a value proportional to how "established" **and recent** the
    /// element is.  The raw survival score `2^round` is multiplied by an
    /// exponential decay factor `exp(-λ · Δt)` where `Δt` is the number of
    /// seconds since the element was last seen.
    ///
    /// * If the element is not in the buffer, returns `0.0`.
    /// * If `decay_lambda` is `0.0`, behaves identically to the un-decayed
    ///   version (decay factor is `1.0`).
    pub fn frequency_score(&self, element: &str) -> f64 {
        if self.buffer.contains(element) {
            let base = (1_u64 << self.round.min(63)) as f64;
            let decay = if self.decay_lambda == 0.0 {
                1.0
            } else if let Some(&ts) = self.last_seen.get(element) {
                let dt = ts.elapsed().as_secs_f64();
                (-self.decay_lambda * dt).exp()
            } else {
                // Element in buffer but missing from last_seen (shouldn't happen
                // in normal use). Treat as maximally stale — full decay.
                0.0
            };
            base * decay
        } else {
            0.0
        }
    }

    // ── internal ────────────────────────────────────────────────────────

    /// Begin a new round: randomly discard ~half the buffer, bump round counter.
    fn start_new_round(&mut self) {
        let mut rng = rand::thread_rng();
        let retain_count = self.buffer.len() / 2;

        // Drain into a Vec, shuffle, keep the first half.
        let mut elems: Vec<String> = self.buffer.drain().collect();
        let len = elems.len();
        // Fisher–Yates partial shuffle (only need `retain_count` positions).
        for i in 0..retain_count.min(len) {
            let j = rng.gen_range(i..len);
            elems.swap(i, j);
        }
        // Elements beyond retain_count are evicted — remove their timestamps.
        for evicted in &elems[retain_count..] {
            self.last_seen.remove(evicted);
        }
        elems.truncate(retain_count);
        self.buffer = elems.into_iter().collect();

        self.round += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_counter_estimates_zero() {
        let c = CvmCounter::new(64, 256);
        assert_eq!(c.estimate(), 0);
        assert_eq!(c.round(), 0);
        assert_eq!(c.memory_size(), 64);
    }

    #[test]
    fn single_element() {
        let mut c = CvmCounter::new(64, 256);
        c.process("hello");
        assert!(c.contains("hello"));
        assert!(c.estimate() >= 1);
        assert_eq!(c.round(), 0); // buffer far from full
    }

    #[test]
    fn duplicates_do_not_inflate_buffer() {
        let mut c = CvmCounter::new(64, 256);
        for _ in 0..10 {
            c.process("word");
        }
        // "word" either survived or was evicted; buffer has at most 1 entry.
        assert!(c.buffer.len() <= 1);
        assert_eq!(c.round(), 0);
    }

    #[test]
    fn many_unique_elements_advances_rounds() {
        // Use a reasonably large buffer to keep variance manageable.
        let mut c = CvmCounter::new(256, 1024);
        for i in 0..1000 {
            c.process(&format!("word_{i}"));
        }
        // With capacity 256 and 1000 unique inserts, at least a few rounds fire.
        assert!(
            c.round() > 0,
            "expected at least one round with 1000 unique elements and capacity 256"
        );
        // CVM is a rough probabilistic estimator — the estimate is
        // `buffer_len * 2^round`, so it can overshoot or undershoot.
        // We just sanity-check it's in the right order of magnitude.
        let est = c.estimate();
        assert!(
            est >= 100 && est <= 50_000,
            "estimate {est} out of plausible range for 1000 unique elements"
        );
    }

    #[test]
    fn adaptive_memory_growth() {
        let mut c = CvmCounter::new(32, 256);
        assert_eq!(c.memory_size(), 32);

        // Error rate above threshold doubles capacity.
        c.adjust_memory(0.15);
        assert_eq!(c.memory_size(), 64);

        // Again.
        c.adjust_memory(0.20);
        assert_eq!(c.memory_size(), 128);

        // Low error rate — no change.
        c.adjust_memory(0.05);
        assert_eq!(c.memory_size(), 128);

        // Growth capped at max.
        c.adjust_memory(0.50);
        assert_eq!(c.memory_size(), 256);
        c.adjust_memory(0.50);
        assert_eq!(c.memory_size(), 256); // still capped
    }

    #[test]
    fn round_advancement_halves_buffer() {
        let mut c = CvmCounter::new(10, 1000);
        // Fill exactly to capacity to force a round.
        for i in 0..10 {
            c.process(&format!("el_{i}"));
        }
        // After the round fires the buffer should be roughly half.
        assert_eq!(c.round(), 1);
        assert!(
            c.buffer.len() <= 6,
            "buffer should be ~5 after halving 10, got {}",
            c.buffer.len()
        );
    }

    #[test]
    fn frequency_score_reflects_survival() {
        let mut c = CvmCounter::new(64, 256);
        // Element not present → score 0.
        assert_eq!(c.frequency_score("absent"), 0.0);

        c.process("present");
        // In round 0, surviving element has score ≈ 2^0 * exp(-λ·Δt) ≈ 1.0.
        // Δt is sub-millisecond so the decay factor is effectively 1.0.
        if c.contains("present") {
            let score = c.frequency_score("present");
            assert!(
                (score - 1.0).abs() < 1e-4,
                "expected ~1.0 for freshly inserted element, got {score}"
            );
        }
    }

    #[test]
    fn contains_no_false_positives() {
        let mut c = CvmCounter::new(64, 256);
        c.process("alpha");
        c.process("beta");
        // Never-inserted element must not be found.
        assert!(!c.contains("gamma"));
    }

    #[test]
    #[should_panic(expected = "initial_size must be > 0")]
    fn zero_initial_size_panics() {
        CvmCounter::new(0, 100);
    }

    #[test]
    #[should_panic(expected = "max_size must be >= initial_size")]
    fn max_less_than_initial_panics() {
        CvmCounter::new(200, 100);
    }

    // ── Recency decay tests ────────────────────────────────────────────

    #[test]
    fn zero_decay_lambda_matches_undecayed_score() {
        let mut c = CvmCounter::new(64, 256).with_decay_lambda(0.0);
        c.process("word");
        if c.contains("word") {
            // With lambda=0, decay factor is exactly 1.0 regardless of elapsed time.
            assert!(
                (c.frequency_score("word") - 1.0).abs() < f64::EPSILON,
                "with lambda=0, score should be exactly 2^0 = 1.0"
            );
        }
    }

    #[test]
    fn decay_reduces_score_over_time() {
        // Use a large lambda so even a short sleep produces measurable decay.
        let mut c = CvmCounter::new(64, 256).with_decay_lambda(100.0);
        c.process("fading");
        assert!(c.contains("fading"));

        let score_before = c.frequency_score("fading");
        // Sleep long enough for visible decay (100 * 0.02s = 2.0 → exp(-2) ≈ 0.135).
        std::thread::sleep(std::time::Duration::from_millis(20));
        let score_after = c.frequency_score("fading");

        assert!(
            score_after < score_before,
            "score should decrease over time: before={score_before}, after={score_after}"
        );
        // With λ=100 and Δt≈0.02s, decay ≈ exp(-2) ≈ 0.135.
        // Allow generous range because CI timing is noisy.
        assert!(
            score_after < 0.5,
            "score should have decayed significantly, got {score_after}"
        );
    }

    #[test]
    fn refresh_resets_decay_clock() {
        // Large lambda for visible decay.
        let mut c = CvmCounter::new(64, 256).with_decay_lambda(100.0);
        c.process("refresh_me");
        assert!(c.contains("refresh_me"));

        // Let some time pass so the score decays.
        std::thread::sleep(std::time::Duration::from_millis(15));
        let score_stale = c.frequency_score("refresh_me");

        // Re-process the element — this refreshes last_seen (if it survives the
        // probabilistic eviction). Run several times to increase chance of
        // surviving at least once.
        for _ in 0..20 {
            c.process("refresh_me");
        }

        if c.contains("refresh_me") {
            let score_refreshed = c.frequency_score("refresh_me");
            // Freshly refreshed score should be higher than the stale one.
            assert!(
                score_refreshed > score_stale,
                "refreshed score ({score_refreshed}) should exceed stale ({score_stale})"
            );
        }
    }

    #[test]
    fn with_decay_lambda_builder() {
        let c = CvmCounter::new(64, 256).with_decay_lambda(0.05);
        assert!((c.decay_lambda() - 0.05).abs() < f64::EPSILON);
    }

    #[test]
    fn default_decay_lambda() {
        let c = CvmCounter::new(64, 256);
        assert!(
            (c.decay_lambda() - DEFAULT_DECAY_LAMBDA).abs() < f64::EPSILON,
            "default lambda should be {DEFAULT_DECAY_LAMBDA}, got {}",
            c.decay_lambda()
        );
    }

    #[test]
    #[should_panic(expected = "decay_lambda must be >= 0.0")]
    fn negative_decay_lambda_panics() {
        CvmCounter::new(64, 256).with_decay_lambda(-1.0);
    }

    #[test]
    fn last_seen_cleaned_on_eviction_in_process() {
        let mut c = CvmCounter::new(64, 256).with_decay_lambda(0.0);
        c.process("victim");
        assert!(c.last_seen.contains_key("victim"));

        // Repeatedly process the same element; probabilistic eviction will
        // eventually remove it (given enough attempts in round 0, p=0.5 each).
        let mut evicted = false;
        for _ in 0..200 {
            c.process("victim");
            if !c.contains("victim") {
                evicted = true;
                break;
            }
        }
        if evicted {
            assert!(
                !c.last_seen.contains_key("victim"),
                "last_seen should be cleaned when element is evicted"
            );
        }
    }

    #[test]
    fn last_seen_cleaned_on_round_eviction() {
        // Small capacity to force rounds quickly.
        let mut c = CvmCounter::new(10, 1000).with_decay_lambda(0.0);
        for i in 0..10 {
            c.process(&format!("el_{i}"));
        }
        // A round should have fired, evicting ~half the elements.
        assert_eq!(c.round(), 1);
        // Every element in the buffer should have a last_seen entry,
        // and every evicted element should NOT.
        for elem in c.buffer.iter() {
            assert!(
                c.last_seen.contains_key(elem),
                "surviving element {elem} should have a last_seen entry"
            );
        }
        assert_eq!(
            c.buffer.len(),
            c.last_seen.len(),
            "last_seen should have exactly as many entries as the buffer"
        );
    }
}
