"""Thin, IBus-free adapter that wires smartkey's prediction lifecycle to the
Phase-A logger. Kept free of any ``gi``/IBus import so its resolution logic is
unit-testable without the desktop stack.

RESOLUTION CONTRACT (Codex B2 — must cover EVERY committed next-token, not only
the predictions the engine's own ghost wins):

  A prediction is logged at the next-word point (the engine's ShowGhost action,
  which carries no typed prefix → the cursor is at a word boundary). It records
  the token list of the context at that instant (``ctx_tokens``, ``n_ctx`` = its
  length). The event RESOLVES to the token that later occupies slot ``n_ctx`` in
  the surrounding text — i.e. whatever word the user actually produced next,
  whether the engine predicted it, the user typed it out and it was forwarded,
  or it was a rejection. ``outcome = (that token ∈ top3)``; a non-top-3 next
  token records ``outcome = 0`` (NOT "unresolved").

  Two resolution signals, whichever fires first (both proven in the self-test):
    1. surrounding-text delta (``observe_context``): when a later context is a
       prefix-extension of ``ctx_tokens``, the word at slot ``n_ctx`` is the
       actual next token. Covers forwarded/typed/rejected words uniformly.
    2. commit fast-path (``on_commit``): an explicit engine commit/replace of a
       word (Tab-accept, space-autocommit, preedit-commit-on-boundary) resolves
       immediately — a corroborating signal for apps without surrounding text.

  An event only stays UNRESOLVED when neither signal is observable (e.g. an app
  exposing no surrounding text AND emitting no commit). That is an honest
  plumbing gap; ``>5% unresolved`` is a spec FAIL, which correctly flags it.

All top3 words and the resolved token are lowercased so they align with the
(lowercase) corpus keys and each other — the one engine-coupled choice.

The adapter also writes a mechanical ENGINE-IDENTITY receipt (Codex MAJOR) so
the switch script can prove the SELECTED ibus engine is this lab process (pid /
exe / lab commit / phase_a=1 / heartbeat), not the installed component.
"""

from __future__ import annotations

import json
import logging
import os
import sys
import time
from pathlib import Path

from .freqmodel import FreqModel, _WORD_RE
from .harness import Pending, PhaseALogger
from .paths import default_db_path, engine_identity_file

log = logging.getLogger("smartkey.phase_a")


def _tokenize(text: str | None) -> list[str]:
    """Lowercased word tokens of the surrounding text before the cursor."""
    if not text:
        return []
    return [t.lower() for t in _WORD_RE.findall(text)]


class PhaseAAdapter:
    """Observes prediction events; never influences engine output."""

    def __init__(
        self,
        db_path,
        corpus_files,
        engine_commit: str | None = None,
        notes: str | None = None,
        identity_file: str | Path | None = None,
    ) -> None:
        if Path(db_path).resolve() == default_db_path().resolve():
            if os.environ.get("SMARTKEY_PHASE_A") != "1" or not os.environ.get(
                "SMARTKEY_PHASEA_COMMIT"
            ):
                raise RuntimeError(
                    "Refusing to write the live Phase-A events DB outside the "
                    "SMARTKEY_PHASE_A lab engine process"
                )
        self.freq = FreqModel.load(corpus_files)
        self.logger = PhaseALogger(
            db_path,
            self.freq,
            synthetic=False,
            engine_commit=engine_commit,
            notes=notes,
        )
        self.engine_commit = engine_commit
        self.pending: Pending | None = None
        self.last_tokens: list[str] = []
        self.last_context: str = ""
        self._events = 0
        self._identity_file = (
            Path(identity_file) if identity_file is not None else engine_identity_file()
        )
        self._write_identity(started=True)

    # ---- mechanical engine-identity receipt (Codex MAJOR) --------------------
    def _write_identity(self, started: bool = False) -> None:
        try:
            payload = {
                "pid": os.getpid(),
                "exe": sys.executable,
                "argv": sys.argv,
                "cwd": os.getcwd(),
                "lab_commit": self.engine_commit,
                "phase_a": 1,
                "events": self._events,
                "heartbeat_ts": time.time(),
            }
            if started:
                payload["started_ts"] = time.time()
            self._identity_file.write_text(
                json.dumps(payload, ensure_ascii=False), encoding="utf-8"
            )
        except Exception:
            log.debug("phase-a: identity write failed", exc_info=True)

    def heartbeat(self) -> None:
        """Called on focus/key so the receipt proves the engine is ACTIVE."""
        self._write_identity()

    # ---- context observation + resolution ------------------------------------
    def observe_surrounding(self, surrounding_text: str | None, cursor_pos) -> None:
        """IBus gives (text, cursor_pos). Only the text BEFORE the cursor is the
        committed context; resolving against after-cursor words when the operator
        edits mid-text would grab a token they have not typed yet (Codex B4).
        Slice to the before-cursor prefix when cursor_pos is valid."""
        text = surrounding_text or ""
        if isinstance(cursor_pos, int) and 0 <= cursor_pos <= len(text):
            text = text[:cursor_pos]
        self.observe_context(text)

    def observe_context(self, surrounding_text_before_cursor: str | None) -> None:
        """Update context and resolve a pending prediction if the actual next
        token now occupies its predicted slot (surrounding-text delta)."""
        toks = _tokenize(surrounding_text_before_cursor)
        self._resolve_from_tokens(toks)
        self.last_tokens = toks
        self.last_context = toks[-1] if toks else ""
        self._write_identity()

    def _resolve_from_tokens(self, toks: list[str]) -> None:
        p = self.pending
        if p is None:
            return
        n = p.n_ctx
        # The actual next token is the word that now fills slot n, provided the
        # new context still begins with the prediction-time context (forward
        # typing). Editing/backspace breaks the prefix → leave for the commit
        # fast-path or unresolved.
        if len(toks) > n and toks[:n] == p.ctx_tokens:
            self.logger.resolve(p, toks[n])
            self.pending = None

    # ---- candidate-generation point (next-word ghost) ------------------------
    def note_context(self, surrounding_text_before_cursor: str | None) -> None:
        """Back-compat shim: same as observe_context (also resolves)."""
        self.observe_context(surrounding_text_before_cursor)

    def on_next_word_prediction(self, top3_words: list[str]) -> None:
        norm = [w.lower() for w in top3_words[:3] if w]
        if not norm:
            return
        # Redraw of the identical prediction for the same context: skip.
        if (
            self.pending is not None
            and self.pending.ctx_tokens == self.last_tokens
            and self.pending.top3 == norm
        ):
            return
        # A new, different prediction before the previous one resolved: the old
        # one was superseded — leave it as outcome=NULL (unresolved).
        p = self.logger.log_prediction(self.last_context, norm)
        p.ctx_tokens = list(self.last_tokens)
        p.n_ctx = len(self.last_tokens)
        self.pending = p
        self._events += 1

    # ---- resolution fast-path (explicit commit/replace) ----------------------
    def on_commit(self, committed_text: str | None) -> None:
        if self.pending is None:
            return
        toks = _tokenize(committed_text)
        if not toks:
            return
        # The committed word's trailing token is the actual next token.
        self.logger.resolve(self.pending, toks[-1])
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
