#!/usr/bin/env python3
"""Phase-A engine->adapter INTEGRATION guardrail (the test that was missing).

Drives the REAL SmartKeyEngine._execute_actions with the REAL Rust-core action
stream for a multi-word sentence, and asserts the CORRECT (ghost-only,
next_token) instrumentation:

  * ~1 event per WORD boundary (NOT one per keystroke -> flood),
  * most events resolved (outcome 0/1),
  * low unresolved rate.

Discriminates the fixes:
  - ghost-only (correct)      -> ~N events, ~N-1 resolved, ~0% unresolved  -> PASS
  - composing hook (5b61500)  -> ~5N events, N resolved, ~78% unresolved   -> FAIL (flood assert)

HYGIENE: sets SMARTKEY_PHASEA_DATA to a throwaway temp dir BEFORE importing any
phase_a module, so the events DB / ENGINE-IDENTITY.json / context_salt / receipts
are ALL redirected there and the LIVE phase_a_data is never touched. Run it from
anywhere; it never writes to the real telemetry.

Usage:
    python3 test_engine_integration.py
Optional (verify against a specific engine file instead of the installed one):
    SMARTKEY_ENGINE_FILE=/path/to/smartkey_engine.py python3 test_engine_integration.py
"""

from __future__ import annotations

import glob
import importlib
import importlib.util
import os
import sys
import tempfile

LAB = os.environ.get("SMARTKEY_LAB", "/home/rmanov/smartkey-phase-a-lab")

# --- HYGIENE: isolate ALL phase_a writes to a temp dir before importing phase_a.
# SMARTKEY_PHASEA_DATA redirects the events DB / identity / salt / receipts to a
# throwaway temp dir; SMARTKEY_PHASE_A=1 satisfies the adapter's live-run guard so
# the test is an allowed (but fully isolated) writer. The live phase_a_data is
# never touched. ---
os.environ["SMARTKEY_PHASEA_DATA"] = tempfile.mkdtemp(prefix="phasea_itest_")
os.environ.setdefault("SMARTKEY_PHASE_A", "1")
sys.path.insert(0, LAB)

from phase_a.engine_adapter import PhaseAAdapter  # noqa: E402
from phase_a.harness import connect  # noqa: E402
from phase_a.paths import default_db_path  # noqa: E402
from smartkey_py import PyInputMethodCore  # noqa: E402

# evdev keycodes (Wayland live path uses process_keycode)
EV = {c: k for c, k in zip(
    "abcdefghijklmnopqrstuvwxyz",
    [30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31,
     20, 22, 47, 17, 45, 21, 44])}
EV[" "] = 57


def _load_engine():
    override = os.environ.get("SMARTKEY_ENGINE_FILE")
    if override:
        spec = importlib.util.spec_from_file_location("skeng_under_test", override)
        m = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(m)
        return m.SmartKeyEngine
    return importlib.import_module("ibus.smartkey_engine").SmartKeyEngine


def _corpus():
    return sorted(glob.glob(os.path.join(LAB, "corpus", "corpus_*.json")))


def _build_engine(SmartKeyEngine, core, adapter):
    """Construct the dispatch object without __init__/IBus; stub display-only methods."""
    eng = SmartKeyEngine.__new__(SmartKeyEngine)
    eng._core = core
    eng._phase_a = adapter
    eng._surrounding_text = None
    eng._surrounding_cursor_pos = None
    eng._preedit_active = False
    eng._active_prediction = None
    eng._prediction_seq = 0
    eng._caps = 0
    eng._phase_a_action_trace = None       # tolerate trees that carry trace fields
    eng._phase_a_prediction_trace = None
    for name in ("_show_ghost", "_show_composing", "_clear_ghost", "_safe_commit",
                 "_track_prediction_shown"):
        setattr(eng, name, lambda *a, **k: None)
    return eng


def run(sentence: str):
    SmartKeyEngine = _load_engine()
    core = PyInputMethodCore(None)
    for f in _corpus():
        core.load_corpus_file(f)
    from phase_a.paths import data_dir
    _db = data_dir() / "engine_itest.db"          # distinct path -> avoids the live-DB guard
    for _s in ("", "-wal", "-shm"):
        _p = type(_db)(str(_db) + _s)
        if _p.exists():
            _p.unlink()
    adapter = PhaseAAdapter(_db, _corpus(), engine_commit="itest",
                            identity_file=data_dir() / "engine_itest_id.json")
    eng = _build_engine(SmartKeyEngine, core, adapter)
    for ch in sentence:
        eng._phase_a_observe()                         # mirror do_process_key_event
        eng._execute_actions(core.process_keycode(EV[ch], 0))
    adapter.close()
    conn = connect(_db, readonly=True)
    tot = conn.execute("SELECT COUNT(*) FROM events").fetchone()[0]
    res = conn.execute("SELECT COUNT(*) FROM events WHERE outcome IS NOT NULL").fetchone()[0]
    conn.close()
    return tot, res


def main() -> int:
    sentence = "the quick brown fox jumps over "   # 6 words, trailing space
    n_words = 6
    tot, res = run(sentence)
    unresolved = (tot - res) / tot if tot else 1.0
    print(f"engine-integration: {n_words} words -> events={tot} resolved={res} "
          f"unresolved={unresolved:.0%}  (isolated dir: {os.environ['SMARTKEY_PHASEA_DATA']})")

    problems = []
    if tot > n_words + 1:
        problems.append(
            f"FLOOD: {tot} events for {n_words} words (expected ~{n_words}). "
            f"Per-keystroke composing logging not reverted?")
    if res < n_words - 1:
        problems.append(f"RESOLUTION BROKEN: only {res}/{tot} resolved")
    if unresolved > 0.20:
        problems.append(f"UNRESOLVED too high: {unresolved:.0%} (>20%)")

    if problems:
        print("FAIL:")
        for p in problems:
            print("  -", p)
        return 1
    print("PASS: one event per word boundary, resolved, low unresolved (ghost-only next_token).")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
