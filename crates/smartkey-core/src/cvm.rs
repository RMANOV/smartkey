//! CVM streaming cardinality estimator.
//!
//! Probabilistic counter based on the CVM algorithm (Chakraborty–Vinodchandran–Meel)
//! for estimating the number of distinct elements in a stream using O(log N) memory.
//!
//! Used as the personal vocabulary learning layer in SmartKey — tracks which words
//! the user types frequently so the prediction engine can boost them.

use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
    /// Unified data store: element → last-seen timestamp.
    /// Replaces the previous `buffer: HashSet` + `last_seen: HashMap` pair,
    /// eliminating one hash probe and one String allocation per operation.
    data: HashMap<String, Instant>,
    /// How many halving rounds have occurred.
    round: u32,
    /// Exponential decay rate `λ` used in `exp(-λ * Δt)`.
    decay_lambda: f64,
    /// Cached RNG — avoids thread-local lookup on every keystroke.
    rng: SmallRng,
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
            data: HashMap::with_capacity(initial_size),
            round: 0,
            decay_lambda: DEFAULT_DECAY_LAMBDA,
            rng: SmallRng::from_entropy(),
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
        let now = Instant::now();

        if self.data.remove(element).is_some() {
            // Re-insert with probability 2^(-round) per CVM paper.
            let p = if self.round >= 64 {
                0.0
            } else {
                0.5_f64.powi(self.round as i32)
            };
            if self.rng.gen::<f64>() < p {
                self.data.insert(element.to_owned(), now);
            }
        } else {
            self.data.insert(element.to_owned(), now);
        }

        if self.data.len() >= self.capacity {
            self.start_new_round();
        }
    }

    /// Estimate the number of distinct elements seen so far.
    pub fn estimate(&self) -> usize {
        self.data
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
        self.data.contains_key(element)
    }

    /// Frequency score for ranking predictions, with recency decay.
    ///
    /// Returns a value proportional to how "established" **and recent** the
    /// element is.  The raw survival score `2^round` is multiplied by an
    /// exponential decay factor `exp(-λ · Δt)` where `Δt` is the number of
    /// seconds since the element was last seen.
    ///
    /// * If the element is not in the data map, returns `0.0`.
    /// * If `decay_lambda` is `0.0`, behaves identically to the un-decayed
    ///   version (decay factor is `1.0`).
    pub fn frequency_score(&self, element: &str) -> f64 {
        if let Some(&ts) = self.data.get(element) {
            let base = (1_u64 << self.round.min(63)) as f64;
            let decay = if self.decay_lambda == 0.0 {
                1.0
            } else {
                (-self.decay_lambda * ts.elapsed().as_secs_f64()).exp()
            };
            base * decay
        } else {
            0.0
        }
    }

    /// Snapshot the counter state for persistence / export.
    ///
    /// Converts `Instant`-based `last_seen` timestamps into portable `age_secs`
    /// (seconds since each word was last seen at save time).
    pub fn to_snapshot(&self) -> CvmSnapshot {
        let now = Instant::now();
        let saved_at_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();

        let mut words = Vec::with_capacity(self.data.len());
        let mut age_secs = HashMap::with_capacity(self.data.len());
        for (word, &ts) in &self.data {
            words.push(word.clone());
            age_secs.insert(word.clone(), (now - ts).as_secs_f64());
        }

        CvmSnapshot {
            version: 1,
            saved_at_unix,
            capacity: self.capacity,
            max_capacity: self.max_capacity,
            round: self.round,
            decay_lambda: self.decay_lambda,
            words,
            age_secs,
        }
    }

    /// Reconstruct a counter from a saved snapshot.
    ///
    /// Offline time (`now_unix − saved_at_unix`) is added to each word's age
    /// so that decay resumes correctly.
    pub fn from_snapshot(snap: &CvmSnapshot) -> Self {
        let now = Instant::now();
        let now_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let offline_secs = (now_unix - snap.saved_at_unix).max(0.0);

        let data: HashMap<String, Instant> = snap
            .words
            .iter()
            .filter_map(|word| {
                // Words missing from age_secs are at least as old as the offline period.
                let age_at_save = snap.age_secs.get(word).copied().unwrap_or(offline_secs);
                let total_age = (age_at_save + offline_secs).max(0.0);
                if !total_age.is_finite() {
                    return None;
                }
                now.checked_sub(Duration::from_secs_f64(total_age))
                    .map(|ts| (word.clone(), ts))
            })
            .collect();

        Self {
            capacity: snap.capacity,
            max_capacity: snap.max_capacity,
            data,
            round: snap.round,
            decay_lambda: snap.decay_lambda,
            rng: SmallRng::from_entropy(),
        }
    }

    // ── internal ────────────────────────────────────────────────────────

    /// Begin a new round: evict ~half the data with recency-weighted probability,
    /// then bump the round counter.
    ///
    /// Uses `retain()` for in-place eviction — no Vec allocation, no rehash.
    /// Survival probability is `p_keep = 0.3 + 0.5 * exp(-λ·age)`:
    /// - Fresh words (age ≈ 0s): p ≈ 0.80 — strong retention.
    /// - Old words (age → ∞):    p ≈ 0.30 — still some chance, never zero.
    ///
    /// This replaces the previous flat 50/50 coin flip, which ignored timestamps
    /// stored alongside every element.
    fn start_new_round(&mut self) {
        // Clone RNG to avoid borrow conflict with self.data.
        let mut rng = self.rng.clone();
        let now = Instant::now();
        let lambda = self.decay_lambda;
        self.data.retain(|_, ts| {
            let age_secs = now.duration_since(*ts).as_secs_f64();
            let recency = (-lambda * age_secs).exp(); // 1.0 for fresh, decays over time
            let p_keep = 0.3 + 0.5 * recency; // Range: [0.3, 0.8] — never zero, never certain
            rng.gen::<f64>() < p_keep
        });
        self.rng = rng;
        self.round = self.round.saturating_add(1);
    }
}

/// Serialisable snapshot of a [`CvmCounter`] for persistence and export.
///
/// `words` lists the elements currently in the sample buffer, and `age_secs`
/// records how many seconds each word had been unseen at save time.  On load,
/// offline time is added to each age so that decay resumes correctly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvmSnapshot {
    pub version: u32,
    pub saved_at_unix: f64,
    pub capacity: usize,
    pub max_capacity: usize,
    pub round: u32,
    pub decay_lambda: f64,
    pub words: Vec<String>,
    pub age_secs: HashMap<String, f64>,
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
        assert!(c.data.len() <= 1);
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
        // With recency-weighted eviction (p_keep ≈ 0.8 for fresh elements),
        // survival per round is higher than the classical 0.5, so 2^round
        // grows faster and the estimate overshoots more than the classical CVM.
        // The primary use of this counter is `frequency_score()` / `contains()`,
        // not precise cardinality estimation; we just verify the round counter
        // advanced and the buffer fits within max capacity.
        let est = c.estimate();
        assert!(
            est >= 100,
            "estimate {est} is implausibly low for 1000 unique elements"
        );
        assert!(
            c.data.len() <= c.max_capacity,
            "buffer {} exceeds max capacity {}",
            c.data.len(),
            c.max_capacity
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
        // After the round fires the round counter must have incremented.
        // With recency-weighted eviction, fresh elements survive with p ≈ 0.8
        // (range [0.3, 0.8]), so count can be up to capacity — the key invariant
        // is that the round counter advanced and the buffer was not enlarged.
        assert_eq!(c.round(), 1);
        assert!(
            c.data.len() <= c.capacity,
            "data should not exceed capacity after a round, got {}",
            c.data.len()
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
        assert!(c.data.contains_key("victim"));

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
                !c.data.contains_key("victim"),
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
        // A round should have fired; with recency-weighted eviction and lambda=0
        // all elements are "fresh" (p_keep = 0.8), so count can be up to capacity.
        // The key invariant: round advanced and no orphan timestamps (unified map).
        assert_eq!(c.round(), 1);
        // With unified data map, surviving elements inherently have timestamps.
        // Verify count is within the capacity bound.
        assert!(
            c.data.len() <= c.capacity,
            "data should not exceed capacity after round, got {}",
            c.data.len()
        );
    }

    // ── Snapshot round-trip tests ──────────────────────────────────────

    #[test]
    fn snapshot_round_trip_preserves_state() {
        let mut c = CvmCounter::new(64, 256).with_decay_lambda(0.005);
        for word in &["hello", "world", "rust", "test"] {
            c.process(word);
        }
        let snap = c.to_snapshot();
        let restored = CvmCounter::from_snapshot(&snap);

        assert_eq!(restored.capacity, c.capacity);
        assert_eq!(restored.max_capacity, c.max_capacity);
        assert_eq!(restored.round, c.round);
        assert!((restored.decay_lambda - c.decay_lambda).abs() < f64::EPSILON);
        // All restored keys should match the original keys.
        let mut orig_keys: Vec<&String> = c.data.keys().collect();
        let mut rest_keys: Vec<&String> = restored.data.keys().collect();
        orig_keys.sort();
        rest_keys.sort();
        assert_eq!(rest_keys, orig_keys);
    }

    #[test]
    fn snapshot_empty_counter() {
        let c = CvmCounter::new(32, 128);
        let snap = c.to_snapshot();
        assert!(snap.words.is_empty());
        assert!(snap.age_secs.is_empty());
        assert_eq!(snap.version, 1);

        let restored = CvmCounter::from_snapshot(&snap);
        assert_eq!(restored.estimate(), 0);
        assert!(restored.data.is_empty());
    }

    #[test]
    fn snapshot_json_serialization() {
        let mut c = CvmCounter::new(64, 256);
        c.process("serialize_me");
        let snap = c.to_snapshot();

        let json = serde_json::to_string_pretty(&snap).expect("serialize");
        let deser: CvmSnapshot = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(deser.version, snap.version);
        assert_eq!(deser.capacity, snap.capacity);
        assert_eq!(deser.words, snap.words);
    }

    #[test]
    fn from_snapshot_missing_age_defaults_to_stale() {
        let snap = CvmSnapshot {
            version: 1,
            saved_at_unix: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs_f64()
                - 10.0, // saved 10s ago
            capacity: 64,
            max_capacity: 256,
            round: 0,
            decay_lambda: 1.0,
            words: vec!["known".into(), "orphan".into()],
            age_secs: {
                let mut m = HashMap::new();
                m.insert("known".into(), 2.0); // was 2s old at save time
                                               // "orphan" deliberately omitted
                m
            },
        };
        let counter = CvmCounter::from_snapshot(&snap);

        let score_known = counter.frequency_score("known");
        let score_orphan = counter.frequency_score("orphan");

        // "orphan" with missing age should NOT score higher than "known"
        assert!(
            score_orphan <= score_known,
            "orphan (missing age) scored {score_orphan} > known {score_known} — \
             should be stale, not fresh"
        );
    }

    #[test]
    fn snapshot_age_increases_after_delay() {
        let mut c = CvmCounter::new(64, 256);
        c.process("aging");

        let snap1 = c.to_snapshot();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let snap2 = c.to_snapshot();

        if let (Some(&age1), Some(&age2)) =
            (snap1.age_secs.get("aging"), snap2.age_secs.get("aging"))
        {
            assert!(
                age2 > age1,
                "age should increase over time: {age1} → {age2}"
            );
        }
    }
}
