"""Thin, IBus-free adapter that wires smartkey's prediction lifecycle to the
Phase-A logger. Kept free of any ``gi``/IBus import so its pending/resolve/
supersede logic is unit-testable without the desktop stack.

Resolver semantics = ``next_token_in_top3`` (spec blocker 3): we log at the
**next-word** prediction point (the engine's ShowGhost action, which carries no
typed prefix — i.e. the cursor is at a word boundary and the whole next token is
being anticipated). The event resolves to the *next token committed to the
application*. In-word completions (ShowComposing, with a typed prefix) are
deliberately not logged here: they are a different semantic and would supersede
one another per keystroke. One clean event per predicted word position.

All top3 words and the resolved token are lowercased before membership/lookup so
they align with the (lowercase) corpus keys and each other — this is the one
place coupled to engine token representation, and it is documented as such.
"""

from __future__ import annotations

import logging

from .freqmodel import FreqModel, last_context_word
from .harness import Pending, PhaseALogger

log = logging.getLogger("smartkey.phase_a")


class PhaseAAdapter:
    """Observes prediction events; never influences engine output."""

    def __init__(
        self,
        db_path,
        corpus_files,
        engine_commit: str | None = None,
        notes: str | None = None,
    ) -> None:
        self.freq = FreqModel.load(corpus_files)
        self.logger = PhaseALogger(
            db_path,
            self.freq,
            synthetic=False,
            engine_commit=engine_commit,
            notes=notes,
        )
        self.pending: Pending | None = None
        self.last_context: str = ""

    # ---- context -------------------------------------------------------------
    def note_context(self, surrounding_text_before_cursor: str | None) -> None:
        self.last_context = last_context_word(surrounding_text_before_cursor)

    # ---- candidate-generation point (next-word ghost) ------------------------
    def on_next_word_prediction(self, top3_words: list[str]) -> None:
        norm = [w.lower() for w in top3_words[:3] if w]
        if not norm:
            return
        # Redraw of the identical prediction for the same context: skip.
        if (
            self.pending is not None
            and self.pending.context == self.last_context
            and self.pending.top3 == norm
        ):
            return
        # A new, different prediction before the previous one resolved: the old
        # one was superseded — leave it as outcome=NULL (unresolved).
        self.pending = self.logger.log_prediction(self.last_context, norm)

    # ---- resolution point (next committed token) -----------------------------
    def on_commit(self, committed_text: str | None) -> None:
        if self.pending is None:
            return
        tok = last_context_word(committed_text)
        if not tok:
            return
        self.logger.resolve(self.pending, tok)
        self.pending = None

    # ---- lifecycle -----------------------------------------------------------
    def on_reset(self) -> None:
        # Focus-out / reset: any in-flight prediction stays unresolved.
        self.pending = None

    def close(self) -> None:
        try:
            self.logger.close()
        except Exception:  # never let teardown crash the engine
            log.debug("phase-a: logger close failed", exc_info=True)
