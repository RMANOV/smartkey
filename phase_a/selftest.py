"""End-to-end self-test for the n-gram calibration harness.

Proves the whole pipeline WITHOUT IBus, the GUI, or the Rust engine — the
measured path is the pure n-gram FreqModel over the pinned corpus. All data is
synthetic (synthetic=1) in dedicated scratch DBs, never real evidence.

Checks:
  1 GREEN  : a clean corpus-replay run drives PASS.
  2 RED    : an injected overconfidence skew drives FAIL via the >=3-bucket
             calibration-gap clause (forced-FAIL receipt).
  3-6      : FAIL-clause reachability — >1% >20ms, >5% unresolved, missed
             watchdog without alarm, and the in-band-alarm distinction.
  7 SCHEMA : the no-LLM DB CHECK rejects a non-script/sql resolver and a
             non-'machine' class.
  8 LIVE   : run_sweep + run_watchdog produce a receipt and exit 0.
  9 FIREWALL (static): no measured-path module imports the ensemble / PyO3.
  10 CORPUS PIN : every pinned corpus file matches its expected sha256.
  11 DATA GUARD : data_dir() refuses a path inside the live/operator tree.

Exit 0 iff every check passes.
"""

from __future__ import annotations

import datetime as _dt
import os
import sqlite3
import time
from pathlib import Path

from .analyze import analyze, format_report
from .constants import EVENT_CLASS, FIREWALL_LINES, RESOLVER
from .corpus_replay import run_replay
from .harness import connect
from .paths import (
    PINNED_CORPUS_SHA256,
    alarm_file,
    data_dir,
    lab_corpus_files,
    verify_lab_corpus,
)
from .sweep import run_sweep, run_watchdog

_MEASURED_MODULES = ("constants", "paths", "freqmodel", "harness", "analyze", "sweep", "corpus_replay")
_FORBIDDEN_IMPORT_TOKENS = ("smartkey_py", "smartkey_core", "ensemble", "reranker", "hedge", "ppm", "bpe")


def _fresh(path: Path) -> Path:
    for suffix in ("", "-wal", "-shm"):
        p = Path(str(path) + suffix)
        if p.exists():
            p.unlink()
    return path


def _sim_daily_sweeps(db_path: Path, first_ts: float, last_ts: float) -> None:
    conn = connect(db_path)
    d = _dt.date.fromtimestamp(first_ts)
    d1 = _dt.date.fromtimestamp(last_ts)
    while d <= d1:
        noon = time.mktime(d.timetuple()) + 43200
        conn.execute(
            "INSERT INTO sweeps (sweep_date, ran_ts, events, resolutions, "
            "latency_p99_us, over_budget_pct, watchdog_status, receipt) "
            "VALUES (?,?,?,?,?,?,?,?)",
            (d.isoformat(), noon, 0, 0, 0, 0.0, "OK", "simulated daily sweep"),
        )
        d += _dt.timedelta(days=1)
    conn.commit()
    conn.close()


def _craft_db(name: str, rows: list[tuple], with_sweeps: bool) -> Path:
    """Direct-insert a crafted synthetic DB for gate-clause reachability.
    rows: list of (ts, p, outcome_or_None, latency_us)."""
    path = _fresh(data_dir() / f"selftest_gate_{name}.db")
    conn = connect(path)
    conn.execute(
        "INSERT INTO run_metadata (run_id, created_ts, synthetic, freq_table_hash, "
        "freq_table_files, p_model, engine_commit, harness_version, scope, notes) "
        "VALUES (?,?,?,?,?,?,?,?,?,?)",
        (f"craft-{name}", time.time(), 1, "crafted", "[]", "crafted", None,
         "selftest", "ngram_component", name),
    )
    for ts, p, outcome, lat in rows:
        conn.execute(
            "INSERT INTO events (run_id, ts, context_hash, n_candidates, p_top3, "
            "latency_us, outcome, class, resolver, synthetic) "
            "VALUES (?,?,?,?,?,?,?,?,?,1)",
            (f"craft-{name}", ts, "ch", 3, p, lat, outcome, EVENT_CLASS, RESOLVER),
        )
    conn.commit()
    if with_sweeps:
        conn.close()
        _sim_daily_sweeps(path, rows[0][0], rows[-1][0])
    else:
        conn.close()
    return path


def _clean_calib_rows(n, rng, ts0, unresolved_frac=0.0, over_budget_frac=0.0, span_days=1):
    rows = []
    for i in range(n):
        p = rng.choice([0.1, 0.2, 0.3, 0.4, 0.5])
        ts = ts0 + (i / n) * span_days * 86400
        outcome = None if rng.random() < unresolved_frac else (1 if rng.random() < p else 0)
        lat = 25000 if rng.random() < over_budget_frac else 300
        rows.append((ts, p, outcome, lat))
    return rows


# ---------------------------------------------------------------------------
# Checks
# ---------------------------------------------------------------------------
def check_green() -> tuple[bool, str]:
    db = _fresh(data_dir() / "selftest_green.db")
    summ = run_replay(db, lab_corpus_files("json"), n_events=6000, seed=101,
                      inject_skew=None, notes="selftest GREEN")
    res = analyze(db, synthetic=True)
    ok = res.verdict == "PASS"
    detail = (
        f"verdict={res.verdict} res={res.resolutions} buckets={len(res.buckets)} "
        f"maxgap={max((b.gap_cal for b in res.buckets), default=0):.3f} "
        f"raw_hit={summ['raw_hit_rate']:.3f} missed_wd={res.missed_watchdog}"
    )
    return ok, detail


def check_red() -> tuple[bool, str]:
    db = _fresh(data_dir() / "selftest_red.db")
    run_replay(db, lab_corpus_files("json"), n_events=6000, seed=202,
               inject_skew=0.35, notes="selftest RED overconfidence skew")
    res = analyze(db, synthetic=True)
    n_over = sum(1 for b in res.buckets if b.gap_cal > 0.10)
    ok = res.verdict == "FAIL" and n_over >= 3
    return ok, f"verdict={res.verdict} buckets_gap>0.10={n_over} reasons={res.reasons}"


def check_over_budget_fail() -> tuple[bool, str]:
    import random
    rows = _clean_calib_rows(2000, random.Random(3), time.time() - 86400, over_budget_frac=0.02)
    db = _craft_db("overbudget", rows, with_sweeps=True)
    res = analyze(db, synthetic=True)
    ok = res.verdict == "FAIL" and any(">20ms" in r for r in res.reasons)
    return ok, f"verdict={res.verdict} reasons={res.reasons}"


def check_unresolved_fail() -> tuple[bool, str]:
    import random
    rows = _clean_calib_rows(2000, random.Random(4), time.time() - 86400, unresolved_frac=0.10)
    db = _craft_db("unresolved", rows, with_sweeps=True)
    res = analyze(db, synthetic=True)
    ok = res.verdict == "FAIL" and any("unresolved" in r for r in res.reasons)
    return ok, f"verdict={res.verdict} reasons={res.reasons}"


def check_missed_watchdog_fail() -> tuple[bool, str]:
    import random
    rows = _clean_calib_rows(2000, random.Random(5), time.time() - 3 * 86400, span_days=3)
    db = _craft_db("missedwd", rows, with_sweeps=False)
    if alarm_file().exists():
        alarm_file().unlink()
    res = analyze(db, synthetic=True)
    ok = res.verdict == "FAIL" and any("watchdog" in r for r in res.reasons)
    return ok, f"verdict={res.verdict} missed_wd={res.missed_watchdog} reasons={res.reasons}"


def check_inband_alarm_distinction() -> tuple[bool, str]:
    import random
    rows = _clean_calib_rows(2000, random.Random(6), time.time() - 3 * 86400, span_days=3)
    db = _craft_db("inbandalarm", rows, with_sweeps=False)
    alarm_file().write_text("in-band alarm present\n", encoding="utf-8")
    res = analyze(db, synthetic=True)
    alarm_file().unlink(missing_ok=True)
    ok = res.verdict != "FAIL" or not any("without in-band alarm" in r for r in res.reasons)
    return ok, f"verdict={res.verdict} reasons={res.reasons}"


def check_schema_invariant() -> tuple[bool, str]:
    db = _fresh(data_dir() / "selftest_schema.db")
    conn = connect(db)
    conn.execute(
        "INSERT INTO run_metadata (run_id, created_ts, synthetic, freq_table_hash, "
        "freq_table_files, p_model, harness_version) VALUES ('s',0,1,'h','[]','m','v')"
    )
    rejected = False
    try:
        conn.execute(
            "INSERT INTO events (run_id, ts, context_hash, n_candidates, p_top3, "
            "latency_us, class, resolver, synthetic) "
            "VALUES ('s',0,'c',3,0.0,1,'machine','llm:gpt',1)"
        )
        conn.commit()
    except sqlite3.IntegrityError:
        rejected = True
    rejected_class = False
    try:
        conn.execute(
            "INSERT INTO events (run_id, ts, context_hash, n_candidates, p_top3, "
            "latency_us, class, resolver, synthetic) "
            "VALUES ('s',0,'c',3,0.0,1,'human','script:x',1)"
        )
        conn.commit()
    except sqlite3.IntegrityError:
        rejected_class = True
    conn.close()
    ok = rejected and rejected_class
    return ok, f"resolver_rejected={rejected} class_rejected={rejected_class}"


def check_live_sweep_watchdog() -> tuple[bool, str]:
    db = data_dir() / "selftest_green.db"  # reuse GREEN db (has data + sweeps)
    receipt = run_sweep(db, synthetic=True)
    wd = run_watchdog(db, synthetic=True)
    ends_ok = receipt.strip().endswith(FIREWALL_LINES[-1])
    ok = "PHASE-A DAILY RECEIPT" in receipt and ends_ok and wd == 0
    return ok, f"receipt_ok={'PHASE-A DAILY RECEIPT' in receipt} ends_firewall={ends_ok} watchdog_exit={wd}"


def check_scope_firewall_static() -> tuple[bool, str]:
    """Structural firewall: no measured-path module may import the ensemble /
    PyO3 binding. Scans the module sources for forbidden import tokens."""
    here = Path(__file__).resolve().parent
    bad = []
    for mod in _MEASURED_MODULES:
        src = (here / f"{mod}.py").read_text(encoding="utf-8")
        for line in src.splitlines():
            s = line.strip()
            if not (s.startswith("import ") or s.startswith("from ")):
                continue
            for tok in _FORBIDDEN_IMPORT_TOKENS:
                if tok in s:
                    bad.append(f"{mod}: {s}")
    ok = not bad
    return ok, f"forbidden_imports={bad if bad else 'none'}"


def check_corpus_pin() -> tuple[bool, str]:
    seen = verify_lab_corpus("json")
    expected = {
        name: digest
        for name, digest in PINNED_CORPUS_SHA256.items()
        if name.endswith(".json")
    }
    ok = seen == expected
    files = lab_corpus_files("json")
    ok = ok and len(files) == 3 and len({f.parent for f in files}) == 1
    return ok, f"pins_ok={ok} json_files={[f.name for f in files]}"


def check_data_guard() -> tuple[bool, str]:
    """data_dir() must refuse a path inside EITHER forbidden root — the live
    tree (~/smartkey) AND the operator config (~/.config/smartkey). Dual-root
    guard gets an executed probe for each root; declaration/import is not
    treated as proof."""
    old = os.environ.get("SMARTKEY_PHASEA_DATA")
    probes = {
        "live": Path.home() / "smartkey" / "should-refuse",
        "config": Path.home() / ".config" / "smartkey" / "should-refuse",
    }
    refused: dict[str, bool] = {}
    try:
        for name, probe in probes.items():
            os.environ["SMARTKEY_PHASEA_DATA"] = str(probe)
            try:
                data_dir()
                refused[name] = False
            except RuntimeError:
                refused[name] = True
    finally:
        if old is not None:
            os.environ["SMARTKEY_PHASEA_DATA"] = old
        else:
            os.environ.pop("SMARTKEY_PHASEA_DATA", None)
    ok = all(refused.values()) and len(refused) == 2
    return ok, (
        f"live_path_refused={refused.get('live')} "
        f"config_path_refused={refused.get('config')}"
    )


CHECKS = [
    ("1 GREEN  -> PASS (corpus replay)", check_green),
    ("2 RED    -> FAIL (injected overconfidence skew)", check_red),
    ("3 >20ms  -> FAIL", check_over_budget_fail),
    ("4 unresolved>5% -> FAIL", check_unresolved_fail),
    ("5 missed watchdog no-alarm -> FAIL", check_missed_watchdog_fail),
    ("6 missed watchdog +alarm -> not-that-FAIL", check_inband_alarm_distinction),
    ("7 schema invariant rejects llm:/human", check_schema_invariant),
    ("8 live sweep + watchdog", check_live_sweep_watchdog),
    ("9 scope firewall (no ensemble/PyO3 import)", check_scope_firewall_static),
    ("10 corpus pin matches expected sha256", check_corpus_pin),
    ("11 data guard refuses live + operator config roots", check_data_guard),
]


def main() -> int:
    print("=" * 72)
    print(" O1 / PHASE-A MACHINERY SELF-TEST (synthetic data, synthetic=1)")
    print(" prediction_scope: ngram_component  (n-gram component ONLY)")
    print("=" * 72)
    all_ok = True
    for name, fn in CHECKS:
        try:
            ok, detail = fn()
        except Exception as exc:
            ok, detail = False, f"EXCEPTION: {exc!r}"
        all_ok = all_ok and ok
        print(f" [{'PASS' if ok else 'FAIL'}] {name}")
        print(f"        {detail}")
    print("-" * 72)

    green = analyze(data_dir() / "selftest_green.db", synthetic=True)
    print("\n---- GREEN analysis report (evidence) ----")
    print(format_report(green, data_dir() / "selftest_green.db"))
    red = analyze(data_dir() / "selftest_red.db", synthetic=True)
    print("\n---- RED verdict (forced-FAIL evidence) ----")
    print(f"verdict={red.verdict} reasons={red.reasons}")
    print("\n" + "=" * 72)
    print(f" SELF-TEST OVERALL: {'ALL CHECKS PASS' if all_ok else 'FAILURES PRESENT'}")
    print("=" * 72)
    for line in FIREWALL_LINES:
        print(line)
    return 0 if all_ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
