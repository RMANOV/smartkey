"""End-to-end machinery self-test on SIMULATED keystroke data (spec §12.4 F).

Proves the whole pipeline works WITHOUT IBus, the GUI, or the Rust engine:
logging -> 50/50 split -> quantile bucketing -> isotonic calibration -> gate ->
sweep -> watchdog. All data is clearly synthetic (synthetic=1) in dedicated DB
files, never mixed with real data (events.db).

Checks:
  1. GREEN  : a well-calibrated, high-volume run drives a PASS verdict.
  2. RED    : a deliberately non-stationary (miscalibrated) run proves the gate
              emits FAIL via the calibration-gap clause (>=3 buckets > 0.10).
  3-6. FAIL-clause reachability: >1% >20ms, >5% unresolved, missed-watchdog-
       without-alarm, and the in-band-alarm distinction.
  7. SCHEMA : the no-LLM schema invariant rejects a non-script/sql resolver.
  8. LIVE   : run_sweep + run_watchdog produce a receipt and correct exit code.

Exit code 0 iff every check passes.
"""

from __future__ import annotations

import datetime as _dt
import json
import os
import random
import sqlite3
import time
from pathlib import Path

from .analyze import analyze, format_report
from .constants import EVENT_CLASS, RESOLVER
from .harness import PhaseALogger, connect
from .paths import alarm_file, data_dir, engine_identity_file
from .sweep import run_sweep, run_watchdog


class SyntheticFreq:
    """Freq-model stub: p_top3 is encoded in top3[0] so the (raw_p, outcome)
    distribution is fully controllable for calibration testing."""

    pmodel = "synthetic-selftest"
    files: list[dict] = []

    def table_hash(self) -> str:
        return "synthetic-selftest-freqhash"

    def p_top3(self, context: str, top3: list[str]) -> float:
        try:
            v = float(top3[0])
        except (ValueError, IndexError):
            return 0.0
        return max(0.0, min(1.0, v))


def _fresh(path: Path) -> Path:
    for suffix in ("", "-wal", "-shm"):
        p = Path(str(path) + suffix)
        if p.exists():
            p.unlink()
    return path


def _sim_daily_sweeps(db_path: Path, first_ts: float, last_ts: float) -> None:
    """Insert one 'OK' sweep row per date in the event window (watchdog cover)."""
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


def _gen_run(logger: PhaseALogger, n: int, hit_fn, resolve_frac: float,
             ts_start: float, ts_span: float, rng: random.Random) -> None:
    for i in range(n):
        p = rng.uniform(0.02, 0.6)
        ts = ts_start + (i / max(n, 1)) * ts_span
        top3 = [f"{p:.6f}", f"b{i % 7}", f"c{i % 5}"]
        pend = logger.log_prediction("ctx", top3, ts=ts)
        if rng.random() < resolve_frac:
            hit = rng.random() < hit_fn(p, i, n)
            logger.resolve(pend, top3[0] if hit else "MISS_TOKEN")


def _craft_db(name: str, rows: list[tuple], with_sweeps: bool) -> Path:
    """Direct-insert a crafted synthetic DB for gate-clause reachability tests.
    rows: list of (ts, p, outcome_or_None, latency_us)."""
    path = _fresh(data_dir() / f"selftest_gate_{name}.db")
    conn = connect(path)
    conn.execute(
        "INSERT INTO run_metadata (run_id, created_ts, synthetic, freq_table_hash, "
        "freq_table_files, p_model, engine_commit, harness_version, notes) "
        "VALUES (?,?,?,?,?,?,?,?,?)",
        (f"craft-{name}", time.time(), 1, "crafted", "[]", "crafted", None, "selftest", name),
    )
    for ts, p, outcome, lat in rows:
        conn.execute(
            "INSERT INTO events (run_id, ts, context_hash, n_candidates, p_top3, "
            "latency_us, outcome, class, resolver, synthetic) "
            "VALUES (?,?,?,?,?,?,?,?,?,1)",
            (
                f"craft-{name}", ts, "ch", 3, p, lat,
                outcome, EVENT_CLASS, RESOLVER,
            ),
        )
    conn.commit()
    if with_sweeps:
        conn.close()
        _sim_daily_sweeps(path, rows[0][0], rows[-1][0])
    else:
        conn.close()
    return path


def _clean_calib_rows(n: int, rng: random.Random, ts0: float,
                      unresolved_frac: float = 0.0,
                      over_budget_frac: float = 0.0,
                      span_days: int = 1) -> list[tuple]:
    """Well-calibrated rows (raw p ~= hit rate) with tunable unresolved/latency."""
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
    rng = random.Random(1)
    db = _fresh(data_dir() / "selftest_green.db")
    logger = PhaseALogger(db, SyntheticFreq(), synthetic=True, notes="selftest GREEN")
    now = time.time()
    _gen_run(logger, 12000, lambda p, i, n: 0.15 + 0.5 * p, 0.97,
             now - 14 * 86400, 14 * 86400, rng)
    logger.close()
    # first/last event ts for sweep coverage:
    conn = connect(db, readonly=True)
    lo, hi = conn.execute("SELECT MIN(ts), MAX(ts) FROM events").fetchone()
    conn.close()
    _sim_daily_sweeps(db, lo, hi)
    res = analyze(db, synthetic=True)
    ok = res.verdict == "PASS"
    detail = (
        f"verdict={res.verdict} res={res.resolutions} unres={res.unresolved_frac:.2%} "
        f">20ms={res.over_budget_frac:.3%} buckets={len(res.buckets)} "
        f"maxgap={max((b.gap_cal for b in res.buckets), default=0):.3f} missed_wd={res.missed_watchdog}"
    )
    return ok, detail


def check_red() -> tuple[bool, str]:
    rng = random.Random(2)
    db = _fresh(data_dir() / "selftest_red.db")
    logger = PhaseALogger(db, SyntheticFreq(), synthetic=True, notes="selftest RED miscalibrated")
    now = time.time()

    # Non-stationary: fit half (first-resolved) high hit-rate, score half shifted
    # down by ~0.35 -> isotonic (learned on fit) overshoots score by >0.10.
    def hit_fn(p, i, n):
        base = 0.25 + 0.6 * p
        return base if i < n // 2 else max(0.0, base - 0.35)

    _gen_run(logger, 8000, hit_fn, 1.0, now - 2 * 86400, 86400, rng)
    logger.close()
    conn = connect(db, readonly=True)
    lo, hi = conn.execute("SELECT MIN(ts), MAX(ts) FROM events").fetchone()
    conn.close()
    _sim_daily_sweeps(db, lo, hi)
    res = analyze(db, synthetic=True)
    n_over = sum(1 for b in res.buckets if b.gap_cal > 0.10)
    ok = res.verdict == "FAIL" and n_over >= 3
    detail = f"verdict={res.verdict} buckets_gap>0.10={n_over} reasons={res.reasons}"
    return ok, detail


def check_over_budget_fail() -> tuple[bool, str]:
    rng = random.Random(3)
    rows = _clean_calib_rows(2000, rng, time.time() - 86400, over_budget_frac=0.02)
    db = _craft_db("overbudget", rows, with_sweeps=True)
    res = analyze(db, synthetic=True)
    ok = res.verdict == "FAIL" and any(">20ms" in r for r in res.reasons)
    return ok, f"verdict={res.verdict} reasons={res.reasons}"


def check_unresolved_fail() -> tuple[bool, str]:
    rng = random.Random(4)
    rows = _clean_calib_rows(2000, rng, time.time() - 86400, unresolved_frac=0.10)
    db = _craft_db("unresolved", rows, with_sweeps=True)
    res = analyze(db, synthetic=True)
    ok = res.verdict == "FAIL" and any("unresolved" in r for r in res.reasons)
    return ok, f"verdict={res.verdict} reasons={res.reasons}"


def check_missed_watchdog_fail() -> tuple[bool, str]:
    rng = random.Random(5)
    rows = _clean_calib_rows(2000, rng, time.time() - 3 * 86400, span_days=3)
    db = _craft_db("missedwd", rows, with_sweeps=False)  # no sweeps
    if alarm_file().exists():
        alarm_file().unlink()  # no in-band alarm
    res = analyze(db, synthetic=True)
    ok = res.verdict == "FAIL" and any("watchdog" in r for r in res.reasons)
    return ok, f"verdict={res.verdict} missed_wd={res.missed_watchdog} reasons={res.reasons}"


def check_inband_alarm_distinction() -> tuple[bool, str]:
    """Missed watchdog WITH an in-band alarm must NOT trip the 'without alarm'
    FAIL clause (should be INCONCLUSIVE, not FAIL-by-watchdog)."""
    rng = random.Random(6)
    rows = _clean_calib_rows(2000, rng, time.time() - 3 * 86400, span_days=3)
    db = _craft_db("inbandalarm", rows, with_sweeps=False)
    alarm_file().write_text("in-band alarm present\n", encoding="utf-8")
    res = analyze(db, synthetic=True)
    alarm_file().unlink(missing_ok=True)
    ok = res.verdict != "FAIL" or not any("without in-band alarm" in r for r in res.reasons)
    return ok, f"verdict={res.verdict} reasons={res.reasons}"


def check_schema_invariant() -> tuple[bool, str]:
    """The DB CHECK must reject a resolver that is not script:/sql: prefixed."""
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
    # also reject class != 'machine'
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


def check_b2_outcome_coverage() -> tuple[bool, str]:
    """Codex B2 proof: the adapter resolves EVERY next-token via surrounding-text
    delta — a non-predicted next word records outcome=0 (not 'unresolved') —
    plus the commit fast-path. Also proves the MAJOR engine-identity receipt."""
    from pathlib import Path

    from .engine_adapter import PhaseAAdapter
    from .harness import connect

    db = _fresh(data_dir() / "selftest_b2.db")
    if engine_identity_file().exists():
        engine_identity_file().unlink()
    ad = PhaseAAdapter(db, [Path("corpus/corpus_tech.json")], engine_commit="deadbeef",
                       notes="selftest B2")

    def predict_then_type(top3, typed_surrounding):
        ad.on_next_word_prediction(top3)
        ad.observe_context(typed_surrounding)  # resolves the pending via delta

    ad.observe_context("")                                   # start, ctx=[]
    predict_then_type(["cat", "dog", "bird"], "cat ")        # -> 'cat' in top3   => 1
    predict_then_type(["sat", "ran", "xyz"], "cat sat ")     # -> 'sat' in top3   => 1
    predict_then_type(["on", "in", "at"], "cat sat zzz ")    # -> 'zzz' NOT top3  => 0  (the fix)
    predict_then_type(["the", "a", "up"], "cat sat zzz the big mat ")  # multiword -> 'the' => 1
    ad.on_next_word_prediction(["end", "stop", "done"])      # pending, then...
    ad.observe_context(None)                                 # no surrounding text -> stays unresolved
    ad.close()

    conn = connect(db, readonly=True)
    outcomes = [r[0] for r in conn.execute("SELECT outcome FROM events ORDER BY id")]
    conn.close()

    # commit fast-path (fresh adapter): explicit commit resolves outcome=1
    db2 = _fresh(data_dir() / "selftest_b2_commit.db")
    ad2 = PhaseAAdapter(db2, [Path("corpus/corpus_tech.json")], engine_commit="d2")
    ad2.observe_context("hello ")
    ad2.on_next_word_prediction(["world", "there", "you"])
    ad2.on_commit("world")                                    # fast-path -> 1
    ad2.close()
    conn2 = connect(db2, readonly=True)
    commit_outcome = conn2.execute("SELECT outcome FROM events ORDER BY id").fetchone()[0]
    conn2.close()

    # identity receipt present + valid (Codex MAJOR)
    ident_ok = False
    if engine_identity_file().exists():
        ident = json.loads(engine_identity_file().read_text())
        ident_ok = ident.get("phase_a") == 1 and ident.get("pid") == os.getpid()

    ok = (
        outcomes == [1, 1, 0, 1, None]
        and commit_outcome == 1
        and ident_ok
    )
    return ok, (
        f"delta outcomes={outcomes} (expect [1,1,0,1,None]) commit_fastpath={commit_outcome} "
        f"identity_ok={ident_ok}"
    )


def check_live_sweep_watchdog() -> tuple[bool, str]:
    """run_sweep produces a receipt; run_watchdog returns 0 right after a sweep."""
    db = data_dir() / "selftest_green.db"  # reuse GREEN db (has data + sweeps)
    receipt = run_sweep(db, synthetic=True)
    wd = run_watchdog(db, synthetic=True)
    ok = "PHASE-A DAILY RECEIPT" in receipt and receipt.strip().endswith(
        "Phase-A зелено ≠ доказателство за team tier."
    ) and wd == 0
    return ok, f"receipt_ok={'PHASE-A DAILY RECEIPT' in receipt} watchdog_exit={wd}"


CHECKS = [
    ("1 GREEN  -> PASS", check_green),
    ("2 RED    -> FAIL (calibration gap)", check_red),
    ("3 >20ms  -> FAIL", check_over_budget_fail),
    ("4 unresolved>5% -> FAIL", check_unresolved_fail),
    ("5 missed watchdog no-alarm -> FAIL", check_missed_watchdog_fail),
    ("6 missed watchdog +alarm -> not-that-FAIL", check_inband_alarm_distinction),
    ("7 schema invariant rejects llm:/human", check_schema_invariant),
    ("8 live sweep + watchdog", check_live_sweep_watchdog),
    ("9 B2 outcome coverage (non-top3 -> 0) + identity", check_b2_outcome_coverage),
]


def main() -> int:
    print("=" * 70)
    print(" PHASE-A MACHINERY SELF-TEST (synthetic data, synthetic=1)")
    print("=" * 70)
    all_ok = True
    for name, fn in CHECKS:
        try:
            ok, detail = fn()
        except Exception as exc:  # a crash is a failed check
            ok, detail = False, f"EXCEPTION: {exc!r}"
        all_ok = all_ok and ok
        print(f" [{'PASS' if ok else 'FAIL'}] {name}")
        print(f"        {detail}")
    print("-" * 70)

    # Show the full GREEN report and RED verdict as evidence receipts.
    green = analyze(data_dir() / "selftest_green.db", synthetic=True)
    print("\n---- GREEN analysis report (evidence) ----")
    print(format_report(green, data_dir() / "selftest_green.db"))
    red = analyze(data_dir() / "selftest_red.db", synthetic=True)
    print("\n---- RED verdict (forced-FAIL evidence) ----")
    print(f"verdict={red.verdict} reasons={red.reasons}")
    print("\n" + "=" * 70)
    print(f" SELF-TEST OVERALL: {'ALL CHECKS PASS' if all_ok else 'FAILURES PRESENT'}")
    print("=" * 70)
    print("Phase-A зелено ≠ доказателство за team tier.")
    return 0 if all_ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
