use serde::Serialize;

/// Why a prediction was not accepted by the virtual user.
#[derive(Debug, Clone, Serialize)]
pub enum RejectReason {
    /// Ghost text didn't match the remaining target.
    Mismatch,
    /// Confidence below the acceptance threshold.
    LowConfidence,
    /// No ghost text was shown by the engine.
    NoGhost,
}

/// A single recorded event during a simulation session.
#[derive(Debug, Clone, Serialize)]
pub enum TimelineEvent {
    CharTyped {
        offset: usize,
        ch: char,
    },
    PredictionShown {
        offset: usize,
        ghost: String,
        confidence: f64,
        accepted: bool,
    },
    PredictionRejected {
        offset: usize,
        ghost: String,
        confidence: f64,
        reason: RejectReason,
    },
    PredictionAccepted {
        offset: usize,
        ghost: String,
        chars_saved: usize,
    },
    WordCommitted {
        offset: usize,
        word: String,
        was_predicted: bool,
    },
    LanguageSwitched {
        offset: usize,
        from: String,
        to: String,
    },
    TypoInjected {
        offset: usize,
        wrong_char: char,
        correct_char: char,
    },
    PauseTriggered {
        offset: usize,
    },
}
