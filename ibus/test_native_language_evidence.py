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


@pytest.mark.parametrize("fixture,expected", [("both-supported", "wix"), ("one-sided", "statiqx")])
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
