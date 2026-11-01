"""Real IBus/GI cache contract on a fresh private bus, without native Core.

The child has a temporary HOME and imports the real adapter/GI in its own
process. A constructor-free callback seam records Core delivery; it cannot
load a SmartKey native module, personal profile or the desktop bus.
"""
from __future__ import annotations

import json
from pathlib import Path
import shutil
import subprocess
import sys

import pytest


_PROBE = r'''
import json,sys,types
from pathlib import Path
import gi
gi.require_version("IBus", "1.0")
from gi.repository import IBus,Gio
sys.path.insert(0,sys.argv[1])
blocked=types.ModuleType("smartkey_py")
def forbidden(*args,**kwargs):
    raise AssertionError("native Core constructor must not run")
blocked.PyInputMethodCore=forbidden
sys.modules["smartkey_py"]=blocked
from ibus import smartkey_engine as adapter
assert adapter._HAS_IBUS
connection=Gio.bus_get_sync(Gio.BusType.SESSION,None)
text="todo неделя todo нед"
position=len(text)

def cached(engine):
    result=engine.get_surrounding_text()
    assert isinstance(result,tuple) and len(result)==3
    return [result[0].get_text(),result[1],result[2]]

control=IBus.Engine(connection=connection,object_path="/org/freedesktop/IBus/Engine/Control")
control.emit("set-surrounding-text",IBus.Text.new_from_string(text),position,position)
control_report=cached(control)
# Keep the native VFuncInfo descriptor intact: replacing it with a Python
# function changes implicit-self binding and can hide a broken parent call.

class CoreRecorder:
    def __init__(self): self.reports=[]
    def set_surrounding_text(self,text,cursor): self.reports.append([text,cursor])

class CallbackProbe(adapter.SmartKeyEngine):
    __gtype_name__="SmartKeySurroundingCacheRegression"
    def __init__(self,path,sensitive=False):
        IBus.Engine.__init__(self,connection=connection,object_path=path,active_surrounding_text=True)
        self._sensitive=sensitive
        self._core=CoreRecorder()
        self._surrounding_text=None
        self._surrounding_cursor_pos=None
        self._surrounding_anchor_pos=None

engine=CallbackProbe("/org/freedesktop/IBus/Engine/Candidate")
for value in ["todo неделя todo ",text]:
    n=len(value)
    engine.emit("set-surrounding-text",IBus.Text.new_from_string(value),n,n)
callback_report=[engine._surrounding_text,engine._surrounding_cursor_pos,engine._surrounding_anchor_pos]
cache_report=cached(engine)
engine._refresh_surrounding_text()
refreshed=[engine._surrounding_text,engine._surrounding_cursor_pos,engine._surrounding_anchor_pos]
sensitive=CallbackProbe("/org/freedesktop/IBus/Engine/Sensitive",True)
sensitive.emit("set-surrounding-text",IBus.Text.new_from_string("synthetic-secret"),16,16)
sensitive_cache=cached(sensitive)
assert sys.modules["smartkey_py"] is blocked
print(json.dumps(dict(control=control_report,callback=callback_report,cache=cache_report,
    refreshed=refreshed,core_reports=engine._core.reports,
    sensitive_core_reports=sensitive._core.reports,sensitive_cache_empty=sensitive_cache==["",0,0],
    sensitive_python_empty=sensitive._surrounding_text is None,real_bus_unique_name=connection.get_unique_name())))
'''


def test_real_gi_surrounding_callback_populates_parent_cache_before_refresh(tmp_path):
    if sys.platform != "linux":
        pytest.skip("IBus GI contract is Linux-specific")
    python = Path("/usr/bin/python3")
    dbus = shutil.which("dbus-run-session")
    if not python.exists() or dbus is None:
        pytest.skip("system Python/private D-Bus launcher unavailable")
    env = {"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8", "SMARTKEY_DEBUG": "off", "PYTHONDONTWRITEBYTECODE": "1"}
    for key in ("HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "TMPDIR", "SMARTKEY_PHASEA_DATA", "SMARTKEY_DEBUG_DIR"):
        path = tmp_path / key.lower()
        path.mkdir()
        env[key] = str(path)
    available = subprocess.run([str(python), "-I", "-B", "-c", 'import gi;gi.require_version("IBus","1.0");from gi.repository import IBus,Gio'], env=env, capture_output=True, text=True, timeout=10)
    if available.returncode:
        pytest.skip("system IBus GI typelib unavailable")
    result = subprocess.run([dbus, "--", str(python), "-I", "-B", "-c", _PROBE, str(Path(__file__).resolve().parents[1])], env=env, capture_output=True, text=True, timeout=20)
    assert result.returncode == 0, result.stderr
    (tmp_path / "gi-child.json").write_text(result.stdout)
    (tmp_path / "gi-child.stderr").write_text(result.stderr)
    assert result.stderr == "", result.stderr
    observed = json.loads(result.stdout)
    expected = ["todo неделя todo нед", 20, 20]
    assert observed["control"] == observed["callback"] == expected
    assert observed["cache"] == observed["refreshed"] == expected, observed
    assert observed["core_reports"] == [["todo неделя todo ", 17], [expected[0], 20], [expected[0], 20]]
    assert observed["sensitive_core_reports"] == []
    assert observed["sensitive_cache_empty"] and observed["sensitive_python_empty"]
    assert observed["real_bus_unique_name"].startswith(":")
