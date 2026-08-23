"""Adapter-side guards for the opt-in Space-accept mode (Step 1, 2026-08-22).

The core owns the decision (see ``space_accept_tests`` in
``crates/smartkey-core/src/input.rs``).  The adapter needs NO new logic: when
the core emits ``[hide, commit("word ")]`` with no ``forward``, the existing
dispatcher commits the single payload and returns ``True`` (consumed), so IBus
never delivers the Space a second time.  These tests pin that mechanism and the
two adapter-level exclusions (sensitive field, default-off path) so a later
adapter change cannot silently reintroduce a double delimiter.

Safety: fake IBus + fake core, temp Phase-A dir — the live engine is never
touched.  Harness mirrors ``test_accept_backspace.py``.
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

_fake_gi = types.ModuleType("gi")


def _require_version(*_a, **_k):  # noqa: ANN002, ANN003
    raise ValueError("forced fake IBus for tests")


_fake_gi.require_version = _require_version  # type: ignore[attr-defined]
sys.modules["gi"] = _fake_gi
sys.modules.pop("gi.repository", None)

_ENGINE_PATH = pathlib.Path(__file__).resolve().parent / "smartkey_engine.py"
_spec = importlib.util.spec_from_file_location("smartkey_engine_space_accept", _ENGINE_PATH)
assert _spec and _spec.loader
ske = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(ske)
assert ske._HAS_IBUS is False


def _mk_text(s: str):
    t = types.SimpleNamespace(s=s)
    t.set_attributes = lambda *_a, **_k: None
    return t


class _AttrList:
    def append(self, *_a) -> None:  # noqa: ANN002
        return None


ske.IBus.Text.new_from_string = staticmethod(_mk_text)  # type: ignore[attr-defined]
ske.IBus.AttrList = _AttrList  # type: ignore[attr-defined]


class FakeCore:
    """Scripted core.  ``word`` mirrors what the real core would report as the
    in-flight word right before a boundary key (the adapter captures it for
    O1 and for rejection bookkeeping)."""

    def __init__(self, scripts: list[list[tuple[str, str]]]) -> None:
        self._scripts = list(scripts)
        self.word = ""
        self.rejections: list[tuple[str, str]] = []

    def process_keycode(self, _evdev: int, _state: int) -> list[tuple[str, str]]:
        return self._next()

    def handle_key(self, _keyval: int, _state: int) -> list[tuple[str, str]]:
        return self._next()

    def _next(self) -> list[tuple[str, str]]:
        actions = self._scripts.pop(0) if self._scripts else [("forward", "")]
        # Keep ``word`` in step with the script, like the real core would.
        for action_type, payload in actions:
            if action_type == "composing":
                self.word = payload.split("\x00", 1)[0]
            elif action_type in {"commit", "replace"}:
                self.word = ""
        return actions

    def predictions(self) -> list[tuple[str, float, float]]:
        return [("hello", 1.0, 0.9)]

    def current_word(self) -> str:
        return self.word

    def record_ghost_rejection(self, prefix: str, completion: str) -> None:
        self.rejections.append((prefix, completion))


class FakeO1:
    def __init__(self) -> None:
        self.words: list[str] = []

    def on_word_commit(self, word: str) -> None:
        self.words.append(word)

    def on_abandon(self) -> None:
        return None


def capture_replay(eng) -> list[tuple[str, dict]]:
    events: list[tuple[str, dict]] = []
    eng._log_replay_event = lambda event, **payload: events.append((event, payload))
    return events


class Recorder:
    def __init__(self) -> None:
        self.commits: list[str] = []
        self.preedits: list[tuple[str, bool]] = []
        self.forwarded_keys: list[tuple[int, int, int]] = []


def build_engine(scripts: list[list[tuple[str, str]]]):
    eng = ske.SmartKeyEngine.__new__(ske.SmartKeyEngine)
    eng._core = FakeCore(scripts)
    eng._caps = 0
    eng._preedit_active = False
    eng._preedit_mode = None
    eng._active_prediction = None
    eng._prediction_seq = 0
    eng._surrounding_text = None
    eng._surrounding_cursor_pos = None
    eng._trace = None
    eng._last_composing_typed = ""
    rec = Recorder()
    eng.commit_text = lambda text_obj: rec.commits.append(text_obj.s)
    eng.update_preedit_text = lambda text_obj, _cursor, visible: rec.preedits.append(
        (text_obj.s, visible)
    )
    eng.hide_preedit_text = lambda: None
    eng.forward_key_event = lambda kv, kc, st: rec.forwarded_keys.append((kv, kc, st))
    eng.delete_surrounding_text = lambda off, n: None
    return eng, rec


# --- Accounting (ADVOCATE_CODEX bd9219aa448c A): eligible Space = acceptance --


def test_space_accept_is_recorded_as_acceptance_not_rejection():
    eng, _rec = build_engine(
        scripts=[
            [("composing", "hel\x00lo")],
            [("hide", ""), ("commit", "hello ")],
        ]
    )
    events = capture_replay(eng)
    assert eng.do_process_key_event(ord("l"), 38, 0) is True
    assert eng._active_prediction is not None

    assert eng.do_process_key_event(ske.IBus.KEY_space, 57, 0) is True

    kinds = [event for event, _ in events]
    assert "accepted" in kinds, f"eligible Space must log an acceptance, got {events}"
    assert "rejected" not in kinds and "completed" not in kinds
    accepted = next(payload for event, payload in events if event == "accepted")
    assert accepted["reason"] == "space"
    assert accepted["word"] == "hello"
    assert eng._core.rejections == [], "acceptance must never feed rejection memory"
    assert eng._active_prediction is None, "the acceptance consumes the prediction"


def test_space_accept_feeds_o1_the_full_word_without_delimiter():
    eng, _rec = build_engine(
        scripts=[
            [("composing", "hel\x00lo")],
            [("hide", ""), ("commit", "hello ")],
        ]
    )
    eng._o1 = FakeO1()
    assert eng.do_process_key_event(ord("l"), 38, 0) is True

    assert eng.do_process_key_event(ske.IBus.KEY_space, 57, 0) is True

    assert eng._o1.words == ["hello"], f"O1 gets the accepted word, got {eng._o1.words}"


def test_literal_space_keeps_word_boundary_accounting():
    # Flag off / ineligible: today's bookkeeping must be untouched — the typed
    # word is what the app received, so O1 gets the prefix and the visible
    # completion is a genuine word-boundary rejection.
    eng, _rec = build_engine(
        scripts=[
            [("composing", "hel\x00lo")],
            [("hide", ""), ("commit", "hel"), ("forward", "")],
        ]
    )
    eng._o1 = FakeO1()
    events = capture_replay(eng)
    assert eng.do_process_key_event(ord("l"), 38, 0) is True

    assert eng.do_process_key_event(ske.IBus.KEY_space, 57, 0) is False

    assert "accepted" not in [event for event, _ in events]
    rejected = next(payload for event, payload in events if event == "rejected")
    assert rejected["reason"] == "word_boundary"
    assert eng._core.rejections == [("hel", "hello")]
    assert eng._o1.words == ["hel"]


def test_dismissed_ghost_then_space_is_literal_not_acceptance():
    # Escape dismissed the completion (composing re-shown without ghost); the
    # following Space is an ordinary boundary — no acceptance event.
    eng, rec = build_engine(
        scripts=[
            [("composing", "hel\x00lo")],
            [("composing", "hel\x00")],
            [("hide", ""), ("commit", "hel"), ("forward", "")],
        ]
    )
    eng._o1 = FakeO1()
    events = capture_replay(eng)
    assert eng.do_process_key_event(ord("l"), 38, 0) is True
    assert eng.do_process_key_event(ske.IBus.KEY_Escape, 1, 0) is True
    assert eng._active_prediction is None

    assert eng.do_process_key_event(ske.IBus.KEY_space, 57, 0) is False

    assert rec.commits == ["hel"]
    assert "accepted" not in [event for event, _ in events]
    assert eng._o1.words == ["hel"]


def test_space_accept_single_commit_with_delimiter_is_consumed():
    # Core (flag on, eligible ghost) emits the atomic one-press payload.
    eng, rec = build_engine(
        scripts=[
            [("composing", "hel\x00lo")],
            [("hide", ""), ("commit", "hello ")],
        ]
    )
    assert eng.do_process_key_event(ord("l"), 38, 0) is True

    consumed = eng.do_process_key_event(ske.IBus.KEY_space, 57, 0)

    assert rec.commits == ["hello "], f"one commit, one delimiter — got {rec.commits}"
    assert consumed is True, "Space must be consumed so IBus cannot deliver a second one"
    assert rec.forwarded_keys == []
    assert eng._preedit_active is False
    # The composing preedit was replaced by an empty invisible one before commit
    # (browser doubling guard), never merely hidden.
    assert rec.preedits[-1] == ("", False)


def test_space_accept_rapid_second_space_is_literal_at_adapter():
    # One-shot invariant at the integration boundary: the first Space lands the
    # single "hello " commit (consumed); the core has nothing left, so the
    # second Space is forwarded by IBus itself and commits nothing.
    eng, rec = build_engine(
        scripts=[
            [("composing", "hel\x00lo")],
            [("hide", ""), ("commit", "hello ")],
            [("hide", ""), ("forward", "")],
        ]
    )
    assert eng.do_process_key_event(ord("l"), 38, 0) is True
    assert eng.do_process_key_event(ske.IBus.KEY_space, 57, 0) is True

    consumed = eng.do_process_key_event(ske.IBus.KEY_space, 57, 0)

    assert consumed is False, "second Space must be literal (forwarded)"
    assert rec.commits == ["hello "], "exactly one delimiter came from SmartKey"
    assert rec.forwarded_keys == [], "no synthetic key events were injected"


def test_space_accept_default_off_path_is_unchanged():
    # Flag off (today's behaviour): core commits the typed word and forwards Space.
    eng, rec = build_engine(
        scripts=[
            [("composing", "hel\x00lo")],
            [("hide", ""), ("commit", "hel"), ("forward", "")],
        ]
    )
    assert eng.do_process_key_event(ord("l"), 38, 0) is True

    consumed = eng.do_process_key_event(ske.IBus.KEY_space, 57, 0)

    assert rec.commits == ["hel"]
    assert consumed is False


def test_space_in_sensitive_field_never_reaches_the_core():
    eng, rec = build_engine(scripts=[[("hide", ""), ("commit", "hello ")]])
    eng._sensitive = True
    eng._content_type_key = ("password", 0)

    consumed = eng.do_process_key_event(ske.IBus.KEY_space, 57, 0)

    assert consumed is False, "sensitive field: the key goes straight back to IBus"
    assert rec.commits == []
    assert eng._core._scripts, "the core must not have been consulted"
