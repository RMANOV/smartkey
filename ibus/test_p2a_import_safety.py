"""Import-safety regressions for the bounded P2a offline harness."""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import sys

import pytest


_IBUS_DIR = Path(__file__).resolve().parent
_HARNESS_MODULES = (
    _IBUS_DIR / "test_space_accept.py",
    _IBUS_DIR / "test_accept_backspace.py",
)


@pytest.mark.parametrize("module_path", _HARNESS_MODULES, ids=lambda path: path.stem)
def test_supplied_phasea_path_does_not_allocate_tempdir(tmp_path, module_path):
    """Importing a harness with an explicit data path must allocate nothing."""
    supplied_phase = tmp_path / "supplied-phase-does-not-exist"
    code = r"""
import importlib.util
import pathlib
import sys
import tempfile

module_path = pathlib.Path(sys.argv[1])

def fail_allocator(*_args, **_kwargs):
    raise AssertionError("tempfile.mkdtemp reached with SMARTKEY_PHASEA_DATA supplied")

tempfile.mkdtemp = fail_allocator
sys.modules["smartkey_py"] = None
sys.path.insert(0, str(module_path.parent))
spec = importlib.util.spec_from_file_location("p2a_import_target", module_path)
assert spec is not None and spec.loader is not None
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
"""
    env = {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "PYTHONDONTWRITEBYTECODE": "1",
        "SMARTKEY_DEBUG": "off",
        "SMARTKEY_PHASEA_DATA": str(supplied_phase),
        "XDG_CONFIG_HOME": str(tmp_path / "config-does-not-exist"),
        "XDG_DATA_HOME": str(tmp_path / "data-does-not-exist"),
        "XDG_STATE_HOME": str(tmp_path / "state-does-not-exist"),
    }
    completed = subprocess.run(
        [sys.executable, "-I", "-S", "-B", "-c", code, str(module_path)],
        check=False,
        capture_output=True,
        env=env,
        text=True,
        timeout=10,
    )
    assert completed.returncode == 0, completed.stderr
    assert not supplied_phase.exists()
