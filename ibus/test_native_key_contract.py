"""Raw and semantic key contracts through an explicitly audited PyO3 artifact.

Synthetic words only; the core never loads or saves a personal profile. These
tests validate native API behavior, not physical keyboard or client delivery.
"""

from __future__ import annotations

import json
import os

import pytest

from .test_native_space_accept import ZDRA, _compose, _native

pytestmark = pytest.mark.skipif(
    not os.environ.get("SMARTKEY_NATIVE_MODULE_DIR")
    and os.environ.get("SMARTKEY_NATIVE_AUDIT_REQUIRED") != "1",
    reason="needs explicit native audit artifact",
)

RELEASE = 1 << 30
SUPER_MASKS = (1 << 6, 1 << 26, (1 << 6) | (1 << 26))


def _core(*, space_accept=False):
    config = {
        "ghost_text_separation_margin": 0.0,
        "use_ppm": False,
        "use_reranker": False,
    }
    if space_accept:
        config["accept"] = {"space_accept": True}
    core = _native().PyInputMethodCore(json.dumps(config))
    core.load_word("здравей", 1_000_000)
    return core


def _special(core, api, keyval, keycode, modifiers=0):
    if api == "raw":
        return core.process_keycode(keycode, modifiers)
    return core.handle_key(keyval, modifiers)


@pytest.mark.parametrize("super_mask", SUPER_MASKS, ids=("mod4", "super", "both"))
@pytest.mark.parametrize("api", ("raw", "semantic"))
def test_native_super_escape_toggles_on_press_and_not_release(api, super_mask):
    """Both IBus Super encodings reach the same press-only kill switch."""
    core = _core()
    _compose(core)

    assert _special(core, api, 0xFF1B, 1, super_mask) == [("hide", "")]
    assert core.current_word() == ""
    assert core.process_keycode(ZDRA[0], 0) == [("forward", "")]
    assert core.current_word() == "", "the press must disable composition"

    assert _special(core, api, 0xFF1B, 1, super_mask | RELEASE) == [("forward", "")]
    assert core.process_keycode(ZDRA[0], 0) == [("forward", "")]
    assert core.current_word() == "", "release must leave the engine disabled"

    assert _special(core, api, 0xFF1B, 1, super_mask) == [("hide", "")]
    assert _special(core, api, 0xFF1B, 1, super_mask | RELEASE) == [("forward", "")]
    core.process_keycode(ZDRA[0], 0)
    assert core.current_word() == "з", "the second press must re-enable composition"


@pytest.mark.parametrize("keyval,keycode", ((0xFF09, 15), (0xFF53, 106), (0xFF51, 105)))
def test_native_raw_special_keys_match_semantic_actions_and_following_input(keyval, keycode):
    """Acceptance/navigation must preserve the next-word behavior as well."""
    semantic, raw = _core(), _core()
    _compose(semantic)
    _compose(raw)
    assert raw.process_keycode(keycode, 0) == semantic.handle_key(keyval, 0)
    assert raw.current_word() == semantic.current_word()
    for code in ZDRA:
        assert raw.process_keycode(code, 0) == semantic.process_keycode(code, 0)
        assert raw.current_word() == semantic.current_word()


def test_native_raw_backspace_teaches_the_same_repeated_rejection_as_semantic():
    semantic, raw = _core(), _core()
    _compose(semantic)
    _compose(raw)
    for _ in range(2):
        assert raw.process_keycode(14, 0) == semantic.handle_key(0xFF08, 0)
        assert raw.current_word() == semantic.current_word() == "здр"
        assert raw.process_keycode(ZDRA[-1], 0) == semantic.process_keycode(ZDRA[-1], 0)
        assert raw.current_word() == semantic.current_word() == "здра"


@pytest.mark.parametrize("api", ("raw", "semantic"))
def test_native_backspace_release_cannot_teach_a_ghost_rejection(api):
    with_release, control = _core(), _core()
    _compose(with_release)
    _compose(control)
    assert _special(with_release, api, 0xFF08, 14, RELEASE) == [("forward", "")]
    assert with_release.current_word() == "здра"
    # One explicit adapter feedback call must remain one rejection. A key-up
    # must not add a hidden earlier rejection and prematurely suppress the pair.
    for core in (with_release, control):
        core.record_ghost_rejection("здра", "здравей")
        core.reset()
    for code in ZDRA:
        assert with_release.process_keycode(code, 0) == control.process_keycode(code, 0)


@pytest.mark.parametrize("super_mask", SUPER_MASKS, ids=("mod4", "super", "both"))
@pytest.mark.parametrize("api", ("raw", "semantic"))
def test_native_super_space_stays_literal_with_space_accept_enabled(api, super_mask):
    core = _core(space_accept=True)
    _compose(core)

    actions = _special(core, api, 0x20, 57, super_mask)

    assert ("commit", "здра") in actions
    assert ("forward", "") in actions
    assert not any(kind == "commit" and "здравей" in text for kind, text in actions)
    assert core.current_word() == ""


def _completion_adapter(monkeypatch):
    # Import the bus recorder only after the audited native loader has checked
    # provenance. The adapter and Core are real; only IBus delivery is recorded.
    core = _core()
    from .test_input_safety import build_engine, ske

    engine, recording = build_engine()
    monkeypatch.setattr(ske, "_HAS_CORE", True)
    engine._core = core
    engine._session_id = "native-completion-test"
    return engine, recording, ske


def _adapter_owned_prefix(engine, recording):
    for code, key in zip(ZDRA, "zdra"):
        assert engine.do_process_key_event(ord(key), code, 0) is True
    assert engine._core.current_word() == "здра"
    assert recording.preedits[-1] == ("здравей", True)
    assert recording.commits == []


def test_native_adapter_completion_refinement_stays_client_owned(monkeypatch):
    """Refining the native list must not reintroduce a cancellable preedit."""
    engine, rec, _ske = _completion_adapter(monkeypatch)
    _adapter_owned_prefix(engine, rec)
    assert engine.do_process_key_event(0x20, 57, 4) is False
    engine.do_set_surrounding_text("здра", 4, 4)
    before = len(rec.preedits)

    assert engine.do_process_key_event(ord("w"), 17, 0) is True

    assert rec.commits == ["здра", "в"]
    assert not any(visible for _text, visible in rec.preedits[before:])
    assert engine._core.current_word() == "здрав"


@pytest.mark.parametrize("keyval,keycode", [(ord("."), 52), (ord("."), 0), (ord("1"), 2), (ord("1"), 0)])
def test_native_adapter_completion_delimiter_restores_ordinary_composition(monkeypatch, keyval, keycode):
    engine, rec, _ske = _completion_adapter(monkeypatch)
    _adapter_owned_prefix(engine, rec)
    assert engine.do_process_key_event(0x20, 57, 4) is False
    engine.do_set_surrounding_text("здра", 4, 4)

    assert engine.do_process_key_event(keyval, keycode, 0) is False

    assert engine._core.current_word() == ""
    assert rec.commits == ["здра"]
    assert engine.do_process_key_event(ord("z"), 44, 0) is True
    assert rec.commits == ["здра"], "the new raw word must belong to preedit again"
    assert rec.preedits[-1][1] is True


@pytest.mark.parametrize("text,cursor,anchor", [
    ("здра ", 5, 5), ("здравей", 7, 7), ("аздра", 5, 5),
    ("здра", 4, 0), ("здра", 4, None), (None, None, None),
])
def test_native_adapter_stale_completion_retires_before_o1_capture(
    tmp_path, monkeypatch, text, cursor, anchor,
):
    from .o1_shim import O1Collector

    engine, rec, _ske = _completion_adapter(monkeypatch)
    _adapter_owned_prefix(engine, rec)
    assert engine.do_process_key_event(0x20, 57, 4) is False
    engine._sync_surrounding_text(text, cursor, anchor)
    corpus = tmp_path / "corpus.json"
    corpus.write_text('{"unigrams":{"synthetic":1}}')
    collector = O1Collector(engine._core, {"data_dir": str(tmp_path / "o1")}, [str(corpus)], "test")
    engine._o1 = collector
    collector.on_word_commit("synthetic")
    engine._core.export_personal(str(tmp_path / "before.json"))
    before = json.loads((tmp_path / "before.json").read_text())
    outcomes = []
    engine._log_replay_event = lambda event, **fields: outcomes.append((event, fields))
    try:
        assert engine.do_process_key_event(0x20, 57, 0) is False
        assert engine._core.current_word() == ""
        assert rec.commits == ["здра"]
        assert collector._pending_seq is None
        assert outcomes == []
        engine._core.export_personal(str(tmp_path / "after.json"))
        after = json.loads((tmp_path / "after.json").read_text())
        for key in ("weights", "light_profile", "corrections", "markov_bigrams", "markov_trigrams"):
            assert after[key] == before[key], key
        assert after["cvm"]["words"] == before["cvm"]["words"]
        assert after["cvm"]["round"] == before["cvm"]["round"]
    finally:
        collector.close()


@pytest.mark.parametrize("keyval,keycode,state", [
    (ord("w"), 17, RELEASE), (0xFFE3, 29, 4), (0xFFE1, 42, 1),
])
def test_native_adapter_native_word_survives_nonediting_callbacks(monkeypatch, keyval, keycode, state):
    engine, rec, _ske = _completion_adapter(monkeypatch)
    _adapter_owned_prefix(engine, rec)
    assert engine.do_process_key_event(0x20, 57, 4) is False
    # Handoff deliberately invalidated the old report. Release/modifier must
    # not retire the retained word merely because its commit echo is pending.
    assert engine.do_process_key_event(keyval, keycode, state) is False
    assert engine._core.current_word() == "здра"
    engine.do_set_surrounding_text("здра", 4, 4)
    assert engine.do_process_key_event(ord("w"), 17, 0) is True
    assert rec.commits == ["здра", "в"]


@pytest.mark.parametrize("keycode", [57, 0], ids=["raw", "keyval"])
def test_native_adapter_completion_handoff_delivers_prefix_without_learning(
    tmp_path, monkeypatch, keycode,
):
    """Missing handoff leaves a live preedit; focus_lost would instead learn it."""
    import sqlite3

    from .o1_shim import O1Collector

    engine, rec, _ske = _completion_adapter(monkeypatch)
    # Start with nonempty learned history so equality also protects existing
    # Markov/profile state, not just an empty first-run model.
    for word in ("one", "two", "three", "four", "five"):
        for char in word:
            engine._core.handle_key(ord(char), 0)
        engine._core.handle_key(0x20, 0)
    engine._core.flush_preedit_for_layout()
    _adapter_owned_prefix(engine, rec)
    assert engine._active_prediction is not None
    outcomes = []
    engine._log_replay_event = lambda event, **fields: outcomes.append((event, fields))
    engine._sync_surrounding_text("synthetic context", 17)
    corpus = tmp_path / "corpus.json"
    corpus.write_text('{"unigrams":{"synthetic":1}}')
    collector = O1Collector(
        engine._core, {"data_dir": str(tmp_path / "o1")}, [str(corpus)], "test",
    )
    engine._o1 = collector
    collector.on_word_commit("synthetic")
    engine._core.export_personal(str(tmp_path / "before.json"))
    before = json.loads((tmp_path / "before.json").read_text())
    assert before["markov_bigrams"] and before["markov_trigrams"]
    try:
        assert engine.do_process_key_event(0x20, keycode, 4) is False
        assert rec.commits == ["здра"], "unaccepted ghost must never be delivered"
        assert rec.preedits[-1] == ("", False)
        assert engine._core.current_word() == "здра"
        assert engine._active_prediction is None
        assert engine._surrounding_text is None
        assert engine._surrounding_cursor_pos is None
        assert engine._keys_since_content_type == 0
        assert collector._pending_seq is None
        assert outcomes == [], "delivery is neither prediction acceptance nor rejection"
        # Repeats, release, then ordinary Space must not recommit or lazily
        # learn the delivered prefix. The original shortcut is forwarded by
        # the False return, never re-emitted through IBus.forward_key_event.
        assert engine.do_process_key_event(0x20, keycode, 4) is False
        assert engine.do_process_key_event(0x20, keycode, 4 | RELEASE) is False
        assert engine.do_process_key_event(0x20, keycode, 0) is False
        assert rec.commits == ["здра"]
        assert rec.forwarded_keys == []
        engine._core.export_personal(str(tmp_path / "after.json"))
        after = json.loads((tmp_path / "after.json").read_text())
        for key in ("weights", "light_profile", "corrections", "markov_bigrams", "markov_trigrams"):
            assert after[key] == before[key], key
        assert after["cvm"]["words"] == before["cvm"]["words"]
        assert after["cvm"]["round"] == before["cvm"]["round"]
    finally:
        collector.close()
    with sqlite3.connect(collector._db_path) as conn:
        assert conn.execute("SELECT outcome FROM events ORDER BY id").fetchall() == [(None,)]


def test_native_adapter_completion_handoff_does_not_repeat_already_delivered_semantic_text(monkeypatch):
    engine, rec, _ske = _completion_adapter(monkeypatch)
    for key in "hel":
        assert engine.do_process_key_event(ord(key), 0, 0) is False
    assert engine._core.current_word() == "hel"
    assert rec.commits == []

    assert engine.do_process_key_event(0x20, 0, 4) is False

    assert engine._core.current_word() == "hel"
    assert rec.commits == [], "semantic characters already belong to the client"


@pytest.mark.parametrize("report,expected", [
    (("здра", 4, 4), "здра"),
    (("здрасинтетична", 13, 13), ""),
    (None, ""),
    (("здра", 4, 0), ""),
    (("здра", 2, 2), ""),
    (("здра ", 5, 5), ""),
], ids=["matching", "replaced", "missing", "selection", "moved", "boundary"])
def test_native_adapter_repeated_completion_reconciles_before_handoff(
    tmp_path, monkeypatch, report, expected,
):
    """A repeat must retire a stale token before forwarding the shortcut."""
    engine, rec, _ske = _completion_adapter(monkeypatch)
    _adapter_owned_prefix(engine, rec)
    assert engine.do_process_key_event(0x20, 57, 4) is False
    engine._core.export_personal(str(tmp_path / "before.json"))
    if report is not None:
        engine.do_set_surrounding_text(*report)

    assert engine.do_process_key_event(0x20, 57, 4) is False

    assert engine._core.current_word() == expected
    assert rec.commits == ["здра"], "a repeat cannot deliver the prefix twice"
    assert rec.forwarded_keys == [], "False forwards the original shortcut"
    engine._core.export_personal(str(tmp_path / "after.json"))
    before = json.loads((tmp_path / "before.json").read_text())
    after = json.loads((tmp_path / "after.json").read_text())
    for key in ("weights", "light_profile", "corrections", "markov_bigrams", "markov_trigrams"):
        assert after[key] == before[key], key
    assert after["cvm"]["words"] == before["cvm"]["words"]
    assert after["cvm"]["round"] == before["cvm"]["round"]


@pytest.mark.parametrize("separator,initial,after,collapsed,retained", [
    ("_", "foo_bar", "foo_ba", True, True),
    ("1", "foo1bar", "foo1ba", True, True),
    ("_", "foo_bar", "x_ba", True, False),
    ("_", None, "foo_ba", True, False),
    ("_", "foobar", "fooba", True, False),
    ("_", "foo_bar", "foo_ba", False, False),
], ids=["underscore", "digit", "changed-prefix", "unknown", "alphabetic", "selection"])
def test_native_adapter_identifier_handoff_uses_observed_token_anchor(
    monkeypatch, separator, initial, after, collapsed, retained,
):
    """Raw delimiter tokenization and adapter caret metadata must agree."""
    engine, rec, _ske = _completion_adapter(monkeypatch)
    engine._core.load_word("food", 1_000_000)
    engine._core.load_word("bar", 1_000_000)
    for code, key in ((33, "f"), (24, "o"), (24, "o")):
        assert engine.do_process_key_event(ord(key), code, 0) is True
    assert engine._core.current_word() == "foo"
    assert engine.do_process_key_event(
        ord(separator), 12 if separator == "_" else 2, 1 if separator == "_" else 0,
    ) is False
    for code, key in ((48, "b"), (30, "a")):
        assert engine.do_process_key_event(ord(key), code, 0) is True
    assert engine._core.current_word() == "ba"
    assert rec.preedits[-1] == ("bar", True)
    assert rec.commits == ["foo"]
    if initial is not None:
        cursor = len(initial) - 1  # The visible unaccepted ghost is the final r.
        engine.do_set_surrounding_text(initial, cursor, cursor if collapsed else 0)
    assert engine.do_process_key_event(0x20, 57, 4) is False
    assert rec.commits == ["foo", "ba"]
    engine.do_set_surrounding_text(after, len(after), len(after))
    before = len(rec.preedits)

    assert engine.do_process_key_event(ord("r"), 19, 0) is True

    assert engine._core.current_word() == ("bar" if retained else "r")
    assert rec.commits == (["foo", "ba", "r"] if retained else ["foo", "ba"])
    assert any(visible for _text, visible in rec.preedits[before:]) is not retained


@pytest.mark.parametrize("keyval,keycode,state", [
    (0x20, 57, 4 | RELEASE), (0xFFE3, 29, 4),
    (ord("a"), 30, 4), (0x20, 57, 4 | 1), (0x20, 57, 4 | 8),
    (0x20, 57, 4 | (1 << 6)), (0x20, 57, 4 | (1 << 26)),
    (0x20, 57, 4 | (1 << 5)), (0x20, 57, 4 | (1 << 7)),
    (0x20, 57, 4 | (1 << 27)), (0x20, 57, 4 | (1 << 28)),
])
def test_native_adapter_other_shortcuts_preserve_owned_composition(monkeypatch, keyval, keycode, state):
    engine, rec, _ske = _completion_adapter(monkeypatch)
    _adapter_owned_prefix(engine, rec)

    assert engine.do_process_key_event(keyval, keycode, state) is False

    assert engine._core.current_word() == "здра"
    assert rec.commits == []


@pytest.mark.parametrize("locks", [0, 2, 16, 18])
def test_native_adapter_completion_ignores_caps_and_numlock(monkeypatch, locks):
    engine, rec, _ske = _completion_adapter(monkeypatch)
    _adapter_owned_prefix(engine, rec)

    assert engine.do_process_key_event(0x20, 57, 4 | locks) is False

    assert rec.commits == ["здра"]
    assert engine._core.current_word() == "здра"


class _OptionalNativeAPIs:
    """Expose an older Core's optional API surface, keep all other native work."""

    def __init__(self, core, *, raw=True, flush=True):
        self.core = core
        self.raw = raw
        self.flush = flush

    def __getattr__(self, name):
        if (name == "process_keycode" and not self.raw
                or name in ("handoff_preedit_for_native_completion", "reconcile_native_completion_word")
                and not self.flush):
            raise AttributeError(name)
        return getattr(self.core, name)


@pytest.mark.parametrize("raw,keyval,keycode,commits,word", [
    (True, ord("a"), 57, ["здра"], "здра"),
    (True, 0x20, 30, [], "здра"),
    (False, 0x20, 30, ["здра"], "здра"),
    (False, ord("a"), 57, [], "здра"),
])
def test_native_adapter_completion_space_identity_follows_actual_dispatch(
    monkeypatch, raw, keyval, keycode, commits, word,
):
    engine, rec, _ske = _completion_adapter(monkeypatch)
    _adapter_owned_prefix(engine, rec)
    engine._core = _OptionalNativeAPIs(engine._core, raw=raw)

    assert engine.do_process_key_event(keyval, keycode, 4) is False

    assert rec.commits == commits
    assert engine._core.current_word() == word


def test_native_adapter_older_core_without_handoff_keeps_existing_forward_route(monkeypatch):
    engine, rec, _ske = _completion_adapter(monkeypatch)
    _adapter_owned_prefix(engine, rec)
    engine._core = _OptionalNativeAPIs(engine._core, flush=False)

    assert engine.do_process_key_event(0x20, 57, 4) is False

    assert rec.commits == []
    assert engine._core.current_word() == "здра"


def test_native_adapter_completion_retires_unselected_language_choice(monkeypatch):
    engine, rec, _ske = _completion_adapter(monkeypatch)
    engine._core.set_language_choice_supported(True)
    for code in (16, 45, 44):
        engine._execute_actions(engine._core.process_keycode(code, 0))
    engine._language_choice_offer = engine._core.language_choice_offer()
    assert engine._language_choice_offer is not None
    typed = engine._core.current_word()
    engine._language_choice_selected = 1

    assert engine.do_process_key_event(0x20, 57, 4) is False

    assert rec.commits == [typed]
    assert engine._language_choice_offer is None
    assert engine._language_choice_selected is None
    assert engine._core.language_choice_offer() is None


def test_native_adapter_completion_handoff_respects_sensitive_guard(monkeypatch):
    engine, rec, _ske = _completion_adapter(monkeypatch)
    _adapter_owned_prefix(engine, rec)
    engine._sensitive = True

    assert engine.do_process_key_event(0x20, 57, 4) is False

    assert engine._core.current_word() == "здра"
    assert rec.commits == []


@pytest.mark.parametrize("level", [1, 2], ids=["structural", "full"])
def test_native_adapter_completion_handoff_trace_records_forwarded_delivery(tmp_path, monkeypatch, level):
    """An early return must not leave a trace sequence with only key_in."""
    from .smartkey_debug import KeystrokeTrace

    engine, rec, _ske = _completion_adapter(monkeypatch)
    _adapter_owned_prefix(engine, rec)
    trace = KeystrokeTrace(level=level, directory=tmp_path / "trace")
    engine._trace = trace

    assert engine.do_process_key_event(0x20, 57, 4) is False

    assert rec.commits == ["здра"]
    trace.flush()
    raw = trace._file.read_text()
    events = [json.loads(line) for line in raw.splitlines()]
    assert {event["seq"] for event in events} == {1}
    assert {"key_in", "core", "action", "outcome"} <= {event["event"] for event in events}
    assert not any(event["event"] == "accept" for event in events), (
        "handing off the typed prefix must not claim prediction acceptance"
    )
    verdicts = [event["verdict"] for event in events if event["event"] == "core"]
    assert verdicts == ["forward"], "delivery actions must not label the shortcut consumed"
    actions = [event for event in events if event["event"] == "action"]
    assert [(event["action_name"], event["payload_len"]) for event in actions] == [("hide", 0), ("commit", 4)]
    outcome = [event for event in events if event["event"] == "outcome"]
    assert len(outcome) == 1 and outcome[0]["committed_to_app_len"] == 4
    if level == 2:
        assert actions[1]["payload_text"] == outcome[0]["committed_to_app"] == "здра"
    else:
        assert "здра" not in raw and "здравей" not in raw
        assert all("payload_text" not in event for event in actions)
        assert "committed_to_app" not in outcome[0]


@pytest.mark.parametrize("level", [1, 2], ids=["structural", "full"])
def test_native_adapter_explicit_completion_trace_keeps_accept_event(tmp_path, monkeypatch, level):
    from .smartkey_debug import KeystrokeTrace

    engine, rec, _ske = _completion_adapter(monkeypatch)
    _adapter_owned_prefix(engine, rec)
    trace = KeystrokeTrace(level=level, directory=tmp_path / "trace")
    engine._trace = trace

    assert engine.do_process_key_event(0xFF09, 15, 0) is True

    assert rec.commits == ["здравей"]
    trace.flush()
    events = [json.loads(line) for line in trace._file.read_text().splitlines()]
    accepted = [event for event in events if event["event"] == "accept"]
    assert len(accepted) == 1
    if level == 2:
        assert accepted[0]["committed_text"] == "здравей"
