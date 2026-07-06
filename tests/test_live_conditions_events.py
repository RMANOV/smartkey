#!/usr/bin/env python3
"""Phase-A LIVE-CONDITIONS guardrail — the discrepancy the other two tests miss.

test_engine_integration and test_empty_predictions_fix both drive a core whose
``predictions()`` returns real top-3 words. But the LIVE ibus failure mode is the
opposite: at the ghost (next-word) hook the core's ``predictions()`` is EMPTY —
its text is only in the action payload. That is exactly why a real typing session
logged 0 events even though ghosts were firing.

This test pins the LIVE path:
  * core ``predictions()`` returns []  (the live condition),
  * surrounding text advances like a GTK app,
  * the EXACT live action batch ['hide','commit','forward','ghost'] per boundary,
  * the REAL Phase-A trace hooks (NOT stubbed) so prediction_trace.jsonl is
    actually written and can be asserted.

RED  (reproduces the live bug): empty predictions + NO payload fallback (shown="")
     -> on_next_word_prediction([]) -> reason 'empty_prediction' -> 0 events.
GREEN (the 9d5cbb4 fix + this change): the ghost payload is the top-1 fallback
     -> 1 logged event per word boundary, and prediction_trace.jsonl carries a
     per-ghost reason for EVERY hook plus a harness_init build fingerprint.

Also guards two regressions the live forensics implicated:
  * the redraw-dedup guard must NOT collapse repeated word boundaries to ~1, and
  * the harness_init self-diagnostic must stamp trace_version + engine_file so a
    stale live engine is detectable at a glance.

HYGIENE: SMARTKEY_PHASEA_DATA -> a throwaway temp dir before importing phase_a,
so the live phase_a_data is never touched.

Usage:
    python3 tests/test_live_conditions_events.py
    SMARTKEY_ENGINE_FILE=$PWD/ibus/smartkey_engine.py python3 tests/test_live_conditions_events.py
"""
from __future__ import annotations

import glob
import importlib
import importlib.util
import json
import os
import sys
import tempfile
from pathlib import Path

LAB = os.environ.get("SMARTKEY_LAB", "/home/rmanov/smartkey-phase-a-lab")

os.environ["SMARTKEY_PHASEA_DATA"] = tempfile.mkdtemp(prefix="phasea_live_")
os.environ.setdefault("SMARTKEY_PHASE_A", "1")
sys.path.insert(0, LAB)

from phase_a.engine_adapter import PhaseAAdapter  # noqa: E402
from phase_a.harness import connect  # noqa: E402
from phase_a.paths import data_dir  # noqa: E402

CORPUS = sorted(glob.glob(os.path.join(LAB, "corpus", "corpus_*.json")))


def load_engine():
    override = os.environ.get("SMARTKEY_ENGINE_FILE")
    if override:
        spec = importlib.util.spec_from_file_location("skeng_live", override)
        m = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(m)
        return m
    return importlib.import_module("ibus.smartkey_engine")


class EmptyPredCore:
    """Live-like core: predictions() is EMPTY at the ghost hook (text is only in
    the action payload)."""

    def predictions(self):
        return []

    def set_surrounding_text(self, *a, **k):
        pass


def make_adapter(tag):
    db = data_dir() / f"live_{tag}.db"
    for s in ("", "-wal", "-shm"):
        p = Path(str(db) + s)
        if p.exists():
            p.unlink()
    ad = PhaseAAdapter(
        db,
        CORPUS,
        engine_commit="livetest",
        identity_file=data_dir() / f"live_{tag}_id.json",
        engine_meta={"engine_file": "livetest", "trace_version": "livetest"},
    )
    return ad, db


def build(SmartKeyEngine, core, adapter, *, real_traces: bool):
    eng = SmartKeyEngine.__new__(SmartKeyEngine)
    eng._core = core
    eng._phase_a = adapter
    eng._surrounding_text = None
    eng._surrounding_cursor_pos = None
    eng._preedit_active = False
    eng._active_prediction = None
    eng._prediction_seq = 0
    eng._caps = 0x20  # SURROUNDING_TEXT capable, like a GTK app
    if real_traces:
        eng._phase_a_action_trace = data_dir() / "action_trace.jsonl"
        eng._phase_a_callback_trace = data_dir() / "callback_trace.jsonl"
        eng._phase_a_prediction_trace = data_dir() / "prediction_trace.jsonl"
    else:
        eng._phase_a_action_trace = None
        eng._phase_a_callback_trace = None
        eng._phase_a_prediction_trace = None
    for name in ("_show_ghost", "_show_composing", "_clear_ghost", "_safe_commit",
                 "_track_prediction_shown"):
        setattr(eng, name, lambda *a, **k: None)
    return eng


def count_events(db):
    conn = connect(db, readonly=True)
    tot = conn.execute("SELECT COUNT(*) FROM events").fetchone()[0]
    res = conn.execute("SELECT COUNT(*) FROM events WHERE outcome IS NOT NULL").fetchone()[0]
    conn.close()
    return tot, res


def read_trace():
    pt = data_dir() / "prediction_trace.jsonl"
    if not pt.exists():
        return []
    return [json.loads(l) for l in pt.read_text().splitlines() if l.strip()]


def drive_live(eng, adapter, words, ghosts):
    """Mirror do_process_key_event ordering per word boundary under live-mode."""
    committed = ""
    for w, g in zip(words, ghosts):
        committed += w + " "
        eng._surrounding_text = committed
        eng._surrounding_cursor_pos = len(committed)
        eng._phase_a_observe()  # resolve prior pending via context delta
        batch = [("hide", ""), ("commit", w + " "), ("forward", ""), ("ghost", g)]
        eng._execute_actions(batch)
    adapter.close()


def main() -> int:
    eng_mod = load_engine()
    SmartKeyEngine = eng_mod.SmartKeyEngine
    problems = []

    # --- RED: empty predictions + no payload fallback (the old naive hook) ----
    ad, db = make_adapter("red")
    eng = build(SmartKeyEngine, EmptyPredCore(), ad, real_traces=False)
    for _ in range(4):
        eng._phase_a_ghost("")  # shown="" -> nothing to log under empty preds
    ad.close()
    red_tot, _ = count_events(db)
    if red_tot != 0:
        problems.append(f"RED expected 0 events (empty preds, no payload), got {red_tot}")

    # --- GREEN: the exact live batch, ghost payload carries the word ----------
    words = ["the", "quick", "brown", "fox", "jumps"]
    ghosts = ["quick", "brown", "fox", "jumps", "over"]
    ad, db = make_adapter("green")
    eng = build(SmartKeyEngine, EmptyPredCore(), ad, real_traces=True)
    drive_live(eng, ad, words, ghosts)
    green_tot, green_res = count_events(db)
    n = len(words)
    if green_tot != n:
        problems.append(f"GREEN expected {n} events (one per boundary), got {green_tot}")
    if green_res < n - 1:
        problems.append(f"GREEN resolution broken: {green_res}/{green_tot} resolved")

    # prediction_trace.jsonl must be written with a logged reason per ghost -----
    trace = read_trace()
    logged = [r for r in trace if r.get("reason") in ("logged", "superseded_logged")]
    ghost_hooks = [r for r in trace if r.get("source") == "action_payload"]
    if len(logged) != n:
        problems.append(
            f"prediction_trace: expected {n} logged lines, got {len(logged)} "
            f"(trace has {len(trace)} lines total)")
    if len(ghost_hooks) != n:
        problems.append(
            f"prediction_trace: expected {n} action_payload ghost hooks, got {len(ghost_hooks)}")

    # --- DEDUP must NOT collapse repeated boundaries to ~1 --------------------
    ad, db = make_adapter("dedup")
    eng = build(SmartKeyEngine, EmptyPredCore(), ad, real_traces=False)
    drive_live(eng, ad, ["the"] * 4, ["the"] * 4)
    dedup_tot, _ = count_events(db)
    if dedup_tot != 4:
        problems.append(
            f"DEDUP over-suppression: repeated 'the' boundaries logged {dedup_tot}, expected 4")

    # --- SELF-DIAGNOSTIC: harness_init stamps trace_version + engine_file -----
    # Exercise the exact write __init__ performs, on a fresh trace file.
    sd_dir = Path(tempfile.mkdtemp(prefix="phasea_live_sd_"))
    eng2 = build(SmartKeyEngine, EmptyPredCore(),
                 make_adapter("sd")[0], real_traces=True)
    eng2._phase_a_prediction_trace = sd_dir / "prediction_trace.jsonl"
    eng2._phase_a_trace_prediction_hook(
        "harness_init", 0, False, "startup",
        extra={"engine_file": eng_mod.__file__,
               "trace_version": eng_mod.PHASE_A_TRACE_VERSION,
               "lab_commit": "sd", "pid": os.getpid()})
    sd_lines = [json.loads(l) for l in
                (sd_dir / "prediction_trace.jsonl").read_text().splitlines() if l.strip()]
    if not sd_lines or sd_lines[0].get("source") != "harness_init":
        problems.append("SELF-DIAG: harness_init line missing from prediction_trace")
    elif sd_lines[0].get("trace_version") != eng_mod.PHASE_A_TRACE_VERSION:
        problems.append(
            f"SELF-DIAG: trace_version not stamped "
            f"(got {sd_lines[0].get('trace_version')!r})")
    elif "engine_file" not in sd_lines[0]:
        problems.append("SELF-DIAG: engine_file not stamped in harness_init")

    print(f"live-conditions: RED(empty,no-payload)={red_tot}  "
          f"GREEN(live-batch)={green_tot}/{n} resolved={green_res}  "
          f"dedup-repeat={dedup_tot}/4  "
          f"prediction_trace lines={len(trace)} logged={len(logged)}  "
          f"self-diag={'ok' if sd_lines and sd_lines[0].get('source')=='harness_init' else 'MISSING'}")
    print(f"  (isolated dir: {os.environ['SMARTKEY_PHASEA_DATA']})")

    if problems:
        print("FAIL:")
        for p in problems:
            print("  -", p)
        return 1
    print("PASS: live empty-predictions path logs one event per word boundary, "
          "prediction_trace is per-ghost + build-stamped, dedup does not over-suppress.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
