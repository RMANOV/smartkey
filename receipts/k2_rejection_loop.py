"""Headless K=2 rejection-loop receipt driver (lab vehicle).

Demonstrates the session RejectionMemory end-to-end through the REAL adapter +
REAL Rust core, with NO live-engine interaction:
  * pipeline: do_process_key_event -> _execute_actions -> _finalize_prediction_outcome
  * core:     worktree-built smartkey_py (Rust MasterLoop + RejectionMemory)
  * ibus:     the module's _FakeIBus fallback (no display server / GObject)
  * data:     in-memory corpus only; NO personal profile; scratch data dir

It shows the same input "hel" offering the "hello" ghost on attempts 1 & 2
(each rejected at the word boundary) and SUPPRESSED on attempt 3 (K=2).

Build the native module (from the repo root), then run:

    cargo build --release -p smartkey-py
    mkdir -p /tmp/skpymod && cp target/release/libsmartkey_py.so /tmp/skpymod/smartkey_py.so
    python3 receipts/k2_rejection_loop.py /tmp/skpymod

Exits 0 iff the ghost is suppressed exactly on the 3rd attempt. Pass an output
path as the 2nd arg to override the default (receipts/k2-rejection-loop-receipt.txt).
"""

from __future__ import annotations

import datetime
import importlib.util
import os
import pathlib
import sys
import tempfile
import types

if len(sys.argv) < 2:
    sys.exit("usage: k2_rejection_loop.py <dir-with-smartkey_py.so> [receipt-out]")

PYMOD = sys.argv[1]
_HERE = pathlib.Path(__file__).resolve().parent
RECEIPT = pathlib.Path(sys.argv[2]) if len(sys.argv) > 2 else _HERE / "k2-rejection-loop-receipt.txt"
_ENGINE = _HERE.parent / "ibus" / "smartkey_engine.py"

# --- Isolation: worktree native module first; forced _FakeIBus; scratch data --
sys.path.insert(0, PYMOD)
os.environ["SMARTKEY_PHASEA_DATA"] = tempfile.mkdtemp(prefix="smartkey-receipt-")

_fake_gi = types.ModuleType("gi")
_fake_gi.require_version = lambda *_a, **_k: (_ for _ in ()).throw(ValueError("fake"))
sys.modules["gi"] = _fake_gi
sys.modules.pop("gi.repository", None)

_spec = importlib.util.spec_from_file_location("smartkey_engine_receipt", _ENGINE)
ske = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(ske)
assert ske._HAS_IBUS is False, "must use _FakeIBus"
assert ske._HAS_CORE is True, "must have the native core"


def _mk_text(s):
    t = types.SimpleNamespace(s=s)
    t.set_attributes = lambda *_a, **_k: None
    return t


ske.IBus.Text.new_from_string = staticmethod(_mk_text)
ske.IBus.AttrList = type("AttrList", (), {"append": lambda *_a: None})

# --- Build the engine on the REAL native core --------------------------------
core = ske.PyInputMethodCore('{"ghost_text_separation_margin": 0.0}')
core.load_word("hello", 100000)

eng = ske.SmartKeyEngine.__new__(ske.SmartKeyEngine)
eng._core = core
eng._caps = 0
eng._preedit_active = False
eng._preedit_mode = None
eng._active_prediction = None
eng._prediction_seq = 0
eng._surrounding_text = None
eng._surrounding_cursor_pos = None
eng._trace = None
eng._last_composing_typed = ""
eng._session_id = "receipt"
eng.commit_text = lambda t: None
eng.update_preedit_text = lambda t, c, v: None
eng.hide_preedit_text = lambda: None
eng.forward_key_event = lambda kv, kc, st: None
eng.delete_surrounding_text = lambda off, n: None

_captured: list[list[tuple[str, str]]] = []
_orig_exec = eng._execute_actions
eng._execute_actions = lambda actions: (_captured.append(list(actions)), _orig_exec(actions))[1]

KEY_SPACE = ske.IBus.KEY_space


def press(keyval: int) -> list[tuple[str, str]]:
    _captured.clear()
    eng.do_process_key_event(keyval, 0, 0)  # keycode=0 → keyval path (no dual buffer)
    return [a for batch in _captured for a in batch]


def ghost_shown(actions: list[tuple[str, str]]) -> bool:
    for kind, payload in actions:
        if kind == "ghost" and payload:
            return True
        if kind == "composing":
            decoded = ske.ffi_decode_composing_payload(payload)
            if decoded and decoded[1]:
                return True
    return False


lines: list[str] = []
verdicts: list[bool] = []
for attempt in range(1, 4):
    press(ord("h"))
    press(ord("e"))
    acts = press(ord("l"))  # completion "hello" decided here
    shown = ghost_shown(acts)
    lines.append(
        f"  attempt {attempt}: type 'h','e','l'  -> ghost for 'hello' "
        f"{'SHOWN' if shown else 'SUPPRESSED'}   (core actions on 'l': {acts})"
    )
    if attempt < 3:
        press(KEY_SPACE)  # Space commits 'hel' -> word_boundary rejection
        lines.append(
            "             reject via Space (commit 'hel')          -> "
            f"recorded rejection #{attempt} of ('hel','hello')"
        )
    verdicts.append(shown)
    # RejectionMemory is session-scoped and survives reset; only the word
    # buffer / anticipatory ghost are cleared for a clean next attempt.
    core.reset()
    eng._active_prediction = None

ok = verdicts == [True, True, False]

ts = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
receipt = f"""SmartKey — K=2 Rejection-Loop Structural Receipt
=================================================
generated: {ts}
vehicle:   headless lab driver (receipts/k2_rejection_loop.py); NO live-engine interaction

Pipeline exercised (REAL, end-to-end):
  do_process_key_event -> _execute_actions -> _finalize_prediction_outcome
  core:  worktree-built smartkey_py (Rust MasterLoop + RejectionMemory), the
         actual suppression logic — not a stub
  ibus:  module _FakeIBus fallback (no display server, no GObject)
  data:  in-memory corpus {{"hello": 100000}}; NO personal profile; NO disk;
         SMARTKEY_PHASEA_DATA -> throwaway tempdir
  config: {{"ghost_text_separation_margin": 0.0}}

Semantics under test: K=2 — the same completion rejected for the same typed
input twice is not offered on the 3rd attempt (session-scoped, decays with the
process).

Sequence:
{chr(10).join(lines)}

Verdict: attempts (ghost shown?) = {verdicts}  (expected [True, True, False])
RESULT: {'PASS — ghost SUPPRESSED on the 3rd attempt after K=2 rejections' if ok else 'FAIL'}
"""

RECEIPT.parent.mkdir(parents=True, exist_ok=True)
RECEIPT.write_text(receipt, encoding="utf-8")
print(receipt)
sys.exit(0 if ok else 1)
