"""Whole-word language choices through an explicitly audited PyO3 artifact.

These tests cover the Python wire contract and MasterLoop learning consumer.
They use synthetic input and private exported profiles, never a live engine.
"""
from __future__ import annotations

import json
import os

import pytest

from .test_native_space_accept import _native

pytestmark = pytest.mark.skipif(
    not os.environ.get("SMARTKEY_NATIVE_MODULE_DIR")
    and os.environ.get("SMARTKEY_NATIVE_AUDIT_REQUIRED") != "1",
    reason="needs explicit native audit artifact",
)


def _offered():
    core = _native().PyInputMethodCore(json.dumps({"ghost_text": False}))
    core.set_language_choice_supported(True)
    for code in (16, 45, 44):  # qxz / яьз: neither reading is loaded.
        core.process_keycode(code, 0)
    offer = core.language_choice_offer()
    assert offer is not None
    assert set(offer) == {"revision", "current_language", "en", "bg"}
    assert offer["en"] == "qxz" and offer["bg"] == "яьз"
    assert isinstance(offer["revision"], int)
    assert core.current_word() == offer[offer["current_language"]]
    assert core.language_choice_offer() == offer
    return core, offer


def _profile(core, tmp_path, name):
    path = tmp_path / name
    core.export_personal(str(path))
    return json.loads(path.read_text())["light_profile"]


@pytest.mark.parametrize("language,index", [("bg", 0), ("en", 1)])
def test_native_language_choice_wire_commits_exactly_once_and_learns_once(tmp_path, language, index):
    core, offer = _offered()
    before = _profile(core, tmp_path, "before.json")
    assert core.accept_language_choice(offer["revision"], "tech") is None
    assert core.language_choice_offer() == offer

    actions = core.accept_language_choice(offer["revision"], language)

    assert actions == [("hide", ""), ("commit", offer[language])]
    assert core.current_word() == "" and core.language_choice_offer() is None
    after = _profile(core, tmp_path, "after.json")
    expected = [0.98 * value + (0.02 if i == index else 0.0)
                for i, value in enumerate(before["lang_prior"])]
    assert after["lang_prior"] == pytest.approx(expected)
    assert after["ghost_accept_rate"] == before["ghost_accept_rate"]
    assert core.accept_language_choice(offer["revision"], language) is None
    core.process_keycode(57, 0)
    assert _profile(core, tmp_path, "after-boundary.json") == after


def test_native_language_choice_erased_and_retyped_offer_cannot_accept_old_revision(tmp_path):
    core, old = _offered()
    core.process_keycode(14, 0)
    core.process_keycode(44, 0)
    new = core.language_choice_offer()
    assert new is not None
    assert (new["en"], new["bg"]) == (old["en"], old["bg"])
    assert new["revision"] != old["revision"]
    before = _profile(core, tmp_path, "before-stale.json")

    assert core.accept_language_choice(old["revision"], "bg") is None

    assert core.language_choice_offer() == new
    assert _profile(core, tmp_path, "after-stale.json") == before


def test_native_language_choice_dismissal_and_renderer_loss_keep_current_reading():
    core, offer = _offered()
    current = core.current_word()
    assert core.dismiss_language_choice(offer["revision"])
    assert core.language_choice_offer() is None
    core.set_language_choice_supported(False)

    actions = core.process_keycode(57, 0)

    assert actions.count(("commit", current)) == 1
    assert actions.count(("forward", "")) == 1
    assert not any(kind == "replace" for kind, _ in actions)


@pytest.mark.parametrize("en,bg,codes", [
    ("templejtyt", "темплейтът", (20, 18, 50, 25, 38, 18, 36, 20, 21, 20)),
    ("draftyt", "драфтът", (32, 19, 30, 33, 20, 21, 20)),
    ("markdaun", "маркдаун", (50, 30, 19, 37, 32, 30, 22, 49)),
    ("tn", "тн", (20, 49)),
    ("top", "топ", (20, 24, 25)),
])
def test_native_historical_whole_readings_preserve_space_and_accept_explicit_bg(en, bg, codes):
    for select_bg in (False, True):
        core = _native().PyInputMethodCore(json.dumps({"ghost_text": False}))
        for word in ("temple", "drafting", "markdown", "with"):
            core.load_word(word, 7_079_457)
        for word in ("tn", "top"):
            core.load_word(word, 12_302)
        for word in ("тн", "топ"):
            core.load_word(word, 2_137_962)
        context = "Искам да благодаря "
        core.set_surrounding_text(context, len(context))
        core.set_language_choice_supported(True)
        for code in codes:
            actions = core.process_keycode(code, 0)
            assert not any(kind in {"commit", "replace", "forward"} for kind, _ in actions)
        offer = core.language_choice_offer()
        assert offer is not None
        assert (offer["en"], offer["bg"]) == (en, bg)
        current = core.current_word()
        assert current == offer[offer["current_language"]]
        if select_bg:
            assert core.accept_language_choice(offer["revision"], "bg") == [
                ("hide", ""), ("commit", bg),
            ]
        else:
            actions = core.process_keycode(57, 0)
            assert actions.count(("commit", current)) == 1
            assert actions.count(("forward", "")) == 1
            assert not any(kind == "replace" for kind, _ in actions)
        assert core.current_word() == "" and core.language_choice_offer() is None
