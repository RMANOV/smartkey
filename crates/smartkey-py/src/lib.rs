use pyo3::prelude::*;
use smartkey_core::ffi_protocol::{
    decode_composing_payload, decode_replace_payload, encode_composing_payload,
    encode_replace_payload,
};
use smartkey_core::input::{Action, InputConfig, Key, KeyEvent, Modifiers};
use smartkey_core::MasterLoop;

// ======================================================================
// New API — InputMethodCore with full state machine.
// Platform adapters call handle_key() and execute the returned actions.
// ======================================================================

/// unsendable: InputMethodCore contains SmartKeyEngine (which contains RefCell, not Send).
/// IBus runs single-threaded under the GIL — this is correct and safe.
#[pyclass(unsendable)]
struct PyInputMethodCore {
    inner: MasterLoop,
}

/// Convert an `Action` to a Python-friendly `(type, payload)` tuple.
fn action_to_tuple(action: &Action) -> (String, String) {
    match action {
        Action::ShowGhost(text) => ("ghost".into(), text.clone()),
        Action::HideGhost => ("hide".into(), String::new()),
        Action::CommitText(text) => ("commit".into(), text.clone()),
        Action::ForwardKey => ("forward".into(), String::new()),
        Action::ReplaceWord { replace_len, text } => {
            ("replace".into(), encode_replace_payload(*replace_len, text))
        }
        Action::ShowComposing { typed, ghost } => {
            ("composing".into(), encode_composing_payload(typed, ghost))
        }
    }
}

#[pyfunction]
fn ffi_decode_replace_payload(payload: &str) -> Option<(usize, String)> {
    decode_replace_payload(payload).map(|(replace_len, text)| (replace_len, text.to_owned()))
}

#[pyfunction]
fn ffi_decode_composing_payload(payload: &str) -> Option<(String, String)> {
    decode_composing_payload(payload).map(|(typed, ghost)| (typed.to_owned(), ghost.to_owned()))
}

/// Translate a raw keyval (IBus / X11 keysym or Unicode codepoint) into a `Key`.
///
/// The Python layer converts legacy keysyms (e.g. Cyrillic 0x06xx) to Unicode
/// codepoints via `IBus.keyval_to_unicode()` before calling us, so we accept
/// both raw keysyms for special keys and Unicode codepoints for printable chars.
fn keyval_to_key(keyval: u32) -> Key {
    match keyval {
        0xFF09 => Key::Tab,
        0xFF1B => Key::Escape,
        0xFF53 => Key::Right,
        0xFF08 => Key::Backspace,
        0x0020 => Key::Space,
        0xFF0D => Key::Return,
        // ASCII printable
        k if (0x21..=0x7E).contains(&k) => Key::Char(char::from(k as u8)),
        // Unicode codepoints (Cyrillic, Latin Extended, etc.)
        k if (0x80..=0xEFFF).contains(&k) => match char::from_u32(k) {
            Some(ch) if !ch.is_control() => Key::Char(ch),
            _ => Key::Other(k),
        },
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
        let config = match config_json {
            Some(json_str) => InputConfig::from_json(json_str),
            None => InputConfig::default(),
        };
        Self {
            inner: MasterLoop::new(config),
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

    /// Load a corpus file (JSON or bincode). Auto-detects format by extension.
    fn load_corpus_file(&mut self, path: &str) -> PyResult<()> {
        self.inner
            .load_corpus_file(std::path::Path::new(path))
            .map_err(pyo3::exceptions::PyIOError::new_err)
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

    /// Update surrounding text from the platform adapter.
    fn set_surrounding_text(&mut self, text: Option<&str>, cursor_pos: Option<usize>) {
        self.inner
            .set_surrounding_text(text.map(str::to_owned), cursor_pos);
    }

    /// Reset internal state.
    fn reset(&mut self) -> Vec<(String, String)> {
        self.inner.reset().iter().map(action_to_tuple).collect()
    }

    /// Save personal profile to the default path (~/.config/smartkey/personal.json).
    fn save_personal(&self) -> PyResult<()> {
        self.inner
            .save_personal_default()
            .map_err(pyo3::exceptions::PyIOError::new_err)
    }

    /// Load personal profile from the default path (no-op if missing).
    fn load_personal(&mut self) -> PyResult<()> {
        self.inner
            .load_personal_default()
            .map_err(pyo3::exceptions::PyIOError::new_err)
    }

    /// Export personal profile to a custom path.
    fn export_personal(&self, path: &str) -> PyResult<()> {
        self.inner
            .save_personal(std::path::Path::new(path))
            .map_err(pyo3::exceptions::PyIOError::new_err)
    }

    /// Import personal profile from a custom path.
    fn import_personal(&mut self, path: &str) -> PyResult<()> {
        self.inner
            .load_personal(std::path::Path::new(path))
            .map_err(pyo3::exceptions::PyIOError::new_err)
    }

    /// Process a key event using the raw hardware scancode (evdev keycode).
    ///
    /// This enables the dual-buffer engine: the scancode identifies the
    /// physical key, which is mapped to both EN QWERTY and BG Phonetic
    /// characters. The engine scores both interpretations and commits the
    /// winner, eliminating the need for manual language switching.
    ///
    /// `keycode`: evdev hardware scancode (e.g. 43 for the 'H' key position).
    /// `modifiers`: raw IBus modifier bitmask.
    fn process_keycode(&mut self, keycode: u16, modifiers: u32) -> Vec<(String, String)> {
        let event = KeyEvent {
            key: Key::RawCode(keycode),
            modifiers: raw_mods_to_modifiers(modifiers),
        };
        self.inner
            .handle_key(event)
            .iter()
            .map(action_to_tuple)
            .collect()
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

#[pymodule]
fn smartkey_py(_py: Python, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyInputMethodCore>()?;
    m.add_function(wrap_pyfunction!(ffi_decode_replace_payload, m)?)?;
    m.add_function(wrap_pyfunction!(ffi_decode_composing_payload, m)?)?;
    Ok(())
}
