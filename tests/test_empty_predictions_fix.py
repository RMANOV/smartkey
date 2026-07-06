#!/usr/bin/env python3
"""Regression test for the empty-predictions() ghost hook bug — ONE engine per
process (GObject allows a single SmartKeyEngine GType per process).

  SMARTKEY_ENGINE_FILE unset  -> import the installed/committed engine (BUGGY)
  SMARTKEY_ENGINE_FILE=<path> -> load that engine file (FIXED)

Drives the REAL _execute_actions with a STUB core whose predictions() we control.
Fully isolated (own data dir + db + identity). Asserts the expected profile.
"""
from __future__ import annotations
import glob, importlib, importlib.util, os, sys, tempfile
from pathlib import Path

LAB = "/home/rmanov/smartkey-phase-a-lab"
os.environ["SMARTKEY_PHASEA_DATA"] = tempfile.mkdtemp(prefix="phasea_regr_")
sys.path.insert(0, LAB)

from phase_a.engine_adapter import PhaseAAdapter          # noqa: E402
from phase_a.harness import connect                        # noqa: E402
from phase_a.paths import data_dir                          # noqa: E402
from smartkey_py import PyInputMethodCore                   # noqa: E402

CORPUS = sorted(glob.glob(os.path.join(LAB, "corpus", "corpus_*.json")))
EV = {c: k for c, k in zip("abcdefghijklmnopqrstuvwxyz",
      [30,48,46,32,18,33,34,35,23,36,37,38,50,49,24,25,16,19,31,20,22,47,17,45,21,44])}
EV[" "] = 57

ENGINE_FILE = os.environ.get("SMARTKEY_ENGINE_FILE")
PROFILE = "FIXED" if ENGINE_FILE else "BUGGY"


def load_engine():
    if ENGINE_FILE:
        spec = importlib.util.spec_from_file_location("eng_under_test", ENGINE_FILE)
        m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
        return m.SmartKeyEngine
    return importlib.import_module("ibus.smartkey_engine").SmartKeyEngine


class StubCore:
    def __init__(self, preds): self._preds = preds
    def predictions(self): return self._preds


def make_adapter(tag):
    db = data_dir() / f"regr_{tag}.db"
    for s in ("", "-wal", "-shm"):
        p = Path(str(db) + s)
        if p.exists(): p.unlink()
    ad = PhaseAAdapter(db, CORPUS, engine_commit="regrtest",
                       identity_file=data_dir() / f"regr_{tag}_id.json")
    return ad, db


def build(SmartKeyEngine, core, adapter):
    eng = SmartKeyEngine.__new__(SmartKeyEngine)
    eng._core = core; eng._phase_a = adapter
    eng._surrounding_text = None; eng._surrounding_cursor_pos = None
    eng._preedit_active = False; eng._active_prediction = None
    eng._prediction_seq = 0; eng._caps = 0; eng._phase_a_action_trace = None
    for m in ("_show_ghost", "_show_composing", "_clear_ghost", "_safe_commit",
              "_track_prediction_shown"):
        setattr(eng, m, lambda *a, **k: None)
    eng._phase_a_trace_prediction_hook = lambda *a, **k: None
    eng._coalesce_same_batch_commit_replace = lambda actions: actions
    return eng


def count(db):
    conn = connect(db, readonly=True)
    n = conn.execute("SELECT COUNT(*) FROM events").fetchone()[0]
    conn.close()
    return n


def run_actions(SmartKeyEngine, preds, actions, tag):
    ad, db = make_adapter(tag)
    eng = build(SmartKeyEngine, StubCore(preds), ad)
    eng._execute_actions(actions)
    ad.close()
    return count(db)


def main() -> int:
    E = load_engine()

    a = run_actions(E, [], [("ghost", "the")], "empty")             # empty preds + payload
    preds = [("the", 0.9, 0.5), ("a", 0.3, 0.2), ("and", 0.3, 0.2)]
    a2 = run_actions(E, preds, [("ghost", "the")], "norm")          # normal ghost

    real = PyInputMethodCore(None)
    for cf in CORPUS: real.load_corpus_file(cf)
    comp = next((p for t, p in real.process_keycode(EV["t"], 0) if t == "composing"), None)
    assert comp is not None
    b = run_actions(E, preds, [("composing", comp)], "comp")        # composing action

    print(f"[{PROFILE}] ghost+emptypreds={a}  ghost+preds={a2}  composing={b}")

    if PROFILE == "FIXED":
        want = {"ghost+emptypreds": (a, 1), "ghost+preds": (a2, 1), "composing": (b, 0)}
    else:
        want = {"ghost+emptypreds": (a, 0), "ghost+preds": (a2, 1), "composing": (b, 1)}
    ok = all(got == exp for got, exp in want.values())
    for k, (got, exp) in want.items():
        print(f"    {k}: got {got} want {exp}  {'ok' if got == exp else 'MISMATCH'}")
    print(f"[{PROFILE}] {'PASS' if ok else 'FAIL'}")
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
