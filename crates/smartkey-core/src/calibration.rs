// Confidence calibration — isotonic regression for prediction confidence.
//
// Maps raw confidence scores (score/sum ratios) to calibrated probabilities
// that reflect actual acceptance likelihood. Uses a rolling buffer of
// (predicted_confidence, was_accepted) pairs and fits a monotone non-decreasing
// function via the Pool Adjacent Violators (PAV) algorithm.
//
// Effect: ghost gates operate on real probabilities instead of arbitrary
// score ratios, improving both precision and recall.

/// Maximum number of observations in the calibration buffer.
const BUFFER_SIZE: usize = 1000;

/// Minimum observations before calibration is applied.
const MIN_OBSERVATIONS: usize = 50;

/// Number of bins for the isotonic regression lookup table.
const NUM_BINS: usize = 20;

/// A single observation: predicted confidence and whether it was accepted.
#[derive(Debug, Clone, Copy)]
struct CalibrationSample {
    confidence: f64,
    accepted: bool,
}

/// Isotonic regression calibrator for prediction confidence.
///
/// Maintains a rolling buffer of recent predictions and their outcomes.
/// Periodically rebuilds a monotone calibration function using the
/// Pool Adjacent Violators algorithm.
pub struct ConfidenceCalibrator {
    /// Rolling buffer of (confidence, accepted) observations.
    buffer: Vec<CalibrationSample>,
    /// Write pointer into the circular buffer.
    write_pos: usize,
    /// Total observations recorded (may exceed BUFFER_SIZE).
    total_observations: usize,
    /// Calibration lookup table: bin boundaries → calibrated probability.
    /// Rebuilt periodically from the buffer.
    bins: Vec<CalibrationBin>,
    /// Whether the lookup table needs rebuilding.
    dirty: bool,
}

#[derive(Debug, Clone)]
struct CalibrationBin {
    /// Upper bound of this bin (exclusive, except last bin).
    upper: f64,
    /// Calibrated probability for this bin.
    calibrated: f64,
}

impl ConfidenceCalibrator {
    pub fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(BUFFER_SIZE),
            write_pos: 0,
            total_observations: 0,
            bins: Vec::new(),
            dirty: false,
        }
    }

    /// Record a prediction outcome.
    ///
    /// `confidence`: the raw confidence from ensemble scoring (0.0..1.0).
    /// `accepted`: true if the user accepted this prediction (Tab press).
    pub fn observe(&mut self, confidence: f64, accepted: bool) {
        let sample = CalibrationSample {
            confidence,
            accepted,
        };

        if self.buffer.len() < BUFFER_SIZE {
            self.buffer.push(sample);
        } else {
            self.buffer[self.write_pos] = sample;
        }
        self.write_pos = (self.write_pos + 1) % BUFFER_SIZE;
        self.total_observations += 1;

        // Rebuild every 50 observations.
        if self.total_observations.is_multiple_of(50) {
            self.dirty = true;
        }
    }

    /// Calibrate a raw confidence score to a probability.
    ///
    /// Returns `None` if not enough data for calibration (falls back to raw).
    pub fn calibrate(&mut self, raw_confidence: f64) -> Option<f64> {
        if self.total_observations < MIN_OBSERVATIONS {
            return None;
        }

        if self.dirty {
            self.rebuild();
            self.dirty = false;
        }

        if self.bins.is_empty() {
            return None;
        }

        // Binary search for the bin containing raw_confidence.
        let calibrated = self
            .bins
            .iter()
            .find(|b| raw_confidence <= b.upper)
            .map(|b| b.calibrated)
            .unwrap_or_else(|| self.bins.last().map(|b| b.calibrated).unwrap_or(raw_confidence));

        Some(calibrated.clamp(0.0, 1.0))
    }

    /// Whether calibration is ready (enough data collected).
    pub fn is_ready(&self) -> bool {
        self.total_observations >= MIN_OBSERVATIONS
    }

    /// Rebuild the calibration lookup table using Pool Adjacent Violators.
    fn rebuild(&mut self) {
        if self.buffer.len() < MIN_OBSERVATIONS {
            return;
        }

        // Sort observations by confidence.
        let mut sorted: Vec<(f64, f64)> = self
            .buffer
            .iter()
            .map(|s| (s.confidence, if s.accepted { 1.0 } else { 0.0 }))
            .collect();
        sorted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        // Bin the observations.
        let bin_size = sorted.len() / NUM_BINS;
        if bin_size == 0 {
            return;
        }

        let mut bins = Vec::with_capacity(NUM_BINS);
        for i in 0..NUM_BINS {
            let start = i * bin_size;
            let end = if i == NUM_BINS - 1 {
                sorted.len()
            } else {
                (i + 1) * bin_size
            };
            let slice = &sorted[start..end];

            let upper = slice.last().map(|(c, _)| *c).unwrap_or(1.0);
            let accept_rate: f64 = slice.iter().map(|(_, a)| a).sum::<f64>() / slice.len() as f64;

            bins.push(CalibrationBin {
                upper,
                calibrated: accept_rate,
            });
        }

        // Pool Adjacent Violators: enforce monotone non-decreasing.
        // Merge adjacent bins where calibrated probability decreases.
        let mut pav: Vec<(f64, f64, usize)> = bins
            .iter()
            .map(|b| (b.upper, b.calibrated, 1))
            .collect();

        let mut i = 0;
        while i < pav.len() - 1 {
            if pav[i].1 > pav[i + 1].1 {
                // Pool: merge bins i and i+1.
                let total_weight = pav[i].2 + pav[i + 1].2;
                let merged_val = (pav[i].1 * pav[i].2 as f64
                    + pav[i + 1].1 * pav[i + 1].2 as f64)
                    / total_weight as f64;
                let merged_upper = pav[i + 1].0;
                pav[i] = (merged_upper, merged_val, total_weight);
                pav.remove(i + 1);
                // Back up to check the previous pair.
                i = i.saturating_sub(1);
            } else {
                i += 1;
            }
        }

        self.bins = pav
            .into_iter()
            .map(|(upper, calibrated, _)| CalibrationBin { upper, calibrated })
            .collect();
    }
}

impl Default for ConfidenceCalibrator {
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
    fn test_calibrator_not_ready_with_few_samples() {
        let mut cal = ConfidenceCalibrator::new();
        for i in 0..10 {
            cal.observe(i as f64 / 10.0, i > 5);
        }
        assert!(!cal.is_ready());
        assert!(cal.calibrate(0.5).is_none());
    }

    #[test]
    fn test_calibrator_ready_after_enough_samples() {
        let mut cal = ConfidenceCalibrator::new();
        // Generate 100 samples where acceptance correlates with confidence.
        for i in 0..100 {
            let conf = i as f64 / 100.0;
            let accepted = conf > 0.4; // Accept if confidence > 0.4
            cal.observe(conf, accepted);
        }
        assert!(cal.is_ready());
        let result = cal.calibrate(0.8);
        assert!(result.is_some());
        // High confidence should calibrate to high probability.
        assert!(
            result.unwrap() > 0.5,
            "high confidence should map to high probability: {:?}",
            result
        );
    }

    #[test]
    fn test_calibrator_monotone() {
        let mut cal = ConfidenceCalibrator::new();
        for i in 0..200 {
            let conf = (i % 100) as f64 / 100.0;
            let accepted = conf > 0.3 + (i as f64 / 1000.0).sin() * 0.1;
            cal.observe(conf, accepted);
        }
        // Check monotonicity of calibrated outputs.
        let mut prev = 0.0_f64;
        for step in 0..20 {
            let conf = step as f64 / 20.0;
            if let Some(cal_val) = cal.calibrate(conf) {
                assert!(
                    cal_val >= prev - 1e-9,
                    "calibration should be monotone: {prev} > {cal_val} at conf={conf}"
                );
                prev = cal_val;
            }
        }
    }

    #[test]
    fn test_calibrator_bounds() {
        let mut cal = ConfidenceCalibrator::new();
        for i in 0..100 {
            cal.observe(i as f64 / 100.0, i > 50);
        }
        // All calibrated values should be in [0, 1].
        for step in 0..100 {
            let conf = step as f64 / 100.0;
            if let Some(val) = cal.calibrate(conf) {
                assert!(val >= 0.0 && val <= 1.0, "out of bounds: {val}");
            }
        }
    }
}
