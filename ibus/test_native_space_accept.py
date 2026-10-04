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
import hashlib
import json
import os
import sys
import tempfile
from pathlib import Path

import pytest

if "SMARTKEY_PHASEA_DATA" not in os.environ:
    os.environ["SMARTKEY_PHASEA_DATA"] = tempfile.mkdtemp(prefix="smartkey-test-phasea-")

_MODULE_DIR = os.environ.get("SMARTKEY_NATIVE_MODULE_DIR")
_REQUIRED = os.environ.get("SMARTKEY_NATIVE_AUDIT_REQUIRED") == "1"
pytestmark = pytest.mark.skipif(
    not _MODULE_DIR and not _REQUIRED,
    reason="SMARTKEY_NATIVE_MODULE_DIR not set (audit artifact only)",
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
        assert _MODULE_DIR, "required native artifact directory missing"
        root = Path(_MODULE_DIR).resolve(strict=True)
        assert not any(name == "smartkey_py" or name.startswith("smartkey_py.")
                       for name in sys.modules), "another smartkey_py already loaded"
        sys.path.insert(0, str(root))
        native = importlib.import_module("smartkey_py")
        Path(native.__file__).resolve(strict=True).relative_to(root)
        extensions = []
        for name, module in tuple(sys.modules.items()):
            if name == "smartkey_py" or name.startswith("smartkey_py."):
                origin = Path(module.__file__).resolve(strict=True)
                origin.relative_to(root)
                if origin.suffix in {".so", ".pyd"}:
                    extensions.append(origin)
        assert len(set(extensions)) == 1, extensions
        extension = extensions[0]
        assert extension == Path(os.environ["SMARTKEY_NATIVE_EXTENSION"]).resolve(strict=True)
        assert hashlib.sha256(extension.read_bytes()).hexdigest() == os.environ["SMARTKEY_NATIVE_EXTENSION_SHA256"]
        assert native.__build_git_sha__ == os.environ["SMARTKEY_EXPECT_BUILD_GIT_SHA"]
        assert native.__build_dirty__ is (os.environ["SMARTKEY_EXPECT_BUILD_GIT_DIRTY"] == "1")
        _NATIVE = native
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
