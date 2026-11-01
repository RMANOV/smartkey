"""KDE's explicit BG selection must bypass automatic SmartKey input.

Only the external Core and bus are fakes; the actual adapter dispatch runs.
Native Qt/Wayland tests establish separate real-client delivery evidence.
"""
from types import SimpleNamespace

import pytest

from ibus.test_input_safety import FakeTrace, SpyCore, build_engine, ske


class Core(SpyCore):
    def __init__(self):
        super().__init__()
        self.word = ""

    def current_word(self):
        return self.word

    def process_keycode(self, code, state):
        self.calls.append(("process_keycode", (code, state)))
        self.word = "по"
        return [("composing", "по\0дай")]

    def flush_preedit_for_layout(self):
        self.calls.append(("flush_preedit_for_layout", ()))
        word, self.word = self.word, ""
        return [("hide", "")] + ([("commit", word)] if word else [])

    def focus_gained(self):
        self.calls.append(("focus_gained", ()))


def engine(monkeypatch, manual=False):
    obj, recording = build_engine()
    monkeypatch.setattr(ske, "_HAS_CORE", True)
    obj._core = Core()
    obj._kde_layout = SimpleNamespace(manual_bg=manual)
    obj._manual_layout_passthrough = False
    return obj, recording


def test_manual_bg_from_first_key_never_enters_prediction_or_character_learning(monkeypatch):
    obj, rec = engine(monkeypatch, manual=True)
    obj._trace = FakeTrace()
    for value, code in [(ord("т"), 20), (ord("е"), 18), (ord("с"), 31),
                        (ord("т"), 20), (0x20, 57), (0xFF08, 14)]:
        assert obj.do_process_key_event(value, code, 0) is False
    assert obj._core.key_calls() == []
    assert "predictions" not in obj._core.names()
    assert rec.commits == [] and rec.preedits == []
    assert obj._trace.events == [] and obj._trace.begins == 0


def test_entering_manual_bg_delivers_only_owned_typed_prefix_once(monkeypatch):
    obj, rec = engine(monkeypatch)
    assert obj.do_process_key_event(ord("p"), 25, 0) is True
    assert rec.preedits[-1] == ("подай", True)
    obj._kde_layout.manual_bg = True
    # Metadata changes alone cannot write into an arbitrary focused client.
    assert rec.commits == []
    assert obj.do_process_key_event(ord("а"), 30, 0) is False
    assert rec.commits == ["по"]
    assert ("", False) in rec.preedits
    assert obj._core.word == ""
    assert len(obj._core.key_calls()) == 1
    for value, code in [(ord("б"), 48), (0x20, 57)]:
        assert obj.do_process_key_event(value, code, 0) is False
    assert rec.commits == ["по"]
    assert obj._core.names().count("flush_preedit_for_layout") == 1
    assert "focus_lost" not in obj._core.names()


def test_us_selection_resumes_automatic_input_at_next_press(monkeypatch):
    obj, rec = engine(monkeypatch, manual=True)
    assert obj.do_process_key_event(ord("а"), 30, 0) is False
    obj._kde_layout.manual_bg = False
    # A key-up from the old mode must not create a fresh automatic word.
    assert obj.do_process_key_event(ord("а"), 30, ske._MOD_RELEASE) is False
    assert obj._core.key_calls() == []
    assert obj.do_process_key_event(ord("p"), 25, 0) is True
    assert obj._core.names().count("focus_gained") == 1
    assert len(obj._core.key_calls()) == 1
    assert rec.preedits[-1] == ("подай", True)


def test_sensitive_field_remains_a_hard_stop_during_manual_mode_change(monkeypatch):
    obj, rec = engine(monkeypatch, manual=True)
    obj._sensitive = True
    obj._trace = FakeTrace()
    assert obj.do_process_key_event(ord("а"), 30, 0) is False
    assert obj._core.calls == []
    assert obj._trace.events == [] and rec.commits == []


def test_kde_becoming_available_on_focus_preserves_first_bg_key(monkeypatch):
    from ibus import kde_layout

    obj, rec = engine(monkeypatch)
    obj._kde_layout = None
    monkeypatch.setattr(ske, "_HAS_IBUS", True)
    observer = SimpleNamespace(manual_bg=True)
    monkeypatch.setattr(kde_layout, "create_kde_layout_observer", lambda: observer)
    obj.do_focus_in()
    assert obj._kde_layout is observer
    assert obj.do_process_key_event(ord("а"), 30, 0) is False
    assert obj._core.key_calls() == [] and rec.commits == []


@pytest.mark.parametrize("verified", [False, True])
def test_focus_retries_only_unverified_kde_metadata(monkeypatch, verified):
    obj, rec = engine(monkeypatch)
    reads = []
    observer = SimpleNamespace(verified=verified, manual_bg=False)

    def refresh():
        reads.append(True)
        observer.verified = True
        observer.manual_bg = True

    observer.refresh = refresh
    obj._kde_layout = observer
    monkeypatch.setattr(ske, "_HAS_IBUS", True)
    obj.do_focus_in()
    assert reads == ([] if verified else [True])
    if not verified:
        assert obj.do_process_key_event(ord("а"), 30, 0) is False
        assert obj._core.key_calls() == [] and rec.commits == []


def test_destroy_releases_layout_subscription_without_client_text(monkeypatch):
    obj, rec = engine(monkeypatch)
    closed = []
    obj._kde_layout = SimpleNamespace(manual_bg=True, close=lambda: closed.append(True))
    obj.do_destroy()
    obj.do_destroy()
    assert closed == [True]
    assert obj._kde_layout is None and rec.commits == []


@pytest.mark.parametrize("observer", [None, SimpleNamespace(manual_bg=False)])
def test_missing_kde_or_us_selection_preserves_existing_auto_route(monkeypatch, observer):
    obj, rec = engine(monkeypatch)
    obj._kde_layout = observer
    assert obj.do_process_key_event(ord("p"), 25, 0) is True
    assert obj._core.names().count("flush_preedit_for_layout") == 0
    assert obj._core.names().count("focus_lost") == 0
    assert obj._core.names().count("focus_gained") == 0
    assert len(obj._core.key_calls()) == 1


@pytest.mark.parametrize("mode", ["package", "flat"])
def test_layout_observer_supports_both_engine_import_modes(tmp_path, mode):
    import json
    from pathlib import Path
    import subprocess
    import sys

    script = '''
import importlib, json, sys
from types import SimpleNamespace
from pathlib import Path
directory, mode = Path(sys.argv[1]), sys.argv[2]
sys.modules["gi"] = None
sys.modules["smartkey_py"] = None
if mode == "flat":
    sys.path.insert(0, str(directory))
    sys.modules["ibus"] = None
    engine = importlib.import_module("smartkey_engine")
    layout = importlib.import_module("kde_layout")
    assert not engine.__package__
else:
    sys.path.insert(0, str(directory.parent))
    engine = importlib.import_module("ibus.smartkey_engine")
    layout = importlib.import_module("ibus.kde_layout")
    assert engine.__package__ == "ibus"
observer = SimpleNamespace(manual_bg=True, verified=True)
layout.create_kde_layout_observer = lambda: observer
engine._HAS_IBUS = True
obj = engine.SmartKeyEngine.__new__(engine.SmartKeyEngine)
obj._kde_layout = None
obj._init_kde_layout_observer()
assert obj._kde_layout is observer, "layout observer silently disabled"
print(json.dumps({"mode": mode, "manual_bg": obj._kde_layout.manual_bg}))
'''
    result = subprocess.run(
        [sys.executable, "-I", "-B", "-c", script, str(Path(__file__).parent), mode],
        cwd=tmp_path, capture_output=True, text=True, timeout=15,
    )
    assert result.returncode == 0, result.stdout + result.stderr
    assert json.loads(result.stdout) == {"mode": mode, "manual_bg": True}
