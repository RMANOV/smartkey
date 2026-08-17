"""Release-version contract across Cargo, IBus XML, and generated XML."""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import sys
import tomllib
import xml.etree.ElementTree as ET


_REPO = Path(__file__).resolve().parent.parent


def test_legacy_native_version_falls_back_to_distribution(monkeypatch):
    from ibus import smartkey_engine

    monkeypatch.setattr(
        smartkey_engine.importlib.metadata,
        "version",
        lambda distribution: "0.5.0",
    )

    assert smartkey_engine._installed_native_version("unknown") == "0.5.0"


def test_native_version_fallback_stays_explicit_when_distribution_is_missing(
    monkeypatch,
):
    from ibus import smartkey_engine

    def _missing(_distribution):
        raise smartkey_engine.importlib.metadata.PackageNotFoundError

    monkeypatch.setattr(smartkey_engine.importlib.metadata, "version", _missing)

    assert smartkey_engine._installed_native_version("unavailable") == "unavailable"


def test_embedded_native_version_wins_without_metadata_lookup(monkeypatch):
    from ibus import smartkey_engine

    class _Native:
        __version__ = "0.6.2"

    def _unexpected_lookup(_distribution):
        raise AssertionError("embedded version must avoid dist-info lookup")

    monkeypatch.setattr(
        smartkey_engine.importlib.metadata, "version", _unexpected_lookup
    )

    assert smartkey_engine._loaded_native_version(_Native()) == "0.6.2"


def test_failed_native_import_never_claims_a_stale_distribution_version(tmp_path):
    blocker = tmp_path / "smartkey_py"
    blocker.mkdir()
    (blocker / "__init__.py").write_text(
        'raise ImportError("scripted missing native extension")\n',
        encoding="utf-8",
    )
    env = dict(os.environ)
    env["PYTHONPATH"] = os.pathsep.join((str(tmp_path), str(_REPO)))
    proc = subprocess.run(
        [
            sys.executable,
            "-c",
            "from importlib.metadata import version; "
            "assert version('smartkey-py'); "
            "from ibus import smartkey_engine as s; "
            "assert s._HAS_CORE is False; "
            "assert s._NATIVE_VERSION == 'unavailable'",
        ],
        cwd=_REPO,
        env=env,
        text=True,
        capture_output=True,
        timeout=15,
        check=False,
    )

    assert proc.returncode == 0, f"stdout={proc.stdout}\nstderr={proc.stderr}"


def test_ibus_versions_match_the_native_crate_manifest():
    manifest = tomllib.loads(
        (_REPO / "crates" / "smartkey-py" / "Cargo.toml").read_text(encoding="utf-8")
    )
    expected = manifest["package"]["version"]
    static_version = ET.parse(_REPO / "ibus" / "smartkey.xml").getroot().findtext(
        "version"
    )
    proc = subprocess.run(
        [sys.executable, "-m", "ibus.main", "--xml"],
        cwd=_REPO,
        text=True,
        capture_output=True,
        timeout=15,
        check=False,
    )

    assert proc.returncode == 0, f"stdout={proc.stdout}\nstderr={proc.stderr}"
    generated_version = ET.fromstring(proc.stdout).findtext("version")
    assert static_version == expected
    assert generated_version == expected


def test_production_launcher_does_not_enable_lossy_or_content_debug_modes():
    launcher = (_REPO / "ibus" / "ibus-engine-smartkey").read_text(
        encoding="utf-8"
    )
    assert "export SMARTKEY_SENSITIVE_UNTIL_DECLARED=1" not in launcher
    assert "export SMARTKEY_DEBUG=full" not in launcher
    assert "export SMARTKEY_SENSITIVE_UNTIL_DECLARED=0" in launcher
