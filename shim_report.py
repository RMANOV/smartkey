"""O1 shim validity / reconciliation report (Δ3, design §3).

Computes, over a campaign events DB, the frozen validity predicate V1–V7 and
the exclusion reason-code taxonomy, then reconciles

    raw_rows == valid + Σ excluded(per code)          (B.3)

This module READS ``events`` + ``collector_log`` only. It is NOT the verdict
engine — the §12.4 verdict remains ``phase_a/analyze.py`` UNCHANGED (B.4);
this report complements it for the integrity receipt. No row surgery, no
bucket exclusion (B.1): any non-empty kill-switch / degradation class means
HOLD + CONDUCTOR adjudication, reported here, never silently filtered.

Reason codes (first-match order — design §3, frozen):
  EXCL_SYNTHETIC, EXCL_FOREIGN_RUN, EXCL_CORRUPT_ROW, EXCL_OUT_OF_WINDOW,
  EXCL_NO_CANDIDATES, EXCL_KILL_SWITCH_WINDOW, EXCL_COLLECTOR_DEGRADED,
  EXCL_UNRESOLVED
"""

from __future__ import annotations

import json
import sqlite3
import sys
from dataclasses import dataclass, field
from pathlib import Path

#: Half-width (seconds) of the exclusion interval around a collector
#: degradation log entry (queue_overflow / drop_counter / writer_error).
#: Frozen with the taxonomy — not tunable at analysis time.
DEGRADED_WINDOW_S = 60.0

REASON_CODES = (
    "EXCL_SYNTHETIC",
    "EXCL_FOREIGN_RUN",
    "EXCL_CORRUPT_ROW",
    "EXCL_OUT_OF_WINDOW",
    "EXCL_NO_CANDIDATES",
    "EXCL_KILL_SWITCH_WINDOW",
    "EXCL_COLLECTOR_DEGRADED",
    "EXCL_UNRESOLVED",
)

_DEGRADED_KINDS = ("queue_overflow", "drop_counter", "writer_error")


@dataclass
class ValidityReport:
    db_path: str
    campaign_run_id: str | None
    window: tuple[float, float] | None
    raw_rows: int = 0
    valid: int = 0
    excluded: dict[str, int] = field(default_factory=dict)
    drop_total: int = 0
    kill_windows: list[tuple[float, float | None]] = field(default_factory=list)
    degraded_marks: int = 0

    @property
    def reconciles(self) -> bool:
        return self.raw_rows == self.valid + sum(self.excluded.values())

    @property
    def hold_flags(self) -> list[str]:
        flags = []
        if self.excluded.get("EXCL_KILL_SWITCH_WINDOW"):
            flags.append("kill-switch window non-empty")
        if self.excluded.get("EXCL_COLLECTOR_DEGRADED") or self.drop_total:
            flags.append("collector degradation present")
        return flags


def _in_any(ts: float, windows: list[tuple[float, float | None]]) -> bool:
    return any(start <= ts and (end is None or ts <= end) for start, end in windows)


def _kill_windows(rows: list[tuple[float, str]]) -> list[tuple[float, float | None]]:
    """Pair kill_switch_on → kill_switch_off log entries into intervals; an
    unmatched ON is open-ended."""
    windows: list[tuple[float, float | None]] = []
    open_start: float | None = None
    for ts, kind in rows:
        if kind == "kill_switch_on" and open_start is None:
            open_start = ts
        elif kind == "kill_switch_off" and open_start is not None:
            windows.append((open_start, ts))
            open_start = None
    if open_start is not None:
        windows.append((open_start, None))
    return windows


def classify_row(
    row: sqlite3.Row,
    campaign_run_id: str | None,
    window: tuple[float, float] | None,
    kill_windows: list[tuple[float, float | None]],
    degraded_windows: list[tuple[float, float | None]],
) -> str | None:
    """First-match reason code for a non-valid row; None when the row is a
    valid resolution (V1–V7)."""
    ts = row["ts"]
    outcome = row["outcome"]
    resolved_ts = row["resolved_ts"]

    if row["synthetic"] != 0:
        return "EXCL_SYNTHETIC"
    if campaign_run_id is not None and row["run_id"] != campaign_run_id:
        return "EXCL_FOREIGN_RUN"
    corrupt = (
        not (0.0 <= row["p_top3"] <= 1.0)
        or row["latency_us"] < 0
        or outcome not in (0, 1, None)
        or (resolved_ts is not None and outcome is not None and resolved_ts < ts)
    )
    if corrupt:
        return "EXCL_CORRUPT_ROW"
    if window is not None:
        end_ts = resolved_ts if resolved_ts is not None else ts
        if not (window[0] <= ts and end_ts <= window[1]):
            return "EXCL_OUT_OF_WINDOW"
    if row["n_candidates"] < 1:
        return "EXCL_NO_CANDIDATES"
    if _in_any(ts, kill_windows) or (
        resolved_ts is not None and _in_any(resolved_ts, kill_windows)
    ):
        return "EXCL_KILL_SWITCH_WINDOW"
    if _in_any(ts, degraded_windows) or (
        resolved_ts is not None and _in_any(resolved_ts, degraded_windows)
    ):
        return "EXCL_COLLECTOR_DEGRADED"
    if outcome is None or resolved_ts is None:
        return "EXCL_UNRESOLVED"
    return None


def build_report(
    db_path: str | Path,
    campaign_run_id: str | None = None,
    window: tuple[float, float] | None = None,
) -> ValidityReport:
    conn = sqlite3.connect(f"file:{Path(db_path)}?mode=ro", uri=True)
    conn.row_factory = sqlite3.Row
    try:
        if campaign_run_id is None:
            row = conn.execute(
                "SELECT run_id FROM run_metadata WHERE synthetic=0 "
                "ORDER BY created_ts DESC LIMIT 1"
            ).fetchone()
            campaign_run_id = row["run_id"] if row else None

        try:
            log_rows = [
                (r["ts"], r["kind"])
                for r in conn.execute(
                    "SELECT ts, kind, detail FROM collector_log ORDER BY ts"
                )
            ]
            drop_row = conn.execute(
                "SELECT detail FROM collector_log WHERE kind='drop_counter' "
                "ORDER BY ts DESC LIMIT 1"
            ).fetchone()
            degraded_marks = [
                r[0] for r in log_rows if r[1] in _DEGRADED_KINDS
            ]
        except sqlite3.OperationalError:  # no collector_log table (lab DB)
            log_rows, drop_row, degraded_marks = [], None, []

        report = ValidityReport(
            db_path=str(db_path),
            campaign_run_id=campaign_run_id,
            window=window,
            drop_total=int(drop_row["detail"]) if drop_row else 0,
            kill_windows=_kill_windows(log_rows),
            degraded_marks=len(degraded_marks),
        )
        degraded_windows: list[tuple[float, float | None]] = [
            (ts - DEGRADED_WINDOW_S, ts + DEGRADED_WINDOW_S) for ts in degraded_marks
        ]

        for row in conn.execute(
            "SELECT run_id, ts, p_top3, latency_us, n_candidates, outcome, "
            "resolved_ts, synthetic FROM events"
        ):
            report.raw_rows += 1
            code = classify_row(
                row, campaign_run_id, window, report.kill_windows, degraded_windows
            )
            if code is None:
                report.valid += 1
            else:
                report.excluded[code] = report.excluded.get(code, 0) + 1
        return report
    finally:
        conn.close()


def format_report(report: ValidityReport) -> str:
    lines = [
        "O1 SHIM VALIDITY / RECONCILIATION REPORT (Δ3, design §3)",
        f"db: {report.db_path}",
        f"campaign_run_id: {report.campaign_run_id}",
        f"window: {report.window}",
        f"raw_rows: {report.raw_rows}",
        f"valid_resolutions: {report.valid}",
    ]
    for code in REASON_CODES:
        lines.append(f"  {code}: {report.excluded.get(code, 0)}")
    lines.append(f"dropped_events (never rows): {report.drop_total}")
    lines.append(
        f"reconciles (raw == valid + Σ excluded): {report.reconciles}"
    )
    holds = report.hold_flags
    lines.append(
        "HOLD flags: " + ("; ".join(holds) if holds else "none")
    )
    lines.append(
        "NOTE: verdict engine remains phase_a/analyze.py UNCHANGED (B.4); "
        "non-empty HOLD flags require CONDUCTOR adjudication, never filtering."
    )
    return "\n".join(lines)


def main(argv: list[str]) -> int:
    if not argv:
        print("usage: shim_report.py <events.db> [run_id] [start_ts end_ts]")
        return 2
    db = argv[0]
    run_id = argv[1] if len(argv) > 1 else None
    window = (float(argv[2]), float(argv[3])) if len(argv) > 3 else None
    report = build_report(db, run_id, window)
    print(format_report(report))
    print(json.dumps({
        "raw_rows": report.raw_rows,
        "valid": report.valid,
        "excluded": report.excluded,
        "reconciles": report.reconciles,
        "drop_total": report.drop_total,
    }, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
