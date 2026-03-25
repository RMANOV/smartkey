use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
struct ReplayEvent {
    session: String,
    event: String,
    prediction_id: u64,
    #[serde(default)]
    word: Option<String>,
    #[serde(default)]
    ghost: Option<String>,
    #[serde(default)]
    confidence: Option<f64>,
    #[serde(default)]
    reason: Option<String>,
}

/// One completed real-world prediction outcome from the Linux replay log.
#[derive(Debug, Clone, Serialize)]
pub struct ReplaySample {
    pub session: String,
    pub prediction_id: u64,
    pub word: String,
    pub ghost: String,
    pub confidence: f64,
    pub accepted: bool,
    pub reason: Option<String>,
}

/// Threshold-quality metrics for a single confidence floor.
#[derive(Debug, Clone, Serialize)]
pub struct ThresholdMetrics {
    pub threshold: f64,
    pub shown: usize,
    pub accepted: usize,
    pub precision: f64,
    pub recall: f64,
    pub f1: f64,
}

/// Per-session confidence-floor recommendation.
#[derive(Debug, Clone, Serialize)]
pub struct SessionCalibration {
    pub session: String,
    pub total_samples: usize,
    pub accepted_samples: usize,
    pub recommended: Option<ThresholdMetrics>,
}

/// Aggregate calibration output for a replay log.
#[derive(Debug, Clone, Serialize)]
pub struct CalibrationReport {
    pub total_samples: usize,
    pub total_sessions: usize,
    pub aggregate: Option<ThresholdMetrics>,
    pub sessions: Vec<SessionCalibration>,
}

/// Load a Linux replay JSONL file and join `shown` + `accepted/rejected` events.
pub fn load_replay_samples(path: &Path) -> Result<Vec<ReplaySample>, String> {
    let data = std::fs::read_to_string(path)
        .map_err(|err| format!("failed to read replay log {}: {err}", path.display()))?;

    let mut shown_by_id: HashMap<(String, u64), ReplayEvent> = HashMap::new();
    let mut samples = Vec::new();

    for (line_no, line) in data.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let event: ReplayEvent = serde_json::from_str(trimmed).map_err(|err| {
            format!(
                "invalid replay JSON at {}:{}: {err}",
                path.display(),
                line_no + 1
            )
        })?;

        let key = (event.session.clone(), event.prediction_id);
        match event.event.as_str() {
            "shown" => {
                shown_by_id.insert(key, event);
            }
            "accepted" | "rejected" => {
                let Some(shown) = shown_by_id.remove(&key) else {
                    continue;
                };
                samples.push(ReplaySample {
                    session: shown.session,
                    prediction_id: shown.prediction_id,
                    word: shown.word.unwrap_or_default(),
                    ghost: shown.ghost.unwrap_or_default(),
                    confidence: shown.confidence.unwrap_or(0.0),
                    accepted: event.event == "accepted",
                    reason: event.reason,
                });
            }
            _ => {}
        }
    }

    samples.sort_by(|left, right| {
        left.session
            .cmp(&right.session)
            .then(left.prediction_id.cmp(&right.prediction_id))
    });
    Ok(samples)
}

/// Sweep confidence floors in 0.05 increments and compute precision/recall/F1.
pub fn sweep_thresholds(samples: &[ReplaySample]) -> Vec<ThresholdMetrics> {
    let total_accepted = samples.iter().filter(|sample| sample.accepted).count();

    (0..=20)
        .map(|step| {
            let threshold = step as f64 * 0.05;
            let shown = samples
                .iter()
                .filter(|sample| sample.confidence >= threshold)
                .count();
            let accepted = samples
                .iter()
                .filter(|sample| sample.accepted && sample.confidence >= threshold)
                .count();
            let precision = if shown > 0 {
                accepted as f64 / shown as f64
            } else {
                0.0
            };
            let recall = if total_accepted > 0 {
                accepted as f64 / total_accepted as f64
            } else {
                0.0
            };
            let f1 = if precision + recall > 0.0 {
                2.0 * precision * recall / (precision + recall)
            } else {
                0.0
            };

            ThresholdMetrics {
                threshold,
                shown,
                accepted,
                precision,
                recall,
                f1,
            }
        })
        .collect()
}

fn choose_best_threshold(curve: &[ThresholdMetrics]) -> Option<ThresholdMetrics> {
    curve.iter().cloned().max_by(|left, right| {
        left.f1
            .partial_cmp(&right.f1)
            .unwrap_or(Ordering::Equal)
            .then(
                left.precision
                    .partial_cmp(&right.precision)
                    .unwrap_or(Ordering::Equal),
            )
            .then(
                left.recall
                    .partial_cmp(&right.recall)
                    .unwrap_or(Ordering::Equal),
            )
            .then_with(|| right.threshold.total_cmp(&left.threshold))
    })
}

/// Produce aggregate and per-session confidence-floor recommendations.
pub fn calibrate_replay_samples(samples: &[ReplaySample]) -> CalibrationReport {
    let mut sessions: BTreeMap<String, Vec<ReplaySample>> = BTreeMap::new();
    for sample in samples {
        sessions
            .entry(sample.session.clone())
            .or_default()
            .push(sample.clone());
    }

    let session_reports: Vec<SessionCalibration> = sessions
        .into_iter()
        .map(|(session, group)| {
            let curve = sweep_thresholds(&group);
            let accepted_samples = group.iter().filter(|sample| sample.accepted).count();
            SessionCalibration {
                session,
                total_samples: group.len(),
                accepted_samples,
                recommended: choose_best_threshold(&curve),
            }
        })
        .collect();

    let aggregate_curve = sweep_thresholds(samples);
    CalibrationReport {
        total_samples: samples.len(),
        total_sessions: session_reports.len(),
        aggregate: choose_best_threshold(&aggregate_curve),
        sessions: session_reports,
    }
}

/// Print a concise replay-calibration summary.
pub fn print_calibration_report(report: &CalibrationReport) {
    println!("=== SmartKey Replay Calibration ===");
    println!(
        "Samples: {}  |  Sessions: {}",
        report.total_samples, report.total_sessions
    );

    if let Some(aggregate) = &report.aggregate {
        println!(
            "Recommended ghost_text_min_confidence: {:.2}  |  precision={:.1}% recall={:.1}% f1={:.3}",
            aggregate.threshold,
            aggregate.precision * 100.0,
            aggregate.recall * 100.0,
            aggregate.f1,
        );
    } else {
        println!("No aggregate recommendation available.");
    }
    println!();

    if report.sessions.is_empty() {
        return;
    }

    println!(
        "{:<24} {:>8} {:>8} {:>10} {:>12} {:>10}",
        "Session", "Samples", "Accepts", "Threshold", "Precision", "Recall"
    );
    println!("{}", "-".repeat(78));
    for session in &report.sessions {
        if let Some(recommended) = &session.recommended {
            println!(
                "{:<24} {:>8} {:>8} {:>10.2} {:>11.1}% {:>9.1}%",
                session.session,
                session.total_samples,
                session.accepted_samples,
                recommended.threshold,
                recommended.precision * 100.0,
                recommended.recall * 100.0,
            );
        } else {
            println!(
                "{:<24} {:>8} {:>8} {:>10} {:>12} {:>10}",
                session.session,
                session.total_samples,
                session.accepted_samples,
                "-",
                "-",
                "-",
            );
        }
    }
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_replay_samples_joins_shown_and_outcome() {
        let dir = tempfile::tempdir().expect("temp dir should exist");
        let path = dir.path().join("replay.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"session\":\"s1\",\"event\":\"shown\",\"prediction_id\":1,\"word\":\"world\",\"ghost\":\"rld\",\"confidence\":0.91}\n",
                "{\"session\":\"s1\",\"event\":\"accepted\",\"prediction_id\":1,\"confidence\":0.91,\"reason\":\"tab\"}\n",
                "{\"session\":\"s1\",\"event\":\"shown\",\"prediction_id\":2,\"word\":\"word\",\"ghost\":\"rd\",\"confidence\":0.32}\n",
                "{\"session\":\"s1\",\"event\":\"rejected\",\"prediction_id\":2,\"confidence\":0.32,\"reason\":\"continued_typing\"}\n",
            ),
        )
        .expect("replay log should write");

        let samples = load_replay_samples(&path).expect("replay log should parse");
        assert_eq!(samples.len(), 2);
        assert!(samples[0].accepted);
        assert!(!samples[1].accepted);
    }

    #[test]
    fn calibration_prefers_clean_separating_threshold() {
        let samples = vec![
            ReplaySample {
                session: "s1".into(),
                prediction_id: 1,
                word: "world".into(),
                ghost: "rld".into(),
                confidence: 0.90,
                accepted: true,
                reason: Some("tab".into()),
            },
            ReplaySample {
                session: "s1".into(),
                prediction_id: 2,
                word: "word".into(),
                ghost: "rd".into(),
                confidence: 0.80,
                accepted: true,
                reason: Some("tab".into()),
            },
            ReplaySample {
                session: "s1".into(),
                prediction_id: 3,
                word: "weird".into(),
                ghost: "eird".into(),
                confidence: 0.40,
                accepted: false,
                reason: Some("continued_typing".into()),
            },
            ReplaySample {
                session: "s1".into(),
                prediction_id: 4,
                word: "weak".into(),
                ghost: "eak".into(),
                confidence: 0.30,
                accepted: false,
                reason: Some("continued_typing".into()),
            },
        ];

        let report = calibrate_replay_samples(&samples);
        let recommended = report
            .aggregate
            .expect("aggregate recommendation should exist");
        assert!(
            recommended.threshold >= 0.45 && recommended.threshold <= 0.80,
            "expected a separating threshold, got {}",
            recommended.threshold
        );
        assert!((recommended.precision - 1.0).abs() < 1e-9);
        assert!((recommended.recall - 1.0).abs() < 1e-9);
    }
}
