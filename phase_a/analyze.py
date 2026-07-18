"""O1 / Phase-A offline analysis + mechanical PASS/FAIL/INCONCLUSIVE gate.

Executes the hash-committed §12.4 gate verbatim (spec §2 offline scorer):

  split   : first 50% of resolutions by ts = calibration FIT; second 50% =
            SCORING. No post-hoc re-splitting.
  buckets : 5 quantile buckets on raw p_top3 over the FIT half; a SCORE-half
            bucket with <30 resolutions merges into its LEFT neighbour
            (leftmost, having none, merges right); max 5, min 2.
  gate    : calibrated p (sklearn IsotonicRegression fit on the FIT half) vs
            observed hit-rate per surviving bucket. Raw p_top3 is REPORTED as a
            baseline; it does not gate.
  verdict : PASS  = |mean_p - hit_rate| <= 0.05 in every surviving bucket AND
                    >=5000 resolutions AND 0 missed watchdog AND <=1% >20ms.
            FAIL  = |mean_p - hit_rate| > 0.10 in >=3 buckets OR >5% unresolved
                    OR >1% >20ms OR missed watchdog without in-band alarm.
            INCONCLUSIVE = everything else.

FAIL-first precedence (mechanical disambiguation, documented — not a spec
change): a fatal plumbing condition overrides a superficial calibration pass.

Scope: this gate reads ONLY p_top3 / outcome from the n-gram measured path; no
ensemble output ever reaches it. No LLM / no neural: numpy + IsotonicRegression.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import json
import sqlite3
from dataclasses import dataclass, field
from pathlib import Path

import numpy as np
from sklearn.isotonic import IsotonicRegression

from .constants import (
    FIREWALL_LINES,
    GATE_FAIL_BUCKETS,
    GATE_FAIL_GAP,
    GATE_PASS_GAP,
    LATENCY_BUDGET_US,
    MAX_OVER_BUDGET_FRAC,
    MAX_UNRESOLVED_FRAC,
    MIN_BUCKET_RESOLUTIONS,
    MIN_RESOLUTIONS,
    MIN_SURVIVING_BUCKETS,
    N_QUANTILE_BUCKETS,
    PREDICTION_SCOPE,
)
from .harness import connect
from .paths import alarm_file, default_db_path


@dataclass
class BucketResult:
    index: int
    lo: float
    hi: float
    n_score: int
    mean_raw_p: float
    mean_cal_p: float
    hit_rate: float
    gap_cal: float
    gap_raw: float


@dataclass
class AnalysisResult:
    verdict: str
    reasons: list[str] = field(default_factory=list)
    total_events: int = 0
    resolutions: int = 0
    unresolved: int = 0
    unresolved_frac: float = 0.0
    over_budget_frac: float = 0.0
    latency_p99_us: int = 0
    n_fit: int = 0
    n_score: int = 0
    buckets: list[BucketResult] = field(default_factory=list)
    missed_watchdog: int = 0
    alarm_raised: bool = False
    freq_table_hash: str = ""
    scope: str = "real"
    prediction_scope: str = PREDICTION_SCOPE

    def to_dict(self) -> dict:
        d = self.__dict__.copy()
        d["buckets"] = [b.__dict__ for b in self.buckets]
        return d


# ---------------------------------------------------------------------------
# Bucketing / merging
# ---------------------------------------------------------------------------
def _quantile_edges(fit_p: np.ndarray, n_buckets: int) -> list[float]:
    qs = [i / n_buckets for i in range(1, n_buckets)]
    edges = np.quantile(fit_p, qs)
    return sorted(set(round(float(e), 12) for e in edges))


def _assign_buckets(score_p: np.ndarray, edges: list[float]) -> list[list[int]]:
    if not edges:
        return [list(range(len(score_p)))]
    idx = np.searchsorted(np.asarray(edges), score_p, side="right")
    buckets: list[list[int]] = [[] for _ in range(len(edges) + 1)]
    for i, b in enumerate(idx):
        buckets[int(b)].append(i)
    return buckets


def _merge_small(buckets: list[list[int]], min_count: int, min_buckets: int) -> list[list[int]]:
    """Adjacent-merge buckets with <min_count members into their LEFT neighbour
    (leftmost merges right), until all >= min_count or only min_buckets remain."""
    buckets = [list(b) for b in buckets]
    while len(buckets) > min_buckets:
        small = next((i for i, b in enumerate(buckets) if len(b) < min_count), None)
        if small is None:
            break
        if small > 0:
            buckets[small - 1].extend(buckets[small])
            del buckets[small]
        else:
            buckets[1].extend(buckets[0])
            del buckets[0]
    return buckets


# ---------------------------------------------------------------------------
# Watchdog coverage
# ---------------------------------------------------------------------------
def _watchdog_status(conn: sqlite3.Connection, first_ts: float, last_ts: float) -> tuple[int, bool]:
    if first_ts <= 0 or last_ts <= 0:
        return 0, alarm_file().exists()
    d0 = _dt.date.fromtimestamp(first_ts)
    d1 = _dt.date.fromtimestamp(last_ts)
    expected = set()
    d = d0
    while d <= d1:
        expected.add(d.isoformat())
        d += _dt.timedelta(days=1)
    rows = conn.execute("SELECT sweep_date, watchdog_status FROM sweeps").fetchall()
    swept = {r[0] for r in rows}
    alarm_raised = alarm_file().exists() or any(r[1] == "ALARM" for r in rows)
    return len(expected - swept), alarm_raised


# ---------------------------------------------------------------------------
# Core analysis
# ---------------------------------------------------------------------------
def analyze(db_path: str | Path, synthetic: bool = False) -> AnalysisResult:
    conn = connect(db_path, readonly=True)
    scope = "synthetic" if synthetic else "real"
    syn = 1 if synthetic else 0

    all_rows = conn.execute(
        "SELECT ts, p_top3, latency_us, outcome FROM events WHERE synthetic=? ORDER BY ts",
        (syn,),
    ).fetchall()
    fh_row = conn.execute(
        "SELECT freq_table_hash FROM run_metadata WHERE synthetic=? ORDER BY id DESC LIMIT 1",
        (syn,),
    ).fetchone()
    freq_table_hash = fh_row[0] if fh_row else ""

    res = AnalysisResult(verdict="INCONCLUSIVE", scope=scope, freq_table_hash=freq_table_hash)
    res.total_events = len(all_rows)
    if res.total_events == 0:
        res.reasons.append("no events logged")
        conn.close()
        return res

    lat = np.array([r[2] for r in all_rows], dtype=float)
    res.over_budget_frac = float(np.mean(lat > LATENCY_BUDGET_US))
    res.latency_p99_us = int(np.percentile(lat, 99))

    resolved = [r for r in all_rows if r[3] is not None]
    res.resolutions = len(resolved)
    res.unresolved = res.total_events - res.resolutions
    res.unresolved_frac = res.unresolved / res.total_events if res.total_events else 0.0

    res.missed_watchdog, res.alarm_raised = _watchdog_status(
        conn, all_rows[0][0], all_rows[-1][0]
    )
    conn.close()

    # --- split (first 50% resolutions by ts = fit; second 50% = score) -------
    n = len(resolved)
    if n >= 4:
        n_fit = n // 2
        fit = resolved[:n_fit]
        score = resolved[n_fit:]
        fit_p = np.array([r[1] for r in fit], dtype=float)
        fit_y = np.array([r[3] for r in fit], dtype=float)
        score_p = np.array([r[1] for r in score], dtype=float)
        score_y = np.array([r[3] for r in score], dtype=float)
        res.n_fit = len(fit)
        res.n_score = len(score)

        # --- isotonic calibration fitted on the FIT half --------------------
        iso = IsotonicRegression(y_min=0.0, y_max=1.0, increasing=True, out_of_bounds="clip")
        if len(np.unique(fit_p)) >= 2:
            iso.fit(fit_p, fit_y)
            score_cal = iso.predict(score_p)
        else:
            base = float(np.mean(fit_y)) if len(fit_y) else 0.0
            score_cal = np.full_like(score_p, base)

        # --- buckets (quantile edges from FIT, applied to SCORE) ------------
        edges = _quantile_edges(fit_p, N_QUANTILE_BUCKETS)
        raw_buckets = _assign_buckets(score_p, edges)
        merged = _merge_small(raw_buckets, MIN_BUCKET_RESOLUTIONS, MIN_SURVIVING_BUCKETS)

        for bi, members in enumerate(merged):
            if not members:
                continue
            m = np.array(members, dtype=int)
            bp = score_p[m]
            bcal = score_cal[m]
            by = score_y[m]
            res.buckets.append(
                BucketResult(
                    index=bi,
                    lo=float(bp.min()),
                    hi=float(bp.max()),
                    n_score=len(m),
                    mean_raw_p=float(bp.mean()),
                    mean_cal_p=float(bcal.mean()),
                    hit_rate=float(by.mean()),
                    gap_cal=float(abs(bcal.mean() - by.mean())),
                    gap_raw=float(abs(bp.mean() - by.mean())),
                )
            )

    # --- gate (FAIL-first precedence) ----------------------------------------
    surviving = res.buckets
    n_gap_over_fail = sum(1 for b in surviving if b.gap_cal > GATE_FAIL_GAP)

    fail_reasons: list[str] = []
    if n_gap_over_fail >= GATE_FAIL_BUCKETS:
        fail_reasons.append(
            f"|mean_p-hit_rate|>{GATE_FAIL_GAP} in {n_gap_over_fail} buckets (>= {GATE_FAIL_BUCKETS})"
        )
    if res.unresolved_frac > MAX_UNRESOLVED_FRAC:
        fail_reasons.append(f"unresolved {res.unresolved_frac:.1%} > {MAX_UNRESOLVED_FRAC:.0%}")
    if res.over_budget_frac > MAX_OVER_BUDGET_FRAC:
        fail_reasons.append(f">20ms events {res.over_budget_frac:.2%} > {MAX_OVER_BUDGET_FRAC:.0%}")
    if res.missed_watchdog > 0 and not res.alarm_raised:
        fail_reasons.append(f"missed watchdog {res.missed_watchdog} day(s) without in-band alarm")

    if fail_reasons:
        res.verdict = "FAIL"
        res.reasons = fail_reasons
        return res

    # --- PASS check ----------------------------------------------------------
    pass_missing: list[str] = []
    if len(surviving) < MIN_SURVIVING_BUCKETS:
        pass_missing.append(f"only {len(surviving)} surviving bucket(s) (< {MIN_SURVIVING_BUCKETS})")
    if any(b.gap_cal > GATE_PASS_GAP for b in surviving):
        worst = max((b.gap_cal for b in surviving), default=0.0)
        pass_missing.append(f"a bucket has |mean_p-hit_rate|={worst:.3f} > {GATE_PASS_GAP}")
    if res.resolutions < MIN_RESOLUTIONS:
        pass_missing.append(f"{res.resolutions} resolutions < {MIN_RESOLUTIONS}")
    if res.missed_watchdog != 0:
        pass_missing.append(f"{res.missed_watchdog} missed watchdog day(s)")
    if res.over_budget_frac > MAX_OVER_BUDGET_FRAC:
        pass_missing.append(">20ms budget exceeded")

    if not pass_missing and surviving:
        res.verdict = "PASS"
        res.reasons = ["all surviving buckets within +/-0.05; volume/latency/watchdog clean"]
    else:
        res.verdict = "INCONCLUSIVE"
        res.reasons = pass_missing or ["insufficient resolved data to bucket"]
    return res


# ---------------------------------------------------------------------------
# Report formatting (ends with the verbatim firewall lines)
# ---------------------------------------------------------------------------
def format_report(res: AnalysisResult, db_path: str | Path) -> str:
    L = []
    L.append("=" * 72)
    L.append(f" O1 / PHASE-A ANALYSIS REPORT  ({res.scope} data)")
    L.append(f" prediction_scope: {res.prediction_scope}  (n-gram component ONLY; ensemble firewalled out)")
    L.append(f" db: {db_path}")
    L.append(f" freq-table hash (n-gram version pin): {res.freq_table_hash[:16]}...")
    L.append("=" * 72)
    L.append(f" events (total)      : {res.total_events}")
    L.append(f" resolutions         : {res.resolutions}   (>= {MIN_RESOLUTIONS} for PASS)")
    L.append(f" unresolved          : {res.unresolved}  ({res.unresolved_frac:.2%})   (FAIL if > 5%)")
    L.append(f" latency p99         : {res.latency_p99_us} us   (budget {LATENCY_BUDGET_US} us)")
    L.append(f" events > 20ms       : {res.over_budget_frac:.3%}   (PASS <=1% / FAIL >1%)")
    L.append(f" watchdog missed days: {res.missed_watchdog}   (in-band alarm raised: {res.alarm_raised})")
    L.append(f" split               : fit={res.n_fit}  score={res.n_score}")
    L.append("-" * 72)
    if res.buckets:
        L.append(" surviving buckets (gate on CALIBRATED p; raw p is baseline only):")
        L.append("  #  raw_p[lo..hi]         n     raw_p̄   |raw-hit|   cal_p̄   hit     |cal-hit|")
        for b in res.buckets:
            flag = ""
            if b.gap_cal > GATE_FAIL_GAP:
                flag = "  <-- FAIL gap"
            elif b.gap_cal > GATE_PASS_GAP:
                flag = "  <-- >0.05"
            L.append(
                f"  {b.index}  [{b.lo:.5f}..{b.hi:.5f}]  {b.n_score:6d}  "
                f"{b.mean_raw_p:.4f}   |{b.gap_raw:.3f}|    "
                f"{b.mean_cal_p:.4f}  {b.hit_rate:.4f}   {b.gap_cal:.4f}{flag}"
            )
    else:
        L.append(" (no surviving buckets — insufficient resolved data)")
    L.append("-" * 72)
    L.append(f" VERDICT: {res.verdict}")
    for r in res.reasons:
        L.append(f"   - {r}")
    L.append("=" * 72)
    L.append(" note: low raw hit-rate is NOT failure (~20-35% top-3 expected for BG;")
    L.append(" Phase-A tests the plumbing, not the model). Green kills nothing above; red")
    L.append(" kills/redesigns the n-gram plumbing.")
    L.append("")
    for line in FIREWALL_LINES:
        L.append(line)
    return "\n".join(L)


def main() -> int:
    ap = argparse.ArgumentParser(description="O1 / Phase-A analysis + gate")
    ap.add_argument("--db", default=str(default_db_path()))
    ap.add_argument("--synthetic", action="store_true", help="analyse the synthetic self-test DB")
    ap.add_argument("--json", action="store_true", help="emit machine-readable JSON")
    args = ap.parse_args()
    res = analyze(args.db, synthetic=args.synthetic)
    if args.json:
        print(json.dumps(res.to_dict(), ensure_ascii=False, indent=2))
        for line in FIREWALL_LINES:
            print(line)
    else:
        print(format_report(res, args.db))
    return {"PASS": 0, "FAIL": 2, "INCONCLUSIVE": 3}.get(res.verdict, 3)


if __name__ == "__main__":
    raise SystemExit(main())
