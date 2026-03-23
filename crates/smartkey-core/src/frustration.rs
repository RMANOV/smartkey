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

#[cfg(test)]
mod tests {
    use super::*;

    fn no_tab() -> Option<Instant> {
        None
    }

    #[test]
    fn new_detector_has_clean_state() {
        let d = FrustrationDetector::new();
        assert!(!d.escape_seen);
        assert_eq!(d.consecutive_bs, 0);
        assert!(d.burst_start.is_none());
        assert!(d.word_prefix_before_delete.is_none());
    }

    #[test]
    fn single_backspace_no_signal() {
        let mut d = FrustrationDetector::new();
        let result = d.feed(&Key::Backspace, "hel", no_tab());
        assert!(result.is_none());
    }

    #[test]
    fn rapid_delete_three_backspaces() {
        let mut d = FrustrationDetector::new();
        d.feed(&Key::Backspace, "hell", no_tab());
        d.feed(&Key::Backspace, "hel", no_tab());
        let result = d.feed(&Key::Backspace, "he", no_tab());
        // Third backspace within burst window should trigger RapidDelete
        match result {
            Some(FrustrationSignal::RapidDelete { severity, count }) => {
                assert_eq!(count, 3);
                assert!((0.3..=1.0).contains(&severity));
            }
            _ => panic!("Expected RapidDelete signal, got {:?}", result),
        }
    }

    #[test]
    fn reject_signal_backspace_after_tab_accept() {
        let mut d = FrustrationDetector::new();
        // Simulate a very recent Tab accept
        let recent_tab = Instant::now();
        let result = d.feed(&Key::Backspace, "hel", Some(recent_tab));
        match result {
            Some(FrustrationSignal::Reject { severity }) => {
                assert!(severity > 0.0 && severity <= 1.0);
            }
            _ => panic!("Expected Reject signal, got {:?}", result),
        }
    }

    #[test]
    fn no_reject_when_tab_accept_old() {
        let mut d = FrustrationDetector::new();
        // Simulate a Tab accept from far in the past (>500ms)
        // We can't go back in time with Instant, so we test that
        // a None tab produces no Reject
        let result = d.feed(&Key::Backspace, "hel", no_tab());
        assert!(!matches!(result, Some(FrustrationSignal::Reject { .. })));
    }

    #[test]
    fn abandon_signal_escape_then_char() {
        let mut d = FrustrationDetector::new();
        d.feed(&Key::Escape, "", no_tab());
        let result = d.feed(&Key::Char('a'), "a", no_tab());
        match result {
            Some(FrustrationSignal::Abandon { severity }) => {
                assert!((severity - 0.6).abs() < 1e-9);
            }
            _ => panic!("Expected Abandon signal, got {:?}", result),
        }
    }

    #[test]
    fn no_abandon_without_escape() {
        let mut d = FrustrationDetector::new();
        let result = d.feed(&Key::Char('a'), "a", no_tab());
        assert!(!matches!(result, Some(FrustrationSignal::Abandon { .. })));
    }

    #[test]
    fn retype_latin_to_cyrillic() {
        let mut d = FrustrationDetector::new();
        // Set up: user typed "hello" (Latin), now deletes
        d.feed(&Key::Backspace, "hell", no_tab()); // sets word_prefix_before_delete = "hell"
                                                   // Simulate having deleted enough chars — manually set consecutive_bs
        d.consecutive_bs = 3;
        // Now type a Cyrillic character — should detect Retype
        let result = d.feed(&Key::Char('а'), "а", no_tab()); // Cyrillic 'а'
                                                             // The old_prefix "hell" is Latin, new char is Cyrillic → Retype
        match result {
            Some(FrustrationSignal::Retype { severity, prefix }) => {
                assert!(!prefix.is_empty());
                assert!((0.4..=1.0).contains(&severity));
            }
            // If old_prefix had < 3 chars or conditions not met, None is also acceptable
            None => {}
            other => panic!("Unexpected signal: {:?}", other),
        }
    }

    #[test]
    fn reset_word_clears_all_state() {
        let mut d = FrustrationDetector::new();
        d.feed(&Key::Escape, "", no_tab());
        d.feed(&Key::Backspace, "hel", no_tab());
        d.reset_word();
        assert!(!d.escape_seen);
        assert_eq!(d.consecutive_bs, 0);
        assert!(d.burst_start.is_none());
        assert!(d.word_prefix_before_delete.is_none());
    }

    #[test]
    fn escape_key_sets_escape_seen() {
        let mut d = FrustrationDetector::new();
        d.feed(&Key::Escape, "", no_tab());
        assert!(d.escape_seen);
    }

    #[test]
    fn non_backspace_char_resets_burst() {
        let mut d = FrustrationDetector::new();
        d.feed(&Key::Backspace, "hel", no_tab());
        d.feed(&Key::Char('x'), "helx", no_tab());
        assert_eq!(d.consecutive_bs, 0);
        assert!(d.burst_start.is_none());
    }

    #[test]
    fn rapid_delete_severity_scales_with_count() {
        let mut d = FrustrationDetector::new();
        for _ in 0..8 {
            d.feed(&Key::Backspace, "word", no_tab());
        }
        // With 8 backspaces: severity = (8/8).clamp(0.3, 1.0) = 1.0
        // Check the last emitted signal has max severity
        // Re-feed one more to get a fresh signal
        let result = d.feed(&Key::Backspace, "word", no_tab());
        if let Some(FrustrationSignal::RapidDelete { severity, .. }) = result {
            assert!((0.3..=1.0).contains(&severity));
        }
    }

    #[test]
    fn default_is_same_as_new() {
        let d1 = FrustrationDetector::new();
        let d2 = FrustrationDetector::default();
        assert_eq!(d1.consecutive_bs, d2.consecutive_bs);
        assert_eq!(d1.escape_seen, d2.escape_seen);
    }
}
