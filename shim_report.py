"""Validity and reconciliation report for real calibration campaigns.

The report and the calibration gate share ``phase_a.validity`` as their only
row-selection policy. This keeps foreign, malformed, empty, and operationally
excluded rows from being counted differently by the two tools.
"""

from __future__ import annotations

import json
import sqlite3
import sys
from dataclasses import dataclass, field
from pathlib import Path

from phase_a.validity import REASON_CODES, select_campaign_rows


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
        flags: list[str] = []
        if self.kill_windows:
            flags.append("kill-switch activity present")
        if self.degraded_marks or self.drop_total:
            flags.append("collector degradation present")
        return flags


def build_report(
    db_path: str | Path,
    campaign_run_id: str | None = None,
    window: tuple[float, float] | None = None,
) -> ValidityReport:
    conn = sqlite3.connect(f"file:{Path(db_path)}?mode=ro", uri=True)
    try:
        selection = select_campaign_rows(conn, campaign_run_id, window)
        return ValidityReport(
            db_path=str(db_path),
            campaign_run_id=selection.campaign_run_id,
            window=selection.window,
            raw_rows=len(selection.raw_rows),
            valid=len(selection.valid_rows),
            excluded=dict(selection.excluded),
            drop_total=selection.drop_total,
            kill_windows=list(selection.kill_windows),
            degraded_marks=selection.degraded_marks,
        )
    finally:
        conn.close()


def format_report(report: ValidityReport) -> str:
    lines = [
        "CALIBRATION VALIDITY / RECONCILIATION REPORT",
        f"db: {report.db_path}",
        f"campaign_run_id: {report.campaign_run_id}",
        f"window: {report.window}",
        f"raw_rows: {report.raw_rows}",
        f"valid_resolutions: {report.valid}",
    ]
    for code in REASON_CODES:
        lines.append(f"  {code}: {report.excluded.get(code, 0)}")
    lines.append(f"dropped_events (never rows): {report.drop_total}")
    lines.append(f"reconciles (raw == valid + Σ excluded): {report.reconciles}")
    lines.append(
        "HOLD flags: " + ("; ".join(report.hold_flags) if report.hold_flags else "none")
    )
    lines.append(
        "NOTE: reconciliation and calibration use the same campaign selector; "
        "operational HOLD flags can never produce PASS."
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
    print(
        json.dumps(
            {
                "raw_rows": report.raw_rows,
                "valid": report.valid,
                "excluded": report.excluded,
                "reconciles": report.reconciles,
                "drop_total": report.drop_total,
                "hold_flags": report.hold_flags,
            },
            ensure_ascii=False,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
