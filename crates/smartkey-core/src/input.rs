// Platform-agnostic input method state machine.
//
// This module contains the key event dispatcher, ghost text lifecycle,
// word commit logic, and context tracking — shared across Linux (IBus),
// Windows (TSF), and macOS (IMK). Platform adapters call `handle_key()`
// and execute the returned `Action` list.

use std::collections::VecDeque;

use crate::ensemble::{Prediction, SmartKeyEngine};

// ======================================================================
// Key abstraction
// ======================================================================

bitflags::bitflags! {
    /// Modifier keys as a bitmask. Platform adapters translate OS-specific
    /// modifier representations into this common set.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Modifiers: u32 {
        const RELEASE = 0x01;
        const CTRL    = 0x02;
        const ALT     = 0x04;
        const SUPER   = 0x08;
        const SHIFT   = 0x10;
    }
}

/// Platform-neutral key identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Tab,
    Escape,
    Right,
    Backspace,
    Space,
    Return,
    /// Unrecognised key — pass through.
    Other(u32),
}

/// A single key event delivered by the platform.
#[derive(Debug, Clone)]
pub struct KeyEvent {
    pub key: Key,
    pub modifiers: Modifiers,
}

// ======================================================================
// Actions returned to the platform
// ======================================================================

/// An instruction for the platform adapter to execute.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Display `text` as greyed-out ghost completion.
    ShowGhost(String),
    /// Remove any displayed ghost text.
    HideGhost,
    /// Commit `text` to the application (insert it).
    CommitText(String),
    /// Do not consume the key — let it reach the application.
    ForwardKey,
}

// ======================================================================
// Configuration
// ======================================================================

const CONTEXT_SIZE: usize = 5;

/// Platform-agnostic input method configuration.
#[derive(Debug, Clone)]
pub struct InputConfig {
    /// Initial enabled state. After `InputMethodCore::new()`, the core tracks
    /// its own `enabled` flag independently (toggled by the kill switch).
    pub enabled: bool,
    pub ghost_text: bool,
    pub max_candidates: usize,
    pub min_prefix_length: usize,
    pub weights: (f64, f64, f64),
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            ghost_text: true,
            max_candidates: 5,
            min_prefix_length: 2,
            weights: (0.4, 0.4, 0.2),
        }
    }
}

impl InputConfig {
    /// Parse a JSON config string, applying values over defaults.
    ///
    /// Unknown keys are silently ignored. Missing keys keep their defaults.
    pub fn from_json(json_str: &str) -> Self {
        let mut config = Self::default();
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
                let a = w
                    .get("corpus")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(config.weights.0);
                let b = w
                    .get("markov")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(config.weights.1);
                let c = w
                    .get("personal")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(config.weights.2);
                config.weights = (a, b, c);
            }
        }
        config
    }
}

// ======================================================================
// InputMethodCore
// ======================================================================

/// The core state machine shared by all platform adapters.
///
/// Platform code feeds key events via [`handle_key`] and executes the
/// returned [`Action`]s (show ghost text, commit text, etc.).
pub struct InputMethodCore {
    engine: SmartKeyEngine,
    config: InputConfig,
    current_word: String,
    ghost: String,
    context: VecDeque<String>,
    enabled: bool,
    last_predictions: Vec<Prediction>,
}

impl InputMethodCore {
    pub fn new(config: InputConfig) -> Self {
        let enabled = config.enabled;
        Self {
            engine: SmartKeyEngine::new(),
            config,
            current_word: String::new(),
            ghost: String::new(),
            context: VecDeque::with_capacity(CONTEXT_SIZE),
            enabled,
            last_predictions: Vec::new(),
        }
    }

    // -- corpus loading (delegate to engine) ----------------------------

    pub fn load_word(&mut self, word: &str, freq: u32) {
        self.engine.load_word(word, freq);
    }

    pub fn load_bigram(&mut self, ctx: &str, word: &str, count: u32) {
        self.engine.load_bigram(ctx, word, count);
    }

    pub fn load_trigram(&mut self, w1: &str, w2: &str, word: &str, count: u32) {
        self.engine.load_trigram(w1, w2, word, count);
    }

    // -- key event handling ---------------------------------------------

    /// Process a key event and return actions for the platform to execute.
    pub fn handle_key(&mut self, event: KeyEvent) -> Vec<Action> {
        let is_kill = event.key == Key::Escape && event.modifiers.contains(Modifiers::SUPER);

        // Disabled state — only the kill switch can re-enable.
        if !self.enabled {
            if is_kill {
                self.enabled = true;
                return vec![Action::ForwardKey];
            }
            return vec![Action::ForwardKey];
        }

        // Kill switch: disable engine.
        if is_kill {
            self.enabled = false;
            self.reset_word();
            return vec![Action::HideGhost];
        }

        // Ignore key-release events.
        if event.modifiers.contains(Modifiers::RELEASE) {
            return vec![Action::ForwardKey];
        }

        // Ignore events with Ctrl or Alt held.
        if event.modifiers.intersects(Modifiers::CTRL | Modifiers::ALT) {
            return vec![Action::ForwardKey];
        }

        match event.key {
            // Tab: accept entire ghost text.
            Key::Tab => {
                if !self.ghost.is_empty() {
                    let full_word = format!("{}{}", self.current_word, self.ghost);
                    let actions = vec![Action::CommitText(full_word.clone()), Action::HideGhost];
                    self.commit_word(&full_word);
                    self.reset_word();
                    actions
                } else {
                    vec![Action::ForwardKey]
                }
            }

            // Right arrow: accept one character of ghost text.
            Key::Right => {
                if !self.ghost.is_empty() {
                    let ch = self.ghost.remove(0);
                    self.current_word.push(ch);
                    if self.ghost.is_empty() {
                        vec![Action::HideGhost]
                    } else {
                        vec![Action::ShowGhost(self.ghost.clone())]
                    }
                } else {
                    vec![Action::ForwardKey]
                }
            }

            // Escape: dismiss ghost text.
            Key::Escape => {
                if !self.ghost.is_empty() {
                    self.ghost.clear();
                    vec![Action::HideGhost]
                } else {
                    vec![Action::ForwardKey]
                }
            }

            // Space / Return: commit current word, delimit.
            Key::Space | Key::Return => {
                if !self.current_word.is_empty() {
                    let word = std::mem::take(&mut self.current_word);
                    self.commit_word(&word);
                }
                self.reset_word();
                vec![Action::HideGhost, Action::ForwardKey]
            }

            // Backspace: trim the current word.
            Key::Backspace => {
                if !self.current_word.is_empty() {
                    self.current_word.pop();
                }
                let ghost_action = self.update_predictions();
                vec![ghost_action, Action::ForwardKey]
            }

            // Printable character: append and re-predict.
            Key::Char(ch) => {
                self.current_word.push(ch);
                let ghost_action = self.update_predictions();
                vec![ghost_action, Action::ForwardKey]
            }

            // Unknown key: pass through.
            Key::Other(_) => vec![Action::ForwardKey],
        }
    }

    // -- focus / reset --------------------------------------------------

    /// Called when the input field loses focus.
    pub fn focus_lost(&mut self) -> Vec<Action> {
        if !self.current_word.is_empty() {
            let word = std::mem::take(&mut self.current_word);
            self.commit_word(&word);
        }
        self.reset_word();
        vec![Action::HideGhost]
    }

    /// Called when the input field gains focus.
    pub fn focus_gained(&mut self) {
        // Nothing to do — ready for new input.
    }

    /// Reset internal state (e.g. on engine reset request).
    pub fn reset(&mut self) -> Vec<Action> {
        self.reset_word();
        vec![Action::HideGhost]
    }

    // -- read-only accessors --------------------------------------------

    /// The most recent predictions (from the last `handle_key` call).
    pub fn predictions(&self) -> &[Prediction] {
        &self.last_predictions
    }

    /// Whether the engine is currently enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// The word currently being typed.
    pub fn current_word(&self) -> &str {
        &self.current_word
    }

    // -- internal helpers -----------------------------------------------

    fn commit_word(&mut self, word: &str) {
        if !word.is_empty() {
            self.engine.learn(word);
            if self.context.len() >= CONTEXT_SIZE {
                self.context.pop_front();
            }
            self.context.push_back(word.to_string());
        }
    }

    fn reset_word(&mut self) {
        self.current_word.clear();
        self.ghost.clear();
        self.last_predictions.clear();
    }

    fn update_predictions(&mut self) -> Action {
        if self.current_word.len() < self.config.min_prefix_length {
            self.ghost.clear();
            self.last_predictions.clear();
            return Action::HideGhost;
        }

        let ctx_refs: Vec<&str> = self.context.iter().map(|s| s.as_str()).collect();
        self.last_predictions =
            self.engine
                .predict(&self.current_word, &ctx_refs, self.config.max_candidates);

        if let Some(top) = self.last_predictions.first() {
            let suffix = &top.word[self.current_word.len()..];
            if !suffix.is_empty() && self.config.ghost_text {
                self.ghost = suffix.to_string();
                return Action::ShowGhost(self.ghost.clone());
            }
        }

        self.ghost.clear();
        Action::HideGhost
    }
}

// ======================================================================
// Tests
// ======================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a test engine with a small vocabulary.
    fn test_core() -> InputMethodCore {
        let mut core = InputMethodCore::new(InputConfig::default());
        core.load_word("hello", 100);
        core.load_word("help", 80);
        core.load_word("hero", 60);
        core.load_word("helicopter", 20);
        core.load_word("world", 90);
        core.load_bigram("i", "hello", 5);
        core.load_bigram("i", "help", 2);
        core
    }

    fn press(key: Key) -> KeyEvent {
        KeyEvent {
            key,
            modifiers: Modifiers::empty(),
        }
    }

    fn press_with(key: Key, mods: Modifiers) -> KeyEvent {
        KeyEvent {
            key,
            modifiers: mods,
        }
    }

    fn has_action(actions: &[Action], target: &Action) -> bool {
        actions.iter().any(|a| a == target)
    }

    fn ghost_text(actions: &[Action]) -> Option<String> {
        actions.iter().find_map(|a| match a {
            Action::ShowGhost(s) => Some(s.clone()),
            _ => None,
        })
    }

    #[test]
    fn test_basic_predict_cycle() {
        let mut core = test_core();

        // Type "h" — below min_prefix_length (2), no ghost.
        let actions = core.handle_key(press(Key::Char('h')));
        assert!(
            has_action(&actions, &Action::HideGhost)
                || ghost_text(&actions).is_none()
                || ghost_text(&actions) == Some(String::new())
        );

        // Type "e" → "he" — now above threshold, should get ghost.
        let actions = core.handle_key(press(Key::Char('e')));
        // "hello" is top by frequency → ghost should be "llo"
        assert!(has_action(&actions, &Action::ForwardKey));

        // Type "l" → "hel" — should get ghost "lo" (from "hello").
        let actions = core.handle_key(press(Key::Char('l')));
        let g = ghost_text(&actions);
        assert!(g.is_some(), "should have ghost text for 'hel'");
        assert_eq!(g.unwrap(), "lo");

        // Tab → commit "hello".
        let actions = core.handle_key(press(Key::Tab));
        assert!(has_action(&actions, &Action::CommitText("hello".into())));
        assert!(has_action(&actions, &Action::HideGhost));

        // State should be reset.
        assert_eq!(core.current_word(), "");
    }

    #[test]
    fn test_right_arrow_accepts_one() {
        let mut core = test_core();
        // Type "hel" to get ghost "lo".
        core.handle_key(press(Key::Char('h')));
        core.handle_key(press(Key::Char('e')));
        core.handle_key(press(Key::Char('l')));
        assert_eq!(core.ghost, "lo");

        // Right arrow → accept 'l', ghost becomes "o".
        let actions = core.handle_key(press(Key::Right));
        assert_eq!(ghost_text(&actions), Some("o".into()));
        assert_eq!(core.current_word(), "hell");

        // Right arrow again → accept 'o', ghost gone.
        let actions = core.handle_key(press(Key::Right));
        assert!(has_action(&actions, &Action::HideGhost));
        assert_eq!(core.current_word(), "hello");
    }

    #[test]
    fn test_escape_dismisses() {
        let mut core = test_core();
        core.handle_key(press(Key::Char('h')));
        core.handle_key(press(Key::Char('e')));
        core.handle_key(press(Key::Char('l')));
        assert!(!core.ghost.is_empty());

        let actions = core.handle_key(press(Key::Escape));
        assert!(has_action(&actions, &Action::HideGhost));
        assert!(core.ghost.is_empty());

        // Escape with no ghost → forward.
        let actions = core.handle_key(press(Key::Escape));
        assert!(has_action(&actions, &Action::ForwardKey));
    }

    #[test]
    fn test_space_commits_word() {
        let mut core = test_core();
        core.handle_key(press(Key::Char('h')));
        core.handle_key(press(Key::Char('e')));
        core.handle_key(press(Key::Char('l')));
        core.handle_key(press(Key::Char('l')));
        core.handle_key(press(Key::Char('o')));

        let actions = core.handle_key(press(Key::Space));
        assert!(has_action(&actions, &Action::ForwardKey));
        assert_eq!(core.current_word(), "");
        // "hello" should be in context now.
        assert!(core.context.iter().any(|w| w == "hello"));
    }

    #[test]
    fn test_backspace_trims() {
        let mut core = test_core();
        core.handle_key(press(Key::Char('h')));
        core.handle_key(press(Key::Char('e')));
        core.handle_key(press(Key::Char('l')));
        assert_eq!(core.current_word(), "hel");

        let actions = core.handle_key(press(Key::Backspace));
        assert_eq!(core.current_word(), "he");
        assert!(has_action(&actions, &Action::ForwardKey));
    }

    #[test]
    fn test_kill_switch() {
        let mut core = test_core();
        assert!(core.is_enabled());

        // Super+Escape → disable.
        let actions = core.handle_key(press_with(Key::Escape, Modifiers::SUPER));
        assert!(!core.is_enabled());
        assert!(has_action(&actions, &Action::HideGhost));

        // Normal key while disabled → forward.
        let actions = core.handle_key(press(Key::Char('a')));
        assert!(has_action(&actions, &Action::ForwardKey));
        assert_eq!(core.current_word(), "");

        // Super+Escape again → re-enable.
        let actions = core.handle_key(press_with(Key::Escape, Modifiers::SUPER));
        assert!(core.is_enabled());
        assert!(has_action(&actions, &Action::ForwardKey));
    }

    #[test]
    fn test_context_tracking() {
        let mut core = test_core();

        // Type and commit "i".
        core.handle_key(press(Key::Char('i')));
        core.handle_key(press(Key::Space));
        assert!(core.context.iter().any(|w| w == "i"));

        // Type and commit "hello".
        for ch in "hello".chars() {
            core.handle_key(press(Key::Char(ch)));
        }
        core.handle_key(press(Key::Return));
        assert_eq!(core.context.len(), 2);
        assert_eq!(core.context[0], "i");
        assert_eq!(core.context[1], "hello");
    }

    #[test]
    fn test_min_prefix_length() {
        let mut core = test_core();

        // Single char → no ghost (min_prefix_length = 2).
        let actions = core.handle_key(press(Key::Char('h')));
        assert!(
            !has_action(&actions, &Action::ShowGhost("ello".into())),
            "should not show ghost for single char"
        );
    }

    #[test]
    fn test_release_events_forwarded() {
        let mut core = test_core();
        let actions = core.handle_key(press_with(Key::Char('a'), Modifiers::RELEASE));
        assert!(has_action(&actions, &Action::ForwardKey));
        assert_eq!(core.current_word(), "");
    }

    #[test]
    fn test_ctrl_alt_forwarded() {
        let mut core = test_core();
        let actions = core.handle_key(press_with(Key::Char('c'), Modifiers::CTRL));
        assert!(has_action(&actions, &Action::ForwardKey));
        assert_eq!(core.current_word(), "");
    }

    #[test]
    fn test_focus_lost_commits() {
        let mut core = test_core();
        for ch in "hello".chars() {
            core.handle_key(press(Key::Char(ch)));
        }

        let actions = core.focus_lost();
        assert!(has_action(&actions, &Action::HideGhost));
        assert_eq!(core.current_word(), "");
        assert!(core.context.iter().any(|w| w == "hello"));
    }

    #[test]
    fn test_config_from_json_defaults() {
        let config = InputConfig::from_json("{}");
        let default = InputConfig::default();
        assert_eq!(config.enabled, default.enabled);
        assert_eq!(config.ghost_text, default.ghost_text);
        assert_eq!(config.max_candidates, default.max_candidates);
        assert_eq!(config.min_prefix_length, default.min_prefix_length);
        assert_eq!(config.weights, default.weights);
    }

    #[test]
    fn test_config_from_json_overrides() {
        let json = r#"{"enabled": false, "max_candidates": 10, "weights": {"corpus": 0.5, "markov": 0.3, "personal": 0.2}}"#;
        let config = InputConfig::from_json(json);
        assert!(!config.enabled);
        assert!(config.ghost_text); // kept default
        assert_eq!(config.max_candidates, 10);
        assert_eq!(config.weights, (0.5, 0.3, 0.2));
    }

    #[test]
    fn test_config_from_json_invalid() {
        let config = InputConfig::from_json("not json at all");
        let default = InputConfig::default();
        assert_eq!(config.enabled, default.enabled);
    }
}
