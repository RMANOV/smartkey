"""Input-safety tests for the IBus adapter: sensitive fields + phantom keys.

The adapter-policy tests drive the REAL routing seam — ``do_process_key_event``
→ ``_execute_actions`` — through a recording fake core, following
``test_accept_backspace.py``. The B1 regression additionally builds and drives
the current PyO3 core through that adapter, because the cancel defect existed
only when the two worktrees were composed. A test that pokes predicates directly
would prove nothing about bugs in the routing around the core call:

FIX 1 — no ``do_set_content_type`` existed, so a password box was treated like
prose: composed through the dual buffer (which can commit the Cyrillic
hypothesis for a Latin keystroke), predicted on, learned into ``personal.json``
and the OOV trie, and traced at ``LEVEL=full``.  Password input may have been
captured where the client delivered it through IBus.  The tests assert the
adapter now returns False *before any core call*, emits zero trace events, and
fails SAFE on an unset / unrecognised / mid-composition content type.

The declaration is STICKY and deliberately survives ``do_focus_out``, because
IBus only delivers ContentType when it *changes*: ``set_content_type`` is
deduplicated against the proxy's cached property, so returning to the same
password field never re-declares.  Clearing on focus-out would leave the engine
live inside that field. What un-sticks it is a correctly published ordinary
declaration — its purpose differs from the cached secret one. A client that
reuses a context without updating ContentType stays safely inert. The
mid-composition latch is released as soon as the core resolves the word, so it
cannot turn "this field is secret" into "SmartKey is off everywhere".

FIX 2 — the phantom-key filter compared the modifier mask for equality
(``state in (16, 272)``), so every other combination leaked a 25 Hz storm into
the application.  The tests assert all six modifier masks seen in the recorded
traces are consumed and that real keys carrying the same masks are not.

Safety: the module's built-in ``_FakeIBus`` fallback is always used, so no
display server or live IBus engine is touched. Most tests use a fake core; the
single native seam test instantiates an isolated current-checkout core with
temporary Phase-A/debug directories and never loads or saves the personal
profile.
"""

from __future__ import annotations

import importlib.util
import json
import logging
import os
import pathlib
import subprocess
import sys
import tempfile
import types

import pytest

# Belt-and-suspenders: never let any transitive code touch live data.  Forcing
# the debug level OFF before the import matters — at ``SMARTKEY_DEBUG=full`` the
# engine module opens the operator's real predictions.log/replay.jsonl at import
# time, and these tests must never append to them.
os.environ["SMARTKEY_DEBUG"] = "0"
os.environ.setdefault(
    "SMARTKEY_PHASEA_DATA", tempfile.mkdtemp(prefix="smartkey-test-phasea-")
)
os.environ.setdefault(
    "SMARTKEY_DEBUG_DIR", tempfile.mkdtemp(prefix="smartkey-test-debug-")
)

# Force the module's ``_FakeIBus`` fallback: make ``gi.require_version`` raise so
# the ``except (ValueError, ImportError)`` branch selects the stub IBus.
_fake_gi = types.ModuleType("gi")


def _require_version(*_a, **_k):  # noqa: ANN002, ANN003
    raise ValueError("forced fake IBus for tests")


_fake_gi.require_version = _require_version  # type: ignore[attr-defined]
sys.modules["gi"] = _fake_gi
sys.modules.pop("gi.repository", None)

_ENGINE_PATH = pathlib.Path(__file__).resolve().parent / "smartkey_engine.py"
_spec = importlib.util.spec_from_file_location("smartkey_engine_input_safety", _ENGINE_PATH)
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

# --- Content-type vocabulary, resolved from the module under test. -----------
PURPOSE_FREE_FORM = 0
PURPOSE_ALPHA = 1
PURPOSE_EMAIL = 6
PURPOSE_PASSWORD = ske._PURPOSE_PASSWORD
PURPOSE_PIN = ske._PURPOSE_PIN
PURPOSE_TERMINAL = 10
PURPOSE_DATE = 11
PURPOSE_DATETIME = 13
# One past the highest purpose the installed IBus defines — stands in for
# "a member added by a future IBus this adapter has never audited".
PURPOSE_FUTURE = 14
HINT_NONE = 0
HINT_SPELLCHECK = 1
HINT_PRIVATE = ske._HINT_PRIVATE
HINT_HIDDEN_TEXT = ske._HINT_HIDDEN_TEXT

# --- Modifier vocabulary for the phantom-key suite. --------------------------
MOD_SHIFT = 1
MOD_LOCK = 2  # CapsLock
MOD_CONTROL = ske._MOD_CONTROL
MOD_ALT = ske._MOD_ALT
MOD_NUMLOCK = 16
MOD_RELEASE = ske._MOD_RELEASE
PHANTOM_KEYCODE = ske._SPURIOUS_KEYCODE

# Every ``keycode=240`` modifier mask seen in the recorded traces.  The first
# two were the only ones the old equality accepted; the last four are the 63
# leaked events (CapsLock, CapsLock+Shift, a held mouse button, Super).
OBSERVED_PHANTOM_MASKS = (16, 272, 18, 19, 1040, 67108947)


class SpyCore:
    """Scripted core that records every call the adapter makes.

    ``calls`` is the load-bearing assertion surface for FIX 1: "no prediction
    and no learning" is only provable at this seam by showing the core — the
    sole route to the dual buffer, the predictor, the trie and personal.json —
    was never entered at all.
    """

    def __init__(self, scripts: list[list[tuple[str, str]]] | None = None) -> None:
        self._scripts = list(scripts or [])
        self.calls: list[tuple[str, tuple]] = []
        self._preds = [("hello", 1.0, 0.9)]

    def _next(self) -> list[tuple[str, str]]:
        return self._scripts.pop(0) if self._scripts else [("forward", "")]

    def process_keycode(self, evdev: int, state: int) -> list[tuple[str, str]]:
        self.calls.append(("process_keycode", (evdev, state)))
        return self._next()

    def handle_key(self, keyval: int, state: int) -> list[tuple[str, str]]:
        self.calls.append(("handle_key", (keyval, state)))
        return self._next()

    def set_surrounding_text(self, text, cursor_pos) -> None:  # noqa: ANN001
        self.calls.append(("set_surrounding_text", (text, cursor_pos)))

    def reset(self) -> list[tuple[str, str]]:
        self.calls.append(("reset", ()))
        return []

    def focus_lost(self) -> list[tuple[str, str]]:
        self.calls.append(("focus_lost", ()))
        return []

    def focus_gained(self) -> None:
        self.calls.append(("focus_gained", ()))

    def save_personal(self) -> None:
        self.calls.append(("save_personal", ()))

    def predictions(self) -> list[tuple[str, float, float]]:
        self.calls.append(("predictions", ()))
        return self._preds

    # -- assertion helpers --------------------------------------------------
    def names(self) -> list[str]:
        return [name for name, _ in self.calls]

    def key_calls(self) -> list[tuple[str, tuple]]:
        return [c for c in self.calls if c[0] in ("process_keycode", "handle_key")]


class Recorder:
    def __init__(self) -> None:
        self.commits: list[str] = []
        self.preedits: list[tuple[str, bool]] = []
        self.hide_preedit_calls = 0
        self.forwarded_keys: list[tuple[int, int, int]] = []
        self.deleted: list[tuple[int, int]] = []


class FakeTrace:
    """Stand-in for KeystrokeTrace; records what would have hit the JSONL."""

    def __init__(self, level: int = 2) -> None:
        self.enabled = True
        self.level = level
        self.events: list[tuple[str, dict]] = []
        self.begins = 0
        self.flushes = 0

    def begin_keystroke(self) -> int:
        self.begins += 1
        return self.begins

    def emit(self, event: str, **fields) -> None:  # noqa: ANN003
        self.events.append((event, fields))

    def flush(self) -> None:
        self.flushes += 1


def build_engine(
    scripts: list[list[tuple[str, str]]] | None = None, sensitive: bool = False
):
    """Instantiate the real adapter class, bypassing IBus/GObject __init__."""
    eng = ske.SmartKeyEngine.__new__(ske.SmartKeyEngine)
    eng._core = SpyCore(scripts)
    eng._caps = 0
    eng._preedit_active = False
    eng._preedit_mode = None
    eng._active_prediction = None
    eng._prediction_seq = 0
    eng._surrounding_text = None
    eng._surrounding_cursor_pos = None
    eng._trace = None
    eng._last_composing_typed = ""
    # ``do_focus_out``/``do_disable`` debounce the personal-profile save off
    # this; the fake core absorbs the call, nothing reaches disk.
    eng._last_save = 0.0
    # State introduced by the sensitive-input / phantom-key fixes.
    eng._sensitive = sensitive
    eng._content_type_key = None
    eng._keys_since_content_type = 0
    eng._spurious_zero_key_count = 0
    eng._spurious_zero_key_logged = 0.0

    rec = Recorder()
    eng.commit_text = lambda text_obj: rec.commits.append(text_obj.s)
    eng.update_preedit_text = lambda text_obj, _cursor, visible: rec.preedits.append(
        (text_obj.s, visible)
    )

    def _hide() -> None:
        rec.hide_preedit_calls += 1

    eng.hide_preedit_text = _hide
    eng.forward_key_event = lambda kv, kc, st: rec.forwarded_keys.append((kv, kc, st))
    eng.delete_surrounding_text = lambda off, n: rec.deleted.append((off, n))
    return eng, rec


def _type(eng, text: str) -> list[bool]:
    """Feed printable keys through the real key-event entry point."""
    return [eng.do_process_key_event(ord(ch), 38, 0) for ch in text]


# ===========================================================================
# FIX 1 — sensitive fields must be inert.
# ===========================================================================
@pytest.mark.parametrize(
    "purpose,hints",
    [
        (PURPOSE_PASSWORD, HINT_NONE),
        (PURPOSE_PIN, HINT_NONE),
        # A client may flag a secret through the hints alone: PRIVATE means
        # "do not store this text", HIDDEN_TEXT means the client renders it
        # hidden — i.e. a password box that never says "password".
        (PURPOSE_FREE_FORM, HINT_PRIVATE),
        (PURPOSE_ALPHA, HINT_PRIVATE),
        (PURPOSE_FREE_FORM, HINT_HIDDEN_TEXT),
        (PURPOSE_FREE_FORM, HINT_PRIVATE | HINT_HIDDEN_TEXT | HINT_SPELLCHECK),
    ],
)
def test_sensitive_content_type_forwards_keys_without_touching_the_core(purpose, hints):
    # Scripted actions are queued but must never be consumed: reaching the core
    # at all would mean composing, transliteration, prediction and learning.
    eng, rec = build_engine(scripts=[[("composing", "hunter\x002")]])
    eng.do_set_content_type(purpose, hints)
    assert eng._sensitive is True

    results = _type(eng, "hunter2")

    assert results == [False] * 7, "every key must be forwarded to the client untouched"
    assert eng._core.key_calls() == [], "the core must never see a sensitive keystroke"
    assert rec.commits == [], "nothing may be committed in a sensitive field"
    assert rec.preedits == [], "nothing may be composed in a sensitive field"
    assert rec.forwarded_keys == []


def test_sensitive_field_keystrokes_emit_zero_trace_events():
    eng, _rec = build_engine()
    trace = FakeTrace(level=ske.LEVEL_FULL)
    eng._trace = trace
    eng.do_set_content_type(PURPOSE_PASSWORD, HINT_NONE)
    trace.events.clear()

    _type(eng, "hunter2")

    assert trace.begins == 0, "no keystroke may even be opened in the trace"
    assert trace.events == [], f"the trace must stay empty, got {trace.events}"


@pytest.mark.parametrize(
    ("purpose", "hints", "classification", "effective_sensitive"),
    (
        (PURPOSE_FREE_FORM, HINT_NONE, "ordinary", False),
        (PURPOSE_PASSWORD, HINT_NONE, "sensitive", True),
        (PURPOSE_FREE_FORM, HINT_PRIVATE, "sensitive", True),
        ("private-content-sentinel", HINT_NONE, "unknown", True),
        (PURPOSE_FREE_FORM, None, "unknown", True),
        (PURPOSE_FREE_FORM, "private-hint-sentinel", "unknown", True),
        (PURPOSE_FREE_FORM, 1 << 30, "unknown", True),
    ),
)
def test_content_type_observation_is_structural_even_at_full(
    tmp_path, purpose, hints, classification, effective_sensitive
):
    eng, _rec = build_engine()
    trace = ske.KeystrokeTrace(level=ske.LEVEL_FULL, directory=tmp_path)
    eng._trace = trace

    eng.do_set_content_type(purpose, hints)
    trace.flush()

    assert trace._file is not None
    raw = trace._file.read_text(encoding="utf-8")
    lines = raw.splitlines()
    assert len(lines) == 1
    event = json.loads(lines[0])
    assert event == {
        "ts": event["ts"],
        "seq": 0,
        "event": "content_type",
        "classification": classification,
        "effective_sensitive": effective_sensitive,
        "decision_changed": True,
        "state_changed": effective_sensitive,
    }
    assert "private-content-sentinel" not in raw
    assert "private-hint-sentinel" not in raw
    assert trace._seq == 0


def test_content_type_values_are_coerced_once_and_enforcement_matches_trace():
    class _ChangingHint:
        def __init__(self):
            self.calls = 0

        def __int__(self):
            self.calls += 1
            return HINT_NONE if self.calls == 1 else HINT_PRIVATE

    hint = _ChangingHint()
    eng, _rec = build_engine()
    trace = FakeTrace(level=1)
    eng._trace = trace

    eng.do_set_content_type(PURPOSE_FREE_FORM, hint)

    assert hint.calls == 1
    assert eng._sensitive is False
    assert trace.events[-1][1]["classification"] == "ordinary"
    assert trace.events[-1][1]["effective_sensitive"] is False


@pytest.mark.parametrize("hints", (None, "malformed-hint", 1 << 30))
def test_unknown_hint_values_fail_sensitive(hints):
    sensitive, _key = ske.SmartKeyEngine._classify_content_type(
        PURPOSE_FREE_FORM, hints
    )
    assert sensitive is True


@pytest.mark.parametrize("action_type", ("replace", "composing"))
def test_malformed_action_warning_omits_payload_content(caplog, action_type):
    sentinel = "typed-private-payload-sentinel"
    eng, _rec = build_engine()
    with caplog.at_level(logging.WARNING, logger="smartkey"):
        eng._execute_actions([(action_type, sentinel)])

    messages = [record.getMessage() for record in caplog.records]
    assert any(f"malformed {action_type} payload" in message for message in messages)
    assert all(sentinel not in message for message in messages)


def test_content_log_open_repairs_private_directory_and_file_modes(tmp_path):
    log_dir = tmp_path / "smartkey"
    log_dir.mkdir(mode=0o755)
    log_path = log_dir / "smartkey.log"
    log_path.write_text("legacy\n", encoding="utf-8")
    log_path.chmod(0o644)

    handle = ske._open_private_text_log(log_path)
    try:
        handle.write("new\n")
        handle.flush()
    finally:
        handle.close()

    assert log_dir.stat().st_mode & 0o777 == 0o700
    assert log_path.stat().st_mode & 0o777 == 0o600


def test_content_type_observation_names_transition_guard_without_key_count():
    eng, _rec = build_engine()
    trace = FakeTrace(level=1)
    eng._trace = trace
    eng.do_set_content_type(PURPOSE_FREE_FORM, HINT_NONE)
    trace.events.clear()
    eng._keys_since_content_type = 1

    eng.do_set_content_type(PURPOSE_EMAIL, HINT_NONE)

    assert trace.events == [
        (
            "content_type",
            {
                "seq": 0,
                "classification": "transition-guard",
                "effective_sensitive": True,
                "decision_changed": True,
                "state_changed": True,
            },
        )
    ]


def test_undeclared_sensitive_bypass_logs_once_without_key_content(caplog):
    """The strict startup blind spot must be visible without logging a key."""
    eng, _rec = build_engine(sensitive=True)
    with caplog.at_level(logging.WARNING, logger="smartkey"):
        _type(eng, "private")

    messages = [record.getMessage() for record in caplog.records]
    matching = [message for message in messages if "undeclared content type" in message]
    assert len(matching) == 1, "one structural warning, not one log per key"
    assert all("private" not in message for message in messages)


def test_mid_composition_sensitive_escalation_logs_without_text(caplog):
    prefix = "sensitiveprefix"
    suffix = "secretsuffix"
    eng, _rec = build_engine(scripts=[[('composing', f'{prefix}\x00{suffix}')]])
    eng.do_set_content_type(PURPOSE_FREE_FORM, HINT_NONE)
    assert eng.do_process_key_event(ord("l"), 38, 0) is True

    with caplog.at_level(logging.WARNING, logger="smartkey"):
        eng.do_set_content_type(PURPOSE_EMAIL, HINT_NONE)

    messages = [record.getMessage() for record in caplog.records]
    assert any("in-flight composition" in message for message in messages)
    assert all(prefix not in message and suffix not in message for message in messages)


def test_sensitive_field_blocks_prediction_and_learning_entirely():
    # ``predictions()`` is the predictor seam and the core is the only route to
    # engine.learn()/learn_oov_word(); neither may be reached.
    eng, _rec = build_engine(scripts=[[("ghost", "2")]])
    eng.do_set_content_type(PURPOSE_PASSWORD, HINT_NONE)
    eng._core.calls.clear()

    _type(eng, "hunter2")

    assert eng._core.calls == [], f"the core was entered: {eng._core.calls}"


def test_entering_a_sensitive_field_resets_the_core_and_clears_both_buffers():
    eng, rec = build_engine(scripts=[[("composing", "hel\x00lo")]])
    eng.do_set_content_type(PURPOSE_FREE_FORM, HINT_NONE)
    assert eng.do_process_key_event(ord("l"), 38, 0) is True
    assert eng._preedit_active is True and eng._preedit_mode == "composing"
    eng._core.calls.clear()

    eng.do_set_content_type(PURPOSE_PASSWORD, HINT_NONE)

    assert eng._sensitive is True
    assert "reset" in eng._core.names(), "the dual buffer must be cleared"
    assert eng._preedit_active is False and eng._preedit_mode is None
    assert eng._last_composing_typed == ""
    assert eng._active_prediction is None
    assert rec.hide_preedit_calls >= 1, "the stale preedit must be taken down"
    # The pending word is DROPPED, never replayed: the dual buffer holds the
    # engine's interpretation, which can be the transliterated hypothesis.
    assert rec.commits == []


def test_leaving_a_sensitive_field_resets_the_core_and_restores_normal_input():
    eng, _rec = build_engine(scripts=[[("composing", "hel\x00lo")]])
    eng.do_set_content_type(PURPOSE_PASSWORD, HINT_NONE)
    assert eng.do_process_key_event(ord("s"), 39, 0) is False
    eng._core.calls.clear()

    eng.do_set_content_type(PURPOSE_FREE_FORM, HINT_NONE)

    assert eng._sensitive is False
    assert "reset" in eng._core.names(), "no residue may survive the way OUT either"
    # Normal input works again — the kill switch must not be a one-way trip.
    assert eng.do_process_key_event(ord("l"), 38, 0) is True
    assert eng._preedit_active is True


# --- Fail-safe: ambiguity must read as sensitive, never as ordinary text -----
@pytest.mark.parametrize(
    "purpose,label",
    [
        (None, "unset purpose"),
        (PURPOSE_FUTURE, "purpose added by a newer IBus, never audited here"),
        (999, "purpose far outside the enum"),
        (-1, "negative purpose"),
        ("password", "non-numeric purpose"),
        (object(), "opaque object"),
        (True, "bool masquerading as purpose 1"),
        (False, "bool masquerading as purpose 0"),
    ],
)
def test_unknown_or_unset_purpose_fails_safe(purpose, label):
    eng, _rec = build_engine(scripts=[[("composing", "hel\x00lo")]])

    eng.do_set_content_type(purpose, HINT_NONE)

    assert eng._sensitive is True, f"{label} must fail safe, not fall through"
    assert eng.do_process_key_event(ord("l"), 38, 0) is False
    assert eng._core.key_calls() == []


def test_unusable_hints_fail_safe_even_with_a_known_purpose():
    # IBus represents a real "no hints" declaration as numeric HINT_NONE.
    # ``None`` is malformed callback input, not an ordinary declaration.
    eng, _rec = build_engine(scripts=[[("composing", "hel\x00lo")]])
    eng.do_set_content_type(PURPOSE_FREE_FORM, None)
    assert eng._sensitive is True
    assert eng.do_process_key_event(ord("l"), 38, 0) is False
    assert eng._core.key_calls() == []


def test_late_first_ordinary_declaration_keeps_the_current_field_usable():
    """A first declaration describes this field; it is not a field switch.

    Some clients deliver their initial ordinary ContentType after the first
    key event.  With undeclared input enabled, that key can already be in the
    composing preedit.  Treating ``None -> ordinary`` as a cross-field
    transition latches sensitive mode, and IBus may never repeat the identical
    declaration — every later key is then passed through untouched.
    """
    eng, rec = build_engine(
        scripts=[
            [("composing", "s\x00")],
            [("composing", "se\x00")],
        ]
    )
    assert eng._content_type_key is None
    assert eng.do_process_key_event(ord("s"), 31, 0) is True
    assert eng._preedit_active is True

    eng.do_set_content_type(PURPOSE_FREE_FORM, HINT_NONE)

    assert eng._sensitive is False
    assert eng._preedit_active is True
    assert rec.commits == []
    assert eng.do_process_key_event(ord("e"), 18, 0) is True
    assert len(eng._core.key_calls()) == 2


def test_late_first_sensitive_declaration_still_cancels_the_composition():
    eng, rec = build_engine(scripts=[[("composing", "h\x00")]])
    assert eng.do_process_key_event(ord("h"), 35, 0) is True
    assert eng._preedit_active is True

    eng.do_set_content_type(PURPOSE_PASSWORD, HINT_NONE)

    assert eng._sensitive is True
    assert eng._preedit_active is False
    assert rec.commits == []
    assert eng.do_process_key_event(ord("x"), 45, 0) is False
    assert len(eng._core.key_calls()) == 1


def test_content_type_change_mid_composition_fails_safe():
    # A live preedit plus a content-type switch = the composition may already
    # belong to a different field than the one it started in.
    eng, rec = build_engine(scripts=[[("composing", "hel\x00lo")]])
    eng.do_set_content_type(PURPOSE_FREE_FORM, HINT_NONE)
    assert eng.do_process_key_event(ord("l"), 38, 0) is True
    assert eng._preedit_active is True

    eng.do_set_content_type(PURPOSE_EMAIL, HINT_NONE)  # ordinary purpose!

    assert eng._sensitive is True, "a mid-composition switch must read as sensitive"
    assert eng._preedit_active is False
    assert rec.commits == []
    assert eng.do_process_key_event(ord("o"), 24, 0) is False


def test_mid_composition_fail_safe_recovers_on_the_next_clean_declaration():
    # Bounded blast radius: the ambiguity latch costs one word, not the field.
    # Escalation is gated on "the content type changed while something was in
    # flight"; de-escalation is not, so the client's next notification — even
    # an identical re-send, which GTK issues on focus — clears it.
    eng, _rec = build_engine(
        scripts=[[("composing", "hel\x00lo")], [("composing", "hell\x00o")]]
    )
    eng.do_set_content_type(PURPOSE_FREE_FORM, HINT_NONE)
    assert eng.do_process_key_event(ord("l"), 38, 0) is True
    eng.do_set_content_type(PURPOSE_EMAIL, HINT_NONE)
    assert eng._sensitive is True

    eng.do_set_content_type(PURPOSE_EMAIL, HINT_NONE)  # now unambiguous

    assert eng._sensitive is False
    assert eng.do_process_key_event(ord("l"), 38, 0) is True


def test_content_type_change_mid_typing_fails_safe_without_a_visible_preedit():
    # The Rust dual buffer can hold state with no preedit on screen, so the
    # "did the user already type here" signal cannot rely on the preedit alone.
    eng, _rec = build_engine(scripts=[[("forward", "")]])
    eng.do_set_content_type(PURPOSE_FREE_FORM, HINT_NONE)
    assert eng.do_process_key_event(ord("l"), 38, 0) is False
    assert eng._preedit_active is False
    assert eng._keys_since_content_type == 1

    eng.do_set_content_type(PURPOSE_EMAIL, HINT_NONE)

    assert eng._sensitive is True


def test_repeated_or_irrelevant_content_type_updates_are_not_a_field_switch():
    # GTK re-sends the content type routinely; treating a no-op re-send as a
    # field switch would kill the engine mid-word on every keystroke burst.
    eng, _rec = build_engine(scripts=[[("composing", "hel\x00lo")]])
    eng.do_set_content_type(PURPOSE_FREE_FORM, HINT_NONE)
    assert eng.do_process_key_event(ord("l"), 38, 0) is True

    eng.do_set_content_type(PURPOSE_FREE_FORM, HINT_NONE)  # identical re-send
    assert eng._sensitive is False
    eng.do_set_content_type(PURPOSE_FREE_FORM, HINT_SPELLCHECK)  # PRIVATE unchanged
    assert eng._sensitive is False
    assert eng._preedit_active is True, "the composition must survive a no-op update"


def test_a_committed_word_stops_arming_the_mid_composition_fail_safe():
    # The counter is a proxy for "a word is still open in this context", not
    # for "a key was pressed here at some point".  While it was zeroed only at
    # focus/reset/disable it stayed truthy for the rest of the field, so every
    # later content-type change escalated to sensitive: in a client with one IM
    # context per window that re-declares purpose without a focus cycle, moving
    # from a search box to an email box silently killed SmartKey for the whole
    # field.
    eng, _rec = build_engine(
        scripts=[
            [("hide", ""), ("commit", "hello"), ("forward", "")],
            [("composing", "hel\x00lo")],
        ]
    )
    eng.do_set_content_type(PURPOSE_FREE_FORM, HINT_NONE)
    eng.do_process_key_event(ord(" "), 65, 0)  # word boundary: the core commits
    assert eng._preedit_active is False
    assert eng._keys_since_content_type == 0
    assert eng._composition_in_flight() is False, (
        "nothing is live: the word committed and the preedit is gone"
    )

    eng.do_set_content_type(PURPOSE_EMAIL, HINT_NONE)  # search box → email box

    assert eng._sensitive is False, "a settled word must not escalate a field switch"
    assert eng.do_process_key_event(ord("l"), 38, 0) is True


def test_the_composition_counter_measures_keys_since_the_last_resolution():
    # Pins the semantics both ways: a core-reported resolution clears it, and a
    # key the core does NOT resolve re-arms it.  The second half is deliberate —
    # such a key IS typing into this context after the declaration, and the
    # Rust dual buffer can hold it with no preedit on screen.
    eng, _rec = build_engine(
        scripts=[
            [("hide", ""), ("commit", "hello"), ("forward", "")],
            [("forward", "")],
        ]
    )
    eng.do_set_content_type(PURPOSE_FREE_FORM, HINT_NONE)

    eng.do_process_key_event(ord(" "), 65, 0)
    assert eng._keys_since_content_type == 0

    eng.do_process_key_event(ord("x"), 53, 0)
    assert eng._keys_since_content_type == 1


def test_a_failing_preedit_takedown_cannot_abort_the_sensitive_reset():
    # ``hide_preedit_text()`` is a D-Bus round trip and can fail.  It was the
    # one unguarded call in _reset_for_sensitive_switch, so a raise aborted the
    # reset mid-way: the previous field's word stayed in the adapter's state
    # and a stale preedit was left displayed over the now-sensitive field.
    eng, rec = build_engine(scripts=[[("composing", "secr\x00et")]])
    eng.do_set_content_type(PURPOSE_FREE_FORM, HINT_NONE)
    assert eng.do_process_key_event(ord("r"), 27, 0) is True
    assert eng._preedit_active is True
    assert eng._last_composing_typed == "secr"
    assert eng._active_prediction is not None

    def _boom() -> None:
        rec.hide_preedit_calls += 1
        raise RuntimeError("hide_preedit_text: D-Bus round trip failed")

    eng.hide_preedit_text = _boom
    eng._core.calls.clear()

    eng.do_set_content_type(PURPOSE_PASSWORD, HINT_NONE)

    assert eng._sensitive is True, "the keystroke kill switch still holds"
    assert rec.hide_preedit_calls == 1, "the takedown must still be attempted"
    # Everything after the failing call must have run anyway.
    assert eng._preedit_active is False and eng._preedit_mode is None
    assert eng._last_composing_typed == ""
    assert eng._active_prediction is None
    assert eng._keys_since_content_type == 0
    assert ("set_surrounding_text", (None, None)) in eng._core.calls, (
        "the reset aborted before clearing the surrounding text"
    )


def test_ordinary_content_type_keeps_the_engine_fully_live():
    # Regression guard: the kill switch must not become a general kill switch.
    eng, rec = build_engine(scripts=[[("composing", "hel\x00lo")]])
    eng.do_set_content_type(PURPOSE_FREE_FORM, HINT_NONE)

    assert eng.do_process_key_event(ord("l"), 38, 0) is True
    assert eng._core.key_calls(), "the core must still be consulted for prose"
    assert rec.preedits and rec.preedits[-1][0] == "hello"


def test_surrounding_text_is_not_forwarded_while_sensitive():
    eng, _rec = build_engine()
    eng.do_set_content_type(PURPOSE_PASSWORD, HINT_NONE)
    eng._core.calls.clear()

    eng.do_set_surrounding_text("hunter2", 7, 7)

    assert eng._core.calls == [], "a sensitive field's contents must not reach the core"
    assert eng._surrounding_text is None


def test_focus_in_does_not_read_surrounding_text_while_sensitive(monkeypatch):
    # ``_refresh_surrounding_text`` short-circuits outright when IBus is
    # absent, so the harness has to make the read observable first — without
    # the positive control below this test would pass with the guard deleted.
    monkeypatch.setattr(ske, "_HAS_IBUS", True)
    eng, _rec = build_engine()
    eng.get_surrounding_text = lambda: ("hunter2", 7)

    # Positive control: an ordinary field DOES pull the surrounding text in.
    eng.do_set_content_type(PURPOSE_FREE_FORM, HINT_NONE)
    eng._core.calls.clear()
    eng.do_focus_in()
    assert "set_surrounding_text" in eng._core.names(), "harness cannot see the read"

    eng.do_set_content_type(PURPOSE_PASSWORD, HINT_NONE)
    eng._core.calls.clear()
    eng.do_focus_in()

    assert "set_surrounding_text" not in eng._core.names()
    assert eng._surrounding_text is None


# --- The declaration is sticky, because IBus delivers ContentType on CHANGE --
@pytest.mark.parametrize(
    "purpose,hints,label",
    [
        (PURPOSE_PASSWORD, HINT_NONE, "PASSWORD"),
        (PURPOSE_PIN, HINT_NONE, "PIN"),
        (PURPOSE_FREE_FORM, HINT_PRIVATE, "PRIVATE hint"),
        (PURPOSE_FREE_FORM, HINT_HIDDEN_TEXT, "HIDDEN_TEXT hint"),
        (PURPOSE_FUTURE, HINT_NONE, "a purpose this adapter has never audited"),
        (None, HINT_NONE, "an unset purpose"),
    ],
)
def test_a_sensitive_declaration_survives_a_focus_cycle(purpose, hints, label):
    # The declaration MUST outlive focus-out, because IBus only delivers
    # ContentType when it CHANGES.  `ibus_input_context_set_content_type` is
    # deduplicated against the proxy's cached property — verified by
    # disassembling the installed libibus-1.0.so.5, whose call sequence is
    # g_dbus_proxy_get_cached_property, g_variant_new, g_variant_equal, and only
    # on inequality g_dbus_proxy_call — and bus/engineproxy.c does the same
    # daemon-side.
    #
    # So leaving a password field and returning to the SAME field pushes a value
    # equal to the cache, the Set is suppressed, and do_set_content_type never
    # fires again.  An engine that cleared the flag on focus-out would be live
    # inside that password box: the exact hole this feature exists to close.
    eng, _rec = build_engine(scripts=[[("composing", "hel\x00lo")]])
    eng.do_set_content_type(purpose, hints)
    assert eng._sensitive is True
    assert eng.do_process_key_event(ord("l"), 38, 0) is False

    eng.do_focus_out()
    eng.do_focus_in()

    assert eng._sensitive is True, (
        f"{label} must survive a focus cycle: returning to the same field never "
        "re-declares, so clearing here re-opens the capture hole"
    )
    eng._core.calls.clear()
    assert eng.do_process_key_event(ord("l"), 38, 0) is False
    assert not eng._core.key_calls(), "the core was entered inside a sensitive field"


@pytest.mark.parametrize(
    "purpose,hints,label",
    [
        (PURPOSE_PASSWORD, HINT_NONE, "PASSWORD"),
        (PURPOSE_PIN, HINT_NONE, "PIN"),
    ],
)
def test_a_differing_declaration_after_a_focus_cycle_restores_normal_input(
    purpose, hints, label
):
    # The other half of the contract, and why stickiness does not strand the
    # engine: BusInputContext holds per-context purpose/hints defaulting to
    # FREE_FORM (0, 0) and pushes them at every focus-in.  Focusing an ordinary
    # field therefore pushes a value that DIFFERS from the cached secret one, so
    # the Set is not suppressed and sensitivity clears on its own.
    eng, _rec = build_engine(scripts=[[("composing", "hel\x00lo")]])
    eng.do_set_content_type(purpose, hints)
    assert eng._sensitive is True

    eng.do_focus_out()
    eng.do_focus_in()
    eng.do_set_content_type(PURPOSE_FREE_FORM, HINT_NONE)

    assert eng._sensitive is False, (
        f"an ordinary field after {label} must re-enable the engine"
    )
    eng._core.calls.clear()
    assert eng.do_process_key_event(ord("l"), 38, 0) is True
    assert eng._core.key_calls(), "the core was never entered again"


def test_focus_out_clears_only_the_in_flight_counter():
    # focus_out is a context boundary for the *composition*, not for the
    # declaration.  The declaration is the engine's mirror of a daemon-side
    # cached property that is only re-sent on change, so expiring it here would
    # leave the engine live in a password box it can never be told about again.
    eng, _rec = build_engine()
    eng.do_set_content_type(PURPOSE_PASSWORD, HINT_NONE)

    eng.do_focus_out()

    assert eng._sensitive is True, "the declaration must survive the focus boundary"
    assert eng._keys_since_content_type == 0, "nothing is in flight after focus_out"


@pytest.mark.parametrize("boundary", ["do_reset", "do_disable"])
def test_a_within_field_boundary_keeps_the_sensitive_declaration(boundary):
    # Deliberate asymmetry with focus_out: reset (Escape, a client-side context
    # reset) and disable (an IME switch) both fire *inside* the focused field.
    # Dropping the declaration there would re-enable the engine in a password
    # box that already declared itself and will not declare again.
    eng, _rec = build_engine(scripts=[[("composing", "hel\x00lo")]])
    eng.do_set_content_type(PURPOSE_PASSWORD, HINT_NONE)

    getattr(eng, boundary)()

    assert eng._sensitive is True, f"{boundary} is not a field change"
    assert eng.do_process_key_event(ord("l"), 38, 0) is False
    assert eng._core.key_calls() == []


def test_strict_undeclared_mode_arms_at_start_up_only(monkeypatch):
    # Honest scope of the strict switch: it arms once, at engine construction,
    # and the first declaration of the session resolves it.  It is NOT re-armed
    # per focus, because the declaration deliberately survives focus boundaries
    # (see test_a_sensitive_declaration_survives_a_focus_cycle) — IBus only
    # re-sends ContentType on change, so a per-focus re-arm would make the
    # engine inert in every client that declared once and never again.
    monkeypatch.setenv("SMARTKEY_SENSITIVE_UNTIL_DECLARED", "1")
    eng, _rec = build_engine(scripts=[[("composing", "hel\x00lo")]], sensitive=True)
    assert eng._sensitive is True, "strict mode arms before any declaration"
    assert eng.do_process_key_event(ord("l"), 38, 0) is False

    eng.do_set_content_type(PURPOSE_FREE_FORM, HINT_NONE)
    assert eng._sensitive is False
    assert eng.do_process_key_event(ord("l"), 38, 0) is True

    eng.do_focus_out()
    eng.do_focus_in()

    assert eng._sensitive is False, (
        "an ordinary declaration stays in force across the focus boundary"
    )


def test_strict_undeclared_mode_is_opt_in_and_inert_when_on(monkeypatch):
    # IBus content-purpose callbacks are advisory and adapter-specific: many
    # clients never send one, so "never declared" defaults to NOT sensitive
    # (otherwise SmartKey would simply be off everywhere).  Operators who want
    # the strictest stance flip one env var.
    monkeypatch.delenv("SMARTKEY_SENSITIVE_UNTIL_DECLARED", raising=False)
    assert ske._sensitive_until_declared() is False
    monkeypatch.setenv("SMARTKEY_SENSITIVE_UNTIL_DECLARED", "1")
    assert ske._sensitive_until_declared() is True

    eng, _rec = build_engine(scripts=[[("composing", "hel\x00lo")]], sensitive=True)
    assert eng.do_process_key_event(ord("l"), 38, 0) is False
    assert eng._core.key_calls() == []


@pytest.mark.parametrize(
    "purpose,hints,expected",
    [
        # Ordinary purposes — the engine must stay live in all of them.
        (PURPOSE_FREE_FORM, HINT_NONE, False),
        (PURPOSE_EMAIL, HINT_NONE, False),
        (PURPOSE_TERMINAL, HINT_NONE, False),
        # DATE/TIME/DATETIME exist in the installed IBus (11..13); omitting
        # them from the audited set would silently disable date pickers.
        (PURPOSE_DATE, HINT_NONE, False),
        (PURPOSE_DATETIME, HINT_NONE, False),
        (PURPOSE_FREE_FORM, HINT_SPELLCHECK, False),
        # Secret purposes and secrecy hints.
        (PURPOSE_PASSWORD, HINT_NONE, True),
        (PURPOSE_PIN, HINT_NONE, True),
        (PURPOSE_FREE_FORM, HINT_PRIVATE, True),
        (PURPOSE_FREE_FORM, HINT_HIDDEN_TEXT, True),
        # Ambiguity.
        (None, HINT_NONE, True),
        (PURPOSE_FUTURE, HINT_NONE, True),
    ],
)
def test_classify_content_type(purpose, hints, expected):
    sensitive, _key = ske.SmartKeyEngine._classify_content_type(purpose, hints)
    assert sensitive is expected


def _live_ibus_enum(enum_name: str) -> set[int]:
    """Values the INSTALLED IBus defines for *enum_name*, or skip the test.

    Read out of a subprocess on purpose: this module stubs ``sys.modules["gi"]``
    so the whole suite runs headless, so the real typelib cannot be imported
    here without breaking every other test.  Introspection only — nothing is
    instantiated and no IBus daemon is contacted.
    """
    code = (
        "import gi; gi.require_version('IBus','1.0');"
        "from gi.repository import IBus;"
        f"E=IBus.{enum_name};"
        "print(' '.join(str(int(v)) for v in vars(E).values() if isinstance(v,E)))"
    )
    try:
        proc = subprocess.run(  # noqa: S603
            [sys.executable, "-c", code],
            capture_output=True,
            text=True,
            timeout=60,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:  # pragma: no cover
        pytest.skip(f"cannot introspect IBus: {exc}")
    if proc.returncode != 0:
        pytest.skip(f"IBus typelib unavailable: {proc.stderr.strip()[:160]}")
    values = {int(tok) for tok in proc.stdout.split()}
    if not values:
        pytest.skip(f"IBus.{enum_name} exposed no members")
    return values


def test_every_purpose_the_installed_ibus_defines_is_audited():
    # The gap this guard was written after finding: the adapter hardcodes the
    # audited purposes, and DATE/TIME/DATETIME (11..13) were missing — so a
    # date picker would have failed safe straight into "SmartKey is off".
    # An unaudited purpose is a silent functional kill, so make it loud.
    defined = _live_ibus_enum("InputPurpose")
    assert defined <= ske._KNOWN_PURPOSES, (
        f"unaudited IBus purposes {sorted(defined - ske._KNOWN_PURPOSES)}: classify "
        "each one explicitly as ordinary or secret (they currently fail safe, "
        "which disables the engine in those fields)"
    )


def test_no_unaudited_ibus_input_hint_has_appeared():
    # Mirror tripwire on the privacy side: a hint this adapter has never seen
    # might be a new secrecy flag (PRIVATE and HIDDEN_TEXT both are).  Failing
    # here means "a human must classify it", not that anything is broken yet.
    audited = set(ske._KNOWN_HINT_VALUES)
    assert ske._HINT_PRIVATE in audited and ske._HINT_HIDDEN_TEXT in audited
    defined = _live_ibus_enum("InputHints")
    assert defined <= audited, (
        f"unaudited IBus input hints {sorted(defined - audited)}: check whether "
        "any of them declares secrecy and belongs in _SENSITIVE_HINTS"
    )


def test_unknown_purposes_share_one_decision_key():
    # Two different unrecognised values must not look like a field switch to
    # each other, or a noisy client would re-reset the buffer forever.
    _s1, key1 = ske.SmartKeyEngine._classify_content_type(999, HINT_NONE)
    _s2, key2 = ske.SmartKeyEngine._classify_content_type(None, HINT_NONE)
    assert key1 == key2


# ===========================================================================
# FIX 2 — the phantom keycode-240 storm must be consumed on any modifier mask.
# ===========================================================================
@pytest.mark.parametrize("state", OBSERVED_PHANTOM_MASKS)
def test_phantom_zero_key_is_consumed_for_every_observed_modifier_mask(state):
    eng, rec = build_engine()

    assert ske.SmartKeyEngine._is_spurious_zero_key_event(0, PHANTOM_KEYCODE, state)
    assert eng.do_process_key_event(0, PHANTOM_KEYCODE, state) is True, (
        f"mods={state} leaked into the application"
    )
    assert eng._core.calls == [], "a phantom event must not reach the core"
    assert rec.commits == [] and rec.preedits == []


@pytest.mark.parametrize("state", OBSERVED_PHANTOM_MASKS)
def test_real_key_with_the_same_modifiers_is_not_swallowed(state):
    # The predicate keys off the keyval/keycode signature, never the mods, so a
    # genuine keystroke carrying the identical mask must still reach the core.
    eng, _rec = build_engine(scripts=[[("forward", "")]])

    assert not ske.SmartKeyEngine._is_spurious_zero_key_event(ord("a"), 38, state)
    assert eng.do_process_key_event(ord("a"), 38, state) is False
    assert eng._core.key_calls(), f"a real key with mods={state} never reached the core"


@pytest.mark.parametrize(
    "state,label",
    [
        (MOD_NUMLOCK | MOD_CONTROL, "CTRL chord"),
        (MOD_NUMLOCK | MOD_ALT, "ALT chord"),
        (MOD_NUMLOCK | MOD_CONTROL | MOD_ALT, "CTRL+ALT chord"),
        (MOD_NUMLOCK | MOD_RELEASE, "key release"),
        (MOD_LOCK | MOD_SHIFT | MOD_RELEASE, "release with CapsLock+Shift"),
    ],
)
def test_phantom_filter_never_swallows_ctrl_alt_or_release(state, label):
    eng, _rec = build_engine(scripts=[[("forward", "")]])

    assert not ske.SmartKeyEngine._is_spurious_zero_key_event(
        0, PHANTOM_KEYCODE, state
    ), f"{label} must not be swallowed by the phantom filter"
    assert eng.do_process_key_event(0, PHANTOM_KEYCODE, state) is False


@pytest.mark.parametrize(
    "keyval,keycode",
    [
        (0, 239),  # neighbouring hardware code
        (0, 241),
        (0, 0),  # no keycode at all — the keyval path, not the storm
        (ord("a"), PHANTOM_KEYCODE),  # real symbol on the same hardware code
        (ske.IBus.KEY_BackSpace, PHANTOM_KEYCODE),
    ],
)
def test_phantom_filter_matches_only_the_exact_zero_key_signature(keyval, keycode):
    assert not ske.SmartKeyEngine._is_spurious_zero_key_event(
        keyval, keycode, MOD_NUMLOCK
    )


def test_phantom_storm_is_counted_for_the_rate_limited_report():
    eng, _rec = build_engine()
    for state in OBSERVED_PHANTOM_MASKS:
        assert eng.do_process_key_event(0, PHANTOM_KEYCODE, state) is True

    assert eng._spurious_zero_key_count == len(OBSERVED_PHANTOM_MASKS)
    assert eng._core.calls == []


def test_phantom_events_are_consumed_before_the_composition_counter_advances():
    # A phantom storm must not look like typing to the mid-composition
    # fail-safe, or a content-type re-send during a storm would kill the field.
    eng, _rec = build_engine()
    eng.do_set_content_type(PURPOSE_FREE_FORM, HINT_NONE)
    for _ in range(25):
        eng.do_process_key_event(0, PHANTOM_KEYCODE, MOD_NUMLOCK)

    assert eng._keys_since_content_type == 0
    eng.do_set_content_type(PURPOSE_EMAIL, HINT_NONE)
    assert eng._sensitive is False


def test_sensitive_mode_also_short_circuits_the_phantom_path():
    eng, _rec = build_engine()
    eng.do_set_content_type(PURPOSE_PASSWORD, HINT_NONE)

    # In a sensitive field nothing is consumed at all — every event, phantom or
    # not, is handed straight back to IBus untouched.  That ordering is safe
    # for FIX 2 because the zero-key storm is driven by an active preedit, and
    # a sensitive field never gets one.
    assert eng.do_process_key_event(0, PHANTOM_KEYCODE, MOD_NUMLOCK) is False
    assert eng._spurious_zero_key_count == 0


# --- B1: the cancel path must never produce text (cross-worktree seam) -------
class _CancelPathCore(SpyCore):
    """A core that DOES emit a commit from reset().

    This is not a hypothetical. The Rust core briefly did exactly this: a
    `flush_preedit()` was added to `reset()` so navigation would stop losing the
    in-flight word, and `reset()` was swept along with `cursor_moved()` and
    `focus_lost()`. A combined core+adapter replay then showed `do_reset`
    committing a pending preedit — i.e. pressing Escape typed the word.

    Neither worktree's suite could see it: the core change and the adapter that
    executes its actions live in different checkouts, and each side's tests were
    green. So the adapter defends itself, and this core proves the defence works
    even when the other side regresses.
    """

    def reset(self) -> list[tuple[str, str]]:
        self.calls.append(("reset", ()))
        return [("commit", "pending-preedit"), ("hide", "")]


def _engine_with_cancel_path_core():
    eng, rec = build_engine()
    eng._core = _CancelPathCore()
    return eng, rec


def test_do_reset_never_commits_even_if_the_core_emits_one():
    eng, rec = _engine_with_cancel_path_core()

    eng.do_reset()

    assert rec.commits == [], (
        "IBus Reset is a cancel; committing turns the user's Escape into an "
        f"insertion. Committed: {rec.commits}"
    )
    assert "reset" in eng._core.names(), "the core must still be reset"


def test_do_disable_never_commits_even_if_the_core_emits_one():
    eng, rec = _engine_with_cancel_path_core()

    eng.do_disable()

    assert rec.commits == [], (
        f"Disable is a cancel path too; committed: {rec.commits}"
    )


def test_cancel_safe_keeps_non_text_actions():
    # The filter must be surgical: hiding the preedit is exactly what a cancel
    # is supposed to do, so dropping it would break the cancel instead.
    kept = ske.SmartKeyEngine._cancel_safe(
        [("commit", "x"), ("hide", ""), ("replace", "2\x1fy"), ("forward", "")]
    )

    assert kept == [("hide", ""), ("forward", "")]


def test_current_native_core_reset_crosses_the_adapter_without_committing():
    """Exercise B1 through the actual PyO3 core and the Python adapter.

    The scripted ``_CancelPathCore`` above proves the adapter remains safe if
    the core regresses. This test proves the current Rust implementation and
    adapter agree today. CI builds ``smartkey_py`` from this checkout and sets
    ``SMARTKEY_REQUIRE_NATIVE_TESTS=1`` so an absent or stale extension cannot
    turn the cross-language contract into a silent skip.
    """
    if not ske._HAS_CORE:
        if os.environ.get("SMARTKEY_REQUIRE_NATIVE_TESTS") == "1":
            pytest.fail("current-checkout smartkey_py is required for the native seam test")
        pytest.skip("smartkey_py is not built in this local Python environment")

    native = ske.PyInputMethodCore(
        json.dumps(
            {
                "dual_buffer": {
                    "enabled": True,
                    "lock_threshold": 0.85,
                    "min_lock_chars": 4,
                },
                "use_ppm": False,
                "use_reranker": False,
            }
        )
    )
    # The first two raw keys create a real dual-buffer composing preedit. The
    # large frequency gap makes the premise stable without loading a corpus.
    native.load_word("he", 4_800_000)
    native.load_word("хе", 43_000)

    eng, rec = build_engine()
    eng._core = native
    eng.do_set_content_type(PURPOSE_FREE_FORM, HINT_NONE)
    def to_ibus(evdev):
        return evdev if ske._IS_WAYLAND else evdev + 8

    eng.do_process_key_event(ord("h"), to_ibus(35), 0)
    eng.do_process_key_event(ord("e"), to_ibus(18), 0)
    assert native.debug_state()[0] is True, "premise broken: no native dual buffer"
    assert rec.preedits, "premise broken: the adapter never rendered native composing"

    eng.do_reset()

    assert rec.commits == [], (
        "the real native reset crossed do_reset as text — cancel became insertion: "
        f"{rec.commits!r}"
    )
    assert native.debug_state()[0] is False, "native reset left the dual buffer alive"


def test_a_bare_hide_mid_word_does_not_disarm_the_composition_fail_safe():
    """The core emits HideGhost mid-word; that must not read as "resolved".

    ``_composition_in_flight()`` is the fail-safe for a content type that
    arrives while a word is still being composed: the window is treated as
    sensitive because the composition may already belong to a different field.
    An earlier revision counted every ``hide`` as a resolution and zeroed the
    counter — but the core emits a bare ``HideGhost`` whenever the prefix has
    no ghost suffix.

    Path matters, and only one of the two is affected. Measured against the
    real module: ``process_keycode`` returns ``['composing']`` and the counter
    accumulates correctly, while ``handle_key`` returns ``['hide','forward']``
    with a non-empty ``current_word()``. So this pins the KEYVAL COMPAT path —
    the branch taken when the client sends ``keycode == 0`` or the installed
    native module predates ``process_keycode``. Passing ``keycode=0`` below is
    not a shortcut; it is the only way to reach the defect.

    Driven through the real PyO3 core, because the premise ("the core really
    does emit a bare hide here") is a fact about the Rust side, not about a
    fake — a scripted core would prove only that the script was followed.
    """
    if not ske._HAS_CORE:
        if os.environ.get("SMARTKEY_REQUIRE_NATIVE_TESTS") == "1":
            pytest.fail("current-checkout smartkey_py is required for the native seam test")
        pytest.skip("smartkey_py is not built in this local Python environment")

    native = ske.PyInputMethodCore(json.dumps({"use_ppm": False, "use_reranker": False}))
    native.load_word("hello", 1_000)

    eng, _rec = build_engine()
    eng._core = native
    eng.do_set_content_type(PURPOSE_FREE_FORM, HINT_NONE)

    # keycode=0 -> the keyval compat path, where the bare hide actually occurs.
    eng.do_process_key_event(ord("x"), 0, 0)

    assert native.current_word() == "x", "premise broken: the core dropped the word"
    # Assert the COUNTER, not only `_composition_in_flight()`. That helper ORs
    # in `_preedit_active`, which masks the defect: an early version of this
    # test asserted the helper, and a mutation run restoring the pre-fix
    # behaviour still passed. The counter is the half that tracks the dual
    # buffer's invisible word, and it is the half that broke.
    assert eng._keys_since_content_type > 0, (
        "a bare hide mid-word zeroed the key counter: the core is still holding "
        f"{native.current_word()!r}, so the mid-composition fail-safe is disarmed"
    )
    assert eng._composition_in_flight()


def test_a_hide_after_the_word_is_gone_still_counts_as_resolved():
    """The other direction: the fix must not pin the counter on forever.

    Once the core reports no word, a hide IS a resolution, and the counter has
    to fall — otherwise every later content-type change in the field is
    escalated to sensitive long after the word committed, and SmartKey goes
    inert for the rest of the field.
    """
    eng, _rec = build_engine()

    class _ResolvedCore(SpyCore):
        def current_word(self):
            return ""

    eng._core = _ResolvedCore()
    eng.do_set_content_type(PURPOSE_FREE_FORM, HINT_NONE)
    eng._keys_since_content_type = 3

    eng._execute_actions([("hide", "")])

    assert eng._keys_since_content_type == 0, (
        "a hide with no word left in the core must clear the counter"
    )


def test_an_unaskable_core_keeps_the_fail_safe_armed():
    """No ``current_word()`` (older native module, or a fake) -> stay armed."""
    eng, _rec = build_engine()
    eng._core = SpyCore()  # deliberately has no current_word
    assert not hasattr(eng._core, "current_word"), "premise broken"
    eng.do_set_content_type(PURPOSE_FREE_FORM, HINT_NONE)
    eng._keys_since_content_type = 2

    eng._execute_actions([("hide", "")])

    assert eng._keys_since_content_type == 2, (
        "an unaskable core must not be read as 'resolved' — that is the unsafe "
        "direction (kill switch silently not firing inside a password field)"
    )
