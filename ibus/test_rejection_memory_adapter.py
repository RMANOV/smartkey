"""End-to-end adapter tests for session-scoped ghost rejection feedback."""

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

_fake_gi = types.ModuleType("gi")


def _require_version(*_args, **_kwargs):
    raise ValueError("forced fake IBus for tests")


_fake_gi.require_version = _require_version  # type: ignore[attr-defined]
sys.modules["gi"] = _fake_gi
sys.modules.pop("gi.repository", None)

_ENGINE_PATH = pathlib.Path(__file__).resolve().parent / "smartkey_engine.py"
_spec = importlib.util.spec_from_file_location(
    "smartkey_engine_rejection_test", _ENGINE_PATH
)
assert _spec and _spec.loader
ske = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(ske)

assert ske._HAS_IBUS is False


def _make_text(value: str):
    text = types.SimpleNamespace(s=value)
    text.set_attributes = lambda *_args, **_kwargs: None
    return text


class _AttrList:
    def append(self, *_args) -> None:
        return None


ske.IBus.Text.new_from_string = staticmethod(_make_text)  # type: ignore[attr-defined]
ske.IBus.AttrList = _AttrList  # type: ignore[attr-defined]


class FakeCore:
    def __init__(self) -> None:
        self._predictions = [("hello", 1.0, 0.9)]
        self._word = ""
        self._steps: list[tuple[list[tuple[str, str]], str]] = []
        self.recorded: list[tuple[str, str]] = []

    def queue(self, actions: list[tuple[str, str]], post_word: str) -> None:
        self._steps.append((actions, post_word))

    def _advance(self) -> list[tuple[str, str]]:
        if not self._steps:
            return [("forward", "")]
        actions, self._word = self._steps.pop(0)
        return actions

    def process_keycode(self, _keycode: int, _state: int):
        return self._advance()

    def handle_key(self, _keyval: int, _state: int):
        return self._advance()

    def predictions(self):
        return self._predictions

    def current_word(self) -> str:
        return self._word

    def record_ghost_rejection(self, prefix: str, completion: str) -> None:
        self.recorded.append((prefix, completion))


def build_engine():
    engine = ske.SmartKeyEngine.__new__(ske.SmartKeyEngine)
    core = FakeCore()
    engine._core = core
    engine._caps = 0
    engine._preedit_active = False
    engine._preedit_mode = None
    engine._active_prediction = None
    engine._prediction_seq = 0
    engine._surrounding_text = None
    engine._surrounding_cursor_pos = None
    engine._trace = None
    engine._last_composing_typed = ""
    engine._session_id = "test"
    engine.commit_text = lambda _text: None
    engine.update_preedit_text = lambda _text, _cursor, _visible: None
    engine.hide_preedit_text = lambda: None
    engine.forward_key_event = lambda _keyval, _keycode, _state: None
    engine.delete_surrounding_text = lambda _offset, _length: None
    return engine, core


def show_hello(engine, core, prefix: str = "hel", ghost: str = "lo") -> None:
    core.queue([("composing", f"{prefix}\x00{ghost}")], prefix)
    engine.do_process_key_event(ord(prefix[-1]), 38, 0)


def capture_events(engine):
    events: list[tuple[str, dict]] = []
    engine._log_replay_event = lambda event, **payload: events.append(
        (event, payload)
    )
    return events


def test_diverging_typing_records_prior_displayed_pair():
    engine, core = build_engine()
    show_hello(engine, core)

    core.queue([("forward", "")], "helx")
    engine.do_process_key_event(ord("x"), 45, 0)

    assert core.recorded == [("hel", "hello")]


def test_matching_manual_completion_is_not_a_rejection():
    engine, core = build_engine()
    events = capture_events(engine)
    show_hello(engine, core)

    core.queue([("composing", "hell\x00o")], "hell")
    engine.do_process_key_event(ord("l"), 38, 0)

    assert core.recorded == []
    outcomes = [(event, payload.get("reason")) for event, payload in events]
    assert ("completed", "completed_manually") in outcomes
    assert not [item for item in outcomes if item[0] == "rejected"]


def test_short_space_boundary_records_rejection():
    engine, core = build_engine()
    show_hello(engine, core)

    core.queue([("hide", ""), ("commit", "hel"), ("forward", "")], "")
    engine.do_process_key_event(ske.IBus.KEY_space, 57, 0)

    assert core.recorded == [("hel", "hello")]


def test_fully_typed_completion_at_boundary_is_not_rejected():
    engine, core = build_engine()
    show_hello(engine, core, "hello", " next")

    core.queue([("hide", ""), ("commit", "hello"), ("forward", "")], "")
    engine.do_process_key_event(ske.IBus.KEY_space, 57, 0)

    assert core.recorded == []


def test_exclamation_boundary_records_and_still_forwards():
    engine, core = build_engine()
    show_hello(engine, core)

    core.queue([("hide", ""), ("commit", "hel"), ("forward", "")], "")
    consumed = engine.do_process_key_event(ord("!"), 2, 1)

    assert consumed is False
    assert core.recorded == [("hel", "hello")]


def test_commit_and_forward_classifies_digit_as_boundary():
    engine, core = build_engine()
    show_hello(engine, core)

    core.queue([("hide", ""), ("commit", "hel"), ("forward", "")], "")
    engine.do_process_key_event(ord("1"), 2, 0)

    assert core.recorded == [("hel", "hello")]


def test_older_native_module_without_feedback_api_remains_compatible():
    engine, core = build_engine()
    show_hello(engine, core)
    core.record_ghost_rejection = None  # type: ignore[method-assign]

    core.queue([("forward", "")], "helx")
    engine.do_process_key_event(ord("x"), 45, 0)
