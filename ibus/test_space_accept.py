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
import time
import types

if "SMARTKEY_PHASEA_DATA" not in os.environ:
    os.environ["SMARTKEY_PHASEA_DATA"] = tempfile.mkdtemp(
        prefix="smartkey-test-phasea-"
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
# Harness-native self-fence: the adapter module executes right here.  For
# exactly that window the native extension and the content-level debug sinks
# must be unreachable whatever is installed, cached or exported in this
# process; afterwards the exact previous ``sys.modules`` entry and
# ``SMARTKEY_DEBUG`` value are restored, also on an exceptional exit.
_MISSING = object()
_prev_native = sys.modules.get("smartkey_py", _MISSING)
_prev_debug = os.environ.get("SMARTKEY_DEBUG", _MISSING)
sys.modules["smartkey_py"] = None
os.environ["SMARTKEY_DEBUG"] = "off"
try:
    _spec.loader.exec_module(ske)
finally:
    if _prev_native is _MISSING:
        sys.modules.pop("smartkey_py", None)
    else:
        sys.modules["smartkey_py"] = _prev_native
    if _prev_debug is _MISSING:
        os.environ.pop("SMARTKEY_DEBUG", None)
    else:
        os.environ["SMARTKEY_DEBUG"] = _prev_debug
assert ske._HAS_IBUS is False
assert ske._HAS_CORE is False and ske._DEBUG is False


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


# --- S02/P2a Cycle 2: ordered synthetic client buffer + literal-key oracles ---
#
# Independent client model.  Nothing below asks a production symbol what the
# CLIENT types: key values and modifier bits are the literal IBus numbers and
# ``_literal_char`` is a bounded, test-local decoder.  The adapter itself is
# still driven through its real ``do_process_key_event`` seam via
# ``build_engine``; the core stays the scripted fake on every route, so every
# test here is a scripted-core characterization of the adapter, not a proof
# of the real core's policy.

_KEY_SPACE = 0x0020
_KEY_RETURN = 0xFF0D
_KEY_BACKSPACE = 0xFF08
_KEY_HOME = 0xFF50
_KEY_LEFT = 0xFF51
_KEY_RIGHT = 0xFF53
_KEY_END = 0xFF57
_KEY_TAB = 0xFF09
_KEY_ESCAPE = 0xFF1B
_CLIENT_CONTROL = 1 << 2
_CLIENT_ALT = 1 << 3
_CLIENT_RELEASE = 1 << 30


def _literal_char(keyval: int) -> str | None:
    """Bounded, test-local decoding of a literal key into client text."""
    if keyval == _KEY_SPACE:
        return " "
    if keyval == _KEY_RETURN:
        return "\n"
    if 0x21 <= keyval <= 0x7E or 0x0400 <= keyval <= 0x04FF:
        return chr(keyval)
    if 0x01000000 <= keyval <= 0x0110FFFF:
        return chr(keyval & 0x00FFFFFF)
    return None


class ClientBuffer:
    """Ordered synthetic client: exact text, cursor and an event log."""

    def __init__(self, text: str = "", cursor: int | None = None) -> None:
        self.text = text
        self.cursor = len(text) if cursor is None else cursor
        self.log: list[tuple] = []

    def _insert(self, s: str) -> None:
        self.text = self.text[: self.cursor] + s + self.text[self.cursor :]
        self.cursor += len(s)

    def commit(self, s: str) -> None:
        self._insert(s)
        self.log.append(("commit", s))

    def delete_surrounding(self, offset: int, n: int) -> None:
        start = max(0, self.cursor + offset)
        end = min(len(self.text), start + n)
        self.text = self.text[:start] + self.text[end:]
        if offset < 0:
            self.cursor = start
        self.log.append(("delete", offset, n))

    def apply_key(self, keyval: int, state: int, source: str) -> None:
        """Apply one physical key exactly once; chords and releases never type."""
        if state & _CLIENT_RELEASE:
            self.log.append((source, "release", keyval))
            return
        if state & (_CLIENT_CONTROL | _CLIENT_ALT):
            self.log.append((source, "chord", keyval))
            return
        if keyval == _KEY_BACKSPACE:
            if self.cursor > 0:
                self.text = self.text[: self.cursor - 1] + self.text[self.cursor :]
                self.cursor -= 1
        elif keyval == _KEY_HOME:
            self.cursor = 0
        elif keyval == _KEY_END:
            self.cursor = len(self.text)
        elif keyval == _KEY_LEFT:
            self.cursor = max(0, self.cursor - 1)
        elif keyval == _KEY_RIGHT:
            self.cursor = min(len(self.text), self.cursor + 1)
        else:
            ch = _literal_char(keyval)
            if ch is None:
                self.log.append((source, "ignored", keyval))
                return
            self._insert(ch)
        self.log.append((source, keyval))

    def defaults(self) -> list[tuple]:
        return [event for event in self.log if event[0] == "default"]


class RecordingCore(FakeCore):
    """Scripted core that also records every key call the adapter makes."""

    def __init__(self, scripts: list[list[tuple[str, str]]]) -> None:
        super().__init__(scripts)
        self.calls: list[tuple] = []

    def process_keycode(self, evdev: int, state: int) -> list[tuple[str, str]]:
        self.calls.append(("process_keycode", evdev, state))
        return self._next()

    def handle_key(self, keyval: int, state: int) -> list[tuple[str, str]]:
        self.calls.append(("handle_key", keyval, state))
        return self._next()

    def key_calls(self) -> list[tuple]:
        return [call for call in self.calls if call[0] in ("process_keycode", "handle_key")]


def build_client_engine(scripts, *, text: str = "", caps: int = 0):
    """Real adapter + scripted recording core + synthetic client buffer."""
    eng, rec = build_engine(scripts)
    eng._core = RecordingCore(scripts)
    eng._caps = caps
    buf = ClientBuffer(text)
    rec.deleted = []

    def _commit(text_obj) -> None:
        rec.commits.append(text_obj.s)
        buf.commit(text_obj.s)

    def _delete(offset: int, n: int) -> None:
        rec.deleted.append((offset, n))
        buf.delete_surrounding(offset, n)

    def _forward(kv: int, kc: int, st: int) -> None:
        rec.forwarded_keys.append((kv, kc, st))
        buf.apply_key(kv, st, "forward")

    eng.commit_text = _commit
    eng.delete_surrounding_text = _delete
    eng.forward_key_event = _forward
    return eng, rec, buf


def offer(eng, buf: ClientBuffer, keyval: int, keycode: int, state: int = 0) -> bool:
    """Offer one physical key; the client applies it once iff not consumed."""
    consumed = eng.do_process_key_event(keyval, keycode, state)
    if consumed is False:
        buf.apply_key(keyval, state, "default")
    return consumed


def test_client_buffer_replace_and_wrong_buffer_mismatch():
    buf = ClientBuffer("dog")
    buf.delete_surrounding(-3, 3)
    buf.commit("cat")
    assert (buf.text, buf.cursor) == ("cat", 3)
    assert buf.log == [("delete", -3, 3), ("commit", "cat")]
    # Wrong-buffer shapes that real adapter bugs would produce stay distinct.
    appended = ClientBuffer("dog")
    appended.commit("cat")  # forgot to delete the old word
    assert (appended.text, appended.cursor) == ("dogcat", 6)
    short = ClientBuffer("dog")
    short.delete_surrounding(-2, 2)  # off-by-one deletion
    short.commit("cat")
    assert (short.text, short.cursor) == ("dcat", 4)
    assert appended.text != buf.text and short.text != buf.text


def test_client_accept_then_literal_space_exact_text():
    eng, rec, buf = build_client_engine(
        [
            [("composing", "hel\x00lo")],
            [("hide", ""), ("commit", "hello ")],
            [("hide", ""), ("forward", "")],
        ]
    )
    assert offer(eng, buf, ord("l"), 38) is True
    assert buf.text == ""  # the preedit is display-only
    assert offer(eng, buf, _KEY_SPACE, 57) is True
    assert buf.text == "hello "
    assert offer(eng, buf, _KEY_SPACE, 57) is False
    assert (buf.text, buf.cursor) == ("hello  ", 7)
    assert rec.commits == ["hello "]
    assert rec.forwarded_keys == []
    assert buf.log == [("commit", "hello "), ("default", _KEY_SPACE)]


def test_client_scripted_default_off_space_lands_typed_word_then_literal_space():
    # Scripted-core characterization of the default-off action shape
    # [hide, commit(typed), forward]; not a proof of the real flag policy.
    eng, rec, buf = build_client_engine(
        [
            [("composing", "hel\x00lo")],
            [("hide", ""), ("commit", "hel"), ("forward", "")],
        ]
    )
    assert offer(eng, buf, ord("l"), 38) is True
    assert offer(eng, buf, _KEY_SPACE, 57) is False
    assert (buf.text, buf.cursor) == ("hel ", 4)
    assert buf.log == [("commit", "hel"), ("default", _KEY_SPACE)]


def test_client_escape_then_space_exact_text():
    eng, rec, buf = build_client_engine(
        [
            [("composing", "hel\x00lo")],
            [("composing", "hel\x00")],
            [("hide", ""), ("commit", "hel"), ("forward", "")],
        ]
    )
    assert offer(eng, buf, ord("l"), 38) is True
    assert offer(eng, buf, _KEY_ESCAPE, 1) is True
    assert buf.text == ""
    assert offer(eng, buf, _KEY_SPACE, 57) is False
    assert (buf.text, buf.cursor) == ("hel ", 4)


def test_client_literal_cyrillic_keyvals_type_exact_text():
    eng, rec, buf = build_client_engine([[("forward", "")], [("forward", "")]])
    assert offer(eng, buf, 0x0431, 56) is False
    assert offer(eng, buf, 0x01000431, 56) is False
    assert (buf.text, buf.cursor) == ("бб", 2)
    # Both keysym forms reach the keyval route normalised to the code point.
    assert eng._core.key_calls() == [("handle_key", 0x0431, 0), ("handle_key", 0x0431, 0)]


def test_client_return_lands_typed_word_then_newline():
    eng, rec, buf = build_client_engine(
        [
            [("composing", "hel\x00lo")],
            [("hide", ""), ("commit", "hel"), ("forward", "")],
        ]
    )
    assert offer(eng, buf, ord("l"), 38) is True
    assert offer(eng, buf, _KEY_RETURN, 28) is False
    assert (buf.text, buf.cursor) == ("hel\n", 4)
    assert buf.log == [("commit", "hel"), ("default", _KEY_RETURN)]


def test_client_forwarded_backspace_deletes_committed_text_once():
    eng, rec, buf = build_client_engine(
        [[("ghost", "lo")], [("hide", ""), ("forward", "")]], text="hello "
    )
    assert offer(eng, buf, ord("l"), 38) is True
    assert buf.text == "hello "
    assert offer(eng, buf, _KEY_BACKSPACE, 22) is False
    assert (buf.text, buf.cursor) == ("hello", 5)
    assert buf.defaults() == [("default", _KEY_BACKSPACE)]
    assert rec.forwarded_keys == []


def test_client_forwarded_navigation_keys_move_cursor_sequentially():
    eng, rec, buf = build_client_engine([[("forward", "")]] * 4, text="ab")
    assert buf.cursor == 2
    expected = [(_KEY_HOME, 0), (_KEY_LEFT, 0), (_KEY_RIGHT, 1), (_KEY_END, 2)]
    for keyval, cursor in expected:
        assert offer(eng, buf, keyval, 100) is False
        assert (buf.text, buf.cursor) == ("ab", cursor), hex(keyval)
    assert buf.log == [("default", keyval) for keyval, _ in expected]


def test_client_forwarded_navigation_keys_from_explicit_initial_states():
    rows = [
        (2, _KEY_HOME, 0),
        (2, _KEY_LEFT, 1),
        (2, _KEY_RIGHT, 2),
        (2, _KEY_END, 2),
        (1, _KEY_LEFT, 0),
        (1, _KEY_RIGHT, 2),
        (1, _KEY_HOME, 0),
        (1, _KEY_END, 2),
        (0, _KEY_LEFT, 0),
        (0, _KEY_HOME, 0),
    ]
    for start, keyval, cursor in rows:
        eng, rec, buf = build_client_engine([[("forward", "")]], text="ab")
        buf.cursor = start
        assert offer(eng, buf, keyval, 100) is False
        assert (buf.text, buf.cursor) == ("ab", cursor), (start, hex(keyval))
        assert buf.log == [("default", keyval)]


def test_client_ctrl_chord_reaches_core_once_and_types_nothing():
    # No pre-core chord short-circuit: the core sees the key with its state
    # bits exactly once; the client never turns a chord into literal text.
    eng, rec, buf = build_client_engine([[("forward", "")]], text="x")
    assert offer(eng, buf, ord("c"), 46, _CLIENT_CONTROL) is False
    assert eng._core.key_calls() == [("handle_key", ord("c"), _CLIENT_CONTROL)]
    assert buf.text == "x"
    assert buf.log == [("default", "chord", ord("c"))]

    eng2, rec2, buf2 = build_client_engine([[("hide", "")]], text="x")
    assert offer(eng2, buf2, ord("c"), 46, _CLIENT_ALT) is True
    assert eng2._core.key_calls() == [("handle_key", ord("c"), _CLIENT_ALT)]
    assert buf2.text == "x" and buf2.log == []


def test_client_release_reaches_core_once_and_types_nothing():
    eng, rec, buf = build_client_engine([[("forward", "")], [("forward", "")]], text="x")
    eng._keys_since_content_type = 0
    assert offer(eng, buf, ord("c"), 46, _CLIENT_RELEASE) is False
    assert eng._core.key_calls() == [("handle_key", ord("c"), _CLIENT_RELEASE)]
    assert buf.text == "x"
    assert buf.log == [("default", "release", ord("c"))]
    assert eng._keys_since_content_type == 0  # releases are not counted
    assert offer(eng, buf, ord("c"), 46) is False  # a press is
    assert eng._keys_since_content_type == 1
    assert (buf.text, buf.cursor) == ("xc", 2)


def test_client_release_never_logs_prediction_rejection():
    eng, rec, buf = build_client_engine([[("composing", "hel\x00lo")], [("forward", "")]])
    events = capture_replay(eng)
    assert offer(eng, buf, ord("l"), 38) is True
    assert eng._active_prediction is not None
    assert offer(eng, buf, ord("l"), 38, _CLIENT_RELEASE) is False
    kinds = [event for event, _ in events]
    assert "rejected" not in kinds and "accepted" not in kinds
    assert eng._active_prediction is not None
    assert buf.text == ""


def test_client_spurious_zero_key_is_consumed_without_core_but_chord_reaches_core():
    eng, rec, buf = build_client_engine([[("forward", "")]], text="x")
    assert offer(eng, buf, 0, 240, 16) is True
    assert eng._core.key_calls() == []
    assert buf.text == "x" and buf.log == []
    assert offer(eng, buf, 0, 240, _CLIENT_CONTROL) is False
    assert eng._core.key_calls() == [("handle_key", 0, _CLIENT_CONTROL)]
    assert buf.text == "x" and buf.log == [("default", "chord", 0)]


def test_client_replace_uses_surrounding_delete_when_capability_present():
    eng, rec, buf = build_client_engine(
        [[("replace", "3\x1fcat")]], text="dog", caps=0x20
    )
    assert offer(eng, buf, _KEY_SPACE, 57) is True
    assert rec.deleted == [(-3, 3)]
    assert rec.forwarded_keys == []
    assert (buf.text, buf.cursor) == ("cat", 3)
    assert buf.log == [("delete", -3, 3), ("commit", "cat")]


def test_client_replace_falls_back_to_forwarded_backspaces_without_capability():
    eng, rec, buf = build_client_engine([[("replace", "3\x1fcat")]], text="dog", caps=0)
    assert offer(eng, buf, _KEY_SPACE, 57) is True
    assert rec.deleted == []
    assert rec.forwarded_keys == [(_KEY_BACKSPACE, 14, 0)] * 3
    assert (buf.text, buf.cursor) == ("cat", 3)
    assert buf.log == [("forward", _KEY_BACKSPACE)] * 3 + [("commit", "cat")]


def test_client_replace_during_composing_commits_without_deleting():
    eng, rec, buf = build_client_engine(
        [[("composing", "do\x00g")], [("replace", "2\x1fcat")]], caps=0x20
    )
    assert offer(eng, buf, ord("o"), 32) is True
    assert buf.text == ""
    assert offer(eng, buf, _KEY_SPACE, 57) is True
    assert rec.deleted == [] and rec.forwarded_keys == []
    assert (buf.text, buf.cursor) == ("cat", 3)


def test_client_route_epochs_are_synthetic_and_normalised(monkeypatch):
    # Default harness: the fence leaves _HAS_CORE False, so every event takes
    # the keyval route whatever the keycode.
    eng, rec, buf = build_client_engine([[("forward", "")]] * 4)
    assert offer(eng, buf, ord("l"), 38) is False
    assert eng._core.key_calls() == [("handle_key", ord("l"), 0)]
    # Fixture-only route pins (restored by monkeypatch): the raw-scancode
    # route with the SAME scripted core -- no native module is imported.
    monkeypatch.setattr(ske, "_HAS_CORE", True)
    monkeypatch.setattr(ske, "_IS_WAYLAND", False)
    assert offer(eng, buf, ord("l"), 38) is False
    assert eng._core.key_calls()[-1] == ("process_keycode", 30, 0)
    monkeypatch.setattr(ske, "_IS_WAYLAND", True)
    assert offer(eng, buf, ord("l"), 38) is False
    assert eng._core.key_calls()[-1] == ("process_keycode", 38, 0)
    assert offer(eng, buf, ord("l"), 0) is False  # keycode 0 -> keyval route
    assert eng._core.key_calls()[-1] == ("handle_key", ord("l"), 0)
    assert len(eng._core.key_calls()) == 4
    assert (buf.text, buf.cursor) == ("llll", 4)
    assert len(buf.defaults()) == 4


def test_client_tab_and_right_variants_exact_text():
    eng, rec, buf = build_client_engine(
        [[("composing", "hel\x00lo")], [("hide", ""), ("commit", "hello")]]
    )
    assert offer(eng, buf, ord("l"), 38) is True
    assert offer(eng, buf, _KEY_TAB, 23) is True
    assert (buf.text, buf.cursor) == ("hello", 5)
    assert ("", False) in rec.preedits and buf.defaults() == []

    # Right as a one-character composing step is display-only.
    eng2, rec2, buf2 = build_client_engine(
        [[("composing", "hel\x00lo")], [("composing", "hell\x00o")]]
    )
    assert offer(eng2, buf2, ord("l"), 38) is True
    assert offer(eng2, buf2, _KEY_RIGHT, 106) is True
    assert buf2.text == "" and rec2.preedits[-1] == ("hello", True)

    # A final Right commits the full word without a delimiter.
    eng3, rec3, buf3 = build_client_engine(
        [[("composing", "hel\x00lo")], [("hide", ""), ("commit", "hello")]]
    )
    assert offer(eng3, buf3, ord("l"), 38) is True
    assert offer(eng3, buf3, _KEY_RIGHT, 106) is True
    assert (buf3.text, buf3.cursor) == ("hello", 5)

    # A forwarded Right with no composition moves the client cursor once.
    eng4, rec4, buf4 = build_client_engine([[("forward", "")]], text="ab")
    buf4.cursor = 1
    assert offer(eng4, buf4, _KEY_RIGHT, 106) is False
    assert (buf4.text, buf4.cursor) == ("ab", 2)
    assert buf4.log == [("default", _KEY_RIGHT)]


# --- S02/P2a Cycle 3: lifecycle characterizations (scripted core) ------------
#
# ``do_set_content_type`` / ``do_focus_in`` / ``do_focus_out`` / ``do_reset`` /
# ``do_disable`` against the real adapter, a scripted recording core and the
# synthetic client of Cycle 2.  Expected-PASS characterizations of the current
# behaviour; no production change.


class LifecycleCore(RecordingCore):
    """Recording core that also answers the adapter's lifecycle callbacks."""

    def __init__(self, scripts, reset_actions=None) -> None:
        super().__init__(scripts)
        self.reset_actions = list(reset_actions or [])

    def focus_lost(self) -> list[tuple[str, str]]:
        self.calls.append(("focus_lost",))
        return []

    def focus_gained(self) -> None:
        self.calls.append(("focus_gained",))

    def reset(self) -> list[tuple[str, str]]:
        self.calls.append(("reset",))
        return list(self.reset_actions)

    def save_personal(self) -> None:
        self.calls.append(("save_personal",))

    def set_surrounding_text(self, text, cursor_pos) -> None:
        self.calls.append(("set_surrounding_text", text, cursor_pos))

    def lifecycle_calls(self) -> list[tuple]:
        return [c for c in self.calls if c[0] not in ("process_keycode", "handle_key")]


class _ScriptedClock:
    """``time`` stand-in for the adapter: scripted ``monotonic``, rest real."""

    def __init__(self, now: float) -> None:
        self.now = now

    def monotonic(self) -> float:
        return self.now

    def __getattr__(self, name: str):
        return getattr(time, name)


def build_lifecycle_engine(scripts, *, reset_actions=None, text: str = "", caps: int = 0):
    """Cycle-2 client engine whose core also records lifecycle callbacks."""
    eng, rec, buf = build_client_engine(scripts, text=text, caps=caps)
    eng._core = LifecycleCore(scripts, reset_actions)
    eng._last_save = time.monotonic()  # "just saved": focus-out must not save now
    eng._keys_since_content_type = 0
    eng._o1 = None
    rec.hides = 0

    def _hide() -> None:
        rec.hides += 1

    eng.hide_preedit_text = _hide
    return eng, rec, buf


# Text-producing actions a core must never emit on a cancel path; the adapter's
# second lock (``_cancel_safe``) has to drop them.
_CANCEL_ACTIONS = [("commit", "x"), ("replace", "1\x1fy"), ("hide", "")]


def test_lifecycle_password_declaration_is_sticky_across_focus():
    eng, _rec, buf = build_lifecycle_engine([[("forward", "")]] * 3)
    core = eng._core
    eng.do_set_content_type(ske._PURPOSE_PASSWORD, 0)
    assert eng._sensitive is True
    assert core.lifecycle_calls() == [("reset",), ("set_surrounding_text", None, None)]

    # Sensitive: the key never reaches the core; the client gets it by default.
    assert offer(eng, buf, ord("a"), 38) is False
    assert core.key_calls() == []
    assert buf.text == "a"

    # The declaration survives a focus round-trip (IBus deduplicates it).
    core.calls.clear()
    eng.do_focus_out()
    assert ("focus_lost",) in core.calls
    assert ("save_personal",) not in core.calls
    eng.do_focus_in()
    assert ("focus_gained",) in core.calls
    assert eng._sensitive is True
    assert offer(eng, buf, ord("b"), 56) is False
    assert core.key_calls() == []
    assert buf.text == "ab"

    # Only an explicit ordinary declaration un-sticks it.
    core.calls.clear()
    eng.do_set_content_type(0, 0)
    assert eng._sensitive is False
    assert core.calls.count(("reset",)) == 1
    assert offer(eng, buf, ord("c"), 54) is False
    assert core.key_calls() == [("handle_key", ord("c"), 0)]
    assert buf.text == "abc"


def test_lifecycle_unknown_and_bool_purposes_fail_closed_on_fresh_engines():
    for purpose in (99, True):
        eng, _rec, buf = build_lifecycle_engine([[("forward", "")]])
        eng.do_set_content_type(purpose, 0)
        assert eng._sensitive is True, purpose
        assert offer(eng, buf, ord("a"), 38) is False
        assert eng._core.key_calls() == [], purpose
        assert buf.text == "a"

    # An ordinary declaration on a fresh engine changes nothing: no reset,
    # and the key reaches the core.
    eng, _rec, buf = build_lifecycle_engine([[("forward", "")]])
    eng.do_set_content_type(0, 0)
    assert getattr(eng, "_sensitive", False) is False
    assert eng._core.calls.count(("reset",)) == 0
    assert offer(eng, buf, ord("a"), 38) is False
    assert eng._core.key_calls() == [("handle_key", ord("a"), 0)]
    assert buf.text == "a"


def test_lifecycle_focus_out_debounces_save_personal_at_sixty_seconds(monkeypatch):
    eng, _rec, _buf = build_lifecycle_engine([])
    core = eng._core
    clock = _ScriptedClock(1000.0)
    monkeypatch.setattr(ske, "time", clock)

    eng._last_save = 0.0  # the adapter's initial value: first focus-out saves
    eng.do_focus_out()
    assert core.calls.count(("save_personal",)) == 1
    assert eng._last_save == 1000.0

    clock.now = 1059.999
    eng.do_focus_out()
    assert core.calls.count(("save_personal",)) == 1
    assert eng._last_save == 1000.0

    clock.now = 1060.0
    eng.do_focus_out()
    assert core.calls.count(("save_personal",)) == 2
    assert eng._last_save == 1060.0


def test_lifecycle_reset_is_cancel_safe_with_prefilled_client():
    eng, rec, buf = build_lifecycle_engine(
        [], reset_actions=_CANCEL_ACTIONS, text="abc", caps=0x20
    )
    eng.do_reset()
    assert ("reset",) in eng._core.calls
    assert buf.text == "abc"
    assert buf.log == []
    assert rec.commits == []
    assert rec.deleted == []
    assert rec.forwarded_keys == []
    assert rec.hides == 1
    assert eng._active_prediction is None


def test_lifecycle_disable_resets_then_saves_without_client_text():
    eng, rec, buf = build_lifecycle_engine(
        [], reset_actions=_CANCEL_ACTIONS, text="abc", caps=0x20
    )
    eng.do_disable()
    calls = eng._core.calls
    assert calls.count(("save_personal",)) == 1
    assert calls.index(("reset",)) < calls.index(("save_personal",))
    assert buf.text == "abc"
    assert buf.log == []
    assert rec.commits == []
    assert rec.deleted == []
    assert rec.hides == 1
