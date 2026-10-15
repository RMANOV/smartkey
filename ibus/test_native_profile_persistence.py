"""Default-path persistence must preserve the wrapper's adaptive learning.

Explicit audited native artifact, synthetic data, fresh HOME/XDG per test.
No IBus objects or personal user profile are opened.
"""
from __future__ import annotations

import json
import os
from pathlib import Path

import pytest

from .test_native_space_accept import _native

pytestmark = pytest.mark.skipif(
    not os.environ.get("SMARTKEY_NATIVE_MODULE_DIR")
    and os.environ.get("SMARTKEY_NATIVE_AUDIT_REQUIRED") != "1",
    reason="needs explicit native audit artifact",
)


@pytest.fixture
def private_paths(tmp_path, monkeypatch):
    for variable, name in (("HOME", "home"), ("XDG_CONFIG_HOME", "config"),
                           ("XDG_DATA_HOME", "data"), ("XDG_STATE_HOME", "state")):
        path = tmp_path / name
        path.mkdir()
        monkeypatch.setenv(variable, str(path))
    monkeypatch.setenv("SMARTKEY_DEBUG", "off")
    target = tmp_path / "config/smartkey/personal.json"
    target.parent.mkdir()
    assert not target.exists()
    return target


def _export(core, path):
    core.export_personal(str(path))
    return json.loads(path.read_text())


def _seed(tmp_path):
    seed = _export(_native().PyInputMethodCore("{}"), tmp_path / "seed.json")
    seed["markov_bigrams"] = [["audit", "synthetic", 3]]
    seed["markov_trigrams"] = [["audit", "synthetic", "state", 2]]
    seed["weights"]["commit_count"] = 7
    seed["corrections"] = {"entries": [{
        "context_hash": 17, "predicted_prefix": "syn", "actual": "synthetic",
        "count": 3, "access_order": 7,
    }]}
    seed["light_profile"] = {
        "lang_prior": [0.6, 0.3, 0.1], "ghost_accept_rate": 0.8,
        "suppress_after_reject": 1, "confidence_floor": 0.15,
    }
    path = tmp_path / "synthetic-profile.json"
    path.write_text(json.dumps(seed))
    return path, seed


def _assert_learning(before, after):
    for name in ("corrections", "light_profile", "markov_bigrams", "markov_trigrams", "weights"):
        assert after.get(name) == before[name], name


def test_native_explicit_profile_roundtrip_preserves_adaptive_state(tmp_path, private_paths):
    source, seed = _seed(tmp_path)
    core = _native().PyInputMethodCore("{}")
    core.import_personal(str(source))
    exported = tmp_path / "explicit.json"
    _assert_learning(seed, _export(core, exported))
    restored = _native().PyInputMethodCore("{}")
    restored.import_personal(str(exported))
    _assert_learning(seed, _export(restored, tmp_path / "explicit-roundtrip.json"))
    assert not private_paths.exists()


def test_native_default_load_restores_adaptive_state(tmp_path, private_paths):
    _source, seed = _seed(tmp_path)
    private_paths.write_text(json.dumps(seed))
    core = _native().PyInputMethodCore("{}")

    core.load_personal()

    _assert_learning(seed, _export(core, tmp_path / "after-default-load.json"))
    assert json.loads(private_paths.read_text()) == seed


def test_native_default_save_preserves_adaptive_state(tmp_path, private_paths):
    source, seed = _seed(tmp_path)
    core = _native().PyInputMethodCore("{}")
    core.import_personal(str(source))

    core.save_personal()

    _assert_learning(seed, json.loads(private_paths.read_text()))
    restored = _native().PyInputMethodCore("{}")
    restored.load_personal()
    _assert_learning(seed, _export(restored, tmp_path / "after-default-roundtrip.json"))
    assert not (Path(os.environ["HOME"]) / ".config/smartkey/personal.json").exists()


def test_native_default_load_accepts_legacy_v4_without_optional_state(tmp_path, private_paths):
    _source, seed = _seed(tmp_path)
    del seed["corrections"]
    del seed["light_profile"]
    private_paths.write_text(json.dumps(seed))
    core = _native().PyInputMethodCore("{}")

    core.load_personal()
    after = _export(core, tmp_path / "legacy-export.json")

    assert after["version"] == seed["version"] == 4
    for name in ("markov_bigrams", "markov_trigrams", "weights"):
        assert after[name] == seed[name]
    assert after["corrections"] == {"entries": []}
    assert after["light_profile"]["ghost_accept_rate"] == 0.5


def test_native_missing_default_profile_roundtrips_without_creation_on_load(tmp_path, private_paths):
    core = _native().PyInputMethodCore("{}")

    assert core.load_personal() is None
    assert not private_paths.exists()
    core.save_personal()
    saved = json.loads(private_paths.read_text())
    restored = _native().PyInputMethodCore("{}")
    restored.load_personal()
    after = _export(restored, tmp_path / "empty-roundtrip.json")

    assert saved["version"] == after["version"] == 4
    assert saved["markov_bigrams"] == after["markov_bigrams"] == []
    assert saved["markov_trigrams"] == after["markov_trigrams"] == []
    assert saved["weights"]["commit_count"] == after["weights"]["commit_count"] == 0


_NOVEL_WORD = "зориван"
_NOVEL_CODES = (44, 24, 19, 23, 17, 30, 49)
_NOVEL_CONFIG = json.dumps({
    "ghost_text": True,
    "ghost_text_separation_margin": 0.0,
    "post_commit_autocorrect": False,
})


def _type_novel(core, codes=_NOVEL_CODES):
    for code in codes:
        core.process_keycode(code, 0)


def _has_novel_candidate(core):
    return any(word.casefold() == _NOVEL_WORD for word, *_ in core.predictions())


def _confirmed_novel_core(tmp_path):
    core = _native().PyInputMethodCore(_NOVEL_CONFIG)
    seed_path = tmp_path / "novel-seed.json"
    seed = _export(core, seed_path)
    seed["light_profile"]["lang_prior"] = [0.8, 0.1, 0.1]
    seed_path.write_text(json.dumps(seed))
    core.import_personal(str(seed_path))
    _type_novel(core)
    assert core.current_word() == _NOVEL_WORD
    core.handle_key(0x20, 0)
    _type_novel(core, _NOVEL_CODES[:4])
    assert _has_novel_candidate(core)
    actions = core.handle_key(0xFF09, 0)
    assert actions.count(("commit", _NOVEL_WORD)) == 1
    assert core.current_word() == ""
    return core


def test_native_default_reload_retains_confirmed_word_evidence(tmp_path, private_paths):
    core = _confirmed_novel_core(tmp_path)
    core.save_personal()
    restored = _native().PyInputMethodCore(_NOVEL_CONFIG)
    restored.load_personal()
    before = _export(restored, tmp_path / "confirmed-before.json")

    _type_novel(restored)

    assert restored.current_word() == _NOVEL_WORD
    assert restored.debug_state()[1] is True
    restored.handle_key(0x20, 0)
    after = _export(restored, tmp_path / "confirmed-after.json")
    assert after["light_profile"]["lang_prior"][0] > before["light_profile"]["lang_prior"][0]


def test_native_legacy_profile_vocabulary_does_not_invent_confirmation(tmp_path, private_paths):
    core = _confirmed_novel_core(tmp_path)
    path = tmp_path / "legacy-novel.json"
    legacy = _export(core, path)
    legacy.pop("confirmed_words", None)
    path.write_text(json.dumps(legacy))
    restored = _native().PyInputMethodCore(_NOVEL_CONFIG)
    restored.import_personal(str(path))
    before = _export(restored, tmp_path / "legacy-before.json")
    _type_novel(restored, _NOVEL_CODES[:4])
    assert _has_novel_candidate(restored)
    _type_novel(restored, _NOVEL_CODES[4:])

    assert restored.current_word() == _NOVEL_WORD
    assert restored.debug_state()[1] is False
    restored.handle_key(0x20, 0)
    after = _export(restored, tmp_path / "legacy-after.json")
    assert after["light_profile"]["lang_prior"] == before["light_profile"]["lang_prior"]


def test_native_profile_replacement_removes_previous_personal_confirmation(tmp_path, private_paths):
    core = _confirmed_novel_core(tmp_path)
    empty_path = tmp_path / "empty-replacement.json"
    _export(_native().PyInputMethodCore(_NOVEL_CONFIG), empty_path)

    core.import_personal(str(empty_path))
    _type_novel(core, _NOVEL_CODES[:4])

    assert not _has_novel_candidate(core)
    assert core.debug_state()[1] is False
