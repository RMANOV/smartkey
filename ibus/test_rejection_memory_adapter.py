"""End-to-end adapter tests for session-scoped ghost rejection feedback."""

from __future__ import annotations

import importlib.util
import os
import pathlib
import sys
import tempfile
import types

import pytest

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


def test_final_right_accept_clears_prediction_without_later_rejection():
    engine, core = build_engine()
    events = capture_events(engine)
    show_hello(engine, core)

    # First Right accepts only "l" and keeps the remaining "o" visible.
    core.queue([("composing", "hell\x00o")], "hell")
    engine.do_process_key_event(ske.IBus.KEY_Right, 106, 0)
    assert engine._active_prediction is not None
    assert not [event for event, _payload in events if event == "accepted"]

    # Final Right commits the complete composing word and must resolve the
    # displayed prediction as an acceptance, not leave it pending.
    core.queue([("hide", ""), ("commit", "hello")], "")
    engine.do_process_key_event(ske.IBus.KEY_Right, 106, 0)
    accepted = [payload for event, payload in events if event == "accepted"]
    assert len(accepted) == 1
    assert accepted[0]["reason"] == "right"
    assert engine._active_prediction is None
    assert core.recorded == []

    # A later key belongs to a new interaction and must not reject/suppress the
    # suggestion that the user just accepted.
    core.queue([("forward", "")], "x")
    engine.do_process_key_event(ord("x"), 45, 0)
    assert core.recorded == []
    assert not [event for event, _payload in events if event == "rejected"]


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


@pytest.mark.parametrize(
    "letter,post_word,expected_feedback,expected_outcome",
    [
        ("l", "hell", [], ("completed", "completed_manually")),
        ("x", "helx", [("hel", "hello")], ("rejected", "continued_typing")),
    ],
    ids=["matching-continuation", "diverging-continuation"],
)
def test_hybrid_delivery_commit_forward_letter_uses_continued_typing_outcome(
    letter, post_word, expected_feedback, expected_outcome
):
    engine, core = build_engine()
    events = capture_events(engine)
    show_hello(engine, core)

    # A semantic letter hands off the old preedit to client-owned text while
    # extending the SAME canonical token. The letter itself is forwarded.
    core.queue([("hide", ""), ("commit", "hel"), ("forward", "")], post_word)
    assert engine.do_process_key_event(ord(letter), 0, 0) is False

    assert core.recorded == expected_feedback
    outcomes = [
        (event, payload.get("reason"))
        for event, payload in events
        if event in {"completed", "rejected", "accepted"}
    ]
    assert outcomes == [expected_outcome]


def test_hybrid_delivery_handoff_and_raw_fragment_notify_o1_at_final_space_only(
    monkeypatch,
):
    engine, core = build_engine()
    committed_words = []
    engine._o1 = types.SimpleNamespace(on_word_commit=committed_words.append)
    monkeypatch.setattr(ske, "_HAS_CORE", True)
    show_hello(engine, core)

    core.queue([("hide", ""), ("commit", "hel"), ("forward", "")], "hell")
    assert engine.do_process_key_event(ord("l"), 0, 0) is False
    # The next Raw glyph is delivered by CommitText, not by forwarding an
    # event whose keyval could disagree with the selected layout.
    core.queue([("commit", "o")], "hello")
    assert engine.do_process_key_event(ord("o"), 24, 0) is True
    assert committed_words == [], "partial client delivery is not a word commit"

    # The client already owns the complete token; the true boundary carries
    # no CommitText. Its pre/post canonical transition must still feed O1 once.
    core.queue([("hide", ""), ("forward", "")], "")
    assert engine.do_process_key_event(ske.IBus.KEY_space, 57, 0) is False
    assert committed_words == ["hello"]

    core.queue([("hide", ""), ("forward", "")], "")
    engine.do_process_key_event(ske.IBus.KEY_space, 57, 0)
    assert committed_words == ["hello"], "empty Space must not duplicate the tap"


def test_hybrid_delivery_semantic_partial_right_notifies_o1_on_true_boundary():
    engine, core = build_engine()
    committed_words = []
    engine._o1 = types.SimpleNamespace(on_word_commit=committed_words.append)
    for letter, word in [("h", "h"), ("e", "he"), ("l", "hel")]:
        core.queue([("hide", ""), ("forward", "")], word)
        assert engine.do_process_key_event(ord(letter), 0, 0) is False

    core.queue([("commit", "l"), ("ghost", "o")], "hell")
    engine.do_process_key_event(ske.IBus.KEY_Right, 0, 0)
    core.queue([("hide", ""), ("forward", "")], "hello")
    engine.do_process_key_event(ord("o"), 0, 0)
    assert committed_words == []

    core.queue([("hide", ""), ("forward", "")], "")
    engine.do_process_key_event(ske.IBus.KEY_space, 0, 0)
    assert committed_words == ["hello"]


@pytest.mark.parametrize("accept_key", [ske.IBus.KEY_Tab, ske.IBus.KEY_Right])
def test_hybrid_delivery_full_accept_still_notifies_o1_once(accept_key):
    engine, core = build_engine()
    committed_words = []
    engine._o1 = types.SimpleNamespace(on_word_commit=committed_words.append)
    show_hello(engine, core)

    core.queue([("hide", ""), ("commit", "hello")], "")
    engine.do_process_key_event(accept_key, 0, 0)
    assert committed_words == ["hello"]
    assert core.recorded == []

    core.queue([("hide", ""), ("forward", "")], "")
    engine.do_process_key_event(ske.IBus.KEY_space, 0, 0)
    assert committed_words == ["hello"]


def test_hybrid_delivery_bg_raw_bracket_is_continuation_until_real_space(monkeypatch):
    engine, core = build_engine()
    committed_words = []
    engine._o1 = types.SimpleNamespace(on_word_commit=committed_words.append)
    monkeypatch.setattr(ske, "_HAS_CORE", True)
    core._predictions = [("зашеметен", 1.0, 0.9)]
    show_hello(engine, core, prefix="за", ghost="шеметен")

    # A BG rendering layout delivers ш from Raw26 even though the original
    # event's EN keyval is '['. That keyval causes the REAL caller to capture
    # a nonempty O1 pre-word, so post-state must disambiguate the fragment.
    core.queue([("commit", "ш")], "заш")
    assert engine.do_process_key_event(ord("["), 26, 0) is True
    assert core.recorded == [], "matching BG typing must not reject its ghost"
    assert committed_words == [], "a raw letter fragment is not a word boundary"

    core.queue([("hide", ""), ("forward", "")], "")
    assert engine.do_process_key_event(ske.IBus.KEY_space, 57, 0) is False
    assert committed_words == ["заш"]


def test_hybrid_delivery_semantic_en_bracket_can_still_end_composed_word():
    engine, core = build_engine()
    committed_words = []
    engine._o1 = types.SimpleNamespace(on_word_commit=committed_words.append)
    core._predictions = [("query", 1.0, 0.9)]
    show_hello(engine, core, prefix="q", ghost="uery")

    # Pure Raw composition followed by a SEMANTIC '[' retains the existing
    # punctuation guard: commit q, reset, forward the bracket. This differs
    # from a consumed Raw26 append in word-local hybrid delivery mode.
    core.queue([("hide", ""), ("commit", "q"), ("forward", "")], "")
    assert engine.do_process_key_event(ord("["), 0, 0) is False
    assert core.recorded == [("q", "query")]
    assert committed_words == ["q"]


@pytest.mark.parametrize("raw_dispatch", [False, True], ids=["keyval", "raw"])
@pytest.mark.parametrize("query_failure", ["raises", "nonstring"])
@pytest.mark.parametrize(
    "accept_key,keycode,commit",
    [
        (ske.IBus.KEY_Tab, 15, "hello"),
        (ske.IBus.KEY_Right, 106, "hello"),
        (ske.IBus.KEY_space, 57, "hello "),
    ],
    ids=["tab", "final-right", "space-accept"],
)
def test_failed_postword_terminal_accept_does_not_reject_prior_prediction(
    monkeypatch, raw_dispatch, query_failure, accept_key, keycode, commit
):
    engine, core = build_engine()
    events = capture_events(engine)
    committed_words = []
    delivered = []
    engine._o1 = types.SimpleNamespace(on_word_commit=committed_words.append)
    engine.commit_text = lambda text: delivered.append(text.s)
    monkeypatch.setattr(ske, "_HAS_CORE", True)
    show_hello(engine, core)
    counter_before = engine._keys_since_content_type

    def current_word():
        if core._word == "":
            if query_failure == "raises":
                raise RuntimeError("post-state unavailable")
            return 42
        return core._word

    monkeypatch.setattr(core, "current_word", current_word)
    # The real dispatcher delivers a terminal accept, but querying its empty
    # post-state fails. This cannot attest acceptance, O1 or privacy resolution.
    core.queue([("hide", ""), ("commit", commit)], "")
    assert engine.do_process_key_event(
        accept_key, keycode if raw_dispatch else 0, 0
    ) is True
    pending_after_accept = engine._active_prediction
    counter_after_accept = engine._keys_since_content_type
    outcomes_after_accept = [
        event for event, _payload in events
        if event in {"accepted", "completed", "rejected"}
    ]

    # Execute a SECOND real event before asserting: the getter recovers when
    # this new word starts, so a stale prediction would now teach rejection.
    core.queue([("forward", "")], "x")
    assert engine.do_process_key_event(ord("x"), 45 if raw_dispatch else 0, 0) is False

    assert core.recorded == [], "accepted prior text must not teach stale rejection"
    assert pending_after_accept is None, "unclassifiable interaction must retire"
    assert outcomes_after_accept == []
    assert not [
        event for event, _payload in events
        if event in {"accepted", "completed", "rejected"}
    ]
    assert committed_words == [], "failed post-state cannot invent an O1 commit"
    assert counter_after_accept == counter_before + 1, "privacy guard stays armed"
    assert delivered == [commit]


@pytest.mark.parametrize("raw_dispatch", [False, True], ids=["keyval", "raw"])
@pytest.mark.parametrize(
    "accept_key,keycode,commit,reason",
    [
        (ske.IBus.KEY_Tab, 15, "hello", "tab"),
        (ske.IBus.KEY_Right, 106, "hello", "right"),
        (ske.IBus.KEY_space, 57, "hello ", "space"),
    ],
    ids=["tab", "final-right", "space-accept"],
)
def test_missing_postword_getter_preserves_legacy_terminal_accept(
    monkeypatch, raw_dispatch, accept_key, keycode, commit, reason
):
    engine, core = build_engine()
    events = capture_events(engine)
    committed_words = []
    engine._o1 = types.SimpleNamespace(on_word_commit=committed_words.append)
    monkeypatch.setattr(ske, "_HAS_CORE", True)
    show_hello(engine, core)
    # Missing capability is the established legacy action-based contract;
    # an existing getter that fails is deliberately treated differently.
    monkeypatch.setattr(core, "current_word", None)
    core.queue([("hide", ""), ("commit", commit)], "")
    engine.do_process_key_event(accept_key, keycode if raw_dispatch else 0, 0)
    assert engine._active_prediction is None
    assert engine._keys_since_content_type == 0
    assert committed_words == ["hello"]
    assert [
        payload["reason"] for event, payload in events if event == "accepted"
    ] == [reason]
    core.queue([("forward", "")], "x")
    engine.do_process_key_event(ord("x"), 45 if raw_dispatch else 0, 0)
    assert core.recorded == []


@pytest.mark.parametrize("raw_dispatch", [False, True], ids=["keyval", "raw"])
@pytest.mark.parametrize("post_query", ["empty", "raises", "nonstring"])
def test_delivered_no_ghost_tab_notifies_o1_once_only_for_known_boundary(
    monkeypatch, raw_dispatch, post_query
):
    engine, core = build_engine()
    committed_words = []
    delivered = []
    engine._o1 = types.SimpleNamespace(on_word_commit=committed_words.append)
    engine.commit_text = lambda text: delivered.append(text.s)
    core._predictions = []
    monkeypatch.setattr(ske, "_HAS_CORE", True)
    # Both letters are already client-owned, with no ghost to accept.
    for letter, word, code in [("q", "q", 16), ("a", "qa", 30)]:
        core.queue([("hide", ""), ("forward", "")], word)
        engine.do_process_key_event(ord(letter), code if raw_dispatch else 0, 0)
    counter_before = engine._keys_since_content_type

    def current_word():
        if core._word == "":
            if post_query == "raises":
                raise RuntimeError("post-state unavailable")
            if post_query == "nonstring":
                return 42
        return core._word

    monkeypatch.setattr(core, "current_word", current_word)
    # Producer contract: finalize/reset a fully delivered word, hide, then
    # forward Tab. Never commit text already owned by the client a second time.
    core.queue([("hide", ""), ("forward", "")], "")
    assert engine.do_process_key_event(
        ske.IBus.KEY_Tab, 15 if raw_dispatch else 0, 0
    ) is False
    assert core._word == ""
    assert delivered == []
    assert core.recorded == []
    if post_query == "empty":
        assert engine._keys_since_content_type == 0
        assert committed_words == ["qa"]
        # An empty forwarded Tab is not a second boundary for the same word.
        core.queue([("forward", "")], "")
        engine.do_process_key_event(ske.IBus.KEY_Tab, 15 if raw_dispatch else 0, 0)
        assert committed_words == ["qa"]
    else:
        assert engine._keys_since_content_type == counter_before + 1
        assert committed_words == [], "failed query cannot attest a boundary"
