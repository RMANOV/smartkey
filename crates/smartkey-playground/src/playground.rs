use std::path::{Path, PathBuf};

use smartkey_core::input::InputConfig;

use crate::report::TypingReport;
use crate::scenario::Scenario;
use crate::simulator::SessionSimulator;

/// Fluent builder for running simulation scenarios.
pub struct Playground {
    corpus_dir: PathBuf,
    scenarios: Vec<Scenario>,
    threshold: f64,
}

impl Playground {
    /// Create a new playground with default settings.
    ///
    /// Default corpus directory: `../../corpus/` relative to this crate.
    pub fn new() -> Self {
        // Default: locate corpus/ relative to the workspace root
        let default_corpus = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent() // crates/
            .and_then(|p| p.parent()) // smartkey/
            .map(|p| p.join("corpus"))
            .unwrap_or_else(|| PathBuf::from("corpus"));

        Self {
            corpus_dir: default_corpus,
            scenarios: Vec::new(),
            threshold: 0.3,
        }
    }

    /// Set the corpus directory path.
    pub fn corpus_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.corpus_dir = path.into();
        self
    }

    /// Add a scenario to run.
    pub fn scenario(mut self, scenario: Scenario) -> Self {
        self.scenarios.push(scenario);
        self
    }

    /// Set the acceptance confidence threshold (default: 0.3).
    pub fn accept_threshold(mut self, threshold: f64) -> Self {
        self.threshold = threshold;
        self
    }

    /// Run all configured scenarios and return reports.
    pub fn run(self) -> Vec<TypingReport> {
        self.scenarios
            .iter()
            .map(|scenario| {
                let config = InputConfig::default();
                let mut sim = SessionSimulator::new(config, self.threshold);
                sim.load_corpus_dir(&self.corpus_dir);
                let result = sim.run(scenario);
                TypingReport::from_result(&scenario.name, result)
            })
            .collect()
    }

    /// Convenience: run all 5 preset scenarios.
    pub fn run_all_presets() -> Vec<TypingReport> {
        let pg = Self::new();
        let scenarios = Scenario::all_presets();
        scenarios
            .iter()
            .map(|scenario| {
                let config = InputConfig::default();
                let mut sim = SessionSimulator::new(config, pg.threshold);
                sim.load_corpus_dir(&pg.corpus_dir);
                let result = sim.run(scenario);
                TypingReport::from_result(&scenario.name, result)
            })
            .collect()
    }
}

impl Default for Playground {
    fn default() -> Self {
        Self::new()
    }
}
