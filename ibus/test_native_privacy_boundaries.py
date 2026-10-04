"""Real native calls with synthetic state in an explicitly isolated audit packet.

Not physical keyboard/GUI acceptance or real-user corpus accuracy evidence.
"""

import json
import os
from pathlib import Path

import pytest

from .test_native_space_accept import ZDRA, _compose, _native

pytestmark = pytest.mark.skipif(
    not os.environ.get("SMARTKEY_NATIVE_MODULE_DIR")
    and os.environ.get("SMARTKEY_NATIVE_AUDIT_REQUIRED") != "1",
    reason="needs explicit native audit artifact",
)


def _core():
    core = _native().PyInputMethodCore(json.dumps({}))
    core.load_word("здравей", 1_000_000)
    return core


def test_native_private_save_and_load_roundtrip(tmp_path):
    core = _core()
    for _ in range(3):
        for code in ZDRA:
            core.process_keycode(code, 0)
        core.handle_key(0xFF09, 0)
    core.save_personal()
    private = Path(os.environ["XDG_CONFIG_HOME"]).resolve()
    saved = private / "smartkey" / "personal.json"
    saved.resolve(strict=True).relative_to(private)
    profile = json.loads(saved.read_text())
    assert profile["version"] == 4
    assert profile["weights"]["commit_count"] >= 3
    assert profile["markov_bigrams"], "synthetic repeated word must leave learned state"
    restored = _core()
    restored.load_personal()
    exported = tmp_path / "restored.json"
    restored.export_personal(str(exported))
    reloaded = json.loads(exported.read_text())
    assert reloaded["markov_bigrams"] == profile["markov_bigrams"]
    assert reloaded["markov_trigrams"] == profile["markov_trigrams"]
    assert reloaded["weights"]["commit_count"] == profile["weights"]["commit_count"]
    assert not (Path(os.environ["HOME"]) / ".config" / "smartkey" / "personal.json").exists()


def test_native_explicit_private_export_and_import(tmp_path):
    core = _core()
    target = tmp_path / "nested" / "profile.json"
    core.export_personal(str(target))
    profile = json.loads(target.read_text())
    assert profile["version"] == 4
    restored = _core()
    restored.import_personal(str(target))
    control = tmp_path / "control.json"
    restored.export_personal(str(control))
    assert json.loads(control.read_text())["markov_bigrams"] == profile["markov_bigrams"]
    target.write_text("not JSON")
    with pytest.raises(OSError):
        restored.import_personal(str(target))


def test_native_readonly_canary_export_fails_without_change():
    canary = Path(os.environ["SMARTKEY_NATIVE_READONLY_CANARY"])
    before = canary.read_bytes()
    with pytest.raises(OSError):
        _core().export_personal(str(canary))
    assert canary.read_bytes() == before == b"synthetic-readonly-canary\n"


def test_native_tab_accepts_full_display_once():
    core = _core()
    _compose(core)
    assert core.handle_key(0xFF09, 0) == [("hide", ""), ("commit", "здравей")]
    assert core.current_word() == ""
    assert core.handle_key(0xFF09, 0) == [("forward", "")]


def test_native_right_incremental_then_final_full_commit():
    core = _core()
    _compose(core)
    assert core.handle_key(0xFF53, 0) == [("composing", "здрав\x00ей")]
    assert core.current_word() == "здрав"
    assert core.handle_key(0xFF53, 0) == [("composing", "здраве\x00й")]
    assert core.current_word() == "здраве"
    assert core.handle_key(0xFF53, 0) == [("hide", ""), ("commit", "здравей")]
    assert core.current_word() == ""


def test_native_focus_flush_reset_and_no_stale_acceptance():
    core = _core()
    _compose(core)
    assert core.focus_lost() == [("commit", "здра"), ("hide", "")]
    assert core.current_word() == ""
    assert core.focus_lost() == [("hide", "")]
    assert core.focus_gained() is None
    assert core.handle_key(0xFF09, 0) == [("forward", "")]
    _compose(core)
    assert core.handle_key(0xFF09, 0) == [("hide", ""), ("commit", "здравей")]
