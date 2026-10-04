"""Integration tests for the IBus adapter's key-event dispatch.

These drive the REAL engine→adapter path
(``do_process_key_event`` → ``_execute_actions`` → IBus I/O) with a
contract-faithful fake core and a fake-IBus harness, rather than feeding the
adapter hand-made actions.  That indirection is exactly what hid the Phase-A
instrumentation bug: a self-test that fed the adapter directly missed the
key-routing / consume-vs-forward logic that lives *around* ``_execute_actions``.

Covers two live regressions on v0.6.1:
  (a) backspace deletion not always working — the adapter swallowed a
      core-*forwarded* backspace whenever any preedit (incl. an anticipatory
      next-word ghost) was showing;
  (b) the adapter correctly lands the FULL composing word on Tab-accept.

Safety: uses the module's built-in ``_FakeIBus`` fallback (no real IBus /
display server) and a fake core (the live ``smartkey_py`` native module is
never instantiated), so nothing touches the live engine or Phase-A data.
"""

from __future__ import annotations

import importlib.util
import os
import pathlib
import sys
import tempfile
import types

# Belt-and-suspenders: never let any transitive code touch live Phase-A data.
os.environ.setdefault(
    "SMARTKEY_PHASEA_DATA", tempfile.mkdtemp(prefix="smartkey-test-phasea-")
)

# Force the module's ``_FakeIBus`` fallback: make ``gi.require_version`` raise so
# the ``except (ValueError, ImportError)`` branch selects the stub IBus.
_fake_gi = types.ModuleType("gi")


def _require_version(*_a, **_k):  # noqa: ANN002, ANN003
    raise ValueError("forced fake IBus for tests")


_fake_gi.require_version = _require_version  # type: ignore[attr-defined]
sys.modules["gi"] = _fake_gi
sys.modules.pop("gi.repository", None)

# Import the engine module fresh under a private name so its import guard runs
# against the fake ``gi`` above.
_ENGINE_PATH = pathlib.Path(__file__).resolve().parent / "smartkey_engine.py"
_spec = importlib.util.spec_from_file_location("smartkey_engine_under_test", _ENGINE_PATH)
assert _spec and _spec.loader
ske = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(ske)

assert ske._HAS_IBUS is False, "test requires the _FakeIBus fallback path"


# --- Make the stub IBus.Text / AttrList functional enough for the display path.
def _mk_text(s: str):
    t = types.SimpleNamespace(s=s)
    t.set_attributes = lambda *_a, **_k: None
    return t


class _AttrList:
    def append(self, *_a) -> None:  # noqa: ANN002
        return None


ske.IBus.Text.new_from_string = staticmethod(_mk_text)  # type: ignore[attr-defined]
ske.IBus.AttrList = _AttrList  # type: ignore[attr-defined]

_KEY_BACKSPACE = ske.IBus.KEY_BackSpace
_KEY_TAB = ske.IBus.KEY_Tab


class FakeCore:
    """Scripted core: returns the next queued action-list per key event.

    Action payloads use the real FFI wire format so the adapter's decoders run:
    composing → ``"{typed}\\x00{ghost}"``, replace → ``"{len}\\x1F{text}"``.
    """

    def __init__(self, scripts: list[list[tuple[str, str]]]) -> None:
        self._scripts = list(scripts)
        self._preds = [("hello", 1.0, 0.9)]

    def process_keycode(self, _evdev: int, _state: int) -> list[tuple[str, str]]:
        return self._scripts.pop(0) if self._scripts else [("forward", "")]

    def handle_key(self, _keyval: int, _state: int) -> list[tuple[str, str]]:
        return self._scripts.pop(0) if self._scripts else [("forward", "")]

    def predictions(self) -> list[tuple[str, float, float]]:
        return self._preds


class Recorder:
    def __init__(self) -> None:
        self.commits: list[str] = []
        self.preedits: list[tuple[str, bool]] = []
        self.hide_preedit_calls = 0
        self.forwarded_keys: list[tuple[int, int, int]] = []
        self.deleted: list[tuple[int, int]] = []


def build_engine(scripts: list[list[tuple[str, str]]]):
    """Instantiate the real adapter class, bypassing IBus/GObject __init__."""
    eng = ske.SmartKeyEngine.__new__(ske.SmartKeyEngine)
    eng._core = FakeCore(scripts)
    eng._caps = 0
    eng._preedit_active = False
    eng._preedit_mode = None
    eng._active_prediction = None
    eng._prediction_seq = 0
    eng._surrounding_text = None
    eng._surrounding_cursor_pos = None

    rec = Recorder()
    eng.commit_text = lambda text_obj: rec.commits.append(text_obj.s)
    eng.update_preedit_text = lambda text_obj, _cursor, visible: rec.preedits.append(
        (text_obj.s, visible)
    )

    def _hide() -> None:
        rec.hide_preedit_calls += 1

    eng.hide_preedit_text = _hide
    eng.forward_key_event = lambda kv, kc, st: rec.forwarded_keys.append((kv, kc, st))
    eng.delete_surrounding_text = lambda off, n: rec.deleted.append((off, n))
    return eng, rec


# --- Bug (a): backspace must reach the app when the core forwards it ----------
def test_backspace_forwarded_when_core_forwards_under_anticipatory_ghost():
    # 1) A char event shows an anticipatory next-word ghost → preedit active.
    # 2) Backspace: the core has nothing to trim (word already committed) so it
    #    FORWARDS the key (HideGhost + ForwardKey).  The adapter must NOT consume
    #    it — otherwise committed text can never be deleted.
    eng, _rec = build_engine(
        scripts=[
            [("ghost", "lo")],  # ghost shown → _preedit_active=True, mode="ghost"
            [("hide", ""), ("forward", "")],  # core forwards the backspace
        ]
    )
    # Prime the ghost.
    assert eng.do_process_key_event(ord("l"), 38, 0) is True
    assert eng._preedit_active is True and eng._preedit_mode == "ghost"

    # Backspace — must be forwarded (return False), not swallowed.
    consumed = eng.do_process_key_event(_KEY_BACKSPACE, 22, 0)
    assert consumed is False, "backspace must reach the app to delete committed text"


# --- Bug (a) guard: composing-edit backspace stays consumed -------------------
def test_backspace_consumed_during_composing_edit():
    # In full-word composing, backspace edits the preedit and the core returns
    # ShowComposing with no ForwardKey → the adapter must consume it (the word
    # lives only in the preedit; the app must not also delete).
    eng, _rec = build_engine(
        scripts=[
            [("composing", "hel\x00lo")],  # enter composing
            [("composing", "he\x00llo")],  # backspace re-scores preedit
        ]
    )
    assert eng.do_process_key_event(ord("l"), 38, 0) is True
    assert eng._preedit_mode == "composing"

    consumed = eng.do_process_key_event(_KEY_BACKSPACE, 22, 0)
    assert consumed is True, "composing-edit backspace must stay consumed"


# --- Bug (b) adapter side: Tab-accept lands the FULL composing word -----------
def test_tab_accept_commits_full_word_in_composing():
    # With the Rust fix, the core emits CommitText(full_word) in composing mode.
    # The adapter must replace the composing preedit with the full word (not drop
    # the typed prefix), leaving no stale preedit.
    eng, rec = build_engine(
        scripts=[
            [("composing", "hel\x00lo")],  # typed "hel", ghost "lo"
            [("hide", ""), ("commit", "hello")],  # Tab-accept from fixed core
        ]
    )
    assert eng.do_process_key_event(ord("l"), 38, 0) is True

    eng.do_process_key_event(_KEY_TAB, 23, 0)
    assert rec.commits == ["hello"], f"must commit the full word, got {rec.commits}"
    # The composing preedit was replaced with an empty, hidden preedit.
    assert ("", False) in rec.preedits
    assert eng._preedit_active is False
