"""Import-time confinement regressions for the anomaly-registry harness."""

from __future__ import annotations

from contextlib import contextmanager
import importlib.util
from pathlib import Path
import sys
import tempfile


_TARGET = Path(__file__).with_name("test_anomaly_registry.py")


@contextmanager
def _fresh_target(module_name: str):
    """Execute the whole real harness under an isolated module identity."""
    assert module_name not in sys.modules
    spec = importlib.util.spec_from_file_location(module_name, _TARGET)
    assert spec is not None
    assert spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[module_name] = module
    try:
        spec.loader.exec_module(module)
        yield module
    finally:
        sys.modules.pop(module_name, None)


def test_provided_phasea_root_is_retained_without_allocator_call(monkeypatch, tmp_path):
    """Catches eager allocation in the existing-value branch."""
    provided = tmp_path / "provided-phasea"
    provided.mkdir()
    provided_text = str(provided.resolve())
    assert provided_text
    assert Path(provided_text).is_absolute()
    assert not Path(provided_text).is_symlink()
    monkeypatch.setenv("SMARTKEY_PHASEA_DATA", provided_text)

    calls = []

    def fail_on_call(*args, **kwargs):
        calls.append((args, kwargs))
        raise AssertionError("unwanted eager SMARTKEY_PHASEA_DATA allocation")

    monkeypatch.setattr(tempfile, "mkdtemp", fail_on_call)
    with _fresh_target("_smartkey_registry_provided_root_case"):
        assert sys.modules.get("_smartkey_registry_provided_root_case") is not None

    assert calls == []
    assert sys.modules.get("_smartkey_registry_provided_root_case") is None
    assert provided_text == str(provided.resolve())
    assert provided_text == __import__("os").environ["SMARTKEY_PHASEA_DATA"]


def test_missing_phasea_root_allocates_once_and_sets_returned_owned_path(
    monkeypatch, tmp_path
):
    """Catches a missing allocator call, duplicate calls, or wrong assignment."""
    monkeypatch.delenv("SMARTKEY_PHASEA_DATA", raising=False)
    allocated = tmp_path / "allocated-phasea"
    allocated_text = str(allocated.resolve())
    assert allocated_text
    assert Path(allocated_text).is_absolute()
    assert not allocated.exists()
    calls = []

    def allocate_owned(*args, **kwargs):
        calls.append((args, kwargs))
        assert args == ()
        assert kwargs == {"prefix": "smartkey-test-phasea-"}
        allocated.mkdir()
        return allocated_text

    monkeypatch.setattr(tempfile, "mkdtemp", allocate_owned)
    with _fresh_target("_smartkey_registry_missing_root_case"):
        assert sys.modules.get("_smartkey_registry_missing_root_case") is not None

    assert calls == [((), {"prefix": "smartkey-test-phasea-"})]
    assert sys.modules.get("_smartkey_registry_missing_root_case") is None
    assert allocated.is_dir()
    assert not allocated.is_symlink()
    assert allocated_text == __import__("os").environ["SMARTKEY_PHASEA_DATA"]
