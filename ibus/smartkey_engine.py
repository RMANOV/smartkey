"""SmartKey IBus engine -- ghost text predictions powered by Rust via PyO3.

This module implements the IBus.Engine subclass that drives SmartKey.
All key event logic, ghost text lifecycle, and prediction state live in the
Rust ``InputMethodCore`` — this Python layer is a thin IBus adapter.
"""

from __future__ import annotations

import json
import os
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
    from smartkey_py import PyInputMethodCore  # type: ignore[import-untyped]

    _HAS_CORE = True
except ImportError:
    _HAS_CORE = False

    class PyInputMethodCore:  # type: ignore[no-redef]
        """Stub when the native extension is not available."""

        def __init__(self, config_json: str | None = None) -> None: ...
        def load_word(self, word: str, freq: int) -> None: ...
        def load_bigram(self, ctx: str, word: str, count: int) -> None: ...
        def load_trigram(self, w1: str, w2: str, word: str, count: int) -> None: ...
        def handle_key(
            self, keyval: int, modifiers: int
        ) -> list[tuple[str, str]]:
            return [("forward", "")]
        def focus_lost(self) -> list[tuple[str, str]]:
            return []
        def focus_gained(self) -> None: ...
        def reset(self) -> list[tuple[str, str]]:
            return []
        def predictions(self) -> list[tuple[str, float, float]]:
            return []


# ---------------------------------------------------------------------------
# Paths & defaults.
# ---------------------------------------------------------------------------
_CONFIG_DIR = (
    Path(os.environ.get("XDG_CONFIG_HOME", "~/.config")).expanduser() / "smartkey"
)
_CONFIG_FILE = _CONFIG_DIR / "smartkey.json"
_CORPUS_FILE = _CONFIG_DIR / "corpus.json"


def _load_json(
    path: Path, default: dict | list | None = None
) -> dict | list | None:
    """Return parsed JSON from *path*, or *default* on any error."""
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except Exception:
        return default


# ===========================================================================
# SmartKeyEngine -- the IBus.Engine subclass.
# ===========================================================================
class SmartKeyEngine(IBus.Engine):  # type: ignore[misc]
    """IBus engine that displays ghost-text predictions from SmartKey.

    All key event logic lives in the Rust ``InputMethodCore``. This class
    only translates ``Action`` tuples returned from Rust into IBus API calls.
    """

    __gtype_name__ = "SmartKeyEngine"

    # -----------------------------------------------------------------------
    # Lifecycle.
    # -----------------------------------------------------------------------
    def __init__(self) -> None:
        super().__init__()

        # Load user config — Rust core handles defaults, we only pass overrides.
        user_cfg = _load_json(_CONFIG_FILE)
        config_json = json.dumps(user_cfg) if isinstance(user_cfg, dict) else None

        # Create the Rust core with config.
        self._core = PyInputMethodCore(config_json)

        # Load corpus.
        self._load_corpus()

    # -----------------------------------------------------------------------
    # Corpus loading.
    # -----------------------------------------------------------------------
    def _load_corpus(self) -> None:
        """Populate the engine with n-gram data from corpus files."""
        corpus_files: list[Path] = []
        corpus_files.extend(sorted(_CONFIG_DIR.glob("corpus_*.json")))
        if _CORPUS_FILE.is_file():
            corpus_files.append(_CORPUS_FILE)

        for path in corpus_files:
            self._load_corpus_file(path)

    def _load_corpus_file(self, path: Path) -> None:
        """Load a single corpus JSON file into the engine."""
        corpus = _load_json(path)
        if not isinstance(corpus, dict):
            return

        for word, freq in corpus.get("unigrams", {}).items():
            self._core.load_word(word, int(freq))

        for entry in corpus.get("bigrams", []):
            self._core.load_bigram(
                str(entry["ctx"]), str(entry["word"]), int(entry["count"])
            )

        for entry in corpus.get("trigrams", []):
            self._core.load_trigram(
                str(entry["w1"]),
                str(entry["w2"]),
                str(entry["word"]),
                int(entry["count"]),
            )

    # -----------------------------------------------------------------------
    # Ghost text display (IBus-specific).
    # -----------------------------------------------------------------------
    def _show_ghost(self, text: str) -> None:
        """Display *text* as greyed-out auxiliary (ghost) text."""
        ibus_text = IBus.Text.new_from_string(text)
        attrs = IBus.AttrList()
        attrs.append(
            IBus.Attribute.new(IBus.ATTR_TYPE_FOREGROUND, 0xAAAAAA, 0, len(text))
        )
        ibus_text.set_attributes(attrs)
        self.update_auxiliary_text(ibus_text, True)

    def _clear_ghost(self) -> None:
        """Remove any displayed ghost text."""
        self.hide_auxiliary_text()

    # -----------------------------------------------------------------------
    # Action dispatcher — translates Rust actions to IBus API calls.
    # -----------------------------------------------------------------------
    def _execute_actions(
        self, actions: list[tuple[str, str]]
    ) -> bool:
        """Execute action tuples from Rust. Returns True if key was consumed."""
        consumed = True
        for action_type, payload in actions:
            if action_type == "ghost":
                self._show_ghost(payload)
            elif action_type == "hide":
                self._clear_ghost()
            elif action_type == "commit":
                self.commit_text(IBus.Text.new_from_string(payload))
            elif action_type == "forward":
                consumed = False
        return consumed

    # -----------------------------------------------------------------------
    # Key event handler.
    # -----------------------------------------------------------------------
    def do_process_key_event(
        self, keyval: int, keycode: int, state: int
    ) -> bool:
        """Handle an IBus key event via the Rust core."""
        actions = self._core.handle_key(keyval, state)
        return self._execute_actions(actions)

    # -----------------------------------------------------------------------
    # IBus lifecycle callbacks.
    # -----------------------------------------------------------------------
    def do_focus_in(self) -> None:
        self._core.focus_gained()

    def do_focus_out(self) -> None:
        actions = self._core.focus_lost()
        self._execute_actions(actions)

    def do_reset(self) -> None:
        actions = self._core.reset()
        self._execute_actions(actions)

    def do_enable(self) -> None:
        pass  # Rust core handles enabled state via kill switch.

    def do_disable(self) -> None:
        actions = self._core.reset()
        self._execute_actions(actions)
