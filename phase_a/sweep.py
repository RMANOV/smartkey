"""Daily sweep receipt + 48h watchdog (spec §12.4 watchdog row).

  sweep    : compact daily receipt (events, resolutions, running raw-p bucket
             table, latency p99, watchdog status), recorded in the ``sweeps``
             table + a receipt file; clears any stale alarm; re-verifies the
             no-LLM schema invariant.
  watchdog : if the most recent sweep is older than 48h (or missing while events
             exist) -> write a VISIBLE alarm file and exit non-zero; else clear
             the alarm and exit 0.

Every receipt ends with the verbatim firewall lines.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import time
from pathlib import Path

import numpy as np

from .constants import (
    ALLOWED_RESOLVER_PREFIXES,
    FIREWALL_LINES,
    LATENCY_BUDGET_US,
    PREDICTION_SCOPE,
    WATCHDOG_MAX_AGE_H,
)
from .harness import connect
from .paths import alarm_file, default_db_path, receipts_dir


def _invariant_violations(conn, syn: int) -> int:
    like = " OR ".join(f"resolver LIKE '{p}%'" for p in ALLOWED_RESOLVER_PREFIXES)
    row = conn.execute(
        f"SELECT COUNT(*) FROM events WHERE synthetic=? AND "
        f"(class != 'machine' OR NOT ({like}))",
        (syn,),
    ).fetchone()
    return int(row[0])


def _raw_bucket_table(p: np.ndarray, y: np.ndarray, n_buckets: int = 5) -> list[str]:
    if len(p) < n_buckets:
        return ["   (too few resolutions for a bucket table yet)"]
    qs = [i / n_buckets for i in range(1, n_buckets)]
    edges = np.quantile(p, qs)
    idx = np.searchsorted(edges, p, side="right")
    lines = ["   bucket   n     raw_p̄    hit_rate"]
    for b in range(n_buckets):
        m = idx == b
        if not m.any():
            continue
        lines.append(f"     {b}    {int(m.sum()):5d}   {p[m].mean():.4f}    {y[m].mean():.4f}")
    return lines


def run_sweep(db_path: str | Path, synthetic: bool = False) -> str:
    syn = 1 if synthetic else 0
    conn = connect(db_path)
    today = _dt.date.today().isoformat()
    now = time.time()
    day_start = time.mktime(_dt.date.today().timetuple())

    rows = conn.execute(
        "SELECT ts, p_top3, latency_us, outcome FROM events WHERE synthetic=?",
        (syn,),
    ).fetchall()
    total = len(rows)
    resolved = [r for r in rows if r[3] is not None]
    today_events = sum(1 for r in rows if r[0] >= day_start)
    lat = np.array([r[2] for r in rows], dtype=float) if rows else np.array([0.0])
    p99 = int(np.percentile(lat, 99))
    over = float(np.mean(lat > LATENCY_BUDGET_US)) if rows else 0.0
    invariant_bad = _invariant_violations(conn, syn)

    p = np.array([r[1] for r in resolved], dtype=float)
    y = np.array([r[3] for r in resolved], dtype=float)

    prev = conn.execute("SELECT MAX(ran_ts) FROM sweeps").fetchone()[0]
    age_h = (now - prev) / 3600.0 if prev else None
    wd = "OK"
    if age_h is not None and age_h > WATCHDOG_MAX_AGE_H:
        wd = f"WAS-STALE ({age_h:.1f}h gap before this sweep)"

    lines = []
    lines.append("=" * 62)
    lines.append(f" O1 / PHASE-A DAILY RECEIPT  {today}   ({'synthetic' if syn else 'real'})")
    lines.append(f" prediction_scope: {PREDICTION_SCOPE}  (n-gram component ONLY)")
    lines.append("=" * 62)
    lines.append(f" events total      : {total}   (today: {today_events})")
    lines.append(f" resolutions       : {len(resolved)}")
    lines.append(f" unresolved        : {total - len(resolved)}")
    lines.append(f" latency p99       : {p99} us   (budget {LATENCY_BUDGET_US})")
    lines.append(f" events > 20ms     : {over:.3%}")
    lines.append(f" no-LLM invariant  : {'OK' if invariant_bad == 0 else f'VIOLATED x{invariant_bad}'}")
    lines.append(f" watchdog          : {wd}")
    lines.append(" running raw-p bucket table:")
    lines.extend(_raw_bucket_table(p, y))
    lines.append("=" * 62)
    for line in FIREWALL_LINES:
        lines.append(line)
    receipt = "\n".join(lines)

    conn.execute(
        "INSERT INTO sweeps (sweep_date, ran_ts, events, resolutions, "
        "latency_p99_us, over_budget_pct, watchdog_status, receipt) "
        "VALUES (?,?,?,?,?,?,?,?)",
        (today, now, total, len(resolved), p99, over, wd, receipt),
    )
    conn.commit()
    conn.close()

    af = alarm_file()
    if af.exists():
        af.unlink()
    (receipts_dir() / f"sweep-{today}.txt").write_text(receipt + "\n", encoding="utf-8")
    return receipt


def run_watchdog(db_path: str | Path, synthetic: bool = False) -> int:
    """48h watchdog. Returns 0 if fresh, 1 if a visible alarm was raised."""
    syn = 1 if synthetic else 0
    conn = connect(db_path)
    n_events = conn.execute(
        "SELECT COUNT(*) FROM events WHERE synthetic=?", (syn,)
    ).fetchone()[0]
    last = conn.execute("SELECT MAX(ran_ts) FROM sweeps").fetchone()[0]
    conn.close()
    now = time.time()

    if n_events == 0:
        return 0

    stale = last is None or (now - last) / 3600.0 > WATCHDOG_MAX_AGE_H
    af = alarm_file()
    if stale:
        age = "never" if last is None else f"{(now - last) / 3600.0:.1f}h ago"
        msg = "\n".join(
            [
                "!!! PHASE-A WATCHDOG ALARM !!!",
                f"raised: {_dt.datetime.now().isoformat()}",
                f"last sweep: {age}   (threshold {WATCHDOG_MAX_AGE_H}h)",
                "The daily sweep has not run within the watchdog window.",
                "This is an in-band alarm: a missed watchdog WITH this alarm is NOT the",
                "'missed without in-band alarm' FAIL clause, but PASS still requires zero",
                "missed watchdog days. Investigate the cron/driver.",
                "",
                *FIREWALL_LINES,
            ]
        )
        af.write_text(msg + "\n", encoding="utf-8")
        print(msg)
        return 1
    if af.exists():
        af.unlink()
    print(f"watchdog OK: last sweep {(now - last) / 3600.0:.1f}h ago")
    for line in FIREWALL_LINES:
        print(line)
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description="O1 / Phase-A daily sweep / 48h watchdog")
    ap.add_argument("cmd", choices=["sweep", "watchdog"], nargs="?", default="sweep")
    ap.add_argument("--db", default=str(default_db_path()))
    ap.add_argument("--synthetic", action="store_true")
    args = ap.parse_args()
    if args.cmd == "watchdog":
        return run_watchdog(args.db, synthetic=args.synthetic)
    print(run_sweep(args.db, synthetic=args.synthetic))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
