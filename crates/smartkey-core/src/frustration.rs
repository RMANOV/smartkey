// Frustration detector — identifies user dissatisfaction with predictions.
//
// Monitors keystroke patterns for four signals:
//   REJECT:       Backspace within 500ms of Tab (accepted ghost, then deleted)
//   RAPID_DELETE:  3+ Backspace within 1 second
//   RETYPE:        Delete all + retype with different script (wrong lang detected)
//   ABANDON:       Escape + continued manual typing (ghost was unwanted)
//
// Severity scores are [0.0, 1.0] — higher means stronger frustration signal.

use std::time::Instant;

use crate::input::Key;

/// A detected frustration signal with severity.
#[derive(Debug, Clone)]
pub enum FrustrationSignal {
    /// User accepted ghost (Tab) then immediately deleted it.
    Reject { severity: f64 },
    /// Rapid consecutive backspaces (≥3 in <1s).
    RapidDelete { severity: f64, count: u8 },
    /// Deleted and retyped with different script (wrong language).
    Retype { severity: f64, prefix: String },
    /// Dismissed ghost (Escape) and continued typing manually.
    Abandon { severity: f64 },
}

/// Detects frustration patterns from keystroke streams.
pub struct FrustrationDetector {
    /// Word prefix before a deletion sequence (for RETYPE detection).
    word_prefix_before_delete: Option<String>,
    /// Whether Escape was seen during this word (for ABANDON detection).
    escape_seen: bool,
    /// Number of consecutive backspaces in current burst.
    consecutive_bs: u8,
    /// Timestamp of first backspace in current burst.
    burst_start: Option<Instant>,
}

impl FrustrationDetector {
    pub fn new() -> Self {
        Self {
            word_prefix_before_delete: None,
            escape_seen: false,
            consecutive_bs: 0,
            burst_start: None,
        }
    }

    /// Feed a key event and check for frustration signals.
    ///
    /// `current_word`: the word being typed (after key processing).
    /// `last_tab_accept`: when the last Tab acceptance occurred (if any).
    pub fn feed(
        &mut self,
        key: &Key,
        current_word: &str,
        last_tab_accept: Option<Instant>,
    ) -> Option<FrustrationSignal> {
        let now = Instant::now();

        match key {
            Key::Backspace => {
                // Track consecutive backspaces for burst detection.
                if self.consecutive_bs == 0 {
                    self.burst_start = Some(now);
                    // Snapshot the word before deletion starts (for RETYPE).
                    if self.word_prefix_before_delete.is_none() && !current_word.is_empty() {
                        // current_word already had the char popped, so reconstruct
                        self.word_prefix_before_delete = Some(current_word.to_string());
                    }
                }
                self.consecutive_bs = self.consecutive_bs.saturating_add(1);

                // REJECT: Backspace within 500ms of Tab accept.
                if let Some(tab_time) = last_tab_accept {
                    let elapsed = now.duration_since(tab_time);
                    if elapsed.as_millis() < 500 {
                        let severity = 1.0 - (elapsed.as_millis() as f64 / 500.0);
                        return Some(FrustrationSignal::Reject {
                            severity: severity.clamp(0.0, 1.0),
                        });
                    }
                }

                // RAPID_DELETE: 3+ backspaces within 1 second.
                if self.consecutive_bs >= 3 {
                    if let Some(start) = self.burst_start {
                        let burst_duration = now.duration_since(start);
                        if burst_duration.as_millis() < 1000 {
                            let severity = (self.consecutive_bs as f64 / 8.0).clamp(0.3, 1.0);
                            return Some(FrustrationSignal::RapidDelete {
                                severity,
                                count: self.consecutive_bs,
                            });
                        }
                    }
                }

                None
            }

            Key::Escape => {
                self.escape_seen = true;
                self.reset_bs_burst();
                None
            }

            Key::Char(ch) => {
                // RETYPE: After deleting, user retypes with different script.
                if let Some(old_prefix) = self.word_prefix_before_delete.take() {
                    let char_count = old_prefix.chars().count();
                    if self.consecutive_bs > 0 && !current_word.is_empty() && char_count >= 3 {
                        let old_is_latin = old_prefix.chars().all(|c| c.is_ascii_alphabetic());
                        let new_is_cyrillic = is_cyrillic(*ch);
                        let old_is_cyrillic = old_prefix.chars().all(is_cyrillic);
                        let new_is_latin = ch.is_ascii_alphabetic();

                        if (old_is_latin && new_is_cyrillic) || (old_is_cyrillic && new_is_latin) {
                            let severity = (char_count as f64 / 10.0).clamp(0.4, 1.0);
                            self.reset_bs_burst();
                            return Some(FrustrationSignal::Retype {
                                severity,
                                prefix: old_prefix,
                            });
                        }
                    }
                    // Not a script switch — put the prefix back.
                    self.word_prefix_before_delete = Some(old_prefix);
                }

                // ABANDON: Escape was seen, now typing manually.
                if self.escape_seen {
                    self.escape_seen = false;
                    return Some(FrustrationSignal::Abandon { severity: 0.6 });
                }

                self.reset_bs_burst();
                None
            }

            _ => {
                self.reset_bs_burst();
                None
            }
        }
    }

    /// Reset state on word boundary.
    pub fn reset_word(&mut self) {
        self.word_prefix_before_delete = None;
        self.escape_seen = false;
        self.reset_bs_burst();
    }

    fn reset_bs_burst(&mut self) {
        self.consecutive_bs = 0;
        self.burst_start = None;
    }
}

impl Default for FrustrationDetector {
    fn default() -> Self {
        Self::new()
    }
}

/// Check if a character is Cyrillic.
fn is_cyrillic(ch: char) -> bool {
    ('\u{0400}'..='\u{04FF}').contains(&ch)
}
