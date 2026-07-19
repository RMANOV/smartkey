"""Canonical row selection for real calibration campaigns.

Both reconciliation and calibration consume this module so foreign runs,
malformed rows, empty candidate sets, and operational exclusion windows cannot
silently enter one path but not the other. Unresolved eligible rows remain in
the analysis population so the unresolved-rate gate stays meaningful.
"""

from __future__ import annotations

import math
import sqlite3
from dataclasses import dataclass, field

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
class CampaignSelection:
    campaign_run_id: str | None
    window: tuple[float, float] | None
    raw_rows: list[sqlite3.Row] = field(default_factory=list)
    valid_rows: list[sqlite3.Row] = field(default_factory=list)
    analysis_rows: list[sqlite3.Row] = field(default_factory=list)
    excluded: dict[str, int] = field(default_factory=dict)
    drop_total: int = 0
    kill_windows: list[tuple[float, float | None]] = field(default_factory=list)
    degraded_marks: int = 0

    @property
    def reconciles(self) -> bool:
        return len(self.raw_rows) == len(self.valid_rows) + sum(self.excluded.values())

    @property
    def hold_flags(self) -> list[str]:
        flags: list[str] = []
        if self.kill_windows:
            flags.append("kill-switch activity present")
        if self.degraded_marks or self.drop_total:
            flags.append("collector degradation present")
        return flags


def _in_any(ts: float, windows: list[tuple[float, float | None]]) -> bool:
    return any(start <= ts and (end is None or ts <= end) for start, end in windows)


def _kill_windows(rows: list[tuple[float, str]]) -> list[tuple[float, float | None]]:
    """Pair shutdown requests/activations with optional recovery markers."""
    windows: list[tuple[float, float | None]] = []
    open_start: float | None = None
    for ts, kind in rows:
        if kind in ("kill_switch_requested", "kill_switch_on") and open_start is None:
            open_start = ts
        elif kind == "kill_switch_off" and open_start is not None:
            windows.append((open_start, ts))
            open_start = None
    if open_start is not None:
        windows.append((open_start, None))
    return windows


def _finite_number(value: object) -> bool:
    return isinstance(value, (int, float)) and math.isfinite(float(value))


def classify_row(
    row: sqlite3.Row,
    campaign_run_id: str | None,
    window: tuple[float, float] | None,
    kill_windows: list[tuple[float, float | None]],
    degraded_windows: list[tuple[float, float | None]],
) -> str | None:
    """Return the first exclusion code, or ``None`` for a valid resolution."""
    ts = row["ts"]
    outcome = row["outcome"]
    resolved_ts = row["resolved_ts"]

    if row["synthetic"] != 0:
        return "EXCL_SYNTHETIC"
    if campaign_run_id is not None and row["run_id"] != campaign_run_id:
        return "EXCL_FOREIGN_RUN"
    corrupt = (
        not _finite_number(ts)
        or not _finite_number(row["p_top3"])
        or not (0.0 <= float(row["p_top3"]) <= 1.0)
        or not _finite_number(row["latency_us"])
        or float(row["latency_us"]) < 0
        or not isinstance(row["n_candidates"], int)
        or row["n_candidates"] < 0
        or outcome not in (0, 1, None)
        or (resolved_ts is not None and not _finite_number(resolved_ts))
        or (
            resolved_ts is not None
            and outcome is not None
            and float(resolved_ts) < float(ts)
        )
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


def select_campaign_rows(
    conn: sqlite3.Connection,
    campaign_run_id: str | None = None,
    window: tuple[float, float] | None = None,
) -> CampaignSelection:
    """Select one real campaign and classify every row in the database."""
    conn.row_factory = sqlite3.Row
    if campaign_run_id is None:
        latest = conn.execute(
            "SELECT run_id FROM run_metadata WHERE synthetic=0 "
            "ORDER BY created_ts DESC, id DESC LIMIT 1"
        ).fetchone()
        campaign_run_id = latest["run_id"] if latest else None

    try:
        log_rows = [
            (float(row["ts"]), str(row["kind"]))
            for row in conn.execute("SELECT ts, kind FROM collector_log ORDER BY ts")
        ]
        drop_row = conn.execute(
            "SELECT detail FROM collector_log WHERE kind='drop_counter' "
            "ORDER BY ts DESC LIMIT 1"
        ).fetchone()
        degraded_times = [ts for ts, kind in log_rows if kind in _DEGRADED_KINDS]
    except sqlite3.OperationalError:
        log_rows, drop_row, degraded_times = [], None, []

    try:
        drop_total = int(drop_row["detail"]) if drop_row else 0
    except (TypeError, ValueError):
        drop_total = 0

    selection = CampaignSelection(
        campaign_run_id=campaign_run_id,
        window=window,
        drop_total=drop_total,
        kill_windows=_kill_windows(log_rows),
        degraded_marks=len(degraded_times),
    )
    degraded_windows = [
        (ts - DEGRADED_WINDOW_S, ts + DEGRADED_WINDOW_S) for ts in degraded_times
    ]

    selection.raw_rows = list(
        conn.execute(
            "SELECT run_id, ts, p_top3, latency_us, n_candidates, outcome, "
            "resolved_ts, synthetic FROM events ORDER BY ts, id"
        )
    )
    for row in selection.raw_rows:
        code = classify_row(
            row,
            campaign_run_id,
            window,
            selection.kill_windows,
            degraded_windows,
        )
        if code is None:
            selection.valid_rows.append(row)
            selection.analysis_rows.append(row)
        else:
            selection.excluded[code] = selection.excluded.get(code, 0) + 1
            if code == "EXCL_UNRESOLVED":
                selection.analysis_rows.append(row)
    return selection
