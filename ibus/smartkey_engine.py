"""SmartKey IBus engine -- ghost text predictions powered by Rust via PyO3.

This module implements the IBus.Engine subclass that drives SmartKey.
It handles keystroke events, manages ghost (auxiliary) text, and delegates
all prediction work to the native ``smartkey_py.PySmartKeyEngine``.
"""

from __future__ import annotations

import json
import os
from collections import deque
from pathlib import Path

# ---------------------------------------------------------------------------
# IBus GObject introspection -- may not be installed on all systems.
# ---------------------------------------------------------------------------
try:
    import gi

    gi.require_version("IBus", "1.0")
    from gi.repository import IBus  # type: ignore[attr-defined]

    _HAS_IBUS = True
except (ValueError, ImportError):
    _HAS_IBUS = False

    # Minimal stubs so the file is importable for testing / linting even when
    # IBus is absent.  The engine will never be instantiated in that case.
    class _FakeIBus:  # noqa: N801
        class Engine:
            pass

        class AttrList:
            def __init__(self) -> None:
                pass

        class Text:
            @staticmethod
            def new_from_string(s: str) -> "_FakeIBus.Text":
                return _FakeIBus.Text()

        class Attribute:
            @staticmethod
            def new(*_a: object) -> "_FakeIBus.Attribute":
                return _FakeIBus.Attribute()

        # Key symbol constants used in the engine.
        KEY_Tab = 0xFF09
        KEY_Escape = 0xFF1B
        KEY_Right = 0xFF53
        KEY_BackSpace = 0xFF08
        KEY_space = 0x0020
        KEY_Return = 0xFF0D
        KEY_Super_L = 0xFFEB
        KEY_Super_R = 0xFFEC
        ATTR_TYPE_FOREGROUND = 1

    IBus = _FakeIBus  # type: ignore[misc,assignment]

# ---------------------------------------------------------------------------
# Rust prediction engine via PyO3.
# ---------------------------------------------------------------------------
try:
    from smartkey_py import PySmartKeyEngine  # type: ignore[import-untyped]

    _HAS_ENGINE = True
except ImportError:
    _HAS_ENGINE = False

    class PySmartKeyEngine:  # type: ignore[no-redef]
        """Stub when the native extension is not available."""

        def load_word(self, word: str, frequency: int) -> None: ...
        def load_bigram(self, context: str, word: str, count: int) -> None: ...
        def load_trigram(self, w1: str, w2: str, word: str, count: int) -> None: ...
        def learn(self, word: str) -> None: ...
        def predict(
            self, prefix: str, context: list[str], limit: int
        ) -> list[tuple[str, float, float]]:
            return []


# ---------------------------------------------------------------------------
# Paths & defaults.
# ---------------------------------------------------------------------------
_CONFIG_DIR = Path(os.environ.get("XDG_CONFIG_HOME", "~/.config")).expanduser() / "smartkey"
_CONFIG_FILE = _CONFIG_DIR / "smartkey.json"
_CORPUS_FILE = _CONFIG_DIR / "corpus.json"

_DEFAULT_CONFIG: dict = {
    "enabled": True,
    "ghost_text": True,
    "popup_on_alt": True,
    "max_candidates": 5,
    "min_prefix_length": 2,
    "kill_switch": "Super+Escape",
    "weights": {"corpus": 0.4, "markov": 0.4, "personal": 0.2},
    "languages": ["en", "bg"],
    "personal_db": "~/.config/smartkey/personal.db",
}

_CONTEXT_SIZE = 5  # number of preceding words kept for Markov context


# ---------------------------------------------------------------------------
# Helper: load JSON with fallback.
# ---------------------------------------------------------------------------
def _load_json(path: Path, default: dict | list | None = None) -> dict | list | None:
    """Return parsed JSON from *path*, or *default* on any error."""
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except Exception:
        return default


# ===========================================================================
# SmartKeyEngine -- the IBus.Engine subclass.
# ===========================================================================
class SmartKeyEngine(IBus.Engine):  # type: ignore[misc]
    """IBus engine that displays ghost-text predictions from SmartKey."""

    __gtype_name__ = "SmartKeyEngine"

    # -----------------------------------------------------------------------
    # Lifecycle.
    # -----------------------------------------------------------------------
    def __init__(self) -> None:
        super().__init__()

        # Prediction backend.
        self._engine = PySmartKeyEngine()

        # Configuration.
        self._config: dict = dict(_DEFAULT_CONFIG)
        user_cfg = _load_json(_CONFIG_FILE)
        if isinstance(user_cfg, dict):
            self._config.update(user_cfg)

        # Internal state.
        self._enabled: bool = self._config.get("enabled", True)
        self._current_word: str = ""
        self._ghost: str = ""  # current ghost (completion) text
        self._context: deque[str] = deque(maxlen=_CONTEXT_SIZE)

        # Load corpus if available.
        self._load_corpus()

    # -----------------------------------------------------------------------
    # Corpus loading.
    # -----------------------------------------------------------------------
    def _load_corpus(self) -> None:
        """Populate the engine with n-gram data from corpus files.

        Scans ``~/.config/smartkey/`` for ``corpus_*.json`` (per-language)
        and the legacy ``corpus.json``, loading all found corpora into the
        same engine so predictions blend across languages.
        """
        corpus_files: list[Path] = []

        # Per-language corpora (corpus_en.json, corpus_bg.json, ...).
        corpus_files.extend(sorted(_CONFIG_DIR.glob("corpus_*.json")))

        # Legacy single corpus.
        if _CORPUS_FILE.is_file():
            corpus_files.append(_CORPUS_FILE)

        for path in corpus_files:
            self._load_corpus_file(path)

    def _load_corpus_file(self, path: Path) -> None:
        """Load a single corpus JSON file into the engine."""
        corpus = _load_json(path)
        if not isinstance(corpus, dict):
            return

        # Unigrams: {"word": frequency, ...}
        for word, freq in corpus.get("unigrams", {}).items():
            self._engine.load_word(word, int(freq))

        # Bigrams: [{"ctx": ..., "word": ..., "count": ...}, ...]
        for entry in corpus.get("bigrams", []):
            self._engine.load_bigram(
                str(entry["ctx"]),
                str(entry["word"]),
                int(entry["count"]),
            )

        # Trigrams: [{"w1": ..., "w2": ..., "word": ..., "count": ...}, ...]
        for entry in corpus.get("trigrams", []):
            self._engine.load_trigram(
                str(entry["w1"]),
                str(entry["w2"]),
                str(entry["word"]),
                int(entry["count"]),
            )

    # -----------------------------------------------------------------------
    # Ghost text display.
    # -----------------------------------------------------------------------
    def _show_ghost(self, text: str) -> None:
        """Display *text* as greyed-out auxiliary (ghost) text."""
        self._ghost = text
        if not text or not self._config.get("ghost_text", True):
            self.hide_auxiliary_text()
            return

        ibus_text = IBus.Text.new_from_string(text)
        # Grey foreground (0xAAAAAA) over the full span.
        attrs = IBus.AttrList()
        attrs.append(
            IBus.Attribute.new(IBus.ATTR_TYPE_FOREGROUND, 0xAAAAAA, 0, len(text))
        )
        ibus_text.set_attributes(attrs)
        self.update_auxiliary_text(ibus_text, True)

    def _clear_ghost(self) -> None:
        """Remove any displayed ghost text."""
        self._ghost = ""
        self.hide_auxiliary_text()

    # -----------------------------------------------------------------------
    # Prediction request.
    # -----------------------------------------------------------------------
    def _update_predictions(self) -> None:
        """Ask the engine for completions and display the top one as ghost text."""
        min_prefix = self._config.get("min_prefix_length", 2)
        if len(self._current_word) < min_prefix:
            self._clear_ghost()
            return

        limit = self._config.get("max_candidates", 5)
        context = list(self._context)
        predictions = self._engine.predict(self._current_word, context, limit)

        if predictions:
            top_word, _score, _confidence = predictions[0]
            # Ghost text shows only the *remaining* characters after the typed prefix.
            suffix = top_word[len(self._current_word) :]
            if suffix:
                self._show_ghost(suffix)
            else:
                self._clear_ghost()
        else:
            self._clear_ghost()

    # -----------------------------------------------------------------------
    # Word commit helpers.
    # -----------------------------------------------------------------------
    def _commit_word(self, word: str) -> None:
        """Commit *word* to the application and update context."""
        if word:
            self._engine.learn(word)
            self._context.append(word)

    def _reset_word(self) -> None:
        """Clear the current in-progress word and ghost text."""
        self._current_word = ""
        self._clear_ghost()

    # -----------------------------------------------------------------------
    # Key event handler.
    # -----------------------------------------------------------------------
    def do_process_key_event(self, keyval: int, keycode: int, state: int) -> bool:  # noqa: C901
        """Handle an IBus key event.

        Returns ``True`` if the key was consumed, ``False`` to pass it through
        to the application.
        """
        # If engine is disabled, pass everything through.
        if not self._enabled:
            # Re-enable with the kill-switch combo.
            if self._is_kill_switch(keyval, state):
                self._enabled = True
                return True
            return False

        # --- Kill switch: Super+Escape toggles the engine off. ----
        if self._is_kill_switch(keyval, state):
            self._enabled = False
            self._clear_ghost()
            self._reset_word()
            return True

        # Only process key-press events (ignore releases).
        is_release = bool(state & IBus.ModifierType.RELEASE_MASK) if _HAS_IBUS else False
        if is_release:
            return False

        # Ignore events with Ctrl/Alt held (except for our combos above).
        if _HAS_IBUS:
            mods = state & (IBus.ModifierType.CONTROL_MASK | IBus.ModifierType.MOD1_MASK)
            if mods:
                return False

        # --- Tab: accept entire ghost text. ---
        if keyval == IBus.KEY_Tab:
            if self._ghost:
                full_word = self._current_word + self._ghost
                self.commit_text(IBus.Text.new_from_string(full_word))
                self._commit_word(full_word)
                self._reset_word()
                return True
            return False

        # --- Right arrow: accept one character of ghost text. ---
        if keyval == IBus.KEY_Right:
            if self._ghost:
                self._current_word += self._ghost[0]
                self._ghost = self._ghost[1:]
                # Re-display the shortened ghost.
                if self._ghost:
                    self._show_ghost(self._ghost)
                else:
                    self._clear_ghost()
                return True
            return False

        # --- Escape: dismiss ghost text. ---
        if keyval == IBus.KEY_Escape:
            if self._ghost:
                self._clear_ghost()
                return True
            return False

        # --- Space / Enter: commit current word, delimit. ---
        if keyval in (IBus.KEY_space, IBus.KEY_Return):
            if self._current_word:
                self._commit_word(self._current_word)
            self._reset_word()
            return False  # let the space/enter reach the application

        # --- Backspace: trim the current word. ---
        if keyval == IBus.KEY_BackSpace:
            if self._current_word:
                self._current_word = self._current_word[:-1]
                self._update_predictions()
                return False  # let backspace reach the application
            return False

        # --- Printable characters: append and re-predict. ---
        char = chr(keyval) if 0x20 < keyval < 0x7F else ""
        if not char:
            # Non-printable / unmapped -- pass through.
            return False

        self._current_word += char
        self._update_predictions()
        return False  # let the character reach the application

    # -----------------------------------------------------------------------
    # Kill-switch detection.
    # -----------------------------------------------------------------------
    @staticmethod
    def _is_kill_switch(keyval: int, state: int) -> bool:
        """Return ``True`` when keyval+state matches Super+Escape."""
        is_escape = keyval == IBus.KEY_Escape
        if _HAS_IBUS:
            super_held = bool(state & IBus.ModifierType.SUPER_MASK)
        else:
            # Fallback: check raw bit 26 (Mod4/Super on X11).
            super_held = bool(state & (1 << 26))
        return is_escape and super_held

    # -----------------------------------------------------------------------
    # IBus lifecycle callbacks.
    # -----------------------------------------------------------------------
    def do_focus_in(self) -> None:
        """Called when the engine gains input focus."""
        pass

    def do_focus_out(self) -> None:
        """Called when the engine loses input focus."""
        if self._current_word:
            self._commit_word(self._current_word)
        self._reset_word()

    def do_reset(self) -> None:
        """Called when the engine should reset its internal state."""
        self._reset_word()

    def do_enable(self) -> None:
        """Called when the engine is enabled by the user."""
        self._enabled = True

    def do_disable(self) -> None:
        """Called when the engine is disabled by the user."""
        self._enabled = False
        self._reset_word()
