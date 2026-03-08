use pyo3::prelude::*;
use smartkey_core::ensemble::SmartKeyEngine;

#[pyclass]
struct PySmartKeyEngine {
    inner: SmartKeyEngine,
}

#[pymethods]
impl PySmartKeyEngine {
    #[new]
    fn new() -> Self {
        Self {
            inner: SmartKeyEngine::new(),
        }
    }

    /// Insert a word with its corpus frequency into the trie.
    fn load_word(&mut self, word: &str, frequency: u32) {
        self.inner.load_word(word, frequency);
    }

    /// Train a bigram transition `context → word` with the given count.
    fn load_bigram(&mut self, context: &str, word: &str, count: u32) {
        self.inner.load_bigram(context, word, count);
    }

    /// Train a trigram transition `(w1, w2) → word` with the given count.
    fn load_trigram(&mut self, w1: &str, w2: &str, word: &str, count: u32) {
        self.inner.load_trigram(w1, w2, word, count);
    }

    /// Feed a word into the personal CVM layer (call when the user types a word).
    fn learn(&mut self, word: &str) {
        self.inner.learn(word);
    }

    /// Produce up to `limit` predictions for the given prefix and context.
    ///
    /// Returns a list of `(word, score, confidence)` tuples.
    fn predict(&self, prefix: &str, context: Vec<String>, limit: usize) -> Vec<(String, f64, f64)> {
        let ctx_refs: Vec<&str> = context.iter().map(|s| s.as_str()).collect();
        self.inner
            .predict(prefix, &ctx_refs, limit)
            .into_iter()
            .map(|p| (p.word, p.score, p.confidence))
            .collect()
    }
}

#[pymodule]
fn smartkey_py(_py: Python, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PySmartKeyEngine>()?;
    Ok(())
}
