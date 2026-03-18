use std::time::Duration;

use serde::Serialize;

use crate::simulator::SimulationResult;
use crate::timeline::TimelineEvent;

/// Aggregated quality report for a single simulation run.
#[derive(Debug, Clone, Serialize)]
pub struct TypingReport {
    pub scenario_name: String,
    pub total_chars: usize,
    pub total_keystrokes: usize,
    pub words_total: usize,
    pub words_predicted: usize,
    pub chars_saved: usize,
    pub accept_rate: f64,
    pub keystroke_savings: f64,
    pub p50_latency_us: u64,
    pub p99_latency_us: u64,
    #[serde(serialize_with = "serialize_duration")]
    pub duration: Duration,
    #[serde(skip)]
    pub timeline: Vec<TimelineEvent>,
}

fn serialize_duration<S: serde::Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&format!("{}ms", d.as_millis()))
}

impl TypingReport {
    /// Build a report from simulation results.
    pub fn from_result(scenario_name: &str, result: SimulationResult) -> Self {
        let accept_rate = if result.words_total > 0 {
            result.words_predicted as f64 / result.words_total as f64
        } else {
            0.0
        };
        let keystroke_savings = if result.total_chars > 0 {
            result.chars_saved as f64 / result.total_chars as f64
        } else {
            0.0
        };

        Self {
            scenario_name: scenario_name.to_string(),
            total_chars: result.total_chars,
            total_keystrokes: result.total_keystrokes,
            words_total: result.words_total,
            words_predicted: result.words_predicted,
            chars_saved: result.chars_saved,
            accept_rate,
            keystroke_savings,
            p50_latency_us: result.p50_latency_us,
            p99_latency_us: result.p99_latency_us,
            duration: result.duration,
            timeline: result.timeline,
        }
    }

    /// Print a human-readable summary to stdout.
    pub fn print_summary(&self) {
        let pass = if self.accept_rate >= 0.30 {
            "PASS"
        } else {
            "FAIL"
        };

        println!("=== SmartKey Playground: {} ===", self.scenario_name);
        println!(
            "  Words: {}  |  Chars: {}  |  Keystrokes: {}",
            self.words_total, self.total_chars, self.total_keystrokes
        );
        println!(
            "  Accept rate:   {:.1}%  ({}/{} words predicted)",
            self.accept_rate * 100.0,
            self.words_predicted,
            self.words_total
        );
        println!(
            "  Keystroke savings: {:.1}%",
            self.keystroke_savings * 100.0
        );
        println!(
            "  Latency: p50={}us  p99={}us",
            self.p50_latency_us, self.p99_latency_us
        );
        println!("  Duration: {}ms", self.duration.as_millis());
        println!("  -- {} (accept_rate >= 30%) --", pass);
        println!();
    }

    /// Print the full timeline (for --verbose mode).
    pub fn print_timeline(&self) {
        println!("--- Timeline: {} ---", self.scenario_name);
        for event in &self.timeline {
            match event {
                TimelineEvent::CharTyped { offset, ch } => {
                    println!("  [{:4}] CHAR  '{}'", offset, ch);
                }
                TimelineEvent::PredictionShown {
                    offset,
                    ghost,
                    confidence,
                    accepted,
                } => {
                    let mark = if *accepted { "+" } else { "-" };
                    println!(
                        "  [{:4}] GHOST [{}] {:?} (conf={:.3})",
                        offset, mark, ghost, confidence
                    );
                }
                TimelineEvent::PredictionRejected {
                    offset,
                    ghost,
                    reason,
                    ..
                } => {
                    println!("  [{:4}] REJECT {:?} reason={:?}", offset, ghost, reason);
                }
                TimelineEvent::PredictionAccepted {
                    offset,
                    ghost,
                    chars_saved,
                } => {
                    println!(
                        "  [{:4}] ACCEPT {:?} (saved {} chars)",
                        offset, ghost, chars_saved
                    );
                }
                TimelineEvent::WordCommitted {
                    offset,
                    word,
                    was_predicted,
                } => {
                    let mark = if *was_predicted { "P" } else { "T" };
                    println!("  [{:4}] WORD  [{}] {:?}", offset, mark, word);
                }
                TimelineEvent::LanguageSwitched { offset, from, to } => {
                    println!("  [{:4}] LANG  {} -> {}", offset, from, to);
                }
                TimelineEvent::TypoInjected {
                    offset,
                    wrong_char,
                    correct_char,
                } => {
                    println!(
                        "  [{:4}] TYPO  '{}' (correct: '{}')",
                        offset, wrong_char, correct_char
                    );
                }
                TimelineEvent::PauseTriggered { offset } => {
                    println!("  [{:4}] PAUSE", offset);
                }
            }
        }
        println!();
    }

    /// Serialize to JSON string.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|e| format!("{{\"error\": \"{}\"}}", e))
    }
}

// ── Confidence Histogram ───────────────────────────────────────────

/// A single bucket in the confidence histogram.
pub struct ConfidenceBucket {
    pub range: (f64, f64),
    pub shown: usize,
    pub accepted: usize,
}

/// Ten-bucket confidence histogram built from timeline events.
pub struct ConfidenceHistogram {
    pub buckets: Vec<ConfidenceBucket>,
}

/// Build a confidence histogram from timeline events.
///
/// Buckets: [0.0, 0.1), [0.1, 0.2), ..., [0.8, 0.9), [0.9, 1.0].
/// The last bucket is inclusive on both ends to capture confidence = 1.0.
pub fn confidence_histogram(events: &[TimelineEvent]) -> ConfidenceHistogram {
    let mut buckets: Vec<ConfidenceBucket> = (0..10)
        .map(|i| {
            let lo = i as f64 * 0.1;
            let hi = (i + 1) as f64 * 0.1;
            ConfidenceBucket {
                range: (lo, hi),
                shown: 0,
                accepted: 0,
            }
        })
        .collect();

    for event in events {
        if let TimelineEvent::PredictionShown {
            confidence,
            accepted,
            ..
        } = event
        {
            let idx = ((*confidence * 10.0).floor() as usize).min(9);
            buckets[idx].shown += 1;
            if *accepted {
                buckets[idx].accepted += 1;
            }
        }
    }

    ConfidenceHistogram { buckets }
}

/// Print an ASCII bar chart of the confidence histogram.
pub fn print_histogram(scenario_name: &str, hist: &ConfidenceHistogram) {
    println!("Confidence Histogram ({})", scenario_name);
    println!();

    let max_shown = hist
        .buckets
        .iter()
        .map(|b| b.shown)
        .max()
        .unwrap_or(1)
        .max(1);

    for bucket in &hist.buckets {
        let bar_len = (bucket.shown as f64 / max_shown as f64 * 40.0).round() as usize;
        let bar: String = "\u{2588}".repeat(bar_len);
        let accept_pct = if bucket.shown > 0 {
            bucket.accepted as f64 / bucket.shown as f64 * 100.0
        } else {
            0.0
        };
        let end_bracket = if (bucket.range.1 - 1.0).abs() < 1e-9 {
            "]"
        } else {
            ")"
        };
        println!(
            "[{:.1}, {:.1}{}  {:<40}  {:>4}  (accepted: {}/{} = {:.0}%)",
            bucket.range.0,
            bucket.range.1,
            end_bracket,
            bar,
            bucket.shown,
            bucket.accepted,
            bucket.shown,
            accept_pct,
        );
    }
    println!();
}

/// Print a summary table for multiple reports.
pub fn print_summary_table(reports: &[TypingReport]) {
    println!(
        "{:<20} {:>6} {:>6} {:>10} {:>12} {:>8}",
        "Scenario", "Words", "Chars", "Accept %", "KS Savings %", "Status"
    );
    println!("{}", "-".repeat(70));
    for r in reports {
        let status = if r.accept_rate >= 0.30 {
            "PASS"
        } else {
            "FAIL"
        };
        println!(
            "{:<20} {:>6} {:>6} {:>9.1}% {:>11.1}% {:>8}",
            r.scenario_name,
            r.words_total,
            r.total_chars,
            r.accept_rate * 100.0,
            r.keystroke_savings * 100.0,
            status,
        );
    }
    println!();
}
