"""Passive n-gram calibration collector.

One prediction event per word-commit boundary: the adapter resolves the
previous snapshot against the committed word token (outcome bit computed in
Rust — candidate words never cross the FFI), then takes a new snapshot with
the committed word as context. Rows are enqueued in-memory and drained by a
background writer thread into the ``phase_a`` events schema; the hot path
performs zero DB I/O. The DB lives in a config-driven data root that is
guarded fail-closed against the live/operator trees before any open,
via ``phase_a.paths.data_dir()``. The HMAC context salt is the 0600 sidecar
from ``phase_a.paths.context_salt()`` — outside the DB and never logged.

Failure isolation: any collector error disables the tap and never
blocks or alters the input path; queue overflow drops events (counted);
a ``<data_root>/KILL`` file stops the tap without touching the predictor.
"""

from __future__ import annotations

import atexit
import hashlib
import hmac
import json
import logging
import os
import queue
import re
import threading
import time
from pathlib import Path

log = logging.getLogger(__name__)

_WORD_RE = re.compile(r"\w+", re.UNICODE)

SHIM_VERSION = "o1-live-shim/1.0.0"
RESOLVER = "script:next_token_in_top3"
EVENT_CLASS = "machine"
P_MODEL = "live-ngram-component/bigram+unigram-backoff"

_QUEUE_MAX = 1024
_KILL_CHECK_EVERY = 16  # boundaries between hot-path kill-file checks

_COLLECTOR_LOG_SCHEMA = """
CREATE TABLE IF NOT EXISTS collector_log (
    id     INTEGER PRIMARY KEY,
    ts     REAL NOT NULL,
    kind   TEXT NOT NULL,
    detail TEXT
);
"""


def word_token(text: str) -> str:
    """Trailing word token, lowercased — mirrors
    ``phase_a.freqmodel.last_context_word`` exactly."""
    tokens = _WORD_RE.findall(text or "")
    return tokens[-1].lower() if tokens else ""


def freq_table_provenance(corpus_files: list[str]) -> tuple[str, str]:
    """(table_hash, files_json) over the engine's loaded corpus files, in the
    exact ``FreqModel.table_hash`` format (path:sha256:bytes, sorted)."""
    files: list[dict] = []
    for path_str in corpus_files:
        p = Path(path_str)
        try:
            raw = p.read_bytes()
        except OSError:
            continue
        files.append(
            {"path": str(p), "sha256": hashlib.sha256(raw).hexdigest(), "bytes": len(raw)}
        )
    joined = "\n".join(
        f"{f['path']}:{f['sha256']}:{f['bytes']}"
        for f in sorted(files, key=lambda f: f["path"])
    )
    table_hash = hashlib.sha256(joined.encode("utf-8")).hexdigest()
    return table_hash, json.dumps(files, ensure_ascii=False)


def _prepare_core_for_collection(core: object) -> bool:
    """Warm new native modules while keeping old ones usable.

    PyO3 turns a Rust panic into a ``BaseException`` subclass. Convert that
    into an ordinary initialization failure so the engine's optional-tap
    boundary can disable collection, but never swallow process-control exits.
    """
    prepare = getattr(core, "o1_prepare", None)
    if not callable(prepare):
        return False
    try:
        prepare()
    except (KeyboardInterrupt, SystemExit):
        raise
    except BaseException as exc:
        raise RuntimeError("native O1 cache preparation failed") from exc
    return True


class O1Collector:
    """Async, fail-isolated event collector around the core's O1 FFI surface.

    All SQLite access happens in the single writer thread; the hot path only
    calls the two FFI methods, computes the context HMAC, and enqueues."""

    def __init__(
        self,
        core: object,
        cfg: dict,
        corpus_files: list[str],
        engine_commit: str | None,
    ) -> None:
        self._core = core
        self._active = False
        self._pending_seq: int | None = None
        self._seq = 0
        self._boundary_n = 0
        self._dropped = 0
        self._dropped_logged = 0
        self._owner_thread_id = threading.get_ident()
        self._kill_requested = threading.Event()
        self._writer_failed = threading.Event()
        self._writer_failure_detail = "writer failure"

        if not corpus_files:
            log.warning("o1: no corpus loaded at startup — tap stays off")
            return

        # Resolve and guard the data root before any DB open. phase_a's
        # data_dir() raises on the live/operator trees (fail-closed); we set
        # the env var so every phase_a path helper agrees on the same root.
        data_dir_cfg = str(cfg.get("data_dir") or "~/.local/share/smartkey-phasea")
        os.environ["SMARTKEY_PHASEA_DATA"] = str(Path(data_dir_cfg).expanduser())
        from phase_a import paths as phase_a_paths  # deferred: import ≠ proof, call is

        data_root = phase_a_paths.data_dir()  # raises → __init__ fails closed
        self._db_path = data_root / "events.db"
        self._kill_file = data_root / "KILL"
        self._salt = phase_a_paths.context_salt()

        if self._kill_file.exists():
            log.warning("o1: KILL file present at startup — tap stays off")
            return

        _prepare_core_for_collection(self._core)

        table_hash, files_json = freq_table_provenance(corpus_files)
        self.run_id = f"{int(time.time() * 1000)}-live"
        self._meta_row = (
            self.run_id,
            time.time(),
            0,
            table_hash,
            files_json,
            P_MODEL,
            engine_commit,
            SHIM_VERSION,
            "ngram_component",
            "o1 live-run shim collector",
        )
        self._queue: queue.Queue = queue.Queue(maxsize=_QUEUE_MAX)
        self._stop = threading.Event()
        self._writer = threading.Thread(
            target=self._writer_run, name="o1-shim-writer", daemon=True
        )
        self._active = True
        self._writer.start()
        atexit.register(self.close)
        log.info("o1: collector active, run_id=%s db=%s", self.run_id, self._db_path)

    # ------------------------------------------------------------------
    # Hot path (adapter thread)
    # ------------------------------------------------------------------
    def on_word_commit(self, word: str) -> None:
        """One word-commit boundary: resolve the pending snapshot against the
        committed token, then snapshot the new context. Never raises."""
        if not self._on_owner_thread():
            log.error("o1: commit callback arrived off the adapter thread")
            self._writer_failure_detail = "off-owner commit callback"
            self._writer_failed.set()
            self._active = False
            return
        if self._service_control_requests():
            return
        if not self._active:
            return
        try:
            token = word_token(word)
            if not token:
                return
            now = time.time()
            outcome = self._core.o1_resolve(token)
            if outcome is not None and self._pending_seq is not None:
                self._put(("res", self._pending_seq, int(outcome), now))
            self._pending_seq = None

            self._boundary_n += 1
            if self._boundary_n % _KILL_CHECK_EVERY == 0 and self._kill_file.exists():
                self._activate_kill("hot-path check")
                return

            # Timed interval: before the FFI call through row construction
            # (including this latency value) is constructed, pre-enqueue.
            t0 = time.perf_counter_ns()
            n_candidates, p_top3, core_latency_us = self._core.o1_snapshot(token)
            context_hash = hmac.new(
                self._salt, token.encode("utf-8"), hashlib.sha256
            ).hexdigest()[:16]
            self._seq += 1
            seq = self._seq
            latency_us = (time.perf_counter_ns() - t0) // 1000
            row = ("pred", seq, now, context_hash, int(n_candidates), float(p_top3),
                   int(latency_us), int(core_latency_us))
            self._pending_seq = seq
            self._put(row)
        except BaseException:  # native panic must not escape the optional tap
            log.exception("o1: hot-path failure — disabling tap (input unaffected)")
            self._disable("hot-path failure")

    def on_abandon(self) -> None:
        """Focus loss / reset: drop the pending snapshot without resolving —
        its row stays unresolved (outcome NULL), counted honestly."""
        if not self._on_owner_thread():
            log.error("o1: abandon callback arrived off the adapter thread")
            self._writer_failure_detail = "off-owner abandon callback"
            self._writer_failed.set()
            self._active = False
            return
        if self._service_control_requests():
            return
        if not self._active:
            return
        try:
            self._core.o1_abandon()
        except BaseException:  # native panic must not escape the optional tap
            log.exception("o1: abandon failed — disabling tap")
            self._active = False
            self._put(("log", time.time(), "writer_error", "abandon failure"))
        self._pending_seq = None

    def close(self) -> None:
        """Flush and stop the writer (idempotent; called at process exit)."""
        if not self._active and not getattr(self, "_writer", None):
            return
        self._active = False
        self._stop.set()
        writer = getattr(self, "_writer", None)
        if writer is not None and writer.is_alive():
            try:
                self._queue.put_nowait(("stop",))
            except queue.Full:
                pass
            writer.join(timeout=5.0)

    # ------------------------------------------------------------------
    # Internals
    # ------------------------------------------------------------------
    def _put(self, entry: tuple) -> None:
        try:
            self._queue.put_nowait(entry)
        except queue.Full:
            self._dropped += 1

    def _on_owner_thread(self) -> bool:
        return threading.get_ident() == self._owner_thread_id

    def _service_control_requests(self) -> bool:
        """Apply writer-thread requests only from the adapter owner thread."""
        if self._kill_requested.is_set():
            self._activate_kill("writer request")
            return True
        if self._writer_failed.is_set():
            self._disable(self._writer_failure_detail)
            return True
        return False

    def _disable(self, why: str) -> None:
        if not self._on_owner_thread():
            self._writer_failure_detail = why
            self._writer_failed.set()
            self._active = False
            return
        try:
            self._core.o1_abandon()
        except BaseException:
            log.exception("o1: failed to clear pending state while disabling")
        self._active = False
        self._pending_seq = None
        try:
            self._queue.put_nowait(("log", time.time(), "writer_error", why))
        except queue.Full:
            pass

    def _activate_kill(self, where: str) -> None:
        if not self._on_owner_thread():
            self._kill_requested.set()
            self._active = False
            return
        log.warning("o1: KILL switch observed (%s) — tap stopped", where)
        try:
            self._core.o1_abandon()
        except BaseException:
            log.exception("o1: failed to clear pending state at KILL boundary")
        self._pending_seq = None
        self._active = False
        try:
            self._queue.put_nowait(("log", time.time(), "kill_switch_on", where))
        except queue.Full:
            pass

    def _writer_run(self) -> None:
        """Single-owner SQLite writer. One durable commit per drained batch."""
        try:
            from phase_a import harness as phase_a_harness

            conn = phase_a_harness.connect(self._db_path)
            conn.executescript(_COLLECTOR_LOG_SCHEMA)
            conn.execute(
                "INSERT INTO run_metadata (run_id, created_ts, synthetic, "
                "freq_table_hash, freq_table_files, p_model, engine_commit, "
                "harness_version, scope, notes) VALUES (?,?,?,?,?,?,?,?,?,?)",
                self._meta_row,
            )
            conn.execute(
                "INSERT INTO collector_log (ts, kind, detail) VALUES (?,?,?)",
                (time.time(), "start", self.run_id),
            )
            conn.commit()
        except Exception:
            log.exception("o1: writer init failed — disabling tap")
            self._writer_failure_detail = "writer init failure"
            self._writer_failed.set()
            self._active = False
            return

        seq_to_rowid: dict[int, int] = {}
        try:
            while True:
                try:
                    entry = self._queue.get(timeout=1.0)
                    batch = [entry]
                except queue.Empty:
                    batch = []
                while True:
                    try:
                        batch.append(self._queue.get_nowait())
                    except queue.Empty:
                        break

                stop_seen = False
                for item in batch:
                    kind = item[0]
                    if kind == "pred":
                        (_, seq, ts, context_hash, n_candidates, p_top3,
                         latency_us, core_latency_us) = item
                        cur = conn.execute(
                            "INSERT INTO events (run_id, ts, context_hash, "
                            "n_candidates, p_top3, latency_us, outcome, class, "
                            "resolver, synthetic) VALUES (?,?,?,?,?,?,NULL,?,?,0)",
                            (self.run_id, ts, context_hash, n_candidates,
                             p_top3, latency_us, EVENT_CLASS, RESOLVER),
                        )
                        seq_to_rowid[seq] = int(cur.lastrowid)
                        conn.execute(
                            "INSERT INTO collector_log (ts, kind, detail) "
                            "VALUES (?,?,?)",
                            (ts, "core_latency", str(core_latency_us)),
                        )
                    elif kind == "res":
                        _, seq, outcome, resolved_ts = item
                        row_id = seq_to_rowid.pop(seq, None)
                        if row_id is not None:
                            conn.execute(
                                "UPDATE events SET outcome=?, resolved_ts=? "
                                "WHERE id=? AND outcome IS NULL",
                                (outcome, resolved_ts, row_id),
                            )
                    elif kind == "log":
                        _, ts, log_kind, detail = item
                        conn.execute(
                            "INSERT INTO collector_log (ts, kind, detail) "
                            "VALUES (?,?,?)",
                            (ts, log_kind, detail),
                        )
                    elif kind == "stop":
                        stop_seen = True
                if self._dropped != self._dropped_logged:
                    conn.execute(
                        "INSERT INTO collector_log (ts, kind, detail) VALUES (?,?,?)",
                        (time.time(), "drop_counter", str(self._dropped)),
                    )
                    self._dropped_logged = self._dropped
                if batch:
                    conn.commit()
                if self._kill_file.exists() and not self._kill_requested.is_set():
                    # PyInputMethodCore is explicitly unsendable. The writer
                    # may request shutdown, but only the adapter thread may
                    # touch the native core and clear its pending snapshot.
                    self._kill_requested.set()
                    self._active = False
                    conn.execute(
                        "INSERT INTO collector_log (ts, kind, detail) VALUES (?,?,?)",
                        (time.time(), "kill_switch_requested", "writer cycle"),
                    )
                    conn.commit()
                if stop_seen or (self._stop.is_set() and self._queue.empty()):
                    conn.execute(
                        "INSERT INTO collector_log (ts, kind, detail) VALUES (?,?,?)",
                        (time.time(), "stop", self.run_id),
                    )
                    conn.commit()
                    conn.close()
                    return
        except Exception:
            log.exception("o1: writer failure — disabling tap (input unaffected)")
            self._writer_failure_detail = "writer thread exception"
            self._writer_failed.set()
            self._active = False
            try:
                conn.execute(
                    "INSERT INTO collector_log (ts, kind, detail) VALUES (?,?,?)",
                    (time.time(), "writer_error", "writer thread exception"),
                )
                conn.commit()
                conn.close()
            except Exception:
                pass
