"""Native learning needs language evidence; OOV manual input stays usable.

Synthetic words and an explicitly audited artifact only. Profiles exported by
these tests go to pytest's private temporary directory, never the user profile.
"""
from __future__ import annotations

import json
import os

import pytest

from .test_native_space_accept import _compose, _native

pytestmark = pytest.mark.skipif(
    not os.environ.get("SMARTKEY_NATIVE_MODULE_DIR")
    and os.environ.get("SMARTKEY_NATIVE_AUDIT_REQUIRED") != "1",
    reason="needs explicit native audit artifact",
)


def _core():
    core = _native().PyInputMethodCore(json.dumps({
        "ghost_text_separation_margin": 0.0,
        "use_ppm": False,
        "use_reranker": False,
    }))
    core.load_word("hello", 1_000_000)
    return core


def _profile(core, tmp_path, name):
    path = tmp_path / name
    core.export_personal(str(path))
    return json.loads(path.read_text())["light_profile"]


def _unsupported_prefix(core):
    # Neither physical interpretation is loaded; do not assume which script
    # the dual buffer chooses for an unsupported synthetic word.
    for code in (45, 16, 44):
        core.process_keycode(code, 0)
    assert core.current_word()


def _literal_boundary(core):
    actions = core.process_keycode(57, 0)
    assert any(kind == "commit" and text for kind, text in actions)
    assert ("forward", "") in actions
    assert core.current_word() == ""


def test_native_unsupported_manual_boundary_does_not_manufacture_language_prior(tmp_path):
    core = _core()
    before = _profile(core, tmp_path, "before.json")
    _unsupported_prefix(core)

    _literal_boundary(core)

    after = _profile(core, tmp_path, "after.json")
    assert after["lang_prior"] == before["lang_prior"]


def test_native_unsupported_focus_flush_does_not_manufacture_language_prior(tmp_path):
    core = _core()
    before = _profile(core, tmp_path, "before.json")
    _unsupported_prefix(core)
    actions = core.focus_lost()
    assert any(kind == "commit" and text for kind, text in actions)
    assert core.current_word() == ""
    assert _profile(core, tmp_path, "after-focus.json")["lang_prior"] == before["lang_prior"]
    core.focus_gained()
    # An empty following boundary must not lazily attribute the old flush as
    # language evidence when the next ordinary key enters MasterLoop again.
    assert ("forward", "") in core.process_keycode(57, 0)
    assert _profile(core, tmp_path, "after-next-boundary.json")["lang_prior"] == before["lang_prior"]


def test_native_supported_english_and_bulgarian_words_still_update_language_prior(tmp_path):
    core = _core()
    core.load_word("здравей", 1_000_000)
    for index, (word, codes, language_index) in enumerate((
        ("hello", (35, 18, 38, 38, 24), 1),
        ("здравей", (44, 32, 19, 30, 17, 18, 36), 0),
    )):
        before = _profile(core, tmp_path, f"before-{index}.json")
        for code in codes:
            core.process_keycode(code, 0)
        assert core.current_word() == word
        _literal_boundary(core)
        after = _profile(core, tmp_path, f"after-{index}.json")
        assert after["lang_prior"][language_index] > before["lang_prior"][language_index]


def test_native_fresh_completion_remains_predictable_after_unsupported_commit():
    core = _core()
    _unsupported_prefix(core)
    _literal_boundary(core)
    core.load_word("здравей", 1_000_000)

    _compose(core)

    assert core.predictions()[0][0] == "здравей"


def test_native_full_acceptance_remains_independent_of_manual_evidence_policy(tmp_path):
    core = _core()
    core.load_word("здравей", 1_000_000)
    before = _profile(core, tmp_path, "before.json")
    _compose(core)

    actions = core.handle_key(0xFF09, 0)

    assert actions.count(("commit", "здравей")) == 1
    assert ("forward", "") not in actions
    assert core.current_word() == ""
    after = _profile(core, tmp_path, "after.json")
    assert after["ghost_accept_rate"] > before["ghost_accept_rate"]
    assert after["lang_prior"][0] > before["lang_prior"][0]


@pytest.mark.parametrize("token,boundary", [("___", 0x20), ("123", 0xFF0D)])
def test_native_observed_technical_token_retains_language_adaptation(tmp_path, token, boundary):
    core = _core()
    before = _profile(core, tmp_path, "before-tech.json")
    for char in token:
        core.handle_key(ord(char), 0)
    assert core.current_word() == token

    actions = core.handle_key(boundary, 0)

    assert core.current_word() == ""
    assert actions.count(("forward", "")) == 1
    after = _profile(core, tmp_path, "after-tech.json")
    assert after["lang_prior"][2] > before["lang_prior"][2]
    assert after["ghost_accept_rate"] == before["ghost_accept_rate"]


def test_native_nonzero_shared_hardware_digits_supply_direct_tech_evidence(tmp_path):
    core = _native().PyInputMethodCore(json.dumps({"ghost_text": False}))
    before = _profile(core, tmp_path, "before-hardware-tech.json")
    for code in (2, 3, 4):
        core.process_keycode(code, 0)
    assert core.current_word() == "123"

    actions = core.process_keycode(57, 0)

    assert core.current_word() == ""
    assert actions.count(("forward", "")) == 1
    after = _profile(core, tmp_path, "after-hardware-tech.json")
    assert after["lang_prior"][2] == pytest.approx(0.3434)
    assert after["lang_prior"][2] > before["lang_prior"][2]
    assert after["ghost_accept_rate"] == before["ghost_accept_rate"]


@pytest.mark.parametrize("case", ["deleted-inferred-raw", "deleted-script", "deleted-entire-prefix"])
def test_native_backspace_restores_surviving_technical_evidence(tmp_path, case):
    core = _native().PyInputMethodCore(json.dumps({
        "ghost_text": False,
        "dual_buffer": {"enabled": False},
    }))
    before = _profile(core, tmp_path, "before-retraction.json")
    if case == "deleted-inferred-raw":
        core.handle_key(ord("_"), 0)
        core.process_keycode(26, 0)  # EN '[' / BG 'ш', inferred with composition off
        core.handle_key(0xFF08, 0)
        for _ in range(2):
            core.handle_key(ord("_"), 0)
        expected, boundary = "___", 0x20
    elif case == "deleted-script":
        for char in "123a":
            core.handle_key(ord(char), 0)
        core.handle_key(0xFF08, 0)
        expected, boundary = "123", 0xFF0D
    else:
        core.handle_key(ord("a"), 0)
        core.handle_key(0xFF08, 0)
        for char in "123":
            core.handle_key(ord(char), 0)
        expected, boundary = "123", 0x20
    assert core.current_word() == expected

    actions = core.handle_key(boundary, 0)

    assert core.current_word() == ""
    assert actions.count(("forward", "")) == 1
    after = _profile(core, tmp_path, "after-retraction.json")
    assert after["lang_prior"][2] == pytest.approx(0.3434)
    assert after["lang_prior"][2] > before["lang_prior"][2]
    assert after["ghost_accept_rate"] == before["ghost_accept_rate"]


def _shortword_core(context):
    core = _native().PyInputMethodCore()
    core.load_word("like", 2_570_395)
    core.load_word("li", 17_782)
    core.load_word("ли", 2_511_886)
    if context is not None:
        core.set_surrounding_text(context, len(context))
    return core


@pytest.mark.parametrize("context,expected", [
    ("има ", "ли"),
    (None, "li"),
    ("hello ", "li"),
    ("има hello ", "li"),
])
def test_native_shortword_soft_context_selects_live_reading_before_literal_space(tmp_path, context, expected):
    core = _shortword_core(context)
    before = _profile(core, tmp_path, "before-shortword.json")
    core.process_keycode(38, 0)
    actions = core.process_keycode(23, 0)
    assert core.current_word() == expected
    assert list(core.debug_state()) == [True, False, True]
    assert any(kind == "composing" for kind, _ in actions)

    actions = core.process_keycode(57, 0)

    assert actions.count(("commit", expected)) == 1
    assert actions.count(("forward", "")) == 1
    assert not any(kind == "replace" for kind, _ in actions)
    assert core.current_word() == ""
    after = _profile(core, tmp_path, "after-shortword.json")
    assert after["ghost_accept_rate"] == before["ghost_accept_rate"]
    core.process_keycode(57, 0)
    assert _profile(core, tmp_path, "after-empty-boundary.json")["lang_prior"] == after["lang_prior"]


def test_native_shortword_english_continuation_overcomes_bg_context():
    core = _shortword_core("има ")
    for code in (38, 23):
        core.process_keycode(code, 0)
    assert core.current_word() == "ли"
    for code in (37, 18):
        core.process_keycode(code, 0)
    assert core.current_word() == "like"
    actions = core.process_keycode(57, 0)
    assert actions.count(("commit", "like")) == 1
    assert actions.count(("forward", "")) == 1


_TERMINAL_BG_CONTEXT = "Искам да благодаря "


def _terminal_core():
    # Public reduced fixture; the actual copied-corpus replay is a separate gate.
    core = _native().PyInputMethodCore()
    core.load_word("with", 7_079_457)
    core.load_word("wi", 12_302)
    core.load_word("ви", 2_137_962)
    core.set_surrounding_text(_TERMINAL_BG_CONTEXT, len(_TERMINAL_BG_CONTEXT))
    return core


def _terminal_type_wi(core):
    for code in (17, 23):
        core.process_keycode(code, 0)
    assert core.current_word() == "wi"
    assert list(core.debug_state()) == [True, False, True]


def _terminal_assert_boundary(core, code, expected):
    actions = core.process_keycode(code, 0)
    assert [text for kind, text in actions if kind == "commit"] == [expected], actions
    assert actions.count(("forward", "")) == 1
    assert not any(kind == "replace" for kind, _ in actions)
    assert core.current_word() == ""


@pytest.mark.parametrize("boundary", [57, 28])
def test_native_terminal_context_uses_exact_word_without_accepting_ghost(tmp_path, boundary):
    core = _terminal_core()
    before = _profile(core, tmp_path, "before-terminal.json")
    _terminal_type_wi(core)

    _terminal_assert_boundary(core, boundary, "ви")

    after = _profile(core, tmp_path, "after-terminal.json")
    assert after["ghost_accept_rate"] == before["ghost_accept_rate"]
    assert after["lang_prior"][0] == pytest.approx(0.3434)
    core.process_keycode(57, 0)
    core.focus_lost()
    assert _profile(core, tmp_path, "after-terminal-empty.json")["lang_prior"] == after["lang_prior"]


@pytest.mark.parametrize("text,cursor,expected", [
    (None, None, "wi"),
    ("I want to thank ", len("I want to thank "), "wi"),
    (_TERMINAL_BG_CONTEXT, len(_TERMINAL_BG_CONTEXT), "ви"),
    (_TERMINAL_BG_CONTEXT, None, "wi"),
    (_TERMINAL_BG_CONTEXT, len(_TERMINAL_BG_CONTEXT) + 1, "wi"),
    (_TERMINAL_BG_CONTEXT, 0, "wi"),
])
def test_native_terminal_context_refresh_replaces_only_terminal_authority(text, cursor, expected):
    core = _terminal_core()
    _terminal_type_wi(core)

    core.set_surrounding_text(text, cursor)

    _terminal_assert_boundary(core, 57, expected)


@pytest.mark.parametrize("withdraw", [False, True])
def test_native_terminal_context_erase_retype_uses_current_client_cache(withdraw):
    core = _terminal_core()
    _terminal_type_wi(core)
    core.process_keycode(14, 0)
    core.process_keycode(14, 0)
    assert core.current_word() == ""
    if withdraw:
        core.set_surrounding_text(None, None)

    _terminal_type_wi(core)

    _terminal_assert_boundary(core, 57, "wi" if withdraw else "ви")


# The one-sided inferred prefix now recovers before the semantic handoff;
# preserve that complete displayed BG word plus the exact literal suffix.
@pytest.mark.parametrize("fixture,expected", [("both-supported", "wix"), ("one-sided", "статияx")])
def test_native_terminal_resolution_keeps_direct_semantic_suffix(fixture, expected):
    core = _terminal_core() if fixture == "both-supported" else _native().PyInputMethodCore()
    if fixture == "one-sided":
        for word, frequency in (("status", 900_000), ("static", 600_000), ("station", 500_000), ("статия", 50_000)):
            core.load_word(word, frequency)
    engine, client, preedit = _hybrid_client(core)
    codes = (17, 23) if fixture == "both-supported" else (31, 20, 30, 20, 23, 16)
    for code in codes:
        _hybrid_step(core, engine, client, preedit, "raw", code)
    assert core.current_word() == expected[:-1]
    if fixture == "one-sided":
        assert list(core.debug_state()) == [True, True, False]
    _hybrid_step(core, engine, client, preedit, "char", ord("x"))
    assert core.current_word() == expected and client.text == expected and preedit[0] == ""
    boundary = _hybrid_step(core, engine, client, preedit, "raw", 57)
    assert client.text == expected + " " and core.current_word() == ""
    assert not any(kind == "commit" for kind, _ in boundary["actions"])
    assert sum(kind == "forward" for kind, _ in boundary["actions"]) == 1


@pytest.mark.parametrize("context,expected", [(_TERMINAL_BG_CONTEXT, "но"), (None, "no")])
def test_native_no_known_bg_context_and_observational_control(context, expected):
    # Reduced verified current JSON frequencies; copied-corpus replay and the
    # historical invocation/GUI are separate evidence boundaries.
    core = _native().PyInputMethodCore()
    for word, frequency in (("no", 2_238_721), ("not", 4_897_788), ("но", 3_630_780)):
        core.load_word(word, frequency)
    if context is not None:
        core.set_surrounding_text(context, len(context))
    readings = []
    for code in (49, 24):
        actions = core.process_keycode(code, 0)
        word = core.current_word()
        assert any(kind == "composing" and text.split("\x00", 1)[0] == word for kind, text in actions)
        assert not any(kind in {"commit", "forward", "replace"} for kind, _ in actions)
        readings.append(word)

    _terminal_assert_boundary(core, 57, expected)

    assert readings[-1] == expected
    if context is not None:
        assert readings[0] == "н"


_HYBRID_SCENARIOS = {
    "raw_suffix": ([("raw", 16), ("char", ord("a")), ("raw", 45)], "qax"),
    "one_raw_backspace": ([("raw", 16), ("char", ord("a")), ("char", 0xFF08)], "q"),
    "two_raw_backspace": ([("raw", 16), ("raw", 45), ("char", ord("a")), ("char", 0xFF08)], "qx"),
    "direct_prefix": ([("char", ord("a")), ("raw", 16)], "aq"),
    "immediate_space": ([("raw", 16), ("char", ord("a"))], "qa"),
    "raw_only": ([("raw", 16), ("raw", 45), ("char", 0xFF08)], "q"),
    "direct_only": ([("char", ord("q")), ("char", ord("a"))], "qa"),
    "unicode_mixed": ([("char", ord("é")), ("char", ord("д")), ("char", 0xFF08), ("char", ord("я"))], "éя"),
}


def _hybrid_client(core):
    # Actual dispatcher with fake IBus, audited native core and ORIGINAL keyvals.
    from .test_space_accept import build_client_engine

    engine, recorder, client = build_client_engine([])
    engine._core = core
    preedit = [""]

    def show(text, _cursor, visible):
        recorder.preedits.append((text.s, visible))
        preedit[0] = text.s if visible else ""

    engine.update_preedit_text = show
    engine.hide_preedit_text = lambda: preedit.__setitem__(0, "")
    return engine, client, preedit


def _hybrid_step(core, engine, client, preedit, kind, code, original_keyval=None, modifiers=0):
    actions = core.process_keycode(code, modifiers) if kind == "raw" else core.handle_key(code, modifiers)
    assert not any(action == "replace" for action, _ in actions), actions
    consumed = engine._execute_actions(actions)
    if original_keyval is None:
        original_keyval = {16: ord("q"), 17: ord("w"), 20: ord("t"), 23: ord("i"), 30: ord("a"), 31: ord("s"), 45: ord("x"), 57: 0x20}[code] if kind == "raw" else code
        if kind == "char" and 0x80 <= code < 0xF000:
            # Actual Unicode-encoded IBus originals normalize to native Char.
            original_keyval = 0x01000000 | code
    if not consumed:
        client.apply_key(original_keyval, modifiers, "default")
    return {"kind": kind, "key": code, "original_keyval": original_keyval,
            "word": core.current_word(), "actions": actions,
            "committed": client.text, "preedit": preedit[0]}


def _hybrid_run(scenario):
    core = _native().PyInputMethodCore(json.dumps({"ghost_text": False}))
    engine, client, preedit = _hybrid_client(core)
    keys, expected = _HYBRID_SCENARIOS[scenario]
    trace = []
    before_boundary = None
    for kind, code in [*keys, ("raw", 57)]:
        word_before = core.current_word()
        trace.append(_hybrid_step(core, engine, client, preedit, kind, code))
        if (kind, code) == ("raw", 57):
            before_boundary = word_before
            assert core.current_word() == "" and preedit[0] == "", trace
            assert sum(action == "forward" for action, _ in trace[-1]["actions"]) == 1, trace
    print(json.dumps({"scenario": scenario, "trace": trace}, ensure_ascii=False))
    return before_boundary, client.text, expected, trace


@pytest.mark.parametrize("scenario", list(_HYBRID_SCENARIOS)[:4])
def test_native_hybrid_canonical_conservation(scenario):
    canonical, _delivered, expected, trace = _hybrid_run(scenario)
    assert canonical == expected, trace


@pytest.mark.parametrize("scenario", list(_HYBRID_SCENARIOS))
def test_native_hybrid_client_delivery(scenario):
    _canonical, delivered, expected, trace = _hybrid_run(scenario)
    assert delivered == expected + " ", trace



def test_native_hybrid_snapshot_and_semantic_prefix_default(tmp_path):
    core = _native().PyInputMethodCore(json.dumps({"ghost_text": False}))
    core.load_word("яяя", 1000)
    before = _profile(core, tmp_path, "snapshot-before.json")
    engine, client, preedit = _hybrid_client(core)
    _hybrid_step(core, engine, client, preedit, "raw", 16)
    _hybrid_step(core, engine, client, preedit, "char", ord("a"))
    engine._keys_since_content_type = 2
    raw = _hybrid_step(core, engine, client, preedit, "raw", 26, original_keyval=ord("["), modifiers=2)
    assert raw["actions"][0] == ("commit", "Ш") and ("forward", "") not in raw["actions"]
    assert core.current_word() == client.text == "яaШ"
    assert engine._keys_since_content_type == 2
    assert _profile(core, tmp_path, "snapshot-mid.json") == before
    _hybrid_step(core, engine, client, preedit, "raw", 57)
    assert client.text == "яaШ "
    assert _profile(core, tmp_path, "snapshot-after.json")["lang_prior"] == before["lang_prior"]

    core = _native().PyInputMethodCore(json.dumps({"ghost_text": False}))
    engine, client, preedit = _hybrid_client(core)
    _hybrid_step(core, engine, client, preedit, "char", ord("а"))
    raw = _hybrid_step(core, engine, client, preedit, "raw", 16, original_keyval=ord("я"))
    assert core.current_word() == client.text == "аq"
    assert ("commit", "q") in raw["actions"] and ("forward", "") not in raw["actions"]


def test_native_hybrid_handoff_hides_unshown_acceptance(tmp_path):
    core = _native().PyInputMethodCore(json.dumps({"ghost_text": True, "accept": {"space_accept": True}}))
    core.load_word("qabc", 1_000_000)
    before = _profile(core, tmp_path, "hidden-before.json")
    engine, client, preedit = _hybrid_client(core)
    _hybrid_step(core, engine, client, preedit, "raw", 16)
    engine._keys_since_content_type = 1
    handoff = _hybrid_step(core, engine, client, preedit, "char", ord("a"))
    assert client.text == core.current_word() == "qa" and preedit[0] == ""
    assert not any(kind in {"ghost", "composing", "replace"} for kind, _ in handoff["actions"])
    assert engine._keys_since_content_type == 1
    assert _profile(core, tmp_path, "hidden-mid.json") == before
    boundary = _hybrid_step(core, engine, client, preedit, "raw", 57)
    assert client.text == "qa " and core.current_word() == ""
    assert not any(kind == "commit" for kind, _ in boundary["actions"])
    assert sum(kind == "forward" for kind, _ in boundary["actions"]) == 1


def test_native_hybrid_partial_right_and_full_accept_deliver_suffix_once():
    for partial in (False, True):
        core = _native().PyInputMethodCore()
        core.load_word("qaxyz", 1_000_000)
        engine, client, preedit = _hybrid_client(core)
        for kind, code in (("raw", 16), ("char", ord("a")), ("raw", 45)):
            _hybrid_step(core, engine, client, preedit, kind, code)
        assert core.current_word() == client.text == "qax"
        if partial:
            _hybrid_step(core, engine, client, preedit, "char", 0xFF53)
            assert core.current_word() == client.text == "qaxy"
        _hybrid_step(core, engine, client, preedit, "char", 0xFF53 if partial else 0xFF09)
        assert core.current_word() == "" and client.text == "qaxyz"
        boundary = _hybrid_step(core, engine, client, preedit, "raw", 57)
        assert client.text == "qaxyz "
        assert not any(kind == "commit" for kind, _ in boundary["actions"])


def test_native_hybrid_cancel_focus_and_navigation_never_redeliver_prefix():
    for exit_kind in ("reset", "focus", "navigation"):
        core = _native().PyInputMethodCore(json.dumps({"ghost_text": False}))
        engine, client, preedit = _hybrid_client(core)
        _hybrid_step(core, engine, client, preedit, "raw", 16)
        _hybrid_step(core, engine, client, preedit, "char", ord("a"))
        if exit_kind == "reset":
            actions = core.reset()
        elif exit_kind == "focus":
            actions = core.focus_lost()
        else:
            actions = core.handle_key(0xFF51, 0)
        assert not any(kind == "commit" for kind, _ in actions)
        engine._execute_actions(actions)
        assert client.text == "qa" and core.current_word() == ""
        restarted = _hybrid_step(core, engine, client, preedit, "raw", 16)
        assert any(kind == "composing" for kind, _ in restarted["actions"])


@pytest.mark.parametrize("space_kind,space_code", [("raw", 57), ("char", 0x20)])
def test_native_hybrid_space_regained_ghost_stays_literal(space_kind, space_code):
    # A regained visible ghost cannot re-enable Space acceptance after delivery
    # handoff. Pure raw input proves the nested opt-in flag really is enabled.
    for hybrid in (True, False):
        core = _native().PyInputMethodCore(json.dumps({
            "ghost_text": True, "accept": {"space_accept": True},
        }))
        core.load_word("qaxyz", 1_000_000)
        engine, client, preedit = _hybrid_client(core)
        trace = []
        for kind, code in (("raw", 16), ("char", ord("a")) if hybrid else ("raw", 30), ("raw", 45)):
            trace.append(_hybrid_step(core, engine, client, preedit, kind, code))
        assert core.current_word() == "qax" and core.predictions()[0][0].lower() == "qaxyz", trace
        expected_display = ("ghost", "yz") if hybrid else ("composing", "qax\x00yz")
        assert expected_display in trace[-1]["actions"], trace
        assert core.debug_state()[0] is (not hybrid)
        assert client.text == ("qax" if hybrid else ""), trace
        boundary = _hybrid_step(core, engine, client, preedit, space_kind, space_code)
        commits = [text for kind, text in boundary["actions"] if kind == "commit"]
        assert commits == ([] if hybrid else ["qaxyz "]), [*trace, boundary]
        assert sum(kind == "forward" for kind, _ in boundary["actions"]) == int(hybrid)
        assert client.text == ("qax " if hybrid else "qaxyz "), [*trace, boundary]
        assert core.current_word() == "" and not core.predictions() and preedit[0] == ""
        following = _hybrid_step(core, engine, client, preedit, space_kind, space_code)
        assert not any(kind == "commit" for kind, _ in following["actions"])


@pytest.mark.parametrize("mode", ["hybrid", "direct", "composing", "empty"])
def test_native_no_ghost_tab_ends_word_before_next_raw_key(tmp_path, mode):
    core = _native().PyInputMethodCore(json.dumps({
        "ghost_text": mode == "hybrid", "accept": {"space_accept": True},
    }))
    core.load_word("qabc", 1_000_000)
    engine, client, preedit = _hybrid_client(core)
    ordinary_key = client.apply_key

    def apply_key_with_literal_tab(keyval, state, source):
        # Bounded text-client projection only: real clients may move focus.
        if keyval == 0xFF09 and state == 0:
            client._insert("\t")
            client.log.append((source, keyval))
        else:
            ordinary_key(keyval, state, source)

    client.apply_key = apply_key_with_literal_tab

    def learned(name):
        path = tmp_path / name
        core.export_personal(str(path))
        return json.loads(path.read_text())

    keys = {
        "hybrid": (("raw", 16), ("char", ord("a"))),
        "direct": (("char", ord("q")), ("char", ord("a"))),
        "composing": (("raw", 16), ("raw", 30)),
        "empty": (),
    }[mode]
    trace = [_hybrid_step(core, engine, client, preedit, kind, code) for kind, code in keys]
    prefix = "" if mode == "empty" else "qa"
    assert core.current_word() == prefix, trace
    if trace:
        assert not any((kind == "ghost" and text) or
                       (kind == "composing" and text.split("\x00", 1)[1])
                       for kind, text in trace[-1]["actions"]), trace
    if mode == "hybrid":
        assert not core.predictions(), "handoff must clear its hidden display identity"
    assert learned("before-tab.json")["cvm"]["words"] == []
    boundary = _hybrid_step(core, engine, client, preedit, "raw", 15, original_keyval=0xFF09)
    assert [text for kind, text in boundary["actions"] if kind == "commit"] == (["qa"] if mode == "composing" else []), boundary
    assert sum(kind == "forward" for kind, _ in boundary["actions"]) == 1
    assert core.current_word() == "" and preedit[0] == "", [*trace, boundary]
    assert client.text == prefix + "\t", boundary
    after_tab = learned("after-tab.json")
    assert set(after_tab["cvm"]["words"]) == ({"qa"} if prefix else set())
    assert after_tab["markov_bigrams"] == [] and after_tab["weights"]["commit_count"] == 0
    following = _hybrid_step(core, engine, client, preedit, "raw", 45)
    assert core.current_word() == "x" and client.text == prefix + "\t", following
    assert any(kind == "composing" and text.split("\x00", 1)[0] == "x" for kind, text in following["actions"]), following
    assert not any(kind in {"commit", "forward"} for kind, _ in following["actions"]), following
    space = _hybrid_step(core, engine, client, preedit, "raw", 57)
    assert [text for kind, text in space["actions"] if kind == "commit"] == ["x"], space
    assert sum(kind == "forward" for kind, _ in space["actions"]) == 1
    assert client.text == prefix + "\tx " and core.current_word() == "" and preedit[0] == ""
    after_space = learned("after-space.json")
    assert set(after_space["cvm"]["words"]) == ({"qa", "x"} if prefix else {"x"})
    assert after_space["markov_bigrams"] == ([["qa", "x", 1]] if prefix else [])
    assert after_space["weights"]["commit_count"] == 0


@pytest.mark.parametrize("transliterated", [True, False], ids=["active-bg", "literal-en"])
def test_native_active_transliteration_prefix_keeps_raw_rendering(tmp_path, transliterated):
    core = _native().PyInputMethodCore(json.dumps({"ghost_text": False}))
    core.load_word("здравей" if transliterated else "zebra", 1_000_000)
    engine, client, preedit = _hybrid_client(core)
    trace = []

    def step(kind, code, original):
        actions = core.process_keycode(code, 0) if kind == "raw" else core.handle_key(code, 0)
        if not engine._execute_actions(actions):
            client.apply_key(original, 0, "default")
        row = {"kind": kind, "key": code, "actions": actions,
               "word": core.current_word(), "committed": client.text, "preedit": preedit[0]}
        trace.append(row)
        print(json.dumps(row, ensure_ascii=False))
        return actions

    def learned(name):
        path = tmp_path / name
        core.export_personal(str(path))
        return json.loads(path.read_text())

    before = learned("before.json")
    step("char", ord("z"), ord("z"))
    second = step("char", ord("d"), ord("d"))
    prefix = "зд" if transliterated else "zd"
    replacements = [_native().ffi_decode_replace_payload(payload)
                    for kind, payload in second if kind == "replace"]
    assert replacements == ([(1, "зд")] if transliterated else []), trace
    assert core.current_word() == client.text == prefix and preedit[0] == "", trace
    assert core.debug_state()[0] is False, trace

    def append_raw(code, original, suffix):
        actions = step("raw", code, original)
        assert actions.count(("commit", suffix)) == 1, trace
        assert [text for kind, text in actions if kind == "commit"] == [suffix], trace
        assert not any(kind in {"forward", "replace", "composing"} for kind, _ in actions), trace
        assert core.current_word() == client.text, trace
        return actions

    append_raw(19, ord("r"), "р" if transliterated else "r")
    assert core.current_word() == prefix + ("р" if transliterated else "r"), trace
    append_raw(30, ord("a"), "а" if transliterated else "a")
    before_backspace = client.text
    backspace = step("char", 0xFF08, 0xFF08)
    assert sum(kind == "forward" for kind, _ in backspace) == 1, trace
    assert not any(kind in {"commit", "replace"} for kind, _ in backspace), trace
    assert core.current_word() == client.text == before_backspace[:-1], trace
    append_raw(30, ord("a"), "а" if transliterated else "a")
    expected = "здра" if transliterated else "zdra"
    assert core.current_word() == client.text == expected, trace
    mid = learned("mid.json")
    assert mid["cvm"]["words"] == [] and mid["markov_bigrams"] == []
    assert mid["light_profile"]["lang_prior"] == before["light_profile"]["lang_prior"]
    assert not mid.get("confirmed_words", [])
    boundary = step("raw", 57, 0x20)
    assert [text for kind, text in boundary if kind == "commit"] == [], trace
    assert sum(kind == "forward" for kind, _ in boundary) == 1, trace
    assert not any(kind == "replace" for kind, _ in boundary), trace
    assert core.current_word() == "" and client.text == expected + " " and preedit[0] == "", trace
    after = learned("after.json")
    assert after["cvm"]["words"] == [expected] and after["markov_bigrams"] == []
    assert not after.get("confirmed_words", []), "derived rendering must not confirm an OOV terminal"
    assert after["weights"]["commit_count"] == 0, "ordinary boundary is not prediction acceptance"
    again = step("raw", 57, 0x20)
    assert not any(kind in {"commit", "replace"} for kind, _ in again), trace
    final = learned("after-empty-space.json")
    assert final["cvm"]["words"] == [expected] and final["markov_bigrams"] == []
    assert final["light_profile"]["lang_prior"] == after["light_profile"]["lang_prior"]



def _legacy_zd_client_prefix(core, engine, client, preedit):
    trace = []
    for ch in "zd":
        actions = core.handle_key(ord(ch), 0)
        if not engine._execute_actions(actions):
            client.apply_key(ord(ch), 0, "default")
        trace.append({"key": ch, "actions": actions, "word": core.current_word(), "client": client.text})
    replaced = [_native().ffi_decode_replace_payload(text)
                for kind, text in trace[-1]["actions"] if kind == "replace"]
    assert replaced == [(1, "зд")], trace
    assert core.current_word() == client.text == "зд" and preedit[0] == "", trace
    return trace


def _native_profile_fixture(core, tmp_path, name):
    path = tmp_path / name
    core.export_personal(str(path))
    return json.loads(path.read_text())


def test_native_4c_manual_completion_does_not_reject_hidden_ghost(tmp_path):
    core = _native().PyInputMethodCore(json.dumps({"ghost_text": True}))
    core.load_word("qaxyz", 1_000_000)
    engine, client, preedit = _hybrid_client(core)
    trace = []
    for kind, code, original in (("raw", 16, ord("q")), ("char", ord("a"), ord("a")),
                                 ("raw", 45, ord("x")), ("raw", 21, ord("y"))):
        trace.append(_hybrid_step(core, engine, client, preedit, kind, code, original_keyval=original))
    assert core.current_word() == client.text == "qaxy", trace
    assert ("ghost", "z") in trace[-1]["actions"], trace
    for _ in range(3):
        finished = _hybrid_step(core, engine, client, preedit, "raw", 44, original_keyval=ord("z"))
        trace.append(finished)
        assert core.current_word() == client.text == "qaxyz", trace
        assert [text for kind, text in finished["actions"] if kind == "commit"] == ["z"], trace
        assert ("hide", "") in finished["actions"] and ("forward", "") not in finished["actions"], trace
        edited = _hybrid_step(core, engine, client, preedit, "char", 0xFF08)
        trace.append(edited)
        assert core.current_word() == client.text == "qaxy", trace
        assert ("ghost", "z") in edited["actions"], "editing a manually completed word is not rejection: " + repr(trace)
    # Backspace over a genuinely visible ghost must still suppress after two
    # actual rejections; the fix must not disable ordinary rejection learning.
    for attempt in range(2):
        _hybrid_step(core, engine, client, preedit, "char", 0xFF08)
        assert core.current_word() == client.text == "qax"
        shown = _hybrid_step(core, engine, client, preedit, "raw", 21, original_keyval=ord("y"))
        assert core.current_word() == client.text == "qaxy"
        assert (("ghost", "z") in shown["actions"]) is (attempt == 0), shown
    mid = _native_profile_fixture(core, tmp_path, "manual-mid.json")
    assert mid["cvm"]["words"] == [] and mid["markov_bigrams"] == []
    boundary = _hybrid_step(core, engine, client, preedit, "raw", 57)
    assert not any(kind == "commit" for kind, _ in boundary["actions"]), boundary
    assert sum(kind == "forward" for kind, _ in boundary["actions"]) == 1
    assert client.text == "qaxy " and core.current_word() == ""
    final = _native_profile_fixture(core, tmp_path, "manual-final.json")
    assert final["cvm"]["words"] == ["qaxy"] and not final.get("confirmed_words", [])
    assert final["weights"]["commit_count"] == 0


def test_native_4c_active_transliteration_keeps_semantic_letters(tmp_path):
    core = _native().PyInputMethodCore(json.dumps({"ghost_text": False}))
    core.load_word("здравей", 1_000_000)
    engine, client, preedit = _hybrid_client(core)
    before = _profile(core, tmp_path, "semantic-before.json")
    trace = _legacy_zd_client_prefix(core, engine, client, preedit)
    raw = _hybrid_step(core, engine, client, preedit, "raw", 19, original_keyval=ord("r"))
    assert core.current_word() == client.text == "здр" and ("commit", "р") in raw["actions"], [*trace, raw]
    semantic = _hybrid_step(core, engine, client, preedit, "char", ord("a"))
    assert [text for kind, text in semantic["actions"] if kind == "commit"] == ["а"], [*trace, raw, semantic]
    assert not any(kind in {"forward", "replace", "composing"} for kind, _ in semantic["actions"]), semantic
    assert core.current_word() == client.text == "здра" and preedit[0] == ""
    mid = _native_profile_fixture(core, tmp_path, "semantic-mid.json")
    assert mid["cvm"]["words"] == [] and mid["light_profile"]["lang_prior"] == before["lang_prior"]
    boundary = _hybrid_step(core, engine, client, preedit, "raw", 57)
    assert not any(kind == "commit" for kind, _ in boundary["actions"]), boundary
    assert sum(kind == "forward" for kind, _ in boundary["actions"]) == 1
    assert client.text == "здра " and core.current_word() == ""
    final = _native_profile_fixture(core, tmp_path, "semantic-final.json")
    assert final["cvm"]["words"] == ["здра"] and not final.get("confirmed_words", [])
    _hybrid_step(core, engine, client, preedit, "raw", 57)
    assert _profile(core, tmp_path, "semantic-empty-space.json")["lang_prior"] == final["light_profile"]["lang_prior"]


def test_native_4c_erased_transliteration_then_new_preedit_keeps_literal_handoff(tmp_path):
    core = _native().PyInputMethodCore(json.dumps({"ghost_text": False}))
    core.load_word("здравей", 1_000_000)
    engine, client, preedit = _hybrid_client(core)
    before = _profile(core, tmp_path, "fresh-before.json")
    _legacy_zd_client_prefix(core, engine, client, preedit)
    for _ in range(2):
        _hybrid_step(core, engine, client, preedit, "char", 0xFF08)
    assert core.current_word() == client.text == "" and preedit[0] == ""
    fresh = _hybrid_step(core, engine, client, preedit, "raw", 44, original_keyval=ord("z"))
    assert core.current_word() == preedit[0] == "з" and client.text == "", fresh
    assert ("composing", "з\x00") in fresh["actions"], fresh
    handoff = _hybrid_step(core, engine, client, preedit, "char", ord("a"))
    assert [text for kind, text in handoff["actions"] if kind == "commit"] == ["з"], handoff
    assert sum(kind == "forward" for kind, _ in handoff["actions"]) == 1
    assert core.current_word() == client.text == "зa" and preedit[0] == "", handoff
    mid = _native_profile_fixture(core, tmp_path, "fresh-mid.json")
    assert mid["cvm"]["words"] == [] and mid["light_profile"]["lang_prior"] == before["lang_prior"]
    boundary = _hybrid_step(core, engine, client, preedit, "raw", 57)
    assert not any(kind == "commit" for kind, _ in boundary["actions"]), boundary
    assert sum(kind == "forward" for kind, _ in boundary["actions"]) == 1
    assert client.text == "зa " and core.current_word() == ""
    final = _native_profile_fixture(core, tmp_path, "fresh-final.json")
    assert final["cvm"]["words"] == ["зa"] and not final.get("confirmed_words", [])
    assert final["light_profile"]["lang_prior"] == before["lang_prior"]


@pytest.mark.parametrize(
    "opt_in,previous,word,expected_replacement",
    [
        (True, (), "hello", "Hello"),
        (True, ("seed",), "hello", None),
        (True, ("seed.",), "hello", "Hello"),
        (True, ("seed",), "hello.", None),
        (False, (), "hello", None),
    ],
    ids=["first-word", "middle-word", "after-stored-punctuation",
         "current-punctuation-is-not-previous", "default-off"],
)
def test_native_post_commit_caps_uses_preceding_word(
    opt_in, previous, word, expected_replacement
):
    # Exercise real semantic input and the actual post-commit producer. No
    # replacement action or private context is supplied by this fixture.
    core = _native().PyInputMethodCore(json.dumps({
        "ghost_text": False,
        "lang_detection": False,
        "post_commit_autocorrect": opt_in,
    }))
    trace = []
    for token in (*previous, word):
        for char in token:
            trace.append(core.handle_key(ord(char), 0))
        assert core.current_word() == token, trace
        actions = core.handle_key(0x20, 0)
        trace.append(actions)
        assert core.current_word() == "", trace

    replacements = [_native().ffi_decode_replace_payload(payload)
                    for kind, payload in actions if kind == "replace"]
    expected = [] if expected_replacement is None else [(len(word), expected_replacement)]
    assert replacements == expected, trace
    assert actions.count(("forward", "")) == 1, trace


@pytest.mark.parametrize("supported", [False, True], ids=["no-surrounding", "surrounding"])
@pytest.mark.parametrize("lifecycle", ["none", "reset", "focus"])
def test_native_external_rewrite_capability_reaches_real_legacy_producer(
    tmp_path, supported, lifecycle
):
    core = _native().PyInputMethodCore(json.dumps({"ghost_text": False}))
    core.load_word("здравей", 1_000_000)
    core.set_external_rewrite_supported(supported)
    if lifecycle == "reset":
        core.reset()
    elif lifecycle == "focus":
        core.focus_lost()
        core.focus_gained()

    first = core.handle_key(ord("z"), 0)
    second = core.handle_key(ord("d"), 0)
    replacements = [_native().ffi_decode_replace_payload(text)
                    for kind, text in second if kind == "replace"]
    expected_word = "зд" if supported else "zd"
    assert replacements == ([(1, "зд")] if supported else []), (first, second)
    assert core.current_word() == expected_word
    if not supported:
        assert second.count(("forward", "")) == 1
        assert not any(kind in {"commit", "composing"} for kind, _ in second)
    boundary = core.handle_key(0x20, 0)
    assert boundary.count(("forward", "")) == 1
    assert not any(kind == "replace" for kind, _ in boundary)
    profile = _native_profile_fixture(core, tmp_path, "learned.json")
    assert profile["cvm"]["words"] == [expected_word]
    assert "external_rewrite_supported" not in json.dumps(profile)


@pytest.mark.parametrize("word", ["цар", "hello"], ids=["language", "capitalization"])
def test_native_unowned_boundary_without_rewrite_support_learns_literal(tmp_path, word):
    core = _native().PyInputMethodCore(json.dumps({
        "ghost_text": False,
        "post_commit_autocorrect": True,
    }))
    core.load_word("car", 100)
    core.load_word("hello", 1_000)
    core.set_external_rewrite_supported(False)
    for char in word:
        core.handle_key(ord(char), 0)
    assert core.current_word() == word
    actions = core.handle_key(0x20, 0)
    assert not any(kind == "replace" for kind, _ in actions), actions
    assert actions.count(("forward", "")) == 1
    profile = _native_profile_fixture(core, tmp_path, "literal.json")
    assert profile["cvm"]["words"] == [word]
    assert core.current_word() == ""


def _sentence_boundary_core(tmp_path, *, post=None, ghost=False):
    config = {"lang_detection": False, "ghost_text": ghost}
    if post is not None:
        config["post_commit_autocorrect"] = post
    core = _native().PyInputMethodCore(json.dumps(config))
    corpus = tmp_path / "sentence-corpus.json"
    corpus.write_text(json.dumps({
        "unigrams": {"hello": 1_000, "world": 1_000},
        "bigrams": [],
        "trigrams": [],
    }))
    core.load_corpus_file(str(corpus))
    return core


def _raw_sentence_prefix(core, *, whitespace=True):
    trace = []
    for code in (35, 18, 38, 38, 24):  # hello
        actions = core.process_keycode(code, 0)
        trace.append(actions)
        assert any(kind == "composing" for kind, _ in actions), trace
        assert not any(kind in {"commit", "replace", "forward"} for kind, _ in actions), trace
    assert core.current_word() == "hello", trace
    punctuation = core.process_keycode(52, 0)  # .
    assert [text for kind, text in punctuation if kind == "commit"] == ["hello"], punctuation
    assert punctuation.count(("forward", "")) == 1, punctuation
    assert core.current_word() == "", punctuation
    if whitespace:
        space = core.process_keycode(57, 0)
        assert space == [("hide", ""), ("forward", "")], space
        assert core.current_word() == "", space


@pytest.mark.parametrize("post", [False, True], ids=["opt-out", "opt-in"])
@pytest.mark.parametrize("external", [False, True], ids=["no-surrounding", "surrounding"])
def test_native_raw_sentence_boundary_caps_keeps_owned_delivery_and_learning(
    tmp_path, post, external
):
    core = _sentence_boundary_core(tmp_path, post=post)
    core.set_external_rewrite_supported(external)
    _raw_sentence_prefix(core)
    for code in (17, 24, 19, 38, 32):  # world
        step = core.process_keycode(code, 0)
        assert any(kind == "composing" for kind, _ in step), step
        assert not any(kind in {"commit", "replace", "forward"} for kind, _ in step), step
    assert core.current_word() == "world"
    actions = core.process_keycode(57, 0)
    assert [text for kind, text in actions if kind == "commit"] == ["world"], actions
    replacements = [_native().ffi_decode_replace_payload(text)
                    for kind, text in actions if kind == "replace"]
    # The whole word still belongs to the preedit, so the existing ownership
    # permission also applies when surrounding-text rewriting is unavailable.
    assert replacements == ([(5, "World")] if post else []), actions
    assert actions.count(("forward", "")) == 1, actions
    assert core.current_word() == "", actions
    profile = _native_profile_fixture(core, tmp_path, "sentence-profile.json")
    assert set(profile["cvm"]["words"]) == {"hello", "world"}
    assert profile["markov_bigrams"] == [["hello", "world", 1]]
    assert profile["markov_trigrams"] == []


def test_native_default_prediction_remembers_forwarded_sentence_boundary(tmp_path):
    # Omit the post-commit option: ordinary suggestions use this evidence too.
    core = _sentence_boundary_core(tmp_path, ghost=True)
    _raw_sentence_prefix(core)
    actions = core.process_keycode(17, 0)  # w
    assert core.current_word() == "w", actions
    assert any(kind == "composing" for kind, _ in actions), actions
    assert core.predictions()[0][0] == "World", (actions, core.predictions())


def test_native_default_tab_preserves_actual_displayed_pair_once(tmp_path):
    core = _sentence_boundary_core(tmp_path, ghost=True)
    _raw_sentence_prefix(core)
    offer = core.process_keycode(17, 0)  # w
    composing = [_native().ffi_decode_composing_payload(text)
                 for kind, text in offer if kind == "composing"]
    assert composing == [("w", "orld")], offer
    actions = core.process_keycode(15, 0)  # Tab accepts this actual offer.
    # The prediction table can carry sentence casing, but the displayed pair
    # retains the typed prefix. Acceptance must conserve that actual offer.
    assert [text for kind, text in actions if kind == "commit"] == ["world"], actions
    assert not any(kind in {"replace", "forward"} for kind, _ in actions), actions
    assert core.current_word() == "", actions
    profile = _native_profile_fixture(core, tmp_path, "sentence-tab-profile.json")
    assert set(profile["cvm"]["words"]) == {"hello", "world"}
    assert profile["markov_bigrams"] == [["hello", "world", 1]]


@pytest.mark.parametrize("case", ["cancel-local-preedit", "literal-tab", "trim-extra-space"])
@pytest.mark.parametrize("mode", ["default-prediction", "opt-in-postcommit"])
def test_native_sentence_boundary_review_paths_preserve_actual_word_scope(tmp_path, case, mode):
    prediction = mode == "default-prediction"
    core = _sentence_boundary_core(tmp_path, post=None if prediction else True, ghost=prediction)
    _raw_sentence_prefix(core, whitespace=case != "literal-tab")
    if case == "cancel-local-preedit":
        offer = core.process_keycode(16, 0)  # Unsupported q: no eligible displayed suffix.
        composing = [_native().ffi_decode_composing_payload(text)
                     for kind, text in offer if kind == "composing"]
        assert composing == [("q", "")], offer
        assert not any(kind in {"commit", "replace", "forward"} for kind, _ in offer), offer
        cancelled = core.process_keycode(1, 0)  # Escape cancels only this local preedit.
        assert cancelled == [("hide", "")], cancelled
    elif case == "literal-tab":
        # This Core contract applies when the client inserts the forwarded Tab.
        # A client focus change has its separate focus/reset invalidation path.
        tab = core.process_keycode(15, 0)
        assert tab == [("forward", "")], tab
    else:
        extra_space = core.process_keycode(57, 0)
        assert extra_space == [("hide", ""), ("forward", "")], extra_space
        backspace = core.process_keycode(14, 0)
        assert backspace.count(("forward", "")) == 1, backspace
        assert not any(kind in {"commit", "replace"} for kind, _ in backspace), backspace
    assert core.current_word() == ""

    codes = (17,) if prediction else (17, 24, 19, 38, 32)  # w / world
    for code in codes:
        step = core.process_keycode(code, 0)
        assert any(kind == "composing" for kind, _ in step), step
        assert not any(kind in {"commit", "replace", "forward"} for kind, _ in step), step
    if prediction:
        assert core.current_word() == "w"
        assert core.predictions()[0][0] == "World", core.predictions()
        profile = _native_profile_fixture(core, tmp_path, "sentence-review-prediction.json")
        assert set(profile["cvm"]["words"]) == {"hello"}
        assert profile["markov_bigrams"] == []
    else:
        boundary = core.process_keycode(57, 0)
        assert [text for kind, text in boundary if kind == "commit"] == ["world"], boundary
        replacements = [_native().ffi_decode_replace_payload(text)
                        for kind, text in boundary if kind == "replace"]
        assert replacements == [(5, "World")], boundary
        assert boundary.count(("forward", "")) == 1, boundary
        assert core.current_word() == "", boundary
        profile = _native_profile_fixture(core, tmp_path, "sentence-review-postcommit.json")
        assert set(profile["cvm"]["words"]) == {"hello", "world"}
        assert profile["markov_bigrams"] == [["hello", "world", 1]]


def _assert_native_sentence_review_target(
    core, tmp_path, *, prediction, capitalized, expected_previous="hello"
):
    before = _native_profile_fixture(core, tmp_path, "tail-before-target.json")
    before_words = set(before["cvm"]["words"])
    assert before_words == {expected_previous}, before_words
    assert not before_words.intersection({'"', "”", ")", "]", "!", "seed"}), before_words
    assert before["markov_bigrams"] == []
    for code in ((17,) if prediction else (17, 24, 19, 38, 32)):
        step = core.process_keycode(code, 0)
        assert any(kind == "composing" for kind, _ in step), step
        assert not any(kind in {"commit", "replace", "forward"} for kind, _ in step), step
    if prediction:
        assert core.current_word() == "w"
        assert core.predictions()[0][0] == ("World" if capitalized else "world"), core.predictions()
    else:
        actions = core.process_keycode(57, 0)
        assert [text for kind, text in actions if kind == "commit"] == ["world"], actions
        replacements = [_native().ffi_decode_replace_payload(text)
                        for kind, text in actions if kind == "replace"]
        assert replacements == ([(5, "World")] if capitalized else []), actions
        assert actions.count(("forward", "")) == 1 and core.current_word() == "", actions
    after = _native_profile_fixture(core, tmp_path, "tail-after-target.json")
    assert set(after["cvm"]["words"]) == before_words | (set() if prediction else {"world"})
    if prediction:
        assert after["markov_bigrams"] == []
    else:
        assert after["markov_bigrams"] == [[expected_previous, "world", 1]]
    assert after["markov_trigrams"] == []


@pytest.mark.parametrize("route", ["semantic-tail", "shared-raw-tail", "stored-semantic-token"])
@pytest.mark.parametrize("mode", ["default-prediction", "opt-in-postcommit"])
def test_native_sentence_closers_preserve_boundary_and_lexical_scope(tmp_path, route, mode):
    prediction = mode == "default-prediction"
    core = _sentence_boundary_core(tmp_path, post=None if prediction else True, ghost=prediction)
    if route == "stored-semantic-token":
        # Semantic input keeps punctuation in the existing whole-word token.
        # The shared capitalization consumer must recognize its closing quote.
        for char in "hello.”":
            core.handle_key(ord(char), 0)  # Actual codepoints, as normalized by the adapter.
        assert core.current_word() == "hello.”"
        boundary = core.handle_key(0x20, 0)
        assert boundary.count(("forward", "")) == 1 and core.current_word() == "", boundary
    else:
        _raw_sentence_prefix(core, whitespace=False)
        if route == "semantic-tail":
            for char in "”)]":
                actions = core.handle_key(ord(char), 0)
                assert actions == [("forward", "")] and core.current_word() == "", actions
        else:
            # Only actual shared EN/BG glyphs qualify. The Shift key itself and
            # its release must retain the pending boundary without inserting text.
            assert core.process_keycode(42, 1) == [("forward", "")]
            for code in (40, 11, 2):  # Shift: double quote, closing parenthesis, !
                actions = core.process_keycode(code, 1)
                assert actions == [("forward", "")] and core.current_word() == "", actions
            assert core.process_keycode(42, 1 | (1 << 30)) == [("forward", "")]
        space = core.process_keycode(57, 0)
        assert space == [("hide", ""), ("forward", "")], space
    _assert_native_sentence_review_target(
        core, tmp_path, prediction=prediction, capitalized=True,
        expected_previous="hello.”" if route == "stored-semantic-token" else "hello",
    )


@pytest.mark.parametrize("api", ["raw", "keysym"])
@pytest.mark.parametrize("mode", ["default-prediction", "opt-in-postcommit"])
def test_native_shift_insert_withdraws_stale_boundary_after_client_paste(tmp_path, api, mode):
    prediction = mode == "default-prediction"
    core = _sentence_boundary_core(tmp_path, post=None if prediction else True, ghost=prediction)
    _raw_sentence_prefix(core)
    actions = (core.process_keycode(110, 1) if api == "raw"
               else core.handle_key(0xFF63, 1))  # Actual Shift+Insert identities.
    assert actions == [("forward", "")] and core.current_word() == "", actions
    # Synthetic fresh client snapshot after the forwarded paste. No clipboard,
    # widget mutation, Core text assignment or inferred-word training is involved.
    context = "hello. seed "
    core.set_surrounding_text(context, len(context))
    _assert_native_sentence_review_target(core, tmp_path, prediction=prediction, capitalized=False)


@pytest.mark.parametrize("route", ["semantic", "shared-raw"])
@pytest.mark.parametrize("boundary", ["survives", "removed"])
@pytest.mark.parametrize("mode", ["default-prediction", "opt-in-postcommit"])
def test_native_sentence_tail_deletion_preserves_only_surviving_boundary(
    tmp_path, route, boundary, mode
):
    prediction = mode == "default-prediction"
    core = _sentence_boundary_core(tmp_path, post=None if prediction else True, ghost=prediction)
    _raw_sentence_prefix(core, whitespace=False)
    if route == "semantic":
        tails = [core.handle_key(ord(char), 0) for char in "”)]"]
    else:
        # Shared quote, repeated terminator, closing parenthesis: three glyphs.
        tails = [core.process_keycode(code, mask) for code, mask in ((40, 1), (52, 0), (11, 1))]
    assert tails == [[("forward", "")]] * 3, tails
    assert core.current_word() == ""

    def delete_literal():
        actions = core.process_keycode(14, 0)
        assert actions.count(("forward", "")) == 1 and core.current_word() == "", actions
        assert not any(kind in {"commit", "replace", "composing"} for kind, _ in actions), actions

    delete_literal()  # Two known tail glyphs remain after the original terminator.
    for _ in range(2):
        actions = core.process_keycode(57, 0)
        assert actions == [("hide", ""), ("forward", "")], actions
    for _ in range(2):
        delete_literal()  # Delete both Spaces, restoring the same pending tail.
    for _ in range(1 if boundary == "survives" else 3):
        delete_literal()  # Remove a closer, or the whole tail plus original terminator.
    actions = core.process_keycode(57, 0)
    assert actions == [("hide", ""), ("forward", "")], actions
    _assert_native_sentence_review_target(
        core, tmp_path, prediction=prediction, capitalized=boundary == "survives"
    )


@pytest.mark.parametrize("api,identity,mask", [
    ("raw", 29, 1 << 2),          # Left Ctrl, physical IBus control mask.
    ("keysym", 0xFFE4, 1 << 2),  # Right Ctrl.
    ("raw", 100, 1 << 3),        # Right Alt.
    ("keysym", 0xFFE9, 1 << 3),  # Left Alt.
    ("raw", 125, 1 << 6),        # Left Super, physical MOD4 mask.
    ("keysym", 0xFFEC, 1 << 26), # Right Super, virtual SUPER mask.
])
@pytest.mark.parametrize("mode", ["default-prediction", "opt-in-postcommit"])
def test_native_modifier_only_tap_preserves_sentence_hint(tmp_path, api, identity, mask, mode):
    prediction = mode == "default-prediction"
    core = _sentence_boundary_core(tmp_path, post=None if prediction else True, ghost=prediction)
    _raw_sentence_prefix(core)
    call = core.process_keycode if api == "raw" else core.handle_key
    assert call(identity, mask) == [("forward", "")]
    assert call(identity, mask | (1 << 30)) == [("forward", "")]
    assert core.current_word() == ""
    _assert_native_sentence_review_target(core, tmp_path, prediction=prediction, capitalized=True)


@pytest.mark.parametrize("mode", ["default-prediction", "opt-in-postcommit"])
def test_native_refreshed_client_text_withdraws_stale_sentence_hint(tmp_path, mode):
    prediction = mode == "default-prediction"
    core = _sentence_boundary_core(tmp_path, post=None if prediction else True, ghost=prediction)
    _raw_sentence_prefix(core)
    # A fresh client report after an editor-side insertion, with no paste key
    # passing through Core. This updates context, never learned vocabulary.
    context = "hello. seed "
    core.set_surrounding_text(context, len(context))
    assert core.current_word() == ""
    _assert_native_sentence_review_target(core, tmp_path, prediction=prediction, capitalized=False)


@pytest.mark.parametrize("prior_report", ["sentence", "ordinary-word"])
@pytest.mark.parametrize("mode", ["default-prediction", "opt-in-postcommit"])
def test_native_refreshed_sentence_start_revalidates_current_cursor(tmp_path, prior_report, mode):
    prediction = mode == "default-prediction"
    core = _sentence_boundary_core(tmp_path, post=None if prediction else True, ghost=prediction)
    # The observer has an actual known empty report before its own delivery.
    core.set_surrounding_text("", 0)
    _raw_sentence_prefix(core)
    if prior_report == "ordinary-word":
        context = "hello. seed "
        core.set_surrounding_text(context, len(context))
    # A second external edit can establish a new sentence after a prior veto.
    context = "hello. seed. "
    core.set_surrounding_text(context, len(context))
    assert core.current_word() == ""
    _assert_native_sentence_review_target(core, tmp_path, prediction=prediction, capitalized=True)


@pytest.mark.parametrize("context,deletions,restore_space,capitalized", [
    ("hello. seed.  ", 1, False, True),
    ("hello. seed.  ", 2, False, False),
    ("hello. seed.  ", 2, True, True),
    ("hello. seed.!)]  ", 5, True, True),
    ("hello. seed.!)]  ", 6, True, False),
])
@pytest.mark.parametrize("mode", ["default-prediction", "opt-in-postcommit"])
def test_native_refreshed_literal_tail_deletion_preserves_surviving_separator(
    tmp_path, context, deletions, restore_space, capitalized, mode
):
    prediction = mode == "default-prediction"
    core = _sentence_boundary_core(tmp_path, post=None if prediction else True, ghost=prediction)
    core.set_surrounding_text("", 0)
    _raw_sentence_prefix(core)
    core.set_surrounding_text(context, len(context))
    for _ in range(deletions):
        actions = core.process_keycode(14, 0)
        assert actions.count(("forward", "")) == 1 and core.current_word() == "", actions
        assert not any(kind in {"commit", "replace", "composing"} for kind, _ in actions), actions
    if restore_space:
        assert core.process_keycode(57, 0) == [("hide", ""), ("forward", "")]
    _assert_native_sentence_review_target(
        core, tmp_path, prediction=prediction, capitalized=capitalized
    )


@pytest.mark.parametrize("mode", ["default-prediction", "opt-in-postcommit"])
def test_native_refreshed_positive_extra_space_then_shortcut_keeps_stale_context_veto(tmp_path, mode):
    prediction = mode == "default-prediction"
    core = _sentence_boundary_core(tmp_path, post=None if prediction else True, ghost=prediction)
    core.set_surrounding_text("", 0)
    _raw_sentence_prefix(core, whitespace=False)
    # Cache a real sentence-ending lexical hint before the following report
    # changes the cursor context. No context, word or learning state is assigned.
    core.set_surrounding_text("hello.", 6)
    assert core.process_keycode(57, 0) == [("hide", ""), ("forward", "")]
    context = "hello. seed.  "
    core.set_surrounding_text(context, len(context))
    assert core.process_keycode(57, 0) == [("hide", ""), ("forward", "")]
    assert core.process_keycode(47, 1 << 2) == [("forward", "")]  # Ctrl+V.
    assert core.current_word() == ""
    _assert_native_sentence_review_target(core, tmp_path, prediction=prediction, capitalized=False)


@pytest.mark.parametrize("context,deletions,restore_space,capitalized", [
    ("hello.\n  ", 1, False, True),
    ("hello.\n  ", 2, False, True),
    ("hello.\n  ", 3, False, False),
    ("hello.\n  ", 3, True, True),
    ("hello.\n  ", 4, True, False),
    ("hello.\r\n  ", 3, False, True),
    ("  ", 1, False, True),
    ("  ", 2, False, True),
])
@pytest.mark.parametrize("mode", ["default-prediction", "opt-in-postcommit"])
def test_native_refreshed_line_indentation_deletion_reclassifies_exact_cursor(
    tmp_path, context, deletions, restore_space, capitalized, mode
):
    prediction = mode == "default-prediction"
    core = _sentence_boundary_core(tmp_path, post=None if prediction else True, ghost=prediction)
    core.set_surrounding_text("", 0)
    _raw_sentence_prefix(core)
    # A real public client report supplies the layout. Subsequent Backspaces
    # use the raw API; the test never assigns the projected text or caps state.
    core.set_surrounding_text(context, len(context))
    for _ in range(deletions):
        assert core.current_word() == ""
        actions = core.process_keycode(14, 0)
        assert actions.count(("forward", "")) == 1 and core.current_word() == "", actions
        assert not any(kind in {"commit", "replace", "composing"} for kind, _ in actions), actions
    if restore_space:
        assert core.process_keycode(57, 0) == [("hide", ""), ("forward", "")]
    _assert_native_sentence_review_target(
        core, tmp_path, prediction=prediction, capitalized=capitalized
    )
