"""Native (PyO3) regression for B9 / F1 — the first-character language prior
must bias, never lock (ADVOCATE_CODEX ruling 487688994026, 2026-08-23).

Exact live repro: with Latin surrounding text (a shell line) the old engine
committed Bulgarian physical-key words as the EN keymap text
(„имаш" → "ima[", „след" → "sled", „превключване" → "prewkl`wane").

Which module: ``SMARTKEY_NATIVE_MODULE_DIR`` (directory containing the
``smartkey_py`` package, e.g. an unzipped worktree wheel).  Corpora: the
machine's ``~/.config/smartkey/corpus_*.bin`` (read-only).  Without either
the tests are SKIPPED — this file is an audit artifact, never a blind
assumption about the installed module.  Temp Phase-A dir; no live data.
"""

from __future__ import annotations

import importlib
import json
import os
import sys
import tempfile

import pytest

os.environ.setdefault(
    "SMARTKEY_PHASEA_DATA", tempfile.mkdtemp(prefix="smartkey-test-phasea-")
)

_MODULE_DIR = os.environ.get("SMARTKEY_NATIVE_MODULE_DIR")
_CORPUS_DIR = os.path.expanduser("~/.config/smartkey")
_CORPORA = [os.path.join(_CORPUS_DIR, f) for f in ("corpus_en.bin", "corpus_bg.bin")]
pytestmark = pytest.mark.skipif(
    not _MODULE_DIR or not all(os.path.exists(p) for p in _CORPORA),
    reason="needs SMARTKEY_NATIVE_MODULE_DIR and the local corpora (audit artifact)",
)

SPACE = 57
WORDS = {
    "имаш": [23, 50, 30, 26],
    "отговор": [24, 20, 34, 24, 17, 24, 19],
    "след": [31, 38, 18, 32],
    "превкл": [25, 19, 18, 17, 37, 38],
}
ENGLISH = {"the": [20, 35, 18], "is": [23, 31], "git": [34, 23, 20]}
LATIN_CONTEXT = "cd ~/smartkey && cargo test -p smartkey-core "
_NATIVE = None


def _native():
    global _NATIVE  # noqa: PLW0603
    if _NATIVE is None:
        sys.path.insert(0, _MODULE_DIR)
        assert "smartkey_py" not in sys.modules
        _NATIVE = importlib.import_module("smartkey_py")
        assert _NATIVE.__file__.startswith(_MODULE_DIR), _NATIVE.__file__
    return _NATIVE


def _core(surrounding=None):
    smartkey_py = _native()
    core = smartkey_py.PyInputMethodCore(json.dumps({}))
    for path in _CORPORA:
        core.load_corpus_file(path)
    if surrounding is not None:
        core.set_surrounding_text(surrounding, len(surrounding))
    return core


def _type(core, codes):
    for code in codes:
        core.process_keycode(code, 0)
    word = core.current_word()
    core.process_keycode(SPACE, 0)
    return word


def test_native_bulgarian_words_with_latin_surrounding_are_cyrillic():
    core = _core(LATIN_CONTEXT)
    got = {w: _type(core, codes) for w, codes in WORDS.items()}
    assert got == {w: w for w in WORDS}, got


def test_native_bulgarian_words_without_context_are_cyrillic():
    core = _core()
    got = {w: _type(core, codes) for w, codes in WORDS.items()}
    assert got == {w: w for w in WORDS}, got


def test_native_english_words_with_latin_surrounding_stay_english():
    core = _core(LATIN_CONTEXT)
    got = {w: _type(core, codes) for w, codes in ENGLISH.items()}
    assert got == {w: w for w in ENGLISH}, got


def test_native_english_word_after_bulgarian_words_is_english():
    core = _core()
    for codes in WORDS.values():
        _type(core, codes)
    assert _type(core, ENGLISH["the"]) == "the"
