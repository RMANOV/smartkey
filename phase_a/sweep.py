"""Daily calibration sweep and 48-hour watchdog.

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
import hashlib
import os
import sqlite3
import time
from contextlib import suppress
from pathlib import Path

import numpy as np

from .constants import (
    ALLOWED_RESOLVER_PREFIXES,
    FIREWALL_LINES,
    LATENCY_BUDGET_US,
    PREDICTION_SCOPE,
    WATCHDOG_MAX_AGE_H,
)
from .harness import connect, validate_phasea_db
from .paths import alarm_file, default_db_path, receipts_dir, require_existing_db
from .validity import (
    select_campaign_id,
    select_campaign_latest_sweep_ts,
    select_campaign_rows,
    select_campaign_sweep_rows,
)


def _implementation_hash() -> str:
    """Hash the exact Phase-A code bytes that produce a sweep receipt."""
    digest = hashlib.sha256()
    root = Path(__file__).resolve().parent
    for name in ("constants.py", "harness.py", "paths.py", "sweep.py", "validity.py"):
        digest.update(name.encode("utf-8"))
        digest.update(b"\0")
        digest.update((root / name).read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


def _write_receipt_atomic(path: Path, receipt: str) -> None:
    """Durably replace one receipt without exposing a partial file."""
    tmp = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    fd: int | None = None
    try:
        fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        handle = os.fdopen(fd, "w", encoding="utf-8")
        fd = None
        try:
            handle.write(receipt + "\n")
            handle.flush()
            os.fsync(handle.fileno())
        except BaseException:
            with suppress(BaseException):
                handle.close()
            raise
        else:
            handle.close()
        os.replace(tmp, path)
        directory_fd = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory_fd)
        except BaseException:
            with suppress(BaseException):
                os.close(directory_fd)
            raise
        else:
            os.close(directory_fd)
    except BaseException:
        if fd is not None:
            with suppress(BaseException):
                os.close(fd)
        with suppress(BaseException):
            tmp.unlink(missing_ok=True)
        raise


def _invariant_violations(conn, campaign_run_id: str, syn: int) -> int:
    like = " OR ".join(f"resolver LIKE '{p}%'" for p in ALLOWED_RESOLVER_PREFIXES)
    row = conn.execute(
        f"SELECT COUNT(*) FROM events WHERE run_id=? AND synthetic=? AND "
        f"(class != 'machine' OR NOT ({like}))",
        (campaign_run_id, syn),
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


def run_sweep(
    db_path: str | Path,
    synthetic: bool = False,
    campaign_run_id: str | None = None,
) -> str:
    syn = 1 if synthetic else 0
    conn = connect(db_path)
    selection = select_campaign_rows(
        conn, campaign_run_id, synthetic=synthetic
    )
    selected_run_id = selection.campaign_run_id
    if selected_run_id is None:
        conn.close()
        raise ValueError(
            f"no {'synthetic' if synthetic else 'real'} campaign metadata found"
        )
    today = _dt.date.today().isoformat()
    now = time.time()
    day_start = time.mktime(_dt.date.today().timetuple())

    rows = [
        (row["ts"], row["p_top3"], row["latency_us"], row["outcome"])
        for row in selection.analysis_rows
    ]
    total = len(rows)
    resolved = [r for r in rows if r[3] is not None]
    today_events = sum(1 for r in rows if r[0] >= day_start)
    lat = np.array([r[2] for r in rows], dtype=float) if rows else np.array([0.0])
    p99 = int(np.percentile(lat, 99))
    over = float(np.mean(lat > LATENCY_BUDGET_US)) if rows else 0.0
    invariant_bad = _invariant_violations(conn, selected_run_id, syn)

    p = np.array([r[1] for r in resolved], dtype=float)
    y = np.array([r[3] for r in resolved], dtype=float)

    prior_sweeps = select_campaign_sweep_rows(
        conn, selected_run_id, synthetic=synthetic
    )
    prev = max((row["ran_ts"] for row in prior_sweeps), default=None)
    age_h = (now - prev) / 3600.0 if prev else None
    wd = "OK"
    if age_h is not None and age_h > WATCHDOG_MAX_AGE_H:
        wd = f"WAS-STALE ({age_h:.1f}h gap before this sweep)"

    lines = []
    lines.append("=" * 62)
    lines.append(f" O1 / PHASE-A DAILY RECEIPT  {today}   ({'synthetic' if syn else 'real'})")
    lines.append(f" prediction_scope: {PREDICTION_SCOPE}  (n-gram component ONLY)")
    lines.append(f" campaign_run_id: {selected_run_id}")
    lines.append(f" implementation hash: {_implementation_hash()}")
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

    scope = "synthetic" if synthetic else "real"
    campaign_digest = hashlib.sha256(selected_run_id.encode("utf-8")).hexdigest()[:16]
    receipt_path = receipts_dir() / (
        f"sweep-{today}-{scope}-{campaign_digest}-{int(now * 1000)}.txt"
    )
    try:
        conn.execute(
            "INSERT INTO sweeps (run_id, synthetic, sweep_date, ran_ts, events, "
            "resolutions, latency_p99_us, over_budget_pct, watchdog_status, receipt) "
            "VALUES (?,?,?,?,?,?,?,?,?,?)",
            (
                selected_run_id,
                syn,
                today,
                now,
                total,
                len(resolved),
                p99,
                over,
                wd,
                receipt,
            ),
        )
        _write_receipt_atomic(receipt_path, receipt)
        conn.commit()
    except BaseException:
        with suppress(BaseException):
            conn.rollback()
        with suppress(BaseException):
            conn.close()
        with suppress(BaseException):
            receipt_path.unlink(missing_ok=True)
        raise
    conn.close()

    af = alarm_file(selected_run_id, synthetic=synthetic)
    if af.exists():
        af.unlink()
    return receipt


def run_watchdog(
    db_path: str | Path,
    synthetic: bool = False,
    campaign_run_id: str | None = None,
) -> int:
    """48h watchdog. Returns 0 if fresh, 1 if a visible alarm was raised."""
    conn = connect(db_path, readonly=True)
    conn.row_factory = sqlite3.Row
    selected_run_id = select_campaign_id(
        conn, campaign_run_id, synthetic=synthetic
    )
    if campaign_run_id is not None and selected_run_id is None:
        conn.close()
        raise ValueError(
            f"campaign {campaign_run_id!r} not found in "
            f"{'synthetic' if synthetic else 'real'} scope"
        )
    n_events = (
        conn.execute(
            "SELECT COUNT(*) FROM events WHERE run_id=? AND synthetic=?",
            (selected_run_id, 1 if synthetic else 0),
        ).fetchone()[0]
        if selected_run_id is not None
        else 0
    )
    last = select_campaign_latest_sweep_ts(
        conn,
        selected_run_id,
        synthetic=synthetic,
    )
    conn.close()
    now = time.time()

    if n_events == 0:
        return 0

    stale = last is None or (now - last) / 3600.0 > WATCHDOG_MAX_AGE_H
    af = alarm_file(selected_run_id, synthetic=synthetic)
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


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="O1 / Phase-A daily sweep / 48h watchdog")
    ap.add_argument("cmd", choices=["sweep", "watchdog"], nargs="?", default="sweep")
    ap.add_argument(
        "--db",
        help=(
            "event DB (required unless SMARTKEY_PHASEA_DB or "
            "SMARTKEY_PHASEA_DATA selects a safe location)"
        ),
    )
    ap.add_argument("--synthetic", action="store_true")
    ap.add_argument("--run-id", help="campaign run_id (default: latest in selected scope)")
    args = ap.parse_args(argv)
    try:
        if args.db is None:
            args.db = str(default_db_path())
        args.db = str(require_existing_db(args.db))
        validate_phasea_db(args.db)
    except (OSError, RuntimeError, ValueError) as exc:
        ap.error(str(exc))
    try:
        if args.cmd == "watchdog":
            # Resolve the alarm root before the DB read so an unsafe artifact
            # path cannot fail only after watchdog state has been evaluated.
            alarm_file(args.run_id, synthetic=args.synthetic)
            return run_watchdog(
                args.db,
                synthetic=args.synthetic,
                campaign_run_id=args.run_id,
            )
        # Resolve both output roots before run_sweep inserts its receipt row.
        receipts_dir()
        alarm_file(args.run_id, synthetic=args.synthetic)
        receipt = run_sweep(
            args.db,
            synthetic=args.synthetic,
            campaign_run_id=args.run_id,
        )
    except (OSError, RuntimeError, ValueError, sqlite3.Error) as exc:
        ap.error(str(exc))
    print(receipt)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
