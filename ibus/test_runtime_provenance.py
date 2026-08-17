"""Native package and repository provenance must describe the same build."""

from __future__ import annotations

import importlib.metadata
import os
import re
import subprocess
from pathlib import Path

import smartkey_py
from ibus import smartkey_engine


_REPO = Path(__file__).resolve().parents[1]


def _git(*args: str) -> str:
    return subprocess.run(
        ["git", "-C", str(_REPO), *args],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()


def test_native_module_reports_exact_build_provenance():
    expected_sha = os.environ.get("SMARTKEY_EXPECT_BUILD_GIT_SHA")
    expected_dirty_raw = os.environ.get("SMARTKEY_EXPECT_BUILD_GIT_DIRTY")
    assert (expected_sha is None) == (expected_dirty_raw is None)
    if expected_sha is None:
        expected_sha = _git("rev-parse", "--verify", "HEAD")
        expected_dirty = None
    else:
        assert expected_dirty_raw in {"0", "1"}
        expected_dirty = expected_dirty_raw == "1"

    assert smartkey_py.__version__ == importlib.metadata.version("smartkey-py")
    assert re.fullmatch(r"[0-9a-f]{40}", smartkey_py.__build_git_sha__)
    assert smartkey_py.__build_git_sha__ == expected_sha
    assert smartkey_py.__build_dirty__ is expected_dirty
    for name in ("__version__", "__build_git_sha__", "__build_dirty__"):
        assert name in smartkey_py.__all__


def test_phasea_run_provenance_comes_from_loaded_native_not_stale_config():
    expected = smartkey_py.__build_git_sha__
    if smartkey_py.__build_dirty__ is True:
        expected += "+dirty"
    elif smartkey_py.__build_dirty__ is None:
        expected += "+dirty-unknown"

    assert smartkey_engine._native_engine_commit("stale-config") == expected
    assert expected != "stale-config"


def test_embedded_unknown_provenance_never_uses_stale_config(monkeypatch):
    monkeypatch.setattr(smartkey_engine, "_NATIVE_HAS_BUILD_PROVENANCE", True)
    monkeypatch.setattr(smartkey_engine, "_NATIVE_BUILD_GIT_SHA", "unknown")
    monkeypatch.setattr(smartkey_engine, "_NATIVE_BUILD_DIRTY", None)

    assert smartkey_engine._native_engine_commit("stale-config") is None


def test_legacy_module_without_provenance_may_use_config(monkeypatch):
    monkeypatch.setattr(smartkey_engine, "_NATIVE_HAS_BUILD_PROVENANCE", False)
    monkeypatch.setattr(smartkey_engine, "_NATIVE_BUILD_GIT_SHA", "unknown")
    monkeypatch.setattr(smartkey_engine, "_NATIVE_BUILD_DIRTY", None)

    assert smartkey_engine._native_engine_commit("legacy-config") == "legacy-config"
