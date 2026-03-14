"""SmartKey IBus engine -- ghost text predictions powered by Rust via PyO3.

This module implements the IBus.Engine subclass that drives SmartKey.
All key event logic, ghost text lifecycle, and prediction state live in the
Rust ``InputMethodCore`` — this Python layer is a thin IBus adapter.
"""

from __future__ import annotations

import json
import logging
import os
import time
from pathlib import Path

log = logging.getLogger("smartkey")

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
        ATTR_TYPE_UNDERLINE = 2
        ATTR_UNDERLINE_SINGLE = 1

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
        def load_corpus_file(self, path: str) -> None: ...
        def handle_key(self, keyval: int, modifiers: int) -> list[tuple[str, str]]:
            return [("forward", "")]

        def process_keycode(self, keycode: int, modifiers: int) -> list[tuple[str, str]]:
            return [("forward", "")]

        def focus_lost(self) -> list[tuple[str, str]]:
            return []

        def focus_gained(self) -> None: ...
        def reset(self) -> list[tuple[str, str]]:
            return []

        def save_personal(self) -> None: ...
        def load_personal(self) -> None: ...
        def export_personal(self, path: str) -> None: ...
        def import_personal(self, path: str) -> None: ...
        def predictions(self) -> list[tuple[str, float, float]]:
            return []


# ---------------------------------------------------------------------------
# Paths & defaults.
# ---------------------------------------------------------------------------
_IS_WAYLAND = bool(os.environ.get("WAYLAND_DISPLAY"))

_CONFIG_DIR = (
    Path(os.environ.get("XDG_CONFIG_HOME", "~/.config")).expanduser() / "smartkey"
)
_CONFIG_FILE = _CONFIG_DIR / "smartkey.json"
_CORPUS_FILE = _CONFIG_DIR / "corpus.json"


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

        # Load personal profile (learned words from previous sessions).
        try:
            self._core.load_personal()
        except Exception:
            log.debug("smartkey: no personal profile loaded (first run?)")

        # Debounce timer for auto-save on focus-out (60s cooldown).
        self._last_save: float = 0.0

        # IBus client capabilities (set by do_set_capabilities callback).
        self._caps: int = 0

        # Load corpus.
        self._load_corpus()

    # -----------------------------------------------------------------------
    # Corpus loading.
    # -----------------------------------------------------------------------
    def _load_corpus(self) -> None:
        """Populate the engine with n-gram data from corpus files."""
        corpus_files: list[Path] = []
        corpus_files.extend(sorted(_CONFIG_DIR.glob("corpus_*.json")))
        # Add .bin files only if no matching .json exists (Rust auto-caches
        # .json → .bin, so loading both would double all n-gram counts).
        json_stems = {p.stem for p in corpus_files}
        corpus_files.extend(
            p
            for p in sorted(_CONFIG_DIR.glob("corpus_*.bin"))
            if p.stem not in json_stems
        )
        if _CORPUS_FILE.is_file():
            corpus_files.append(_CORPUS_FILE)

        for path in corpus_files:
            try:
                self._core.load_corpus_file(str(path))
            except Exception:
                log.warning("Failed to load corpus %s", path, exc_info=True)

    # -----------------------------------------------------------------------
    # Ghost text display (IBus-specific).
    # -----------------------------------------------------------------------
    def _show_ghost(self, text: str) -> None:
        """Display *text* as greyed-out preedit (ghost) text inline at cursor."""
        ibus_text = IBus.Text.new_from_string(text)
        byte_len = len(text.encode("utf-8"))
        attrs = IBus.AttrList()
        attrs.append(
            IBus.Attribute.new(IBus.ATTR_TYPE_FOREGROUND, 0x888888, 0, byte_len)
        )
        attrs.append(
            IBus.Attribute.new(
                IBus.ATTR_TYPE_UNDERLINE, IBus.ATTR_UNDERLINE_SINGLE, 0, byte_len
            )
        )
        ibus_text.set_attributes(attrs)
        self.update_preedit_text(ibus_text, byte_len, True)

    def _clear_ghost(self) -> None:
        """Remove any displayed ghost text."""
        self.hide_preedit_text()

    # -----------------------------------------------------------------------
    # Action dispatcher — translates Rust actions to IBus API calls.
    # -----------------------------------------------------------------------
    def _execute_actions(self, actions: list[tuple[str, str]]) -> bool:
        """Execute action tuples from Rust. Returns True if key was consumed."""
        consumed = True
        for action_type, payload in actions:
            if action_type == "ghost":
                self._show_ghost(payload)
            elif action_type == "hide":
                self._clear_ghost()
            elif action_type == "commit":
                self.commit_text(IBus.Text.new_from_string(payload))
            elif action_type == "replace":
                n_str, text = payload.split(":", 1)
                replace_len = int(n_str)
                if self._caps & 0x20:  # SURROUNDING_TEXT capability
                    self.delete_surrounding_text(-replace_len, replace_len)
                else:
                    # Fallback: backspace key events for apps without
                    # surrounding text support (GTK4 Wayland, Electron, etc.)
                    for _ in range(replace_len):
                        self.forward_key_event(IBus.KEY_BackSpace, 14, 0)
                self.commit_text(IBus.Text.new_from_string(text))
            elif action_type == "forward":
                consumed = False
        return consumed

    def do_set_capabilities(self, caps: int) -> None:
        self._caps = caps

    # -----------------------------------------------------------------------
    # Key event handler.
    # -----------------------------------------------------------------------
    def do_process_key_event(self, keyval: int, keycode: int, state: int) -> bool:
        """Handle an IBus key event via the Rust core.

        When a valid hardware scancode is available (keycode > 0), the event
        is routed through ``process_keycode()`` which enables the dual-buffer
        engine for layout-agnostic input.  Falls back to the keyval path
        (``handle_key()``) when keycode is unavailable.
        """
        log.debug("key: keyval=0x%04X keycode=%d state=0x%04X", keyval, keycode, state)

        # v0.5.0: prefer raw scancode path for dual-buffer layout-agnostic input.
        # Rust tables use evdev codes. On Wayland IBus sends evdev directly;
        # on X11 IBus sends XKB (evdev + 8) — normalise to evdev.
        if keycode > 0 and _HAS_CORE:
            evdev_keycode = keycode if _IS_WAYLAND else max(keycode - 8, 0)
            actions = self._core.process_keycode(evdev_keycode, state)
            log.debug("actions (keycode): %s", actions)
            return self._execute_actions(actions)

        # Fallback: keyval path (backward compat, or when keycode is 0).
        orig_keyval = keyval
        # Convert legacy X11 keysyms (e.g. Cyrillic 0x06xx) to Unicode
        # codepoints so the Rust core sees them as Key::Char.
        if _HAS_IBUS and hasattr(IBus, "keyval_to_unicode"):
            uni = IBus.keyval_to_unicode(keyval)
            if uni and uni != "\0" and ord(uni) >= 0x80:
                keyval = ord(uni)
        # Fallback: Unicode-encoded keysyms (0x0100xxxx → extract codepoint)
        if keyval >= 0x01000000 and keyval <= 0x0110FFFF:
            keyval = keyval & 0x00FFFFFF
        if keyval != orig_keyval:
            try:
                ch_repr = chr(keyval)
            except (ValueError, OverflowError):
                ch_repr = f"\\u{keyval:04X}"
            log.debug(
                "keysym converted: 0x%04X → 0x%04X (%s)", orig_keyval, keyval, ch_repr
            )
        actions = self._core.handle_key(keyval, state)
        log.debug("actions: %s", actions)
        return self._execute_actions(actions)

    # -----------------------------------------------------------------------
    # IBus lifecycle callbacks.
    # -----------------------------------------------------------------------
    def do_focus_in(self) -> None:
        self._core.focus_gained()

    def do_focus_out(self) -> None:
        actions = self._core.focus_lost()
        self._execute_actions(actions)
        # Debounced auto-save: persist personal profile at most once per 60s.
        now = time.monotonic()
        if now - self._last_save >= 60.0:
            try:
                self._core.save_personal()
                self._last_save = now
            except Exception:
                log.warning("Failed to save personal profile", exc_info=True)

    def do_reset(self) -> None:
        actions = self._core.reset()
        self._execute_actions(actions)

    def do_enable(self) -> None:
        pass  # Rust core handles enabled state via kill switch.

    def do_disable(self) -> None:
        actions = self._core.reset()
        self._execute_actions(actions)
        # Save personal profile on engine disable (session end).
        try:
            self._core.save_personal()
        except Exception:
            log.warning("Failed to save personal profile", exc_info=True)
