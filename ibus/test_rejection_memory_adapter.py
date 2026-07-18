"""Integration tests: session rejection memory + replace hardening.

These drive the REAL adapter path
(``do_process_key_event`` → ``_execute_actions`` → ``_finalize_prediction_outcome``)
with a contract-faithful fake core and the module's ``_FakeIBus`` fallback, so
the divergence guard, telemetry semantics, and the surrounding-text capability
gate are all exercised end-to-end — not by feeding the adapter hand-made calls.

Safety: no real IBus / display server and no ``smartkey_py`` native module are
instantiated, so nothing touches the live engine or Phase-A data.
"""

from __future__ import annotations

import importlib.util
import os
import pathlib
import sys
import tempfile
import types

os.environ.setdefault(
    "SMARTKEY_PHASEA_DATA", tempfile.mkdtemp(prefix="smartkey-test-phasea-")
)

# Force the module's _FakeIBus fallback (make gi.require_version raise).
_fake_gi = types.ModuleType("gi")


def _require_version(*_a, **_k):  # noqa: ANN002, ANN003
    raise ValueError("forced fake IBus for tests")


_fake_gi.require_version = _require_version  # type: ignore[attr-defined]
sys.modules["gi"] = _fake_gi
sys.modules.pop("gi.repository", None)

_ENGINE_PATH = pathlib.Path(__file__).resolve().parent / "smartkey_engine.py"
_spec = importlib.util.spec_from_file_location("smartkey_engine_rejtest", _ENGINE_PATH)
assert _spec and _spec.loader
ske = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(ske)

assert ske._HAS_IBUS is False, "test requires the _FakeIBus fallback path"


def _mk_text(s: str):
    t = types.SimpleNamespace(s=s)
    t.set_attributes = lambda *_a, **_k: None
    return t


class _AttrList:
    def append(self, *_a) -> None:  # noqa: ANN002
        return None


ske.IBus.Text.new_from_string = staticmethod(_mk_text)  # type: ignore[attr-defined]
ske.IBus.AttrList = _AttrList  # type: ignore[attr-defined]

_KEY_SPACE = ske.IBus.KEY_space


class FakeCore:
    """Scripted core with a live ``current_word`` and a rejection recorder.

    Each queued step is ``(actions, post_word)``: ``handle_key`` / ``process_keycode``
    advance ``current_word`` to ``post_word`` and return ``actions`` — mirroring the
    real core, whose ``current_word`` reflects state *after* the key is processed.
    """

    def __init__(self) -> None:
        self._preds = [("hello", 1.0, 0.9)]
        self._current_word = ""
        self._steps: list[tuple[list[tuple[str, str]], str]] = []
        self.recorded: list[tuple[str, str]] = []

    def queue(self, actions: list[tuple[str, str]], post_word: str) -> None:
        self._steps.append((actions, post_word))

    def _advance(self) -> list[tuple[str, str]]:
        if self._steps:
            actions, post_word = self._steps.pop(0)
            self._current_word = post_word
            return actions
        return [("forward", "")]

    def process_keycode(self, _evdev: int, _state: int) -> list[tuple[str, str]]:
        return self._advance()

    def handle_key(self, _keyval: int, _state: int) -> list[tuple[str, str]]:
        return self._advance()

    def predictions(self) -> list[tuple[str, float, float]]:
        return self._preds

    def current_word(self) -> str:
        return self._current_word

    def record_ghost_rejection(self, prefix: str, completion: str) -> None:
        self.recorded.append((prefix, completion))


def build_engine(caps: int = 0):
    """Instantiate the real adapter class, bypassing IBus/GObject __init__."""
    eng = ske.SmartKeyEngine.__new__(ske.SmartKeyEngine)
    core = FakeCore()
    eng._core = core
    eng._caps = caps
    eng._preedit_active = False
    eng._preedit_mode = None
    eng._active_prediction = None
    eng._prediction_seq = 0
    eng._surrounding_text = None
    eng._surrounding_cursor_pos = None
    eng._trace = None
    eng._last_composing_typed = ""
    eng._session_id = "test"

    eng._commits = []
    eng._deleted = []
    eng.commit_text = lambda t: eng._commits.append(t.s)
    eng.update_preedit_text = lambda t, c, v: None
    eng.hide_preedit_text = lambda: None
    eng.forward_key_event = lambda kv, kc, st: None
    eng.delete_surrounding_text = lambda off, n: eng._deleted.append((off, n))
    return eng, core


def _capture_replay(eng) -> list[tuple[str, dict]]:
    events: list[tuple[str, dict]] = []
    eng._log_replay_event = lambda event, **payload: events.append((event, payload))
    return events


def _show_hello_ghost(eng, core, prefix: str, ghost: str) -> None:
    """Drive one keystroke that shows the "hello" completion as a ghost.

    Leaves ``current_word == prefix`` and ``_active_prediction`` primed with
    word="hello", prefix=<prefix>.
    """
    core.queue([("composing", f"{prefix}\x00{ghost}")], prefix)
    eng.do_process_key_event(ord(prefix[-1]), 38, 0)


# --- (a) divergence: typing through/away from the ghost IS a rejection --------
def test_diverging_typethrough_records_rejection():
    eng, core = build_engine()
    _show_hello_ghost(eng, core, "hel", "lo")  # ghost "lo" for "hello" at "hel"

    # Type a diverging 'x' → current_word "helx" is no longer a prefix of "hello".
    core.queue([("forward", "")], "helx")
    eng.do_process_key_event(ord("x"), 45, 0)

    assert core.recorded == [("hel", "hello")], f"expected a record, got {core.recorded}"


# --- (a) manual completion: typing the completion out is NOT a rejection ------
def test_manual_completion_not_recorded_and_logged_completed():
    eng, core = build_engine()
    events = _capture_replay(eng)
    _show_hello_ghost(eng, core, "hel", "lo")

    # Type 'l' matching the ghost → current_word "hell" is still a prefix of "hello".
    core.queue([("composing", "hell\x00o")], "hell")
    eng.do_process_key_event(ord("l"), 38, 0)

    assert core.recorded == [], f"manual completion must NOT be recorded, got {core.recorded}"
    # Telemetry: logged as completed_manually, never as a rejection (GFEAT #5).
    reasons = [(e, p.get("reason")) for e, p in events]
    assert ("completed", "completed_manually") in reasons, reasons
    assert not [e for e, _ in events if e == "rejected"], reasons


# --- (a) short boundary: partial word + Space IS a rejection ------------------
def test_short_word_boundary_space_records():
    eng, core = build_engine()
    _show_hello_ghost(eng, core, "hel", "lo")

    # Space commits the typed "hel" (shorter than "hello") → boundary rejection.
    core.queue([("hide", ""), ("commit", "hel"), ("forward", "")], "")
    eng.do_process_key_event(_KEY_SPACE, 57, 0)

    assert core.recorded == [("hel", "hello")], f"expected a record, got {core.recorded}"


# --- (a) full boundary: fully-typed completion + Space is NOT a rejection -----
def test_full_completion_boundary_not_recorded():
    eng, core = build_engine()
    # Multi-word ghost keeps the prediction pending even though "hello" is fully
    # typed (prefix == completion). Committing it at a boundary is acceptance.
    core.queue([("composing", "hello\x00 world")], "hello")
    eng.do_process_key_event(ord("o"), 24, 0)

    core.queue([("hide", ""), ("commit", "hello"), ("forward", "")], "")
    eng.do_process_key_event(_KEY_SPACE, 57, 0)

    assert core.recorded == [], f"full completion must NOT be recorded, got {core.recorded}"


# --- (ii) punctuation word-boundary: partial word + '.' IS a rejection --------
def test_punctuation_boundary_records():
    eng, core = build_engine()
    _show_hello_ghost(eng, core, "hel", "lo")

    # '.' delimits the word (commits "hel"), rejecting the pending "hello".
    core.queue([("hide", ""), ("commit", "hel"), ("forward", "")], "")
    eng.do_process_key_event(ord("."), 52, 0)

    assert core.recorded == [("hel", "hello")], f"expected a record, got {core.recorded}"


# --- replace hardening: apply only when surrounding-text is supported ---------
def test_replace_applied_when_surrounding_text_supported():
    eng, core = build_engine(caps=ske._CAP_SURROUNDING_TEXT)
    core.queue([("replace", "4\x1fспри")], "")
    eng.do_process_key_event(ord("."), 52, 0)

    assert eng._deleted == [(-4, 4)], f"expected a delete, got {eng._deleted}"
    assert eng._commits == ["спри"], f"expected the replacement commit, got {eng._commits}"


def test_replace_skipped_when_surrounding_text_unsupported():
    eng, core = build_engine(caps=0)
    core.queue([("replace", "4\x1fспри")], "")
    eng.do_process_key_event(ord("."), 52, 0)

    assert eng._deleted == [], f"delete must be skipped, got {eng._deleted}"
    assert eng._commits == [], f"raw text must stand (no commit), got {eng._commits}"
