//! CVM streaming cardinality estimator.
//!
//! Probabilistic counter based on the CVM algorithm (Chakraborty–Vinodchandran–Meel)
//! for estimating the number of distinct elements in a stream using O(log N) memory.
//!
//! Used as the personal vocabulary learning layer in SmartKey — tracks which words
//! the user types frequently so the prediction engine can boost them.

use rand::Rng;
use std::collections::HashSet;

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
        }
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

        if self.buffer.contains(element) {
            // Probabilistic eviction: flip up to (round + 1) coins.
            for _ in 0..=self.round {
                if rng.gen::<f64>() >= 0.5 {
                    self.buffer.remove(element);
                    break;
                }
            }
        } else {
            self.buffer.insert(element.to_owned());
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

    /// Frequency score for ranking predictions.
    ///
    /// Returns a value proportional to how "established" the element is in the
    /// counter. Elements that survived multiple rounds score higher.
    ///
    /// * If the element is not in the buffer, returns `0.0`.
    /// * Otherwise returns `2^round` (the survival multiplier), giving a raw
    ///   frequency signal that the ensemble layer can normalise.
    pub fn frequency_score(&self, element: &str) -> f64 {
        if self.buffer.contains(element) {
            (1_u64 << self.round.min(63)) as f64
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
        // In round 0, surviving element has score 2^0 = 1.0.
        if c.contains("present") {
            assert!((c.frequency_score("present") - 1.0).abs() < f64::EPSILON);
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
}
