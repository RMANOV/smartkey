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


def _ibus_enum_value(
    enum_name: str, member_name: str, legacy_name: str, fallback: int
) -> int:
    """Resolve modern GI enums while retaining compatibility with old IBus."""
    enum_type = getattr(IBus, enum_name, None)
    if enum_type is not None and hasattr(enum_type, member_name):
        return int(getattr(enum_type, member_name))
    legacy = getattr(IBus, legacy_name, None)
    return int(legacy) if legacy is not None else fallback


_ATTR_TYPE_UNDERLINE = _ibus_enum_value(
    "AttrType", "UNDERLINE", "ATTR_TYPE_UNDERLINE", 1
)
_ATTR_TYPE_FOREGROUND = _ibus_enum_value(
    "AttrType", "FOREGROUND", "ATTR_TYPE_FOREGROUND", 2
)
_ATTR_UNDERLINE_SINGLE = _ibus_enum_value(
    "AttrUnderline", "SINGLE", "ATTR_UNDERLINE_SINGLE", 1
)
# Ghost suffix carries a double underline so it is distinguishable from typed
# text by more than colour alone. Space and Tab do opposite things to a visible
# ghost — Tab takes it, Space discards it — and nothing in the preedit signalled
# that a decision was pending. Live trace 2026-08-03/05: 204 Space commits vs 1
# Tab in 37 hours, i.e. the affordance was never discovered.
_ATTR_UNDERLINE_DOUBLE = _ibus_enum_value(
    "AttrUnderline", "DOUBLE", "ATTR_UNDERLINE_DOUBLE", 2
)

# ---------------------------------------------------------------------------
# Sensitive-input policy (IBus content type).
# ---------------------------------------------------------------------------
# IBus.InputPurpose mirrors GtkInputPurpose / the Wayland text-input-v3
# content purposes.  Every purpose this adapter has positively AUDITED as
# ordinary-or-secret is listed here; anything else (a newer enum member, a
# malformed value, an unset value) is treated as sensitive.  The list is
# deliberately explicit rather than read off the live enum: a purpose added by
# a future IBus is one nobody here has classified yet, and guessing "ordinary"
# is exactly the failure mode this policy exists to prevent.
# Verified against the installed IBus typelib: FREE_FORM..DATETIME == 0..13.
_KNOWN_PURPOSES = frozenset(
    _ibus_enum_value("InputPurpose", _name, "INPUT_PURPOSE_" + _name, _fallback)
    for _name, _fallback in (
        ("FREE_FORM", 0),
        ("ALPHA", 1),
        ("DIGITS", 2),
        ("NUMBER", 3),
        ("PHONE", 4),
        ("URL", 5),
        ("EMAIL", 6),
        ("NAME", 7),
        ("PASSWORD", 8),
        ("PIN", 9),
        ("TERMINAL", 10),
        ("DATE", 11),
        ("TIME", 12),
        ("DATETIME", 13),
    )
)
_PURPOSE_PASSWORD = _ibus_enum_value(
    "InputPurpose", "PASSWORD", "INPUT_PURPOSE_PASSWORD", 8
)
_PURPOSE_PIN = _ibus_enum_value("InputPurpose", "PIN", "INPUT_PURPOSE_PIN", 9)
_SENSITIVE_PURPOSES = frozenset({_PURPOSE_PASSWORD, _PURPOSE_PIN})
# Hints that declare secrecy on their own, with the purpose left at FREE_FORM:
#   PRIVATE      — "do not store this text" (Wayland ``sensitive_data``),
#   HIDDEN_TEXT  — the client renders the text hidden, i.e. a password box.
# Either one is honoured independently of the purpose.
_HINT_PRIVATE = _ibus_enum_value("InputHints", "PRIVATE", "INPUT_HINT_PRIVATE", 1 << 11)
_HINT_HIDDEN_TEXT = _ibus_enum_value(
    "InputHints", "HIDDEN_TEXT", "INPUT_HINT_HIDDEN_TEXT", 1 << 12
)
_SENSITIVE_HINTS = _HINT_PRIVATE | _HINT_HIDDEN_TEXT

# IBus modifier bits used by the phantom-key filter.
_MOD_CONTROL = 1 << 2  # IBus.ModifierType.CONTROL_MASK
_MOD_ALT = 1 << 3  # IBus.ModifierType.MOD1_MASK
_MOD_RELEASE = 1 << 30  # IBus.ModifierType.RELEASE_MASK
# The inert zero-key callback storm always carries this hardware code.
_SPURIOUS_KEYCODE = 240


def _sensitive_until_declared() -> bool:
    """Opt-in strict mode: treat a never-declared content type as sensitive.

    IBus content-purpose callbacks are advisory and adapter-specific — plenty
    of clients never send one at all — so defaulting "never declared" to
    sensitive would disable SmartKey wholesale rather than protect anything.
    Every *declared* ambiguity still fails safe (see
    ``SmartKeyEngine._classify_content_type``); this switch extends the same
    policy to the undeclared case for operators who want the strictest stance.
    """
    return os.environ.get("SMARTKEY_SENSITIVE_UNTIL_DECLARED") == "1"


# ---------------------------------------------------------------------------
# Rust prediction engine via PyO3.
# ---------------------------------------------------------------------------
try:
    # Keep the core import independent from optional protocol helpers.  A
    # previously installed smartkey_py may expose PyInputMethodCore but predate
    # ffi_decode_* and process_keycode; that core can still provide a safe,
    # ordinary keyval path while the native extension is rebuilt.
    from smartkey_py import PyInputMethodCore  # type: ignore[import-untyped]

    _HAS_CORE = True
except ImportError:
    _HAS_CORE = False

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


def _decode_replace_payload_fallback(payload: str) -> tuple[int, str] | None:
    """Decode the Rust ReplaceWord wire format without the optional FFI shim."""
    if not isinstance(payload, str) or "\x1f" not in payload:
        return None
    length_text, text = payload.split("\x1f", 1)
    if not length_text or not length_text.isascii() or not length_text.isdigit():
        return None
    return int(length_text), text


def _decode_composing_payload_fallback(payload: str) -> tuple[str, str] | None:
    """Decode the Rust ShowComposing wire format without the optional FFI shim."""
    if not isinstance(payload, str) or "\x00" not in payload:
        return None
    return tuple(payload.split("\x00", 1))  # type: ignore[return-value]


if _HAS_CORE:
    try:
        from smartkey_py import (  # type: ignore[import-untyped]
            ffi_decode_composing_payload,
            ffi_decode_replace_payload,
        )
    except ImportError:
        ffi_decode_replace_payload = _decode_replace_payload_fallback
        ffi_decode_composing_payload = _decode_composing_payload_fallback
else:
    ffi_decode_replace_payload = _decode_replace_payload_fallback
    ffi_decode_composing_payload = _decode_composing_payload_fallback


# ---------------------------------------------------------------------------
# Paths & defaults.
# ---------------------------------------------------------------------------
_IS_WAYLAND = bool(os.environ.get("WAYLAND_DISPLAY"))

# Legacy content logs (predictions.log / replay.jsonl) carry verbatim words,
# so they are gated on the CONTENT level only: SMARTKEY_DEBUG=1 now means
# "structural keystroke trace, no content" (see smartkey_debug.py).
_DEBUG = os.environ.get("SMARTKEY_DEBUG") == "full"
_PRED_LOG = None
_REPLAY_LOG = None
if _DEBUG:
    import pathlib

    _log_dir = pathlib.Path.home() / ".local" / "share" / "smartkey"
    _log_dir.mkdir(parents=True, exist_ok=True)
    # Block-buffered: the OS flushes off the hot path (no per-write flush).
    _PRED_LOG = (_log_dir / "predictions.log").open("a")
    _REPLAY_LOG = (_log_dir / "replay.jsonl").open("a")

# Keystroke diagnostic trace (spec 2026-07-12).  Import must never take the
# engine down; a missing module simply means tracing stays off.
try:
    from smartkey_debug import LEVEL_FULL, KeystrokeTrace, lang_class
except ImportError:  # engine imported from a different sys.path root
    try:
        from ibus.smartkey_debug import (  # type: ignore
            LEVEL_FULL,
            KeystrokeTrace,
            lang_class,
        )
    except ImportError:
        KeystrokeTrace = None  # type: ignore[assignment,misc]
        LEVEL_FULL = 2

        def lang_class(_text: object) -> str:  # type: ignore[misc]
            return "empty"

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

        # Sensitive-input state (see do_set_content_type).  ``_content_type_key``
        # is the decision-relevant projection of the last declared content type
        # so that a mere re-send, or a hints change we do not act on, is not
        # mistaken for a field switch.
        self._sensitive: bool = _sensitive_until_declared()
        self._content_type_key: tuple[int, int] | None = None
        self._keys_since_content_type: int = 0

        # Phantom keycode-240 storm bookkeeping (see _note_spurious_zero_key).
        self._spurious_zero_key_count: int = 0
        self._spurious_zero_key_logged: float = 0.0

        # Preedit lifecycle tracking.  Ghost-only and full-word composing
        # preedits need different commit ordering in browser/Qt clients.
        self._preedit_active: bool = False
        self._preedit_mode: str | None = None
        self._surrounding_text: str | None = None
        self._surrounding_cursor_pos: int | None = None
        self._session_id = f"{int(time.time() * 1000)}-{os.getpid()}"
        self._prediction_seq = 0
        self._active_prediction: dict[str, object] | None = None

        # Keystroke diagnostic trace (off unless SMARTKEY_DEBUG / marker file).
        # Buffer flushes are deferred through GLib.idle_add so they run after
        # the key event completes — never inside the hot path.
        scheduler = None
        if _HAS_IBUS:
            try:
                from gi.repository import GLib  # noqa: PLC0415

                scheduler = GLib.idle_add
            except ImportError:
                scheduler = None
        self._trace = (
            KeystrokeTrace(schedule_flush=scheduler)
            if KeystrokeTrace is not None
            else None
        )
        # Last composing prefix shown to the user — the accept event compares
        # it (typed_lang) against what actually got committed (committed_lang).
        self._last_composing_typed: str = ""

        # Load corpus.
        self._o1_corpus_loaded: list[str] = []
        self._load_corpus()

        # Optional passive calibration tap. It is dark by default and any
        # initialization failure leaves the input path untouched.
        self._o1 = None
        o1_cfg = user_cfg.get("o1_shim") if isinstance(user_cfg, dict) else None
        if (
            isinstance(o1_cfg, dict)
            and o1_cfg.get("enabled")
            and callable(getattr(self._core, "o1_snapshot", None))
        ):
            try:
                from .o1_shim import O1Collector  # noqa: PLC0415

                self._o1 = O1Collector(
                    self._core,
                    o1_cfg,
                    self._o1_corpus_loaded,
                    o1_cfg.get("engine_commit"),
                )
            except Exception:  # noqa: BLE001 -- optional tap must fail isolated
                log.exception("smartkey: calibration collector init failed")
                self._o1 = None

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
                self._o1_corpus_loaded.append(str(path))
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
            "prefix": self._core_current_word(),
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

    @staticmethod
    def _keyval_to_char(keyval: int) -> str:
        if 0x21 <= keyval <= 0x7E:
            return chr(keyval)
        if _HAS_IBUS and hasattr(IBus, "keyval_to_unicode"):
            uni = IBus.keyval_to_unicode(keyval)
            if uni and uni != "\0":
                return uni
        if 0x80 <= keyval <= 0xEFFF:
            try:
                return chr(keyval)
            except (ValueError, OverflowError):
                return ""
        if 0x01000000 <= keyval <= 0x0110FFFF:
            try:
                return chr(keyval & 0x00FFFFFF)
            except (ValueError, OverflowError):
                return ""
        return ""

    @classmethod
    def _is_punctuation_boundary(cls, keyval: int) -> bool:
        ch = cls._keyval_to_char(keyval)
        return (
            len(ch) == 1
            and ch.isprintable()
            and not ch.isalnum()
            and not ch.isspace()
        )

    def _should_log_reject(self, keyval: int, key_release: bool) -> bool:
        if key_release or keyval in (IBus.KEY_Tab, IBus.KEY_Right):
            return False
        if keyval in (IBus.KEY_Escape, IBus.KEY_BackSpace):
            return True
        if self._is_navigation_key(keyval):
            return True
        return self._is_printable_keyval(keyval)

    # A deliberate chord (CTRL/ALT) or a key RELEASE must never be swallowed
    # by the phantom filter, however inert the event looks.
    _REAL_KEY_GUARD_MASK = _MOD_CONTROL | _MOD_ALT | _MOD_RELEASE

    @classmethod
    def _is_spurious_zero_key_event(cls, keyval: int, keycode: int, state: int) -> bool:
        """Recognize the inert IBus/GTK zero-key callback storm.

        Fedora/GTK can emit a ~25 Hz burst of ``keyval=0, keycode=240``
        callbacks while a preedit is active.  They carry no key symbol at all,
        so forwarding them can starve real browser key events.

        The identifying signature is ``keyval == 0 and keycode == 240`` on a
        PRESS.  The modifier mask is incidental — NumLock, CapsLock, Shift, a
        held mouse button and Super all ride along — so it is masked out of
        the decision rather than compared.  The previous ``state in (16, 272)``
        equality let every other combination through: across the recorded
        traces 10 180 events matched and 63 leaked (mods 18, 19, 1040 and
        67108947), each forwarded with an empty outcome.  Only a genuine
        CTRL/ALT chord or a release event is excluded here, so no real key can
        be consumed by this predicate.
        """
        if keyval != 0 or keycode != _SPURIOUS_KEYCODE:
            return False
        return not state & cls._REAL_KEY_GUARD_MASK

    # Log at most one phantom-storm line per interval: a 25 Hz storm should
    # show up in the log without becoming the log.
    _SPURIOUS_LOG_INTERVAL = 60.0

    def _note_spurious_zero_key(self) -> None:
        """Count a consumed phantom event; report the running total, rate-limited."""
        count = getattr(self, "_spurious_zero_key_count", 0) + 1
        self._spurious_zero_key_count = count
        now = time.monotonic()
        if now - getattr(self, "_spurious_zero_key_logged", 0.0) >= (
            self._SPURIOUS_LOG_INTERVAL
        ):
            self._spurious_zero_key_logged = now
            log.debug("smartkey: consumed %d phantom keycode-240 event(s)", count)

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
        # Tab-only accept (2026-07-12): Space/Return commits are the TYPED
        # word, never an acceptance — they fall through to the rejection
        # bookkeeping below (reason="word_boundary").  The old
        # "space_autocommit" reason died with the aggressive path; a Space
        # ``replace`` now only means a post-commit caps/translit correction
        # of the typed word, which must not be logged as an acceptance.
        accepted = keyval == IBus.KEY_Tab and "commit" in action_types

        if accepted:
            self._log_replay_event(
                "accepted",
                prediction_id=prediction_id,
                word=pending_prediction.get("word"),
                ghost=pending_prediction.get("ghost"),
                confidence=pending_prediction.get("confidence"),
                reason="tab",
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
        elif (
            keyval in (IBus.KEY_space, IBus.KEY_Return)
            or self._is_punctuation_boundary(keyval)
            or ("commit" in action_types and "forward" in action_types)
        ):
            reason = "word_boundary"
        else:
            reason = "continued_typing"

        typed_through = reason in ("continued_typing", "word_boundary")
        genuine = typed_through and self._is_genuine_ghost_rejection(
            reason, pending_prediction
        )
        manual_completion = typed_through and not genuine
        self._log_replay_event(
            "completed" if manual_completion else "rejected",
            prediction_id=prediction_id,
            word=pending_prediction.get("word"),
            ghost=pending_prediction.get("ghost"),
            confidence=pending_prediction.get("confidence"),
            reason="completed_manually" if manual_completion else reason,
        )
        if genuine:
            self._feed_rejection_memory(pending_prediction)
        self._clear_active_prediction_if(prediction_id)

    def _o1_notify(
        self,
        keyval: int,
        key_release: bool,
        actions: list[tuple[str, str]],
        pending_prediction: dict[str, object] | None,
        pre_key_word: str,
    ) -> None:
        """Feed exactly one committed word to the optional passive tap."""
        collector = getattr(self, "_o1", None)
        if collector is None or key_release:
            return
        committed = any(action_type == "commit" for action_type, _ in actions)
        if not committed:
            return
        if keyval == IBus.KEY_Tab:
            word = str((pending_prediction or {}).get("word") or "")
        else:
            word = pre_key_word
        if word:
            collector.on_word_commit(word)

    def _core_current_word(self) -> str:
        getter = getattr(self._core, "current_word", None)
        if not callable(getter):
            return ""
        try:
            return getter() or ""
        except Exception:  # noqa: BLE001 -- diagnostics must never break input
            return ""

    def _is_genuine_ghost_rejection(
        self,
        reason: str,
        pending_prediction: dict[str, object],
    ) -> bool:
        completion = str(pending_prediction.get("word") or "")
        if not completion:
            return False
        if reason == "continued_typing":
            live_prefix = self._core_current_word()
            return bool(live_prefix) and not completion.casefold().startswith(
                live_prefix.casefold()
            )
        if reason == "word_boundary":
            committed = str(pending_prediction.get("pre_key_word") or "")
            return not (
                committed and committed.casefold() == completion.casefold()
            )
        return False

    def _feed_rejection_memory(
        self, pending_prediction: dict[str, object]
    ) -> None:
        recorder = getattr(self._core, "record_ghost_rejection", None)
        if not callable(recorder):
            return
        prefix = str(pending_prediction.get("prefix") or "")
        completion = str(pending_prediction.get("word") or "")
        if not prefix or not completion:
            return
        try:
            recorder(prefix, completion)
        except Exception:  # noqa: BLE001 -- feedback must never break input
            log.debug("smartkey: record_ghost_rejection failed", exc_info=True)

    # -----------------------------------------------------------------------
    # Keystroke diagnostic trace (spec 2026-07-12).
    # -----------------------------------------------------------------------
    _KEYNAMES = {
        0xFF09: "Tab",
        0xFF1B: "Escape",
        0xFF08: "BackSpace",
        0x0020: "space",
        0xFF0D: "Return",
        0xFF51: "Left",
        0xFF53: "Right",
        0xFF52: "Up",
        0xFF54: "Down",
    }

    @classmethod
    def _keyname(cls, keyval: int) -> str:
        name = cls._KEYNAMES.get(keyval)
        if name is not None:
            return name
        if 0x21 <= keyval <= 0x7E or 0x80 <= keyval <= 0x2FFFF:
            try:
                return chr(keyval)
            except ValueError:
                pass
        return hex(keyval)

    def _trace_key_result(
        self,
        keyval: int,
        key_release: bool,
        actions: list[tuple[str, str]],
        result: bool,
    ) -> None:
        """Record the core verdict, dispatched actions, accept and outcome
        for the current keystroke.  No-op unless tracing is enabled."""
        trace = getattr(self, "_trace", None)
        if trace is None or not trace.enabled:
            return

        dual_present = locked = hypothesis = None
        debug_state = getattr(self._core, "debug_state", None)
        if callable(debug_state):
            try:
                dual_present, locked, hypothesis = debug_state()
            except Exception:  # noqa: BLE001 — trace must never break input
                pass
        try:
            predictions = [
                (word, round(score, 4), round(conf, 4))
                for word, score, conf in self._core.predictions()[:3]
            ]
        except Exception:  # noqa: BLE001
            predictions = []
        active = self._active_prediction or {}
        trace.emit(
            "core",
            verdict="consume" if result else "forward",
            dual_buffer_present=dual_present,
            locked=locked,
            hypothesis_phase=hypothesis,
            predictions=predictions,
            ghost=active.get("ghost"),
        )

        committed: list[str] = []
        for action_type, payload in actions:
            trace.emit(
                "action",
                action_name=action_type,
                payload_len=len(payload),
                payload_text=payload,
            )
            if action_type == "commit":
                committed.append(payload)
            elif action_type == "replace":
                decoded = ffi_decode_replace_payload(payload)
                if decoded is not None:
                    committed.append(decoded[1])

        if committed and not key_release:
            committed_text = "".join(committed)
            typed_prefix = getattr(self, "_last_composing_typed", "")
            # Same printable-key privacy rule as key_in: a composing-guard
            # commit can be triggered by a printable char (digit/punct).
            keyname = self._keyname(keyval)
            if len(keyname) == 1 and trace.level < LEVEL_FULL:
                keyname = f"char:{lang_class(keyname)}"
            trace.emit(
                "accept",
                key=keyname,
                committed_text=committed_text,
                typed_prefix=typed_prefix,
                committed_lang=lang_class(committed_text),
                typed_lang=lang_class(typed_prefix),
            )
        trace.emit(
            "outcome",
            committed_to_app_len=sum(len(text) for text in committed),
            committed_to_app="".join(committed),
        )

    # -----------------------------------------------------------------------
    # Ghost text display (IBus-specific).
    # -----------------------------------------------------------------------
    def _show_ghost(self, text: str) -> None:
        """Display *text* as greyed-out preedit (ghost) text inline at cursor."""
        ibus_text = IBus.Text.new_from_string(text)
        text_len = len(text)
        attrs = IBus.AttrList()
        attrs.append(
            IBus.Attribute.new(_ATTR_TYPE_FOREGROUND, 0x888888, 0, text_len)
        )
        attrs.append(
            IBus.Attribute.new(
                _ATTR_TYPE_UNDERLINE, _ATTR_UNDERLINE_SINGLE, 0, text_len
            )
        )
        ibus_text.set_attributes(attrs)
        self.update_preedit_text(ibus_text, text_len, True)
        self._preedit_active = True
        self._preedit_mode = "ghost"
        self._last_composing_typed = ""

    def _clear_ghost(self) -> None:
        """Remove any displayed ghost text."""
        self.hide_preedit_text()
        self._preedit_active = False
        self._preedit_mode = None

    def _show_composing(self, typed: str, ghost: str) -> None:
        """Display composing preedit: typed prefix (normal) + ghost suffix (grey)."""
        full_text = typed + ghost
        ibus_text = IBus.Text.new_from_string(full_text)
        typed_len = len(typed)
        total_len = len(full_text)
        attrs = IBus.AttrList()
        # Typed prefix: underline only (normal text color).
        attrs.append(
            IBus.Attribute.new(
                _ATTR_TYPE_UNDERLINE, _ATTR_UNDERLINE_SINGLE, 0, typed_len
            )
        )
        # Ghost suffix: grey + underline.
        if ghost:
            attrs.append(
                IBus.Attribute.new(
                    _ATTR_TYPE_FOREGROUND, 0x888888, typed_len, total_len
                )
            )
            attrs.append(
                IBus.Attribute.new(
                    _ATTR_TYPE_UNDERLINE,
                    _ATTR_UNDERLINE_DOUBLE,
                    typed_len,
                    total_len,
                )
            )
        ibus_text.set_attributes(attrs)
        self.update_preedit_text(ibus_text, typed_len, True)
        self._preedit_active = True
        self._preedit_mode = "composing"
        self._last_composing_typed = typed

    # -----------------------------------------------------------------------
    # Qt-safe commit helper.
    # -----------------------------------------------------------------------
    def _safe_commit(self, text: str) -> None:
        """Commit text with mode-aware preedit ordering.

        A composing preedit contains the user's current word.  Some browser
        clients retain that payload when it is merely hidden, then append the
        explicit CommitText payload a second time.  Replace it with an empty
        invisible preedit before committing.  Ghost-only preedits keep the
        safer hide-before-commit ordering.
        """
        if self._preedit_active and getattr(self, "_preedit_mode", None) == "composing":
            self.update_preedit_text(IBus.Text.new_from_string(""), 0, False)
            self._preedit_active = False
            self._preedit_mode = None
            self.commit_text(IBus.Text.new_from_string(text))
            return
        if self._preedit_active:
            self._clear_ghost()
        self.commit_text(IBus.Text.new_from_string(text))

    @staticmethod
    def _coalesce_same_batch_commit_replace(
        actions: list[tuple[str, str]]
    ) -> list[tuple[str, str]]:
        """Fold commit+replace of the same word into one commit.

        The post-commit pipeline can emit CommitText(word) followed by
        ReplaceWord(len(word), corrected).  In browser clients a surrounding
        delete racing the commit can leave both copies visible.  Coalesce only
        when the replacement length exactly matches the preceding commit;
        standalone replacements still correct already-forwarded text.
        """
        if not actions:
            return actions

        coalesced: list[tuple[str, str]] = []
        pending_commit_idx: int | None = None
        pending_commit_text: str | None = None
        for action_type, payload in actions:
            if action_type == "commit":
                pending_commit_idx = len(coalesced)
                pending_commit_text = payload
                coalesced.append((action_type, payload))
                continue

            if action_type == "replace" and pending_commit_idx is not None:
                decoded = ffi_decode_replace_payload(payload)
                if decoded is not None and pending_commit_text is not None:
                    replace_len, replacement = decoded
                    if replace_len == len(pending_commit_text):
                        coalesced[pending_commit_idx] = ("commit", replacement)
                        pending_commit_text = replacement
                        continue

            coalesced.append((action_type, payload))
        return coalesced

    @staticmethod
    def _drop_forward_race_ghost(
        actions: list[tuple[str, str]]
    ) -> list[tuple[str, str]]:
        """Do not install a fresh preedit before a forwarded boundary lands.

        Returning ``False`` is what lets IBus deliver Space, Return, digits or
        punctuation to the client.  Every action in this list executes before
        that return, so a same-batch next-word ghost can intercept the key in
        browser/Electron/GTK clients.  The Rust core enforces this invariant;
        retain the adapter guard for mixed-version upgrades and future paths.
        """
        resolves_composing = any(
            action_type in {"commit", "replace"} for action_type, _ in actions
        )
        forwards_key = any(action_type == "forward" for action_type, _ in actions)
        if not (resolves_composing and forwards_key):
            return actions
        return [action for action in actions if action[0] != "ghost"]

    # -----------------------------------------------------------------------
    # Action dispatcher — translates Rust actions to IBus API calls.
    # -----------------------------------------------------------------------
    # These two resolve the word BY DEFINITION, whatever else is in the batch.
    # ``forward`` is deliberately absent — the core emits a bare ForwardKey for
    # a CTRL chord or a release while the dual buffer still holds the word.
    #
    # ``hide`` is absent too, and that is a correction. An earlier revision
    # counted it, reasoning that every ``HideGhost`` follows a ``reset_word()``
    # or a commit. It does not: the core emits a bare ``HideGhost`` mid-word
    # whenever the prefix has no ghost suffix.
    #
    # Measured against the real native module, because the two core entry
    # points differ and only one is affected:
    #
    #   process_keycode('x','q','z')  -> ['composing'] each; counter 1,2,3
    #   handle_key('x','q','z')       -> ['hide','forward'] each,
    #                                    with current_word() 'x','xq','xqz'
    #
    # So the dual-buffer path is fine and the KEYVAL COMPAT path is not — the
    # branch taken when the client sends ``keycode == 0`` or the installed
    # native module predates ``process_keycode``. There, every letter of an
    # out-of-vocabulary word zeroed ``_keys_since_content_type``, so
    # ``_composition_in_flight()`` reported "nothing open" while the core was
    # still holding the word, and a content type arriving mid-word was not
    # escalated to sensitive. ``hide`` is still honoured, but only once the core
    # itself confirms the word is gone: see ``_composition_resolved_by``.
    _COMPOSITION_RESOLVING_ACTIONS = frozenset({"commit", "replace"})

    def _composition_resolved_by(self, actions: list[tuple[str, str]]) -> bool:
        """True when this batch means the core is no longer holding a word.

        ``commit``/``replace`` answer on their own. A bare ``hide`` is
        ambiguous, so ask the only component that knows — the core. When it
        cannot be asked (an older native module, or a fake in a test), the
        answer is False: leaving the counter running keeps the fail-safe armed,
        and the cost of a false "still composing" is one content-type change
        treated as sensitive, against the cost of a false "resolved", which is
        the kill switch not firing inside a password field.
        """
        if any(a[0] in self._COMPOSITION_RESOLVING_ACTIONS for a in actions):
            return True
        if not any(a[0] == "hide" for a in actions):
            return False
        current_word = getattr(self._core, "current_word", None)
        if current_word is None:
            return False
        try:
            return not current_word()
        except Exception:  # noqa: BLE001
            log.debug("smartkey: current_word() failed, keeping the fail-safe armed",
                      exc_info=True)
            return False

    def _execute_actions(self, actions: list[tuple[str, str]]) -> bool:
        """Execute action tuples from Rust. Returns True if key was consumed."""
        actions = self._coalesce_same_batch_commit_replace(actions)
        actions = self._drop_forward_race_ghost(actions)
        composing_resolution_pending = (
            self._preedit_active
            and getattr(self, "_preedit_mode", None) == "composing"
        )
        # Decided on the batch, before dispatch: the core's own view of the
        # in-flight word is the tie-breaker for a bare ``hide``, and dispatch
        # does not change it.
        composition_resolved = self._composition_resolved_by(actions)
        consumed = True
        for idx, (action_type, payload) in enumerate(actions):
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
            elif action_type == "hide":
                # A full-word composing preedit must survive until the
                # following commit/replace has cleared it explicitly.  Hiding
                # it first is the browser doubling bug this adapter guards.
                if composing_resolution_pending and any(
                    later_type in {"commit", "replace"}
                    for later_type, _ in actions[idx + 1 :]
                ):
                    continue
                self._clear_ghost()
            elif action_type == "commit":
                self._safe_commit(payload)
                composing_resolution_pending = False
                if _PRED_LOG:
                    _PRED_LOG.write(
                        str(time.time()) + " | TAB_ACCEPT | " + payload + "\n"
                    )
            elif action_type == "replace":
                decoded = ffi_decode_replace_payload(payload)
                if decoded is None:
                    log.warning("smartkey: malformed replace payload %r", payload)
                    continue
                replace_len, text = decoded
                if composing_resolution_pending:
                    # The current word exists only in the composing preedit;
                    # never delete already-committed browser text for it.
                    self._safe_commit(text)
                else:
                    if self._preedit_active:
                        self._clear_ghost()
                    if self._caps & 0x20:  # SURROUNDING_TEXT capability
                        self.delete_surrounding_text(-replace_len, replace_len)
                    else:
                        # Fallback: backspace key events for apps without
                        # surrounding text support (GTK4 Wayland, Electron, etc.)
                        for _ in range(replace_len):
                            self.forward_key_event(IBus.KEY_BackSpace, 14, 0)
                    self._safe_commit(text)
                composing_resolution_pending = False
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
            elif action_type == "forward":
                consumed = False
        if composition_resolved:
            # Word boundary, not just a focus boundary: the mid-composition
            # fail-safe must stop counting keys the core has already resolved,
            # or the counter stays truthy for the rest of the field and the
            # client's next content-type change is escalated to sensitive long
            # after the word committed.
            self._keys_since_content_type = 0
        return consumed

    def do_set_capabilities(self, caps: int) -> None:
        self._caps = caps

    # -----------------------------------------------------------------------
    # Sensitive input (IBus content type).
    # -----------------------------------------------------------------------
    @staticmethod
    def _coerce_content_flag(value: object) -> int | None:
        """Best-effort int for an IBus enum/flags argument; None if unusable.

        ``bool`` is rejected on purpose: it is an ``int`` subclass, so a client
        (or a binding bug) passing True/False would otherwise be read as
        purpose 1/0 — i.e. as an ordinary text field.
        """
        if value is None or isinstance(value, bool):
            return None
        try:
            return int(value)
        except (TypeError, ValueError, OverflowError):
            return None

    @classmethod
    def _classify_content_type(
        cls, purpose: object, hints: object
    ) -> tuple[bool, tuple[int, int]]:
        """Return ``(sensitive, decision_key)`` for a declared content type.

        Fail safe: only a purpose this adapter positively recognises as
        ordinary text yields ``sensitive=False``.  An unset, malformed or
        unrecognised purpose is treated as sensitive, because an IBus
        content-purpose callback is advisory and adapter-specific — reading
        "I do not understand this" as "ordinary text" is the failure mode
        that must not exist.

        ``decision_key`` is the decision-relevant projection of the content
        type (normalised purpose + the secrecy hints).  Hints the adapter does
        not act on are projected away, so a client toggling e.g. spellcheck
        mid-word is not mistaken for a field switch.  Unrecognised purposes
        collapse to ``-1`` so that two different unknown values do not look
        like a field switch to each other either.
        """
        purpose_value = cls._coerce_content_flag(purpose)
        hints_value = cls._coerce_content_flag(hints) or 0
        secret_hint = bool(hints_value & _SENSITIVE_HINTS)
        if purpose_value is None or purpose_value not in _KNOWN_PURPOSES:
            return True, (-1, int(secret_hint))
        return (
            secret_hint or purpose_value in _SENSITIVE_PURPOSES,
            (purpose_value, int(secret_hint)),
        )

    def _composition_in_flight(self) -> bool:
        """True while typing in this context may still be unresolved.

        Two independent signals, because either can be true without the other:

        * a live preedit — the visible half;
        * keystrokes seen since the last content-type declaration that the core
          has **not** yet resolved with a commit/replace/hide.  The Rust dual
          buffer can hold a word with nothing on screen, so the preedit flag
          alone would miss it.

        The counter is zeroed by ``_execute_actions`` on every such resolution
        as well as at focus/reset/disable boundaries; it therefore reports "a
        word is still open", not "a key was pressed at some point in this
        field".
        """
        return bool(
            getattr(self, "_preedit_active", False)
            or getattr(self, "_keys_since_content_type", 0)
        )

    def _reset_for_sensitive_switch(self) -> None:
        """Drop every trace of the in-flight word across a sensitivity switch.

        The dual buffer holds the engine's *interpretation* of the keystrokes
        (it can be the transliterated hypothesis rather than the literal keys),
        so the pending word is discarded rather than committed: replaying it
        could push a transliterated fragment of a secret into the client, or
        the previous field's word into a password box.  The price is one lost
        preedit word when a content type changes mid-word — deliberate, and
        the safe direction.
        """
        try:
            self._core.reset()  # actions discarded on purpose — see docstring
        except Exception:  # noqa: BLE001 — the kill switch must never fail open
            log.warning(
                "smartkey: core reset failed on content-type switch", exc_info=True
            )
        if getattr(self, "_preedit_active", False):
            try:
                self._clear_ghost()
            except Exception:  # noqa: BLE001 — the kill switch must never fail open
                # ``hide_preedit_text()`` is a D-Bus round trip and can fail.
                # Letting it abort the reset would leave the previous field's
                # word in ``_last_composing_typed``/``_active_prediction`` and
                # a stale preedit displayed over the now-sensitive field; the
                # unconditional assignments below run either way.
                log.warning(
                    "smartkey: preedit takedown failed on content-type switch",
                    exc_info=True,
                )
        self._preedit_active = False
        self._preedit_mode = None
        self._last_composing_typed = ""
        self._active_prediction = None
        self._keys_since_content_type = 0
        try:
            self._sync_surrounding_text(None, None)
        except Exception:  # noqa: BLE001
            log.debug("smartkey: could not clear surrounding text", exc_info=True)

    # NOTE: there is deliberately no ``_forget_content_type()``.  One existed
    # and was removed; see the lifetime section of ``do_set_content_type``.

    def do_set_content_type(self, purpose: object, hints: object) -> None:
        """IBus content-type callback — the sensitive-field kill switch.

        Without this callback the adapter treated a password box like ordinary
        prose: password input may have been captured where the client delivered
        it through IBus — composed through the dual buffer (which can commit
        the Cyrillic hypothesis for a Latin keystroke), fed to the predictor,
        learned into the personal profile and the OOV trie, and written to the
        keystroke trace at ``LEVEL=full``.

        The callback is ADVISORY and adapter-specific, so the policy here is
        deliberately asymmetric: any ambiguity in the PURPOSE turns sensitive
        mode ON, and only an explicit, recognised, non-sensitive purpose
        declared outside a live composition turns it back off.  The HINTS axis
        deliberately fails the other way — unusable hints read as "no secrecy
        hint" (``_classify_content_type``), because hints are optional and
        routinely unset, so failing safe on them would disable every ordinary
        field.

        WHAT THIS COVERS, STATED PRECISELY.  This protects password fields in
        clients that correctly publish a ContentType.  It is NOT a general
        guarantee that SmartKey never sees a password, and it must not be
        described as one.  Three gaps are open by construction:

        * A client that never calls ``set_content_type`` — the callback simply
          never fires, and the field is handled as prose.  Whether GTK, Qt,
          Electron and terminal emulators each publish it is a per-client fact,
          not something this adapter can assert.
        * A field that gets its focus before its declaration.  The declaration
          race is one keystroke wide at worst; ``SMARTKEY_SENSITIVE_UNTIL_DECLARED=1``
          closes it at the cost of one dead keystroke per undeclared field.
        * Anything that does not go through IBus at all — a browser password
          manager, a client with its own input handling, an X11 grab.

        The honest claim is a large reduction in exposure with a named
        boundary, and the boundary is the client's behaviour.

        LIFETIME.  The declaration is STICKY: it survives ``do_focus_out`` and
        is replaced only by another declaration.  This is not the obvious
        choice, and an earlier revision did the opposite — expire on focus-out
        — on the reasoning that ``_sensitive`` is a fact about ONE input
        context while this adapter object outlives it.  That reasoning is
        sound; the mechanism it assumed is not.

        IBus deduplicates this callback.  ``ibus_input_context_set_content_type``
        reads the proxy's cached ``ContentType`` property, compares it with
        ``g_variant_equal``, and issues the D-Bus call ONLY when they differ
        (verified against the installed ``libibus-1.0.so.5`` — the ``g_variant_equal``
        and ``g_dbus_proxy_get_cached_property`` relocations are both there in
        that function, and the ``g_dbus_proxy_call`` is on the unequal branch).
        So a client does not re-declare on every focus-in; it declares when the
        type *changes*.  Expiring on focus-out therefore reopens the password
        field itself: focus away from the password box and back, no new
        declaration arrives, and the engine is live inside it.

        What un-sticks the flag is an ordinary declaration.  Moving to a field
        whose content type differs from the cached one makes IBus send it, and
        an explicit recognised non-sensitive purpose clears ``_sensitive``.  The
        residual is the mirror of the discarded design's: a client that
        declares PASSWORD and then hands focus to a client that never declares
        anything at all keeps SmartKey inert.  That direction is safe and
        recoverable — one declaration restores it — whereas the other direction
        types into a password box.

        IBus 1.5.34 does expose a per-context focus API
        (``focus_in_id(object_path, client)`` / ``focus_out_id(object_path)``,
        selected by the ``has-focus-id`` GObject property, default False), which
        would let one engine instance track this per context properly.  Opting
        in means re-plumbing the focus path of the live IME — far wider than
        this policy warrants.

        No chain-up: IBus 1.5.34 exposes no ``content-type`` GObject property,
        and its default ``set_content_type`` vfunc is a no-op (verified — it
        does not even update ``get_content_type()``), so there is nothing for
        the parent to do.
        """
        sensitive, decision_key = self._classify_content_type(purpose, hints)

        # Fail safe: a content type that changes while a word is still in
        # flight means the composition may already belong to a different field
        # than the one it was started in.  Treat that window as sensitive and
        # drop the state instead of guessing which field wins.
        if (
            getattr(self, "_content_type_key", None) != decision_key
            and self._composition_in_flight()
        ):
            sensitive = True

        self._content_type_key = decision_key
        if sensitive == getattr(self, "_sensitive", False):
            return
        self._sensitive = sensitive
        # Entering AND leaving a sensitive field both clear the buffers:
        # nothing composed before the switch may survive it in either
        # direction.
        self._reset_for_sensitive_switch()

    def do_set_surrounding_text(
        self, text: object, cursor_pos: int, anchor_pos: int
    ) -> None:
        _ = anchor_pos
        if getattr(self, "_sensitive", False):
            # A sensitive field's contents must never reach the core, the
            # predictor or the trace.
            return
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
        # Sensitive field: hard stop BEFORE anything else — no trace event, no
        # surrounding-text read, no core call, and therefore no composing, no
        # transliteration, no prediction and no learning.  Returning False
        # hands the untouched key straight back to IBus for the client.
        if getattr(self, "_sensitive", False):
            return False

        log.debug("key: keyval=0x%04X keycode=%d state=0x%04X", keyval, keycode, state)

        trace = getattr(self, "_trace", None)
        tracing = trace is not None and trace.enabled
        if tracing:
            trace.begin_keystroke()
            # Privacy: a printable key IS the typed text.  At structural
            # level only its class + script class are logged; the raw
            # keyname/keyval/keycode (all reconstruct the keystream) are
            # reserved for special keys or level=full.
            keyname = self._keyname(keyval)
            is_printable = len(keyname) == 1
            fields: dict[str, object] = {
                "mods": state,
                "is_release": bool(state & _MOD_RELEASE),
                "cursor_pos": self._surrounding_cursor_pos,
                "surrounding_text": self._surrounding_text,
                "key_class": "char" if is_printable else "special",
                "key_lang": lang_class(keyname) if is_printable else "empty",
            }
            if not is_printable or trace.level >= LEVEL_FULL:
                fields.update(keycode=keycode, keyval=keyval, keyname=keyname)
            trace.emit("key_in", **fields)

        if self._is_spurious_zero_key_event(keyval, keycode, state):
            self._note_spurious_zero_key()
            if tracing:
                trace.emit("core", verdict="consume_spurious")
            return True

        key_release = bool(state & _MOD_RELEASE)  # IBUS_RELEASE_MASK
        if not key_release:
            # Fail-safe input for do_set_content_type: a content type that
            # changes after the user has started typing into this context is
            # treated as a mid-composition switch even when no preedit is
            # currently visible (the dual buffer can hold state without one).
            self._keys_since_content_type = (
                getattr(self, "_keys_since_content_type", 0) + 1
            )

        # Capture the word before a key that can terminate composition. The
        # post-core action list decides whether a commit actually happened.
        o1_pre_word = ""
        if getattr(self, "_o1", None) is not None and not key_release:
            is_digit_boundary = 0x30 <= keyval <= 0x39
            if (
                keyval in (IBus.KEY_space, IBus.KEY_Return)
                or self._is_punctuation_boundary(keyval)
                or is_digit_boundary
            ):
                o1_pre_word = self._core_current_word()
        pending_prediction = (
            dict(self._active_prediction)
            if self._active_prediction is not None
            else None
        )
        if pending_prediction is not None:
            pending_prediction["pre_key_word"] = self._core_current_word()
        if not key_release:
            self._refresh_surrounding_text()

        # v0.5.0: prefer raw scancode path for dual-buffer layout-agnostic input.
        # Rust tables use evdev codes. On Wayland IBus sends evdev directly;
        # on X11 IBus sends XKB (evdev + 8) — normalise to evdev.
        # Older installed native modules may not have the raw-scancode API.
        # Keep those sessions usable by falling back to the ordinary keyval
        # path instead of raising/consuming the browser's key event.
        has_keycode_api = callable(getattr(self._core, "process_keycode", None))
        if keycode > 0 and _HAS_CORE and has_keycode_api:
            evdev_keycode = keycode if _IS_WAYLAND else max(keycode - 8, 0)
            actions = self._core.process_keycode(evdev_keycode, state)
            log.debug("actions (keycode): %s", actions)
            result = self._execute_actions(actions)
            self._finalize_prediction_outcome(
                keyval, key_release, actions, pending_prediction
            )
            self._o1_notify(
                keyval, key_release, actions, pending_prediction, o1_pre_word
            )
            if tracing:
                self._trace_key_result(keyval, key_release, actions, result)
            # Honor the core's forward/consume decision uniformly.  ``result`` is
            # False exactly when the core emitted a ForwardKey (key not
            # consumed).  A composing-edit backspace already returns no
            # ForwardKey, so it never reaches the application; keying off
            # ``preedit_was_active`` instead swallowed a core-*forwarded*
            # backspace whenever an anticipatory next-word ghost was showing,
            # so committed text could not be deleted.
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
        actions = self._core.handle_key(keyval, state)
        log.debug("actions: %s", actions)
        result = self._execute_actions(actions)
        self._finalize_prediction_outcome(
            keyval, key_release, actions, pending_prediction
        )
        self._o1_notify(
            keyval, key_release, actions, pending_prediction, o1_pre_word
        )
        if tracing:
            self._trace_key_result(keyval, key_release, actions, result)
        # Honor the core's forward/consume decision uniformly (see the keycode
        # path above): return ``result`` rather than swallowing a
        # core-forwarded backspace on mere ``preedit_was_active``.
        return result

    # -----------------------------------------------------------------------
    # IBus lifecycle callbacks.
    # -----------------------------------------------------------------------
    def do_focus_in(self) -> None:
        self._core.focus_gained()
        if getattr(self, "_sensitive", False):
            # Never pull a sensitive field's contents into the core.
            return
        self._refresh_surrounding_text()

    def do_focus_out(self) -> None:
        # The sensitivity declaration deliberately SURVIVES focus-out, because a
        # sticky flag is the correct mirror of how IBus actually delivers
        # ContentType.
        #
        # `ibus_input_context_set_content_type` is deduplicated against the
        # proxy's cached property — verified by disassembling the installed
        # libibus-1.0.so.5, whose call sequence is
        # g_dbus_proxy_get_cached_property -> g_variant_new -> g_variant_equal
        # -> (only when unequal) g_dbus_proxy_call -> set_cached_property.  The
        # daemon does the same in bus/engineproxy.c.  So the engine is told the
        # content type only when it CHANGES relative to what this proxy was last
        # told.
        #
        # Clearing the flag here would desynchronise us from that cache and
        # re-open the exact hole this feature exists to close: leave a password
        # field, come back to the SAME field, the pushed (PASSWORD, 0) equals
        # the cache, the Set is suppressed, do_set_content_type never fires, and
        # the password flows into the dual buffer, the predictor,
        # learn()/learn_oov_word() and the trace.
        #
        # Stickiness does not strand correctly declaring clients either:
        # BusInputContext holds per-context purpose/hints defaulting to
        # FREE_FORM (0, 0) and pushes them at every focus-in, so a client that
        # publishes an ordinary field pushes a value that DIFFERS from
        # (PASSWORD, 0); do_set_content_type(0, 0) then clears sensitivity.
        #
        # Only the in-flight keystroke counter is a genuinely per-context value.
        self._keys_since_content_type = 0
        actions = self._core.focus_lost()
        self._execute_actions(actions)
        self._active_prediction = None
        collector = getattr(self, "_o1", None)
        if collector is not None:
            collector.on_abandon()
        self._sync_surrounding_text(None, None)
        # Idle point: flush any buffered trace events off the hot path.
        trace = getattr(self, "_trace", None)
        if trace is not None and trace.enabled:
            trace.flush()
        # Debounced auto-save: persist personal profile at most once per 60s.
        now = time.monotonic()
        if now - self._last_save >= 60.0:
            try:
                self._core.save_personal()
                self._last_save = now
            except OSError:
                log.warning("smartkey: failed to save personal profile", exc_info=True)

    @staticmethod
    def _cancel_safe(actions: list[tuple[str, str]]) -> list[tuple[str, str]]:
        """Strip text-producing actions from a CANCEL path.

        IBus ``Reset`` and ``Disable`` mean *discard the composition*. The
        preedit is rendered with ``PREEDIT_CLEAR``, so those characters were
        never in the application; a ``commit``/``replace`` here would turn the
        user's cancel into an insertion.

        The core is expected to return no such action on this path, and a
        regression test pins that. This filter is the second lock: the core and
        this adapter live in separate worktrees, and the defect this guards
        against was invisible to either one's test suite alone — it appeared
        only when both changes were replayed together. Neither side can
        reintroduce it on its own now.
        """
        dropped = [a for a in actions if a[0] in ("commit", "replace")]
        if dropped:
            log.warning(
                "smartkey: dropped %d text-producing action(s) on a cancel path; "
                "the core should not emit these from reset()",
                len(dropped),
            )
        return [a for a in actions if a[0] not in ("commit", "replace")]

    def do_reset(self) -> None:
        # Composition boundary, not a context boundary: the declared content
        # type survives (see do_focus_out).
        self._keys_since_content_type = 0
        actions = self._cancel_safe(self._core.reset())
        self._execute_actions(actions)
        self._active_prediction = None
        collector = getattr(self, "_o1", None)
        if collector is not None:
            collector.on_abandon()
        self._sync_surrounding_text(None, None)
        trace = getattr(self, "_trace", None)
        if trace is not None and trace.enabled:
            trace.flush()

    def do_enable(self) -> None:
        pass  # Rust core handles enabled state via kill switch.

    def do_disable(self) -> None:
        # Same asymmetry as do_reset: an IME switch inside a password box must
        # not drop that box's declaration — and, like do_reset, this is a cancel
        # path, so no text may be produced from it.
        self._keys_since_content_type = 0
        actions = self._cancel_safe(self._core.reset())
        self._execute_actions(actions)
        self._active_prediction = None
        collector = getattr(self, "_o1", None)
        if collector is not None:
            collector.on_abandon()
        self._sync_surrounding_text(None, None)
        trace = getattr(self, "_trace", None)
        if trace is not None and trace.enabled:
            trace.flush()
        # Save personal profile on engine disable (session end).
        try:
            self._core.save_personal()
        except OSError:
            log.warning("smartkey: failed to save personal profile", exc_info=True)
