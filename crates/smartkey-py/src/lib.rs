use pyo3::prelude::*;
use smartkey_core::ensemble::SmartKeyEngine;
use smartkey_core::input::{Action, InputConfig, InputMethodCore, Key, KeyEvent, Modifiers};

// ======================================================================
// Legacy API — kept for backward compatibility with existing IBus engine.
// ======================================================================

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

    fn load_word(&mut self, word: &str, frequency: u32) {
        self.inner.load_word(word, frequency);
    }

    fn load_bigram(&mut self, context: &str, word: &str, count: u32) {
        self.inner.load_bigram(context, word, count);
    }

    fn load_trigram(&mut self, w1: &str, w2: &str, word: &str, count: u32) {
        self.inner.load_trigram(w1, w2, word, count);
    }

    fn learn(&mut self, word: &str) {
        self.inner.learn(word);
    }

    fn predict(&self, prefix: &str, context: Vec<String>, limit: usize) -> Vec<(String, f64, f64)> {
        let ctx_refs: Vec<&str> = context.iter().map(|s| s.as_str()).collect();
        self.inner
            .predict(prefix, &ctx_refs, limit)
            .into_iter()
            .map(|p| (p.word, p.score, p.confidence))
            .collect()
    }
}

// ======================================================================
// New API — InputMethodCore with full state machine.
// Platform adapters call handle_key() and execute the returned actions.
// ======================================================================

#[pyclass]
struct PyInputMethodCore {
    inner: InputMethodCore,
}

/// Convert an `Action` to a Python-friendly `(type, payload)` tuple.
fn action_to_tuple(action: &Action) -> (String, String) {
    match action {
        Action::ShowGhost(text) => ("ghost".into(), text.clone()),
        Action::HideGhost => ("hide".into(), String::new()),
        Action::CommitText(text) => ("commit".into(), text.clone()),
        Action::ForwardKey => ("forward".into(), String::new()),
    }
}

/// Translate a raw keyval (IBus / X11 keysym) into a `Key`.
fn keyval_to_key(keyval: u32) -> Key {
    match keyval {
        0xFF09 => Key::Tab,
        0xFF1B => Key::Escape,
        0xFF53 => Key::Right,
        0xFF08 => Key::Backspace,
        0x0020 => Key::Space,
        0xFF0D => Key::Return,
        k if (0x21..=0x7E).contains(&k) => Key::Char(char::from(k as u8)),
        other => Key::Other(other),
    }
}

/// Translate raw IBus modifier bitmask into `Modifiers`.
fn raw_mods_to_modifiers(raw: u32) -> Modifiers {
    let mut m = Modifiers::empty();
    // IBus modifier bits:
    if raw & (1 << 30) != 0 {
        m |= Modifiers::RELEASE;
    } // IBUS_RELEASE_MASK
    if raw & (1 << 2) != 0 {
        m |= Modifiers::CTRL;
    } // CONTROL_MASK
    if raw & (1 << 3) != 0 {
        m |= Modifiers::ALT;
    } // MOD1_MASK (Alt)
    if raw & (1 << 26) != 0 {
        m |= Modifiers::SUPER;
    } // MOD4_MASK / SUPER_MASK
    if raw & (1 << 0) != 0 {
        m |= Modifiers::SHIFT;
    } // SHIFT_MASK
    m
}

#[pymethods]
impl PyInputMethodCore {
    #[new]
    #[pyo3(signature = (config_json=None))]
    fn new(config_json: Option<&str>) -> Self {
        let config = if let Some(json_str) = config_json {
            parse_config(json_str)
        } else {
            InputConfig::default()
        };
        Self {
            inner: InputMethodCore::new(config),
        }
    }

    fn load_word(&mut self, word: &str, freq: u32) {
        self.inner.load_word(word, freq);
    }

    fn load_bigram(&mut self, ctx: &str, word: &str, count: u32) {
        self.inner.load_bigram(ctx, word, count);
    }

    fn load_trigram(&mut self, w1: &str, w2: &str, word: &str, count: u32) {
        self.inner.load_trigram(w1, w2, word, count);
    }

    /// Process a key event. Returns list of `(action_type, payload)` tuples.
    ///
    /// `keyval`: IBus/X11 keysym (e.g. 0xFF09 for Tab).
    /// `modifiers`: raw IBus modifier bitmask.
    fn handle_key(&mut self, keyval: u32, modifiers: u32) -> Vec<(String, String)> {
        let event = KeyEvent {
            key: keyval_to_key(keyval),
            modifiers: raw_mods_to_modifiers(modifiers),
        };
        self.inner
            .handle_key(event)
            .iter()
            .map(action_to_tuple)
            .collect()
    }

    /// Called when input focus is lost.
    fn focus_lost(&mut self) -> Vec<(String, String)> {
        self.inner
            .focus_lost()
            .iter()
            .map(action_to_tuple)
            .collect()
    }

    /// Called when input focus is gained.
    fn focus_gained(&mut self) {
        self.inner.focus_gained();
    }

    /// Reset internal state.
    fn reset(&mut self) -> Vec<(String, String)> {
        self.inner.reset().iter().map(action_to_tuple).collect()
    }

    /// Current predictions as `(word, score, confidence)` tuples.
    fn predictions(&self) -> Vec<(String, f64, f64)> {
        self.inner
            .predictions()
            .iter()
            .map(|p| (p.word.clone(), p.score, p.confidence))
            .collect()
    }
}

/// Parse a JSON config string into an `InputConfig`.
fn parse_config(json_str: &str) -> InputConfig {
    let mut config = InputConfig::default();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(json_str) {
        if let Some(b) = v.get("enabled").and_then(|v| v.as_bool()) {
            config.enabled = b;
        }
        if let Some(b) = v.get("ghost_text").and_then(|v| v.as_bool()) {
            config.ghost_text = b;
        }
        if let Some(n) = v.get("max_candidates").and_then(|v| v.as_u64()) {
            config.max_candidates = n as usize;
        }
        if let Some(n) = v.get("min_prefix_length").and_then(|v| v.as_u64()) {
            config.min_prefix_length = n as usize;
        }
        if let Some(w) = v.get("weights").and_then(|v| v.as_object()) {
            let a = w.get("corpus").and_then(|v| v.as_f64()).unwrap_or(0.4);
            let b = w.get("markov").and_then(|v| v.as_f64()).unwrap_or(0.4);
            let c = w.get("personal").and_then(|v| v.as_f64()).unwrap_or(0.2);
            config.weights = (a, b, c);
        }
    }
    config
}

#[pymodule]
fn smartkey_py(_py: Python, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PySmartKeyEngine>()?;
    m.add_class::<PyInputMethodCore>()?;
    Ok(())
}
