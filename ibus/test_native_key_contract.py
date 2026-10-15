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
