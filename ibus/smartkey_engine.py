"""SmartKey IBus engine -- ghost text predictions powered by Rust via PyO3.

This module implements the IBus.Engine subclass that drives SmartKey.
All key event logic, ghost text lifecycle, and prediction state live in the
Rust ``MasterLoop``/``InputMethodCore`` stack — this Python layer is a thin
IBus adapter.
"""

from __future__ import annotations

import json
import logging
import os
import time
from json import JSONDecodeError
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
            def new_from_string(_s: str) -> "_FakeIBus.Text":
                return _FakeIBus.Text()

        class Attribute:
            @staticmethod
            def new(*_a: object) -> "_FakeIBus.Attribute":
                return _FakeIBus.Attribute()

        KEY_Tab = 0xFF09
        KEY_Escape = 0xFF1B
        KEY_Home = 0xFF50
        KEY_Left = 0xFF51
        KEY_Up = 0xFF52
        KEY_Right = 0xFF53
        KEY_Down = 0xFF54
        KEY_Page_Up = 0xFF55
        KEY_Page_Down = 0xFF56
        KEY_End = 0xFF57
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
    from smartkey_py import (  # type: ignore[import-untyped]
        PyInputMethodCore,
        ffi_decode_composing_payload,
        ffi_decode_replace_payload,
    )

    _HAS_CORE = True
except ImportError:
    _HAS_CORE = False

    def ffi_decode_replace_payload(_payload: str) -> tuple[int, str] | None:
        return None

    def ffi_decode_composing_payload(_payload: str) -> tuple[str, str] | None:
        return None

    class PyInputMethodCore:  # type: ignore[no-redef]
        """Stub when the native extension is not available."""

        def __init__(self, _config_json: str | None = None) -> None:
            return None

        def load_word(self, _word: str, _freq: int) -> None:
            return None

        def load_bigram(self, _ctx: str, _word: str, _count: int) -> None:
            return None

        def load_trigram(self, _w1: str, _w2: str, _word: str, _count: int) -> None:
            return None

        def load_corpus_file(self, _path: str) -> None:
            return None

        def handle_key(self, _keyval: int, _modifiers: int) -> list[tuple[str, str]]:
            return [("forward", "")]

        def process_keycode(
            self, _keycode: int, _modifiers: int
        ) -> list[tuple[str, str]]:
            return [("forward", "")]

        def focus_lost(self) -> list[tuple[str, str]]:
            return []

        def focus_gained(self) -> None:
            return None

        def set_surrounding_text(
            self, _text: str | None, _cursor_pos: int | None
        ) -> None:
            return None

        def reset(self) -> list[tuple[str, str]]:
            return []

        def save_personal(self) -> None:
            return None

        def load_personal(self) -> None:
            return None

        def export_personal(self, _path: str) -> None:
            return None

        def import_personal(self, _path: str) -> None:
            return None

        def predictions(self) -> list[tuple[str, float, float]]:
            return []


# ---------------------------------------------------------------------------
# Paths & defaults.
# ---------------------------------------------------------------------------
_IS_WAYLAND = bool(os.environ.get("WAYLAND_DISPLAY"))

_DEBUG = os.environ.get("SMARTKEY_DEBUG") == "1"
_PRED_LOG = None
_REPLAY_LOG = None
if _DEBUG:
    import pathlib

    _log_dir = pathlib.Path.home() / ".local" / "share" / "smartkey"
    _log_dir.mkdir(parents=True, exist_ok=True)
    _PRED_LOG = (_log_dir / "predictions.log").open("a", buffering=1)
    _REPLAY_LOG = (_log_dir / "replay.jsonl").open("a", buffering=1)

_CONFIG_DIR = (
    Path(os.environ.get("XDG_CONFIG_HOME", "~/.config")).expanduser() / "smartkey"
)
_CONFIG_FILE = _CONFIG_DIR / "smartkey.json"
_CORPUS_FILE = _CONFIG_DIR / "corpus.json"

# Fallback corpus directory: repo root / corpus/ (sibling of ibus/).
# An explicit _CORPUS_DIR env var overrides both _CONFIG_DIR and the repo path.
_REPO_DIR = Path(__file__).resolve().parent.parent
_REPO_CORPUS_DIR = _REPO_DIR / "corpus"


def _load_json(path: Path, default: dict | list | None = None) -> dict | list | None:
    """Return parsed JSON from *path*, or *default* on any error."""
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return default
    except (UnicodeDecodeError, JSONDecodeError) as exc:
        log.warning("smartkey: invalid JSON in %s (%s) — using defaults", path, exc)
        return default
    except OSError as exc:
        log.warning("smartkey: cannot read %s (%s) — using defaults", path, exc)
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
        if _HAS_IBUS:
            super().__init__(active_surrounding_text=True)
        else:
            super().__init__()

        if not _HAS_CORE:
            raise RuntimeError(
                "smartkey_py native extension is unavailable; SmartKey cannot start"
            )

        # Load user config — Rust core handles defaults, we only pass overrides.
        user_cfg = _load_json(_CONFIG_FILE)
        config_json = json.dumps(user_cfg) if isinstance(user_cfg, dict) else None

        # Create the Rust core with config.
        self._core = PyInputMethodCore(config_json)

        # Load personal profile (learned words from previous sessions).
        try:
            self._core.load_personal()
        except FileNotFoundError:
            log.debug("smartkey: no personal profile loaded (first run?)")
        except OSError:
            log.warning("smartkey: failed to load personal profile", exc_info=True)

        # Debounce timer for auto-save on focus-out (60s cooldown).
        self._last_save: float = 0.0

        # IBus client capabilities (set by do_set_capabilities callback).
        self._caps: int = 0

        # Preedit lifecycle tracking: True while a preedit window is visible.
        # Used by _safe_commit() to guarantee preedit is hidden before commit,
        # and by backspace handling to consume the key when preedit is active.
        self._preedit_active: bool = False
        self._surrounding_text: str | None = None
        self._surrounding_cursor_pos: int | None = None
        self._session_id = f"{int(time.time() * 1000)}-{os.getpid()}"
        self._prediction_seq = 0
        self._active_prediction: dict[str, object] | None = None

        # Load corpus.
        self._load_corpus()

    # -----------------------------------------------------------------------
    # Corpus loading.
    # -----------------------------------------------------------------------
    def _load_corpus(self) -> None:
        """Populate the engine with n-gram data from corpus files.

        Search order (first non-empty set of files wins for corpus_*.json/bin):
        1. ``$_CORPUS_DIR`` environment variable override (explicit path).
        2. ``_CONFIG_DIR`` (~/.config/smartkey/) — user config takes precedence.
        3. ``_REPO_CORPUS_DIR`` — repo's corpus/ directory next to ibus/.
        The legacy single-file ``corpus.json`` in _CONFIG_DIR is always appended
        if present, regardless of which directory supplied the sharded files.
        """
        corpus_files: list[Path] = []

        # Determine which directories to scan, in priority order.
        env_override = os.environ.get("SMARTKEY_CORPUS_DIR")
        search_dirs: list[Path] = []
        if env_override:
            search_dirs.append(Path(env_override).expanduser())
        search_dirs.extend([_CONFIG_DIR, _REPO_CORPUS_DIR])

        for search_dir in search_dirs:
            json_files = sorted(search_dir.glob("corpus_*.json"))
            if not json_files:
                # Also check for pre-compiled .bin shards.
                bin_files = sorted(search_dir.glob("corpus_*.bin"))
                if bin_files:
                    corpus_files.extend(bin_files)
                    break
                continue
            corpus_files.extend(json_files)
            # Add .bin files only if no matching .json exists (Rust auto-caches
            # .json → .bin, so loading both would double all n-gram counts).
            json_stems = {p.stem for p in json_files}
            corpus_files.extend(
                p
                for p in sorted(search_dir.glob("corpus_*.bin"))
                if p.stem not in json_stems
            )
            break  # found files — stop searching further dirs

        if _CORPUS_FILE.is_file():
            corpus_files.append(_CORPUS_FILE)

        if not corpus_files:
            log.warning(
                "smartkey: no corpus files found in %s or %s",
                _CONFIG_DIR,
                _REPO_CORPUS_DIR,
            )
            return

        log.info(
            "smartkey: loading %d corpus file(s) from %s",
            len(corpus_files),
            corpus_files[0].parent,
        )
        loaded_count = 0
        for path in corpus_files:
            try:
                self._core.load_corpus_file(str(path))
                loaded_count += 1
            except (OSError, ValueError):
                log.warning("Failed to load corpus %s", path, exc_info=True)

        if loaded_count == 0 and corpus_files:
            log.error(
                "smartkey: ALL %d corpus files failed to load — "
                "predictions will be empty until corpus is available",
                len(corpus_files),
            )

    @staticmethod
    def _decode_surrounding_text(text_obj: object | None) -> str | None:
        if text_obj is None:
            return None
        if hasattr(text_obj, "get_text"):
            try:
                decoded = text_obj.get_text()
            except TypeError:
                decoded = None
            if isinstance(decoded, str):
                return decoded
        if isinstance(text_obj, str):
            return text_obj
        return None

    def _sync_surrounding_text(self, text: str | None, cursor_pos: int | None) -> None:
        self._surrounding_text = text if text else None
        self._surrounding_cursor_pos = (
            cursor_pos if cursor_pos is not None and cursor_pos >= 0 else None
        )
        self._core.set_surrounding_text(
            self._surrounding_text, self._surrounding_cursor_pos
        )

    def _refresh_surrounding_text(self) -> None:
        if not _HAS_IBUS or not hasattr(self, "get_surrounding_text"):
            return
        try:
            surrounding = self.get_surrounding_text()
        except TypeError:
            return
        except Exception:
            log.debug("smartkey: failed to refresh surrounding text", exc_info=True)
            return

        text_obj: object | None = None
        cursor_pos: int | None = None
        if isinstance(surrounding, tuple):
            if surrounding:
                text_obj = surrounding[0]
            if len(surrounding) >= 2 and isinstance(surrounding[1], int):
                cursor_pos = surrounding[1]
        else:
            text_obj = surrounding

        self._sync_surrounding_text(
            self._decode_surrounding_text(text_obj),
            cursor_pos,
        )

    def _log_replay_event(self, event: str, **payload: object) -> None:
        if not _REPLAY_LOG:
            return
        record = {
            "ts": time.time(),
            "session": self._session_id,
            "event": event,
        }
        record.update(
            {key: value for key, value in payload.items() if value is not None}
        )
        _REPLAY_LOG.write(json.dumps(record, ensure_ascii=False) + "\n")
        _REPLAY_LOG.flush()

    def _track_prediction_shown(self, ghost: str) -> None:
        preds = self._core.predictions()
        if not ghost or not preds:
            self._active_prediction = None
            return

        top_word, top_score, top_confidence = preds[0]
        self._prediction_seq += 1
        self._active_prediction = {
            "prediction_id": self._prediction_seq,
            "word": top_word,
            "ghost": ghost,
            "score": top_score,
            "confidence": top_confidence,
        }
        self._log_replay_event(
            "shown",
            prediction_id=self._prediction_seq,
            word=top_word,
            ghost=ghost,
            score=round(top_score, 6),
            confidence=round(top_confidence, 6),
        )

    @staticmethod
    def _is_navigation_key(keyval: int) -> bool:
        return keyval in {
            IBus.KEY_Left,
            IBus.KEY_Up,
            IBus.KEY_Down,
            IBus.KEY_Home,
            IBus.KEY_End,
            IBus.KEY_Page_Up,
            IBus.KEY_Page_Down,
        }

    @staticmethod
    def _is_printable_keyval(keyval: int) -> bool:
        if keyval in (IBus.KEY_space, IBus.KEY_Return):
            return True
        if 0x21 <= keyval <= 0x7E:
            return True
        if _HAS_IBUS and hasattr(IBus, "keyval_to_unicode"):
            uni = IBus.keyval_to_unicode(keyval)
            if uni and uni != "\0":
                return uni.isprintable()
        if 0x80 <= keyval <= 0xEFFF:
            try:
                return chr(keyval).isprintable()
            except (ValueError, OverflowError):
                return False
        if 0x01000000 <= keyval <= 0x0110FFFF:
            try:
                return chr(keyval & 0x00FFFFFF).isprintable()
            except (ValueError, OverflowError):
                return False
        return False

    def _should_log_reject(self, keyval: int, key_release: bool) -> bool:
        if key_release or keyval in (IBus.KEY_Tab, IBus.KEY_Right):
            return False
        if keyval in (IBus.KEY_Escape, IBus.KEY_BackSpace):
            return True
        if self._is_navigation_key(keyval):
            return True
        return self._is_printable_keyval(keyval)

    def _clear_active_prediction_if(self, prediction_id: int) -> None:
        if (
            self._active_prediction
            and self._active_prediction.get("prediction_id") == prediction_id
        ):
            self._active_prediction = None

    def _finalize_prediction_outcome(
        self,
        keyval: int,
        key_release: bool,
        actions: list[tuple[str, str]],
        pending_prediction: dict[str, object] | None,
    ) -> None:
        if not pending_prediction:
            return

        prediction_id = int(pending_prediction["prediction_id"])
        action_types = {action_type for action_type, _ in actions}
        accepted = keyval == IBus.KEY_Tab and "commit" in action_types
        auto_accepted = keyval == IBus.KEY_space and "replace" in action_types

        if accepted or auto_accepted:
            self._log_replay_event(
                "accepted",
                prediction_id=prediction_id,
                word=pending_prediction.get("word"),
                ghost=pending_prediction.get("ghost"),
                confidence=pending_prediction.get("confidence"),
                reason="tab" if accepted else "space_autocommit",
            )
            self._clear_active_prediction_if(prediction_id)
            return

        if not self._should_log_reject(keyval, key_release):
            return

        if keyval == IBus.KEY_Escape:
            reason = "escape"
        elif self._is_navigation_key(keyval):
            reason = "navigation"
        elif keyval == IBus.KEY_BackSpace:
            reason = "backspace"
        elif keyval in (IBus.KEY_space, IBus.KEY_Return):
            reason = "word_boundary"
        else:
            reason = "continued_typing"

        self._log_replay_event(
            "rejected",
            prediction_id=prediction_id,
            word=pending_prediction.get("word"),
            ghost=pending_prediction.get("ghost"),
            confidence=pending_prediction.get("confidence"),
            reason=reason,
        )
        self._clear_active_prediction_if(prediction_id)

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
        self._preedit_active = True

    def _clear_ghost(self) -> None:
        """Remove any displayed ghost text."""
        self.hide_preedit_text()
        self._preedit_active = False

    def _show_composing(self, typed: str, ghost: str) -> None:
        """Display composing preedit: typed prefix (normal) + ghost suffix (grey)."""
        full_text = typed + ghost
        ibus_text = IBus.Text.new_from_string(full_text)
        typed_bytes = len(typed.encode("utf-8"))
        total_bytes = len(full_text.encode("utf-8"))
        attrs = IBus.AttrList()
        # Typed prefix: underline only (normal text color).
        attrs.append(
            IBus.Attribute.new(
                IBus.ATTR_TYPE_UNDERLINE, IBus.ATTR_UNDERLINE_SINGLE, 0, typed_bytes
            )
        )
        # Ghost suffix: grey + underline.
        if ghost:
            attrs.append(
                IBus.Attribute.new(
                    IBus.ATTR_TYPE_FOREGROUND, 0x888888, typed_bytes, total_bytes
                )
            )
            attrs.append(
                IBus.Attribute.new(
                    IBus.ATTR_TYPE_UNDERLINE,
                    IBus.ATTR_UNDERLINE_SINGLE,
                    typed_bytes,
                    total_bytes,
                )
            )
        ibus_text.set_attributes(attrs)
        self.update_preedit_text(ibus_text, typed_bytes, True)
        self._preedit_active = True

    # -----------------------------------------------------------------------
    # Qt-safe commit helper.
    # -----------------------------------------------------------------------
    def _safe_commit(self, text: str) -> None:
        """Qt-safe commit: hide preedit BEFORE committing text.

        On some Qt versions, committing text while a preedit window is visible
        causes the preedit content to be inserted twice (doubling bug).
        _clear_ghost() calls hide_preedit_text() which is sufficient; do NOT
        add an extra update_preedit_text("", 0, False) here as that triggers
        double preedit events on certain Qt versions.
        """
        if self._preedit_active:
            self._clear_ghost()
        self.commit_text(IBus.Text.new_from_string(text))

    # -----------------------------------------------------------------------
    # Action dispatcher — translates Rust actions to IBus API calls.
    # -----------------------------------------------------------------------
    def _execute_actions(self, actions: list[tuple[str, str]]) -> bool:
        """Execute action tuples from Rust. Returns True if key was consumed."""
        consumed = True
        for action_type, payload in actions:
            if action_type == "ghost":
                self._show_ghost(payload)
                self._track_prediction_shown(payload)
                if _PRED_LOG:
                    preds = self._core.predictions()
                    top3 = preds[:3]
                    pts = [str(time.time()), "ghost:" + payload]
                    pts.extend(
                        p[0] + ":" + "{:.3f}".format(p[1]) + ":" + "{:.3f}".format(p[2])
                        for p in top3
                    )
                    pts.append("shown")
                    _PRED_LOG.write(" | ".join(pts) + "\n")
                    _PRED_LOG.flush()
            elif action_type == "hide":
                self._clear_ghost()
            elif action_type == "commit":
                self._safe_commit(payload)
                if _PRED_LOG:
                    _PRED_LOG.write(
                        str(time.time()) + " | TAB_ACCEPT | " + payload + "\n"
                    )
                    _PRED_LOG.flush()
            elif action_type == "replace":
                if self._preedit_active:
                    self._clear_ghost()
                decoded = ffi_decode_replace_payload(payload)
                if decoded is None:
                    log.warning("smartkey: malformed replace payload %r", payload)
                    continue
                replace_len, text = decoded
                if self._caps & 0x20:  # SURROUNDING_TEXT capability
                    self.delete_surrounding_text(-replace_len, replace_len)
                else:
                    # Fallback: backspace key events for apps without
                    # surrounding text support (GTK4 Wayland, Electron, etc.)
                    for _ in range(replace_len):
                        self.forward_key_event(IBus.KEY_BackSpace, 14, 0)
                self._safe_commit(text)
            elif action_type == "composing":
                decoded = ffi_decode_composing_payload(payload)
                if decoded is None:
                    log.warning("smartkey: malformed composing payload %r", payload)
                    continue
                typed, ghost = decoded
                self._show_composing(typed, ghost)
                self._track_prediction_shown(ghost)
                if _PRED_LOG:
                    preds2 = self._core.predictions()
                    top3 = preds2[:3]
                    pts2 = [str(time.time()), "composing:" + typed]
                    pts2.extend(
                        p[0] + ":" + "{:.3f}".format(p[1]) + ":" + "{:.3f}".format(p[2])
                        for p in top3
                    )
                    pts2.append("shown")
                    _PRED_LOG.write(" | ".join(pts2) + "\n")
                    _PRED_LOG.flush()
            elif action_type == "forward":
                consumed = False
        return consumed

    def do_set_capabilities(self, caps: int) -> None:
        self._caps = caps

    def do_set_surrounding_text(
        self, text: object, cursor_pos: int, anchor_pos: int
    ) -> None:
        _ = anchor_pos
        self._sync_surrounding_text(
            self._decode_surrounding_text(text),
            cursor_pos,
        )

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

        # Track backspace so we can consume it when preedit is active,
        # preventing the key from also deleting committed text in the app.
        is_backspace = keyval == IBus.KEY_BackSpace
        key_release = bool(state & (1 << 30))  # IBUS_RELEASE_MASK
        pending_prediction = (
            dict(self._active_prediction)
            if self._active_prediction is not None
            else None
        )
        if not key_release:
            self._refresh_surrounding_text()

        # v0.5.0: prefer raw scancode path for dual-buffer layout-agnostic input.
        # Rust tables use evdev codes. On Wayland IBus sends evdev directly;
        # on X11 IBus sends XKB (evdev + 8) — normalise to evdev.
        if keycode > 0 and _HAS_CORE:
            evdev_keycode = keycode if _IS_WAYLAND else max(keycode - 8, 0)
            preedit_was_active = self._preedit_active
            actions = self._core.process_keycode(evdev_keycode, state)
            log.debug("actions (keycode): %s", actions)
            result = self._execute_actions(actions)
            self._finalize_prediction_outcome(
                keyval, key_release, actions, pending_prediction
            )
            # Consume backspace key-press when preedit was active so it does
            # not reach the application.  Key-release is always forwarded.
            if is_backspace and not key_release and preedit_was_active:
                return True
            return result

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
        preedit_was_active = self._preedit_active
        actions = self._core.handle_key(keyval, state)
        log.debug("actions: %s", actions)
        result = self._execute_actions(actions)
        self._finalize_prediction_outcome(
            keyval, key_release, actions, pending_prediction
        )
        # Consume backspace key-press when preedit was active so it does
        # not reach the application.  Key-release is always forwarded.
        if is_backspace and not key_release and preedit_was_active:
            return True
        return result

    # -----------------------------------------------------------------------
    # IBus lifecycle callbacks.
    # -----------------------------------------------------------------------
    def do_focus_in(self) -> None:
        self._core.focus_gained()
        self._refresh_surrounding_text()

    def do_focus_out(self) -> None:
        actions = self._core.focus_lost()
        self._execute_actions(actions)
        self._active_prediction = None
        self._sync_surrounding_text(None, None)
        # Debounced auto-save: persist personal profile at most once per 60s.
        now = time.monotonic()
        if now - self._last_save >= 60.0:
            try:
                self._core.save_personal()
                self._last_save = now
            except OSError:
                log.warning("smartkey: failed to save personal profile", exc_info=True)

    def do_reset(self) -> None:
        actions = self._core.reset()
        self._execute_actions(actions)
        self._active_prediction = None
        self._sync_surrounding_text(None, None)

    def do_enable(self) -> None:
        pass  # Rust core handles enabled state via kill switch.

    def do_disable(self) -> None:
        actions = self._core.reset()
        self._execute_actions(actions)
        self._active_prediction = None
        self._sync_surrounding_text(None, None)
        # Save personal profile on engine disable (session end).
        try:
            self._core.save_personal()
        except OSError:
            log.warning("smartkey: failed to save personal profile", exc_info=True)
