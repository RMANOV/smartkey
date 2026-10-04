"""Native (PyO3) regression for B9 / F1 — the first-character language prior
must bias, never lock (ADVOCATE_CODEX ruling 487688994026, 2026-08-23).

Exact live repro: with Latin surrounding text (a shell line) the old engine
committed Bulgarian physical-key words as the EN keymap text
(„имаш" → "ima[", „след" → "sled", „превключване" → "prewkl`wane").

Which module: ``SMARTKEY_NATIVE_MODULE_DIR`` (directory containing the
``smartkey_py`` package, e.g. an unzipped audited wheel). Fixed synthetic
load_word fixtures replace any user corpus dependency. Missing explicit
artifact skips ordinary optional collection, but required audit mode fails
closed. No claim about actual user-frequency corpus accuracy or live input.
"""

from __future__ import annotations

import json
import os
import tempfile

import pytest

from .test_native_space_accept import _native

if "SMARTKEY_PHASEA_DATA" not in os.environ:
    os.environ["SMARTKEY_PHASEA_DATA"] = tempfile.mkdtemp(prefix="smartkey-test-phasea-")

_MODULE_DIR = os.environ.get("SMARTKEY_NATIVE_MODULE_DIR")
pytestmark = pytest.mark.skipif(
    not _MODULE_DIR and os.environ.get("SMARTKEY_NATIVE_AUDIT_REQUIRED") != "1",
    reason="needs explicit native audit artifact",
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
# Fixed synthetic fixtures, not a claim about real-user corpus accuracy.
FIXTURE_FREQUENCIES = {word: 1_000_000 for word in (*WORDS, *ENGLISH)}


def _core(surrounding=None):
    smartkey_py = _native()
    core = smartkey_py.PyInputMethodCore(json.dumps({}))
    for word, frequency in FIXTURE_FREQUENCIES.items():
        core.load_word(word, frequency)
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
