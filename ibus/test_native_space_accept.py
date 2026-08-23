"""Native (PyO3) regression for the opt-in Space-accept display identity.

Runs the ADVOCATE_CODEX counterexample (c423f1631ad2) through the REAL
compiled module, not a fake core:

  flag ON → tracked prediction → raw keys compose здра|вей → the adapter's
  public ``record_ghost_rejection`` consumes the display attribution while the
  core still holds ghost/top → raw Space must be LITERAL (typed prefix +
  forward, no acceptance).  Positive control: without the feedback call the
  same Space is accepted as one commit "здравей " with no forward.

Which module: ``SMARTKEY_NATIVE_MODULE_DIR`` (a directory containing the
``smartkey_py`` package, e.g. an unzipped worktree wheel) takes precedence;
otherwise the test is SKIPPED — the live installed module must never be
assumed to be the artifact under audit.  Temp Phase-A dir; no live data.
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
pytestmark = pytest.mark.skipif(
    not _MODULE_DIR, reason="SMARTKEY_NATIVE_MODULE_DIR not set (audit artifact only)"
)

SPACE = 57
ZDRA = (44, 32, 19, 30)  # з д р а on the physical keyboard
FLAG_ON = json.dumps({"accept": {"space_accept": True}})


_NATIVE = None


def _native():
    # A compiled extension can be imported only once per process: load it
    # exactly once from the audited directory and reuse it.
    global _NATIVE  # noqa: PLW0603
    if _NATIVE is None:
        sys.path.insert(0, _MODULE_DIR)
        assert "smartkey_py" not in sys.modules, "another smartkey_py already loaded"
        _NATIVE = importlib.import_module("smartkey_py")
        assert _NATIVE.__file__.startswith(_MODULE_DIR), _NATIVE.__file__
    return _NATIVE


def _compose(core):
    last = []
    for code in ZDRA:
        last = core.process_keycode(code, 0)
    assert core.current_word() == "здра"
    assert any(a == "composing" and p.startswith("здра\x00") for a, p in last), last
    top = core.predictions()[0][0]
    assert top.lower() == "здравей", top
    return top


def test_native_space_accepts_exact_displayed_pair():
    smartkey_py = _native()
    core = smartkey_py.PyInputMethodCore(FLAG_ON)
    core.load_word("здравей", 1_000_000)
    _compose(core)

    actions = core.process_keycode(SPACE, 0)

    assert ("commit", "здравей ") in actions, actions
    assert not any(a == "forward" for a, _ in actions), actions


def test_native_space_is_literal_after_public_rejection_feedback():
    smartkey_py = _native()
    core = smartkey_py.PyInputMethodCore(FLAG_ON)
    core.load_word("здравей", 1_000_000)
    top = _compose(core)

    core.record_ghost_rejection("здра", top)
    assert core.current_word() == "здра", "core prefix retained"

    actions = core.process_keycode(SPACE, 0)

    assert ("commit", "здра") in actions, actions
    assert any(a == "forward" for a, _ in actions), actions
    assert not any(a == "commit" and "здравей" in p for a, p in actions), actions


def test_native_flag_off_space_is_literal_with_visible_ghost():
    smartkey_py = _native()
    core = smartkey_py.PyInputMethodCore(json.dumps({}))
    core.load_word("здравей", 1_000_000)
    _compose(core)

    actions = core.process_keycode(SPACE, 0)

    assert ("commit", "здра") in actions, actions
    assert any(a == "forward" for a, _ in actions), actions
