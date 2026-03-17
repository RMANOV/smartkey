use std::time::Instant;

use smartkey_core::input::{Action, InputConfig, InputMethodCore, Key, KeyEvent, Modifiers};

use crate::scenario::{Scenario, ScheduledEvent};
use crate::timeline::{RejectReason, TimelineEvent};
use crate::virtual_user::VirtualUser;

/// Closed-loop session simulator that drives `InputMethodCore` through target text.
pub struct SessionSimulator {
    core: InputMethodCore,
    user: VirtualUser,
    timeline: Vec<TimelineEvent>,
    total_keystrokes: usize,
    chars_saved: usize,
    words_total: usize,
    words_predicted: usize,
}

impl SessionSimulator {
    /// Create a simulator with the given config and acceptance threshold.
    pub fn new(config: InputConfig, threshold: f64) -> Self {
        Self {
            core: InputMethodCore::new(config),
            user: VirtualUser::new(threshold),
            timeline: Vec::new(),
            total_keystrokes: 0,
            chars_saved: 0,
            words_total: 0,
            words_predicted: 0,
        }
    }

    /// Load corpus files from a directory (same pattern as VirtualTyper).
    pub fn load_corpus_dir(&mut self, corpus_dir: &std::path::Path) {
        if !corpus_dir.is_dir() {
            return;
        }
        if let Ok(entries) = std::fs::read_dir(corpus_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default();
                if path.extension().map(|e| e == "json").unwrap_or(false)
                    && name.starts_with("corpus_")
                {
                    let _ = self.core.load_corpus_file(&path);
                }
            }
        }
    }

    /// Run the full simulation for a scenario and return results.
    pub fn run(&mut self, scenario: &Scenario) -> SimulationResult {
        let start = Instant::now();
        let text = &scenario.text;
        let chars: Vec<char> = text.chars().collect();
        let total_chars = chars.len();

        // Pre-sort events by offset for efficient lookup
        let mut events_by_offset: Vec<&ScheduledEvent> = scenario.events.iter().collect();
        events_by_offset.sort_by_key(|e| e.char_offset());
        let mut event_idx = 0;

        let mut cursor = 0; // position in chars[]
        let mut current_word_start = cursor;
        let mut current_word_predicted = false;

        while cursor < total_chars {
            // Fire any scheduled events at this offset
            while event_idx < events_by_offset.len()
                && events_by_offset[event_idx].char_offset() <= cursor
            {
                self.fire_event(events_by_offset[event_idx], cursor);
                event_idx += 1;
            }

            let ch = chars[cursor];

            // Word boundary detection: space, period, comma, newline
            if ch == ' ' || ch == '\n' {
                // Commit current word
                if cursor > current_word_start {
                    self.words_total += 1;
                    let word: String = chars[current_word_start..cursor].iter().collect();
                    self.timeline.push(TimelineEvent::WordCommitted {
                        offset: current_word_start,
                        word,
                        was_predicted: current_word_predicted,
                    });
                    if current_word_predicted {
                        self.words_predicted += 1;
                    }
                }

                // Send the space/newline
                let key = if ch == '\n' { Key::Return } else { Key::Space };
                self.send_key(key);
                self.total_keystrokes += 1;
                cursor += 1;
                current_word_start = cursor;
                current_word_predicted = false;
                continue;
            }

            // Type the character
            self.timeline
                .push(TimelineEvent::CharTyped { offset: cursor, ch });
            self.send_key(Key::Char(ch));
            self.total_keystrokes += 1;
            cursor += 1;

            // Check for ghost text and possible acceptance
            let ghost = self.core.ghost_text().to_string();
            if ghost.is_empty() {
                continue;
            }

            // Get confidence from top prediction
            let confidence = self
                .core
                .predictions()
                .first()
                .map(|p| p.confidence)
                .unwrap_or(0.0);

            // What remains to type in the current word (up to next space/end)
            let remaining: String = chars[cursor..].iter().collect();
            // Find word boundary in remaining
            let word_remaining: String = remaining
                .chars()
                .take_while(|c| *c != ' ' && *c != '\n')
                .collect();

            if self.user.should_accept(&ghost, &word_remaining, confidence) {
                // Accept: send Tab
                self.timeline.push(TimelineEvent::PredictionAccepted {
                    offset: cursor,
                    ghost: ghost.clone(),
                    chars_saved: ghost.chars().count(),
                });
                self.timeline.push(TimelineEvent::PredictionShown {
                    offset: cursor,
                    ghost: ghost.clone(),
                    confidence,
                    accepted: true,
                });

                let ghost_char_len = ghost.chars().count();
                self.chars_saved += ghost_char_len;
                self.send_key(Key::Tab);
                self.total_keystrokes += 1;
                cursor += ghost_char_len; // skip past accepted ghost
                current_word_predicted = true;
            } else {
                // Reject: record why
                let reason = if ghost.is_empty() {
                    RejectReason::NoGhost
                } else if confidence < self.user.threshold {
                    RejectReason::LowConfidence
                } else {
                    RejectReason::Mismatch
                };
                self.timeline.push(TimelineEvent::PredictionShown {
                    offset: cursor,
                    ghost: ghost.clone(),
                    confidence,
                    accepted: false,
                });
                self.timeline.push(TimelineEvent::PredictionRejected {
                    offset: cursor,
                    ghost,
                    confidence,
                    reason,
                });
            }
        }

        // Commit final word if any
        if cursor > current_word_start && current_word_start < total_chars {
            self.words_total += 1;
            let word: String = chars[current_word_start..cursor.min(total_chars)]
                .iter()
                .collect();
            self.timeline.push(TimelineEvent::WordCommitted {
                offset: current_word_start,
                word,
                was_predicted: current_word_predicted,
            });
            if current_word_predicted {
                self.words_predicted += 1;
            }
        }

        let duration = start.elapsed();
        let metrics = self.core.metrics().summary();

        SimulationResult {
            timeline: std::mem::take(&mut self.timeline),
            total_chars,
            total_keystrokes: self.total_keystrokes,
            words_total: self.words_total,
            words_predicted: self.words_predicted,
            chars_saved: self.chars_saved,
            duration,
            p50_latency_us: metrics.p50_latency.as_micros() as u64,
            p99_latency_us: metrics.p99_latency.as_micros() as u64,
        }
    }

    fn send_key(&mut self, key: Key) -> Vec<Action> {
        self.core.handle_key(KeyEvent {
            key,
            modifiers: Modifiers::empty(),
        })
    }

    fn fire_event(&mut self, event: &ScheduledEvent, cursor: usize) {
        match event {
            ScheduledEvent::SwitchLanguage { lang, .. } => {
                let from = format!("{:?}", self.core.detected_language());
                self.core.focus_lost();
                self.core.focus_gained();
                self.timeline.push(TimelineEvent::LanguageSwitched {
                    offset: cursor,
                    from,
                    to: lang.clone(),
                });
            }
            ScheduledEvent::InjectTypo { wrong_char, .. } => {
                let correct_char = if cursor < self.core.current_word().len() {
                    self.core.current_word().chars().last().unwrap_or('?')
                } else {
                    '?'
                };
                // Type wrong char, then backspace
                self.send_key(Key::Char(*wrong_char));
                self.total_keystrokes += 1;
                self.send_key(Key::Backspace);
                self.total_keystrokes += 1;
                self.timeline.push(TimelineEvent::TypoInjected {
                    offset: cursor,
                    wrong_char: *wrong_char,
                    correct_char,
                });
            }
            ScheduledEvent::Pause { .. } => {
                self.core.focus_lost();
                self.core.focus_gained();
                self.timeline
                    .push(TimelineEvent::PauseTriggered { offset: cursor });
            }
        }
    }
}

/// Raw simulation output before aggregation into a report.
pub struct SimulationResult {
    pub timeline: Vec<TimelineEvent>,
    pub total_chars: usize,
    pub total_keystrokes: usize,
    pub words_total: usize,
    pub words_predicted: usize,
    pub chars_saved: usize,
    pub duration: std::time::Duration,
    pub p50_latency_us: u64,
    pub p99_latency_us: u64,
}
