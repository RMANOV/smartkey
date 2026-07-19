"""SQLite event logger for n-gram calibration.

One row per prediction event, INSERTed at candidate-generation time (so events
that never resolve still count toward the unresolved rate) and UPDATEd with
``outcome`` when the next actual token becomes known. ``latency_us`` is
wall-clock microseconds measured in the same event path — the full decision +
durable-write cost the <=20 ms/event budget governs.

The gate needs only ``outcome in {0,1}`` and ``p_top3``. Plaintext ``top3`` and
``resolved_token`` are held IN MEMORY only (to compute the outcome bit) and
never persisted; the DB stores the candidate COUNT and a keyed-HMAC
``context_hash`` instead.

The non-LLM schema invariant is enforced structurally: a CHECK
constraint rejects any resolver not prefixed 'script:'/'sql:' and any class
other than 'machine'.
"""

from __future__ import annotations

import hashlib
import hmac
import json
import sqlite3
import time
from dataclasses import dataclass, field
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
    scope            TEXT,
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
    run_id          TEXT NOT NULL,
    synthetic       INTEGER NOT NULL CHECK (synthetic IN (0, 1)),
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


def _migrate_sweeps_schema(conn: sqlite3.Connection) -> None:
    """Add campaign provenance to pre-provenance databases, idempotently.

    SQLite cannot add a ``NOT NULL`` column to a populated table without a
    default.  A default would incorrectly attribute legacy sweeps, so migrated
    rows deliberately retain ``NULL`` provenance and are never eligible for a
    campaign.  The trigger makes provenance mandatory for every subsequent
    insert while preserving those legacy rows for audit.
    """
    columns = {str(row[1]) for row in conn.execute("PRAGMA table_info(sweeps)")}
    if "run_id" not in columns:
        conn.execute("ALTER TABLE sweeps ADD COLUMN run_id TEXT")
    if "synthetic" not in columns:
        conn.execute(
            "ALTER TABLE sweeps ADD COLUMN synthetic INTEGER "
            "CHECK (synthetic IN (0, 1))"
        )
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_sweeps_campaign "
        "ON sweeps (run_id, synthetic, ran_ts)"
    )
    conn.executescript(
        """
        CREATE TRIGGER IF NOT EXISTS sweeps_provenance_required_insert
        BEFORE INSERT ON sweeps
        WHEN NEW.run_id IS NULL OR trim(NEW.run_id) = '' OR NEW.synthetic IS NULL
        BEGIN
            SELECT RAISE(ABORT, 'sweeps provenance required');
        END;
        """
    )


def context_hash(context: str, salt: bytes) -> str:
    """Keyed 16-hex-char HMAC of the context (salt in a 0600 sidecar, not in the
    DB) — not dictionary-recoverable over the corpus vocabulary."""
    return hmac.new(salt, context.encode("utf-8"), hashlib.sha256).hexdigest()[:16]


@dataclass
class Pending:
    """Handle for an in-flight prediction awaiting its resolved token. ``top3``
    is held in memory only (to compute outcome); never persisted."""

    row_id: int
    top3: list[str]
    context: str
    ctx_tokens: list[str] = field(default_factory=list)
    n_ctx: int = 0


def connect(db_path: str | Path, *, readonly: bool = False) -> sqlite3.Connection:
    """Open (and initialise) the Phase-A SQLite DB with durability-tuned PRAGMAs."""
    db_path = Path(db_path)
    if readonly:
        return sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    db_path.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(str(db_path))
    conn.execute("PRAGMA journal_mode=WAL;")
    conn.execute("PRAGMA synchronous=NORMAL;")
    conn.executescript(SCHEMA)
    _migrate_sweeps_schema(conn)
    conn.commit()
    return conn


class PhaseALogger:
    """Append-only event logger. Not thread-safe (single-threaded by design)."""

    def __init__(
        self,
        db_path: str | Path,
        freq_model,
        *,
        synthetic: bool = False,
        engine_commit: str | None = None,
        notes: str | None = None,
        scope: str | None = None,
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
        self.scope = scope
        self._salt = context_salt()
        # Pipelined latency: each row stores the FULL cost (compute + INSERT +
        # commit) of the PREVIOUS event, so the hot path commits exactly once yet
        # the stored value is honestly commit-inclusive. The one-row attribution
        # shift is invariant for the gate (a distribution over rows).
        self._prev_latency_us = 0
        self.conn = connect(db_path)
        self.run_id = f"{int(time.time() * 1000)}-{'syn' if synthetic else 'real'}"
        self._write_run_metadata(engine_commit, notes)

    def _write_run_metadata(self, engine_commit: str | None, notes: str | None) -> None:
        self.conn.execute(
            "INSERT INTO run_metadata (run_id, created_ts, synthetic, "
            "freq_table_hash, freq_table_files, p_model, engine_commit, "
            "harness_version, scope, notes) VALUES (?,?,?,?,?,?,?,?,?,?)",
            (
                self.run_id,
                time.time(),
                self.synthetic,
                self.freq.table_hash(),
                json.dumps(self.freq.files, ensure_ascii=False),
                self.freq.pmodel,
                engine_commit,
                HARNESS_VERSION,
                self.scope,
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
        p_top3: float | None = None,
    ) -> Pending:
        """Record a prediction event; returns a handle to resolve later.

        ``latency_us`` stored is the full commit-inclusive cost of the PREVIOUS
        event (one durable write per event). If *p_top3* is provided it is used
        verbatim (lets a driver inject a controlled skew for the forced-FAIL
        receipt); otherwise it is computed from the freq model.
        """
        if ts is None:
            ts = time.time()
        t0 = time.perf_counter_ns()
        top3 = list(top3[:3])
        if p_top3 is None:
            p_top3 = self.freq.p_top3(context, top3)
        ch = context_hash(context, self._salt)
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
                self._prev_latency_us,
                EVENT_CLASS,
                self.resolver,
                self.synthetic,
            ),
        )
        row_id = int(cur.lastrowid)
        self.conn.commit()  # single durable write — included in the timing
        self._prev_latency_us = (time.perf_counter_ns() - t0) // 1000
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

    def resolve_bit(self, pending: Pending, outcome: int, ts: float | None = None) -> int:
        """Persist a pre-computed outcome bit (used by the corpus-replay driver,
        which computes membership in memory and never stores the token)."""
        outcome = 1 if outcome else 0
        self.conn.execute(
            "UPDATE events SET outcome=?, resolved_ts=? WHERE id=?",
            (outcome, ts if ts is not None else time.time(), pending.row_id),
        )
        self.conn.commit()
        return outcome

    def close(self) -> None:
        self.conn.commit()
        self.conn.close()
