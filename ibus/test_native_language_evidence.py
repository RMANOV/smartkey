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
    if fixture == "both-supported":
        core = _terminal_core()
        _terminal_type_wi(core)
    else:
        core = _native().PyInputMethodCore()
        for word, frequency in (("status", 900_000), ("static", 600_000), ("station", 500_000), ("статия", 50_000)):
            core.load_word(word, frequency)
        for code in (31, 20, 30, 20, 23, 16):
            core.process_keycode(code, 0)
        assert core.current_word() == "statiq"
        assert list(core.debug_state()) == [True, True, False]
    core.handle_key(ord("x"), 0)
    assert core.current_word() == expected

    _terminal_assert_boundary(core, 57, expected)


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
