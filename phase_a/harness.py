"""SQLite event-logging harness for smartkey Phase-A (spec §12.4 blocker 3).

One row per prediction event. The row is INSERTed at candidate-generation time
(so events that never resolve are still counted toward the unresolved rate) and
UPDATEd with ``outcome`` when the next actual token becomes known.
``latency_us`` is wall-clock microseconds measured in the *same* event path — it
brackets the added instrumentation cost (p_top3 compute + hash + durable
INSERT), which is exactly the overhead the ≤20 ms/event budget governs.

PRIVACY (L0-leakage mitigation, conductor decision): the calibration gate needs
only ``outcome ∈ {0,1}`` and ``p_top3``. Therefore NO plaintext linguistic
content is persisted — not the candidate words, not the resolved token. The
outcome (was the actual next token in the predicted top-3) is computed in memory
at resolution time and only the {0,1} bit is stored. ``context_hash`` is a keyed
HMAC (salt kept in a 0600 sidecar, not in the DB) so it is not a
dictionary-recoverable hash of a single corpus word. Over a 14-day real-typing
window, events.db thus contains only aggregates and hashes — no passwords /
medical / family terms.

The no-LLM schema invariant (Codex major) is enforced at the storage layer: a
CHECK constraint rejects any resolver whose prefix is not 'script:'/'sql:' and
any class other than 'machine'. The nightly audit (sweep.py) re-verifies it.

Engine-agnostic on purpose: the real ibus adapter and the synthetic self-test
drive the exact same API, so the machinery is proven without IBus or the GUI.
"""

from __future__ import annotations

import hashlib
import hmac
import json
import sqlite3
import time
from dataclasses import dataclass
from pathlib import Path

from .constants import (
    ALLOWED_RESOLVER_PREFIXES,
    EVENT_CLASS,
    HARNESS_VERSION,
    RESOLVER,
)
from .paths import context_salt

SCHEMA = """
CREATE TABLE IF NOT EXISTS run_metadata (
    id               INTEGER PRIMARY KEY,
    run_id           TEXT NOT NULL,
    created_ts       REAL NOT NULL,
    synthetic        INTEGER NOT NULL CHECK (synthetic IN (0, 1)),
    freq_table_hash  TEXT NOT NULL,
    freq_table_files TEXT NOT NULL,
    p_model          TEXT NOT NULL,
    engine_commit    TEXT,
    harness_version  TEXT NOT NULL,
    notes            TEXT
);

CREATE TABLE IF NOT EXISTS events (
    id             INTEGER PRIMARY KEY,
    run_id         TEXT NOT NULL,
    ts             REAL NOT NULL,
    context_hash   TEXT NOT NULL,          -- keyed HMAC; NOT recoverable from DB alone
    n_candidates   INTEGER NOT NULL,       -- count only (0-3); NO candidate words stored
    p_top3         REAL NOT NULL,
    latency_us     INTEGER NOT NULL,
    outcome        INTEGER CHECK (outcome IN (0, 1) OR outcome IS NULL),
    class          TEXT NOT NULL DEFAULT 'machine' CHECK (class = 'machine'),
    resolver       TEXT NOT NULL
        CHECK (resolver LIKE 'script:%' OR resolver LIKE 'sql:%'),
    resolved_ts    REAL,
    synthetic      INTEGER NOT NULL CHECK (synthetic IN (0, 1))
    -- NOTE: no top3 / resolved_token columns. Plaintext linguistic content is
    -- never persisted; outcome is computed in memory and only the {0,1} bit kept.
);
CREATE INDEX IF NOT EXISTS idx_events_ts  ON events (ts);
CREATE INDEX IF NOT EXISTS idx_events_run ON events (run_id);

CREATE TABLE IF NOT EXISTS sweeps (
    id              INTEGER PRIMARY KEY,
    sweep_date      TEXT NOT NULL,
    ran_ts          REAL NOT NULL,
    events          INTEGER NOT NULL,
    resolutions     INTEGER NOT NULL,
    latency_p99_us  INTEGER,
    over_budget_pct REAL,
    watchdog_status TEXT NOT NULL,
    receipt         TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_sweeps_date ON sweeps (sweep_date);
"""


def context_hash(context: str, salt: bytes) -> str:
    """Keyed 16-hex-char HMAC of the context. The salt (kept in a 0600 sidecar,
    not in the DB) makes this non-recoverable by dictionary attack over the
    corpus vocabulary — so events.db alone never reveals a context word."""
    return hmac.new(salt, context.encode("utf-8"), hashlib.sha256).hexdigest()[:16]


@dataclass
class Pending:
    """Handle for an in-flight prediction awaiting its resolved token."""

    row_id: int
    top3: list[str]
    context: str


def connect(db_path: str | Path, *, readonly: bool = False) -> sqlite3.Connection:
    """Open (and initialise) the Phase-A SQLite DB with durability-tuned PRAGMAs."""
    db_path = Path(db_path)
    if readonly:
        conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
        return conn
    db_path.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(str(db_path))
    conn.execute("PRAGMA journal_mode=WAL;")
    conn.execute("PRAGMA synchronous=NORMAL;")
    conn.executescript(SCHEMA)
    conn.commit()
    return conn


class PhaseALogger:
    """Append-only event logger. Not thread-safe (IBus is single-threaded/GIL)."""

    def __init__(
        self,
        db_path: str | Path,
        freq_model,
        *,
        synthetic: bool = False,
        engine_commit: str | None = None,
        notes: str | None = None,
        resolver: str = RESOLVER,
    ) -> None:
        if not resolver.startswith(ALLOWED_RESOLVER_PREFIXES):
            raise ValueError(
                f"resolver {resolver!r} violates no-LLM schema invariant "
                f"(must start with one of {ALLOWED_RESOLVER_PREFIXES})"
            )
        self.freq = freq_model
        self.synthetic = 1 if synthetic else 0
        self.resolver = resolver
        self._salt = context_salt()
        self.conn = connect(db_path)
        self.run_id = f"{int(time.time() * 1000)}-{'syn' if synthetic else 'real'}"
        self._write_run_metadata(engine_commit, notes)

    def _write_run_metadata(self, engine_commit: str | None, notes: str | None) -> None:
        self.conn.execute(
            "INSERT INTO run_metadata (run_id, created_ts, synthetic, "
            "freq_table_hash, freq_table_files, p_model, engine_commit, "
            "harness_version, notes) VALUES (?,?,?,?,?,?,?,?,?)",
            (
                self.run_id,
                time.time(),
                self.synthetic,
                self.freq.table_hash(),
                json.dumps(self.freq.files, ensure_ascii=False),
                self.freq.pmodel,
                engine_commit,
                HARNESS_VERSION,
                notes,
            ),
        )
        self.conn.commit()

    # ---- candidate-generation point -----------------------------------------
    def log_prediction(
        self,
        context: str,
        top3: list[str],
        ts: float | None = None,
    ) -> Pending:
        """Record a prediction event; returns a handle to resolve later.

        Latency is measured across the added work (p_top3 compute + hash +
        durable INSERT) — the trailing latency write-back is post-measurement
        bookkeeping, disclosed as such.
        """
        if ts is None:
            ts = time.time()
        t0 = time.perf_counter_ns()
        top3 = list(top3[:3])
        p_top3 = self.freq.p_top3(context, top3)
        ch = context_hash(context, self._salt)
        # Only the candidate COUNT is stored — never the candidate words.
        cur = self.conn.execute(
            "INSERT INTO events (run_id, ts, context_hash, n_candidates, p_top3, "
            "latency_us, outcome, class, resolver, synthetic) "
            "VALUES (?,?,?,?,?,?,NULL,?,?,?)",
            (
                self.run_id,
                ts,
                ch,
                len(top3),
                p_top3,
                0,  # placeholder; overwritten below with the measured latency
                EVENT_CLASS,
                self.resolver,
                self.synthetic,
            ),
        )
        row_id = int(cur.lastrowid)
        latency_us = (time.perf_counter_ns() - t0) // 1000
        self.conn.execute(
            "UPDATE events SET latency_us=? WHERE id=?", (latency_us, row_id)
        )
        self.conn.commit()
        # top3 is retained IN MEMORY only (to compute outcome at resolution);
        # it is never persisted and is discarded when the Pending is dropped.
        return Pending(row_id=row_id, top3=top3, context=context)

    # ---- resolution point ----------------------------------------------------
    def resolve(self, pending: Pending, resolved_token: str) -> int:
        """Compute outcome in memory (was the actual next token in the predicted
        top-3) and persist ONLY the {0,1} bit — the token itself is discarded."""
        outcome = 1 if resolved_token in set(pending.top3) else 0
        self.conn.execute(
            "UPDATE events SET outcome=?, resolved_ts=? WHERE id=?",
            (outcome, time.time(), pending.row_id),
        )
        self.conn.commit()
        return outcome

    def close(self) -> None:
        self.conn.commit()
        self.conn.close()
