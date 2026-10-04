"""Import-safety regressions for the bounded P2a offline harness."""

from __future__ import annotations

import json
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


# --- Harness-native self-fence -----------------------------------------------
#
# Executing a harness module executes the whole adapter module.  The harness
# itself must make the native extension and the content-level debug sinks
# unreachable for exactly that window, whatever is installed, cached or
# exported in the surrounding process, and must restore the exact previous
# ``sys.modules`` entry and ``SMARTKEY_DEBUG`` value afterwards -- also on an
# exceptional exit.  Everything below runs in isolated children against a
# harmless synthetic ``smartkey_py`` that only counts its own executions.

_SENTINEL_SOURCE = '''\
"""Test-owned harmless stand-in for the native extension: counts executions."""
import os
import pathlib

_MARKER = pathlib.Path(os.environ["P2A_SENTINEL_MARKER"])
_MARKER.write_text(str(int(_MARKER.read_text()) + 1) if _MARKER.exists() else "1")


class PyInputMethodCore:
    def __init__(self, _config_json=None):
        return None
'''

_FENCE_PROBE = r"""
import importlib.util
import json
import os
import pathlib
import shutil
import sys

module_path = pathlib.Path(sys.argv[1])
scenario = sys.argv[2]
root = pathlib.Path(sys.argv[3])
marker = pathlib.Path(os.environ["P2A_SENTINEL_MARKER"])
missing = object()

sys.path.insert(0, str(root / "sitefake"))
sys.path.insert(0, str(module_path.parent))


def exec_count():
    return int(marker.read_text()) if marker.exists() else 0


target = module_path
if scenario == "precached":
    import smartkey_py  # noqa: F401 -- intentional pre-import of the sentinel
elif scenario == "debug_full":
    os.environ["SMARTKEY_DEBUG"] = "full"
elif scenario == "debug_custom":
    os.environ["SMARTKEY_DEBUG"] = "structural"
elif scenario == "failure_cleanup":
    import smartkey_py  # noqa: F401 -- intentional pre-import of the sentinel
    os.environ["SMARTKEY_DEBUG"] = "structural"
    broken = root / "broken"
    broken.mkdir()
    shutil.copyfile(module_path, broken / module_path.name)
    (broken / "smartkey_engine.py").write_text(
        'raise RuntimeError("synthetic engine failure")\n'
    )
    target = broken / module_path.name

native_before = sys.modules.get("smartkey_py", missing)
pre_exec = exec_count()

spec = importlib.util.spec_from_file_location("p2a_fence_target", target)
assert spec is not None and spec.loader is not None
module = importlib.util.module_from_spec(spec)
exception_type = None
try:
    spec.loader.exec_module(module)
except Exception as exc:  # noqa: BLE001 -- reported verbatim, asserted by the parent
    exception_type = type(exc).__name__

native_after = sys.modules.get("smartkey_py", missing)
if native_after is missing:
    native_state = "absent"
elif native_before is not missing and native_after is native_before:
    native_state = "same_object"
elif native_after is None:
    native_state = "none"
else:
    native_state = "other"
ske = getattr(module, "ske", None)
report = {
    "exception_type": exception_type,
    "has_core": getattr(ske, "_HAS_CORE", None),
    "debug": getattr(ske, "_DEBUG", None),
    "pre_exec_count": pre_exec,
    "exec_count": exec_count(),
    "native_after": native_state,
    "debug_env_after": os.environ.get("SMARTKEY_DEBUG", "absent"),
    "phase_exists": pathlib.Path(os.environ["SMARTKEY_PHASEA_DATA"]).exists(),
    "xdg_data_smartkey_exists": (
        pathlib.Path(os.environ["XDG_DATA_HOME"]) / "smartkey"
    ).exists(),
}
print(json.dumps(report))
"""

_FENCE_SCENARIOS = ("absent", "precached", "debug_full", "debug_custom", "failure_cleanup")

_FENCE_EXPECTED = {
    "absent": {
        "exception_type": None,
        "has_core": False,
        "debug": False,
        "pre_exec_count": 0,
        "exec_count": 0,
        "native_after": "absent",
        "debug_env_after": "absent",
    },
    "precached": {
        "exception_type": None,
        "has_core": False,
        "debug": False,
        "pre_exec_count": 1,
        "exec_count": 1,
        "native_after": "same_object",
        "debug_env_after": "absent",
    },
    "debug_full": {
        "exception_type": None,
        "has_core": False,
        "debug": False,
        "pre_exec_count": 0,
        "exec_count": 0,
        "native_after": "absent",
        "debug_env_after": "full",
    },
    "debug_custom": {
        "exception_type": None,
        "has_core": False,
        "debug": False,
        "pre_exec_count": 0,
        "exec_count": 0,
        "native_after": "absent",
        "debug_env_after": "structural",
    },
    "failure_cleanup": {
        "exception_type": "RuntimeError",
        "has_core": None,
        "debug": None,
        "pre_exec_count": 1,
        "exec_count": 1,
        "native_after": "same_object",
        "debug_env_after": "structural",
    },
}


@pytest.mark.parametrize("scenario", _FENCE_SCENARIOS)
@pytest.mark.parametrize("module_path", _HARNESS_MODULES, ids=lambda path: path.stem)
def test_harness_self_fences_native_and_debug(tmp_path, module_path, scenario):
    """The harness alone keeps the native core and debug sinks unreachable."""
    sitefake = tmp_path / "sitefake" / "smartkey_py"
    sitefake.mkdir(parents=True)
    (sitefake / "__init__.py").write_text(_SENTINEL_SOURCE, encoding="utf-8")
    supplied_phase = tmp_path / "supplied-phase-does-not-exist"
    env = {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "PYTHONDONTWRITEBYTECODE": "1",
        "P2A_SENTINEL_MARKER": str(tmp_path / "sentinel-exec-count"),
        "SMARTKEY_PHASEA_DATA": str(supplied_phase),
        "XDG_CONFIG_HOME": str(tmp_path / "config-does-not-exist"),
        "XDG_DATA_HOME": str(tmp_path / "data-does-not-exist"),
        "XDG_STATE_HOME": str(tmp_path / "state-does-not-exist"),
    }
    completed = subprocess.run(
        [sys.executable, "-I", "-S", "-B", "-c", _FENCE_PROBE, str(module_path), scenario, str(tmp_path)],
        check=False,
        capture_output=True,
        env=env,
        text=True,
        timeout=10,
    )
    assert completed.returncode == 0, completed.stderr
    report = json.loads(completed.stdout.strip().splitlines()[-1])
    expected = dict(_FENCE_EXPECTED[scenario], phase_exists=False, xdg_data_smartkey_exists=False)
    assert report == expected
