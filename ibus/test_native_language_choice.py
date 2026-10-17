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


def _adapter_with_repeated_top(monkeypatch):
    """Real Core/adapter; only the external IBus display/panel is recorded."""
    core = _native().PyInputMethodCore(json.dumps({"ghost_text": False}))
    from .test_language_choice import _engine, ske

    for word, count in (("неделя", 1_000_000), ("top", 2_137_962), ("топ", 12_302)):
        core.load_word(word, count)
    eng, rec = _engine(monkeypatch)
    eng._core = core
    eng._language_choice_supported = False
    eng.do_set_capabilities(41)
    prefix = "Искам да благодаря "
    eng._sync_surrounding_text(prefix, len(prefix), len(prefix))

    def tap(keyval, code):
        result = eng.do_process_key_event(keyval, code, 0)
        eng.do_process_key_event(keyval, code, 1 << 30)
        return result

    for key, code in zip("nedelq", (49, 18, 32, 18, 38, 16)):
        tap(ord(key), code)
    tap(32, 57)
    assert rec.commits == ["неделя"]
    prefix += "неделя "
    eng._sync_surrounding_text(prefix, len(prefix), len(prefix))
    for key, code in zip("top", (20, 24, 25)):
        tap(ord(key), code)
    assert core.current_word() == "top"
    tap(32, 57)
    assert rec.commits == ["неделя", "top"]
    prefix += "top "
    eng._sync_surrounding_text(prefix, len(prefix), len(prefix))
    for key, code in zip("top", (20, 24, 25)):
        tap(ord(key), code)
    assert rec.lookups[-1] == (("top", "топ"), 0, False, True)
    # Actual Qt reports after final p: committed prefix, then preedit-inclusive
    # text/caret. Neither report rewrites the token or chooses a reading.
    eng._sync_surrounding_text(prefix, len(prefix), len(prefix))
    rendered = prefix + "top"
    eng._sync_surrounding_text(rendered, len(rendered), len(rendered))
    return eng, rec, ske, tap, rendered


def _assert_same_learned_state(before, after):
    for key in ("weights", "light_profile", "corrections", "markov_bigrams", "markov_trigrams"):
        assert after[key] == before[key]
    # Exported age_secs advances with wall time; words/round are the learned
    # state. Language tracks are a serialized map, whose order is not semantic.
    for key in ("words", "round"):
        assert after["cvm"][key] == before["cvm"][key]
        assert {lang: track[key] for lang, track in after["lang_cvm"]["tracks"]} == {
            lang: track[key] for lang, track in before["lang_cvm"]["tracks"]
        }


@pytest.mark.parametrize("arrows,positions", [
    (("Down",), (1,)),
    (("Down", "Up", "Down"), (1, 0, 1)),
])
def test_native_adapter_selected_tab_accepts_after_preedit_surrounding_echo(
    monkeypatch, tmp_path, arrows, positions,
):
    """A fresh arrow selection must bind the current lease, not the old UI lease.

    KWrite reports the rendered preedit after the last letter. That report
    invalidates the Core revision while the visible table stays cached.
    """
    eng, rec, ske, tap, _rendered = _adapter_with_repeated_top(monkeypatch)
    eng._core.export_personal(str(tmp_path / "before-arrows.json"))
    before_arrows = json.loads((tmp_path / "before-arrows.json").read_text())
    before_preedits = list(rec.preedits)
    for arrow, position in zip(arrows, positions):
        assert tap(getattr(ske.IBus, "KEY_" + arrow), 108 if arrow == "Down" else 103) is True
        assert rec.lookups[-1] == (("top", "топ"), position, True, True)
    assert rec.lookups[-1] == (("top", "топ"), 1, True, True)
    assert eng._core.current_word() == "top" and rec.commits == ["неделя", "top"]
    assert rec.preedits == before_preedits
    eng._core.export_personal(str(tmp_path / "after-arrows.json"))
    after_arrows = json.loads((tmp_path / "after-arrows.json").read_text())
    _assert_same_learned_state(before_arrows, after_arrows)

    assert tap(ske.IBus.KEY_Tab, 15) is True

    assert rec.commits == ["неделя", "top", "топ"]
    assert eng._core.current_word() == "" and not eng._preedit_active


@pytest.mark.parametrize("report", ["cursor-move", "changed-text", "noncollapsed"])
def test_native_adapter_selected_tab_rejects_report_change_after_selection(monkeypatch, tmp_path, report):
    """Tab cannot silently rebind a selection after a client state change."""
    eng, rec, ske, tap, rendered = _adapter_with_repeated_top(monkeypatch)
    assert tap(ske.IBus.KEY_Down, 108) is True
    selected_revision = eng._language_choice_offer["revision"]
    if report == "cursor-move":
        eng._sync_surrounding_text(rendered, len(rendered) - 1, len(rendered) - 1)
    elif report == "changed-text":
        changed = rendered + "x"
        eng._sync_surrounding_text(changed, len(changed), len(changed))
    else:
        # The Core API does not receive anchor state; a changed cursor in an
        # actual noncollapsed report must still leave the old lease invalid.
        eng._sync_surrounding_text(rendered, len(rendered) - 1, len(rendered))
    current_offer = eng._core.language_choice_offer()
    assert current_offer is not None and current_offer["revision"] != selected_revision
    before_preedits = list(rec.preedits)
    before_feedback = list(eng._o1.words)
    eng._core.export_personal(str(tmp_path / "before-stale-Tab.json"))

    assert tap(ske.IBus.KEY_Tab, 15) is True

    assert rec.commits == ["неделя", "top"]
    assert eng._core.current_word() == "top"
    assert rec.preedits == before_preedits and eng._o1.words == before_feedback
    eng._core.export_personal(str(tmp_path / "after-stale-Tab.json"))
    _assert_same_learned_state(
        json.loads((tmp_path / "before-stale-Tab.json").read_text()),
        json.loads((tmp_path / "after-stale-Tab.json").read_text()),
    )
