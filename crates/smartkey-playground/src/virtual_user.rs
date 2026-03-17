/// Deterministic virtual user that decides whether to accept a prediction.
///
/// Deliberately simple: measures engine quality, not user behavior modeling.
/// Accepts if and only if the ghost text is a prefix of the remaining target
/// AND the confidence meets the threshold.
pub struct VirtualUser {
    pub threshold: f64,
}

impl VirtualUser {
    pub fn new(threshold: f64) -> Self {
        Self { threshold }
    }

    /// Should the virtual user press Tab to accept this ghost?
    ///
    /// * `ghost` — the ghost/completion text shown by the engine
    /// * `remaining_target` — what's left to type in the target text
    /// * `confidence` — the engine's confidence in the top prediction
    pub fn should_accept(&self, ghost: &str, remaining_target: &str, confidence: f64) -> bool {
        !ghost.is_empty() && confidence >= self.threshold && remaining_target.starts_with(ghost)
    }
}
