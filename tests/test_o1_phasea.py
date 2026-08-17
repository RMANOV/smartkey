"""Pytest surface for the O1 / Phase-A n-gram calibration harness.

Runs every machinery self-test check (GREEN->PASS, injected RED->FAIL, each
FAIL-clause, the schema invariant, sweep/watchdog, the scope firewall, corpus
pin, and the live-tree data guard) plus focused unit assertions. All artifacts
go to a per-session tmp dir via SMARTKEY_PHASEA_DATA; the pinned lab_corpus is
read-only and sha256-verified by the harness itself.
"""

from __future__ import annotations

import datetime as dt
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import time

import pytest


@pytest.fixture(scope="session", autouse=True)
def _isolated_data_dir(tmp_path_factory):
    d = tmp_path_factory.mktemp("o1-phasea-data")
    old = os.environ.get("SMARTKEY_PHASEA_DATA")
    os.environ["SMARTKEY_PHASEA_DATA"] = str(d)
    yield
    if old is not None:
        os.environ["SMARTKEY_PHASEA_DATA"] = old
    else:
        os.environ.pop("SMARTKEY_PHASEA_DATA", None)


from phase_a import selftest, sweep as phase_a_sweep  # noqa: E402
from phase_a.analyze import analyze  # noqa: E402
from phase_a.constants import PREDICTION_SCOPE  # noqa: E402
from phase_a.freqmodel import FreqModel  # noqa: E402
from phase_a.harness import connect, validate_phasea_db  # noqa: E402
from phase_a.paths import alarm_file, lab_corpus_files, receipts_dir  # noqa: E402
from phase_a.sweep import run_sweep, run_watchdog  # noqa: E402
from phase_a.validity import select_campaign_rows  # noqa: E402


_REPO = Path(__file__).resolve().parent.parent


def _add_campaign(
    db,
    run_id: str,
    *,
    synthetic: bool,
    ts: float | None = None,
    n_candidates: int = 3,
) -> float:
    event_ts = ts if ts is not None else time.time() - 60.0
    conn = connect(db)
    conn.execute(
        "INSERT INTO run_metadata (run_id, created_ts, synthetic, freq_table_hash, "
        "freq_table_files, p_model, engine_commit, harness_version, scope, notes) "
        "VALUES (?,?,?,?,?,?,?,?,?,?)",
        (
            run_id,
            event_ts,
            1 if synthetic else 0,
            "test-hash",
            "[]",
            "test",
            None,
            "test",
            "ngram_component",
            "watchdog provenance regression",
        ),
    )
    conn.execute(
        "INSERT INTO events (run_id, ts, context_hash, n_candidates, p_top3, "
        "latency_us, outcome, class, resolver, resolved_ts, synthetic) "
        "VALUES (?,?,?,?,?,?,?,?,?,?,?)",
        (
            run_id,
            event_ts,
            "test-context",
            n_candidates,
            0.5,
            100,
            1,
            "machine",
            "script:test",
            event_ts + 0.01,
            1 if synthetic else 0,
        ),
    )
    conn.commit()
    conn.close()
    return event_ts


def _add_scoped_sweep(db, run_id: str, *, synthetic: bool, event_ts: float) -> None:
    conn = connect(db)
    conn.execute(
        "INSERT INTO sweeps (run_id, synthetic, sweep_date, ran_ts, events, "
        "resolutions, latency_p99_us, over_budget_pct, watchdog_status, receipt) "
        "VALUES (?,?,?,?,?,?,?,?,?,?)",
        (
            run_id,
            1 if synthetic else 0,
            dt.date.fromtimestamp(event_ts).isoformat(),
            time.time(),
            1,
            1,
            100,
            0.0,
            "OK",
            "test sweep",
        ),
    )
    conn.commit()
    conn.close()


def _add_empty_campaign(db, run_id: str, *, synthetic: bool, ts: float) -> None:
    """Insert run metadata only, mirroring an engine restart before first input."""
    conn = connect(db)
    conn.execute(
        "INSERT INTO run_metadata (run_id, created_ts, synthetic, freq_table_hash, "
        "freq_table_files, p_model, engine_commit, harness_version, scope, notes) "
        "VALUES (?,?,?,?,?,?,?,?,?,?)",
        (
            run_id,
            ts,
            1 if synthetic else 0,
            "test-hash",
            "[]",
            "test",
            None,
            "test",
            "ngram_component",
            "empty restart campaign",
        ),
    )
    conn.commit()
    conn.close()


@pytest.mark.parametrize(
    "module",
    ("phase_a.analyze", "phase_a.sweep", "phase_a.selftest"),
)
def test_cli_help_never_resolves_or_creates_a_data_directory(module, tmp_path):
    """A help-only invocation must work even in the forbidden live checkout."""
    env = os.environ.copy()
    env.pop("SMARTKEY_PHASEA_DATA", None)
    env.pop("SMARTKEY_PHASEA_DB", None)
    forbidden_default = _REPO / "phase_a_data"
    assert not forbidden_default.exists(), "test requires an untouched live-tree default"

    proc = subprocess.run(
        [sys.executable, "-m", module, "--help"],
        cwd=_REPO,
        env=env,
        text=True,
        capture_output=True,
        timeout=20,
        check=False,
    )

    assert proc.returncode == 0, f"stdout={proc.stdout}\nstderr={proc.stderr}"
    assert "usage:" in proc.stdout.lower()
    assert not forbidden_default.exists(), "--help must perform zero data writes"

    configured_root = tmp_path / "configured-but-unused"
    env["SMARTKEY_PHASEA_DATA"] = str(configured_root)
    proc = subprocess.run(
        [sys.executable, "-m", module, "--help"],
        cwd=_REPO,
        env=env,
        text=True,
        capture_output=True,
        timeout=20,
        check=False,
    )
    assert proc.returncode == 0, f"stdout={proc.stdout}\nstderr={proc.stderr}"
    assert not configured_root.exists(), "--help must not create a configured root"


@pytest.mark.parametrize(
    ("module", "subcommand"),
    (
        ("phase_a.analyze", []),
        ("phase_a.sweep", ["sweep"]),
        ("phase_a.sweep", ["watchdog"]),
    ),
)
def test_cli_rejects_missing_explicit_db_without_creating_it(
    tmp_path, module, subcommand
):
    """Operator typos must never create an empty, falsely-green event DB."""
    missing = tmp_path / f"missing-{module.rsplit('.', 1)[-1]}-{'-'.join(subcommand)}.db"
    proc = subprocess.run(
        [sys.executable, "-m", module, *subcommand, "--db", str(missing)],
        cwd=_REPO,
        text=True,
        capture_output=True,
        timeout=20,
        check=False,
    )

    assert proc.returncode == 2, f"stdout={proc.stdout}\nstderr={proc.stderr}"
    assert "does not exist" in proc.stderr
    assert "Traceback" not in proc.stderr
    assert not missing.exists()


@pytest.mark.parametrize(
    ("module", "subcommand"),
    (
        ("phase_a.analyze", []),
        ("phase_a.sweep", ["sweep"]),
        ("phase_a.sweep", ["watchdog"]),
    ),
)
def test_cli_rejects_invalid_existing_db_without_traceback(
    tmp_path, module, subcommand
):
    invalid = tmp_path / "invalid.db"
    invalid.write_text("not a sqlite database", encoding="utf-8")
    env = os.environ.copy()
    env["SMARTKEY_PHASEA_DATA"] = str(tmp_path / "artifacts")
    proc = subprocess.run(
        [sys.executable, "-m", module, *subcommand, "--db", str(invalid)],
        cwd=_REPO,
        env=env,
        text=True,
        capture_output=True,
        timeout=20,
        check=False,
    )

    assert proc.returncode == 2, f"stdout={proc.stdout}\nstderr={proc.stderr}"
    assert "error:" in proc.stderr
    assert "Traceback" not in proc.stderr


@pytest.mark.parametrize(
    ("module", "subcommand"),
    (
        ("phase_a.analyze", []),
        ("phase_a.sweep", ["sweep"]),
        ("phase_a.sweep", ["watchdog"]),
    ),
)
def test_cli_rejects_valid_unrelated_sqlite_without_mutating_it(
    tmp_path, module, subcommand
):
    unrelated = tmp_path / "unrelated.db"
    conn = sqlite3.connect(unrelated)
    conn.execute("CREATE TABLE operator_data (value TEXT)")
    conn.execute("INSERT INTO operator_data VALUES ('preserve-me')")
    conn.commit()
    conn.close()
    env = os.environ.copy()
    env["SMARTKEY_PHASEA_DATA"] = str(tmp_path / "artifacts")

    proc = subprocess.run(
        [sys.executable, "-m", module, *subcommand, "--db", str(unrelated)],
        cwd=_REPO,
        env=env,
        text=True,
        capture_output=True,
        timeout=20,
        check=False,
    )

    assert proc.returncode == 2, f"stdout={proc.stdout}\nstderr={proc.stderr}"
    assert "not a Phase-A database" in proc.stderr
    assert "Traceback" not in proc.stderr
    conn = sqlite3.connect(f"file:{unrelated}?mode=ro", uri=True)
    try:
        tables = {
            row[0]
            for row in conn.execute(
                "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name"
            )
        }
        value = conn.execute("SELECT value FROM operator_data").fetchone()[0]
    finally:
        conn.close()
    assert tables == {"operator_data"}
    assert value == "preserve-me"
    assert not Path(f"{unrelated}-wal").exists()
    assert not Path(f"{unrelated}-shm").exists()


@pytest.mark.parametrize(
    ("module", "subcommand"),
    (
        ("phase_a.analyze", []),
        ("phase_a.sweep", ["sweep"]),
        ("phase_a.sweep", ["watchdog"]),
    ),
)
def test_cli_rejects_near_match_schema_before_any_mutation(
    tmp_path, module, subcommand
):
    db = tmp_path / "near-match.db"
    conn = sqlite3.connect(db)
    conn.executescript(
        """
        CREATE TABLE run_metadata (
            run_id TEXT, created_ts REAL, synthetic INTEGER
        );
        CREATE TABLE events (
            run_id TEXT, ts REAL, n_candidates INTEGER, p_top3 REAL,
            latency_us INTEGER, outcome INTEGER, synthetic INTEGER
        );
        CREATE TABLE sweeps (
            sweep_date TEXT, ran_ts REAL, events INTEGER, resolutions INTEGER,
            watchdog_status TEXT, receipt TEXT
        );
        """
    )
    conn.commit()
    conn.close()
    before_bytes = db.read_bytes()
    before_mode = db.stat().st_mode & 0o777
    env = os.environ.copy()
    env["SMARTKEY_PHASEA_DATA"] = str(tmp_path / "artifacts")

    proc = subprocess.run(
        [sys.executable, "-m", module, *subcommand, "--db", str(db)],
        cwd=_REPO,
        env=env,
        text=True,
        capture_output=True,
        timeout=20,
        check=False,
    )

    assert proc.returncode == 2, f"stdout={proc.stdout}\nstderr={proc.stderr}"
    assert "not a Phase-A database" in proc.stderr
    assert "Traceback" not in proc.stderr
    assert db.read_bytes() == before_bytes
    assert db.stat().st_mode & 0o777 == before_mode
    assert not Path(f"{db}-wal").exists()
    assert not Path(f"{db}-shm").exists()


def test_selftest_path_oserror_is_a_clean_cli_error(monkeypatch):
    def _raise_oserror():
        raise OSError("synthetic permission failure")

    monkeypatch.setattr(selftest, "data_dir", _raise_oserror)
    with pytest.raises(SystemExit) as exc:
        selftest.main(["--data-dir", "/tmp/phasea-unreachable-test"])
    assert exc.value.code == 2


def test_selftest_cleanup_failure_preserves_the_root_error(tmp_path, monkeypatch):
    class _FailingScratch:
        name = str(tmp_path / "scratch")

        def cleanup(self):
            raise OSError("synthetic cleanup failure")

    def _raise_root_error():
        raise RuntimeError("synthetic selftest root failure")

    monkeypatch.delenv("SMARTKEY_PHASEA_DATA", raising=False)
    monkeypatch.setattr(
        selftest.tempfile,
        "TemporaryDirectory",
        lambda **_kwargs: _FailingScratch(),
    )
    monkeypatch.setattr(selftest, "_run_selftest", _raise_root_error)

    with pytest.raises(RuntimeError, match="synthetic selftest root failure"):
        selftest.main([])
    assert "SMARTKEY_PHASEA_DATA" not in os.environ


def test_phasea_connect_repairs_private_permissions(tmp_path):
    root = tmp_path / "private-data"
    db = root / "events.db"
    conn = connect(db)
    conn.close()

    assert root.stat().st_mode & 0o777 == 0o700
    assert db.stat().st_mode & 0o777 == 0o600


def test_phasea_connect_does_not_chmod_an_existing_parent(tmp_path):
    shared_parent = tmp_path / "operator-owned-parent"
    shared_parent.mkdir()
    shared_parent.chmod(0o755)

    db = shared_parent / "events.db"
    conn = connect(db)
    conn.close()

    assert shared_parent.stat().st_mode & 0o777 == 0o755
    assert db.stat().st_mode & 0o777 == 0o600


def test_validate_accepts_the_core_phasea_schema_without_collector_log(tmp_path):
    db = tmp_path / "core-phasea.db"
    conn = connect(db)
    conn.close()

    validate_phasea_db(db)


def test_atomic_receipt_cleanup_failure_preserves_the_write_error(
    tmp_path, monkeypatch
):
    opened_fds = []
    real_open = phase_a_sweep.os.open
    real_close = phase_a_sweep.os.close

    def _tracked_open(*args, **kwargs):
        fd = real_open(*args, **kwargs)
        opened_fds.append(fd)
        return fd

    def _fail_fdopen(*_args, **_kwargs):
        raise OSError("synthetic receipt write failure")

    def _fail_close(_fd):
        raise OSError("synthetic cleanup close failure")

    try:
        with monkeypatch.context() as patch:
            patch.setattr(phase_a_sweep.os, "open", _tracked_open)
            patch.setattr(phase_a_sweep.os, "fdopen", _fail_fdopen)
            patch.setattr(phase_a_sweep.os, "close", _fail_close)
            with pytest.raises(OSError, match="synthetic receipt write failure"):
                phase_a_sweep._write_receipt_atomic(
                    tmp_path / "receipt.txt", "receipt"
                )
    finally:
        for fd in opened_fds:
            try:
                real_close(fd)
            except OSError:
                pass


def test_atomic_receipt_handle_close_does_not_mask_write_error(
    tmp_path, monkeypatch
):
    opened_fds = []
    real_open = phase_a_sweep.os.open
    real_close = phase_a_sweep.os.close

    def _tracked_open(*args, **kwargs):
        fd = real_open(*args, **kwargs)
        opened_fds.append(fd)
        return fd

    class _WriteAndCloseFailingHandle:
        def __enter__(self):
            return self

        def __exit__(self, *_exc):
            self.close()

        def write(self, _receipt):
            raise OSError("synthetic handle write failure")

        def close(self):
            raise OSError("synthetic handle close failure")

    try:
        with monkeypatch.context() as patch:
            patch.setattr(phase_a_sweep.os, "open", _tracked_open)
            patch.setattr(
                phase_a_sweep.os,
                "fdopen",
                lambda *_args, **_kwargs: _WriteAndCloseFailingHandle(),
            )
            with pytest.raises(OSError, match="synthetic handle write failure"):
                phase_a_sweep._write_receipt_atomic(
                    tmp_path / "receipt.txt", "receipt"
                )
    finally:
        for fd in opened_fds:
            try:
                real_close(fd)
            except OSError:
                pass


def test_atomic_receipt_directory_close_does_not_mask_fsync_error(
    tmp_path, monkeypatch
):
    fsync_calls = 0
    directory_fds = []
    real_open = phase_a_sweep.os.open
    real_close = phase_a_sweep.os.close
    real_fsync = phase_a_sweep.os.fsync

    def _tracked_open(*args, **kwargs):
        fd = real_open(*args, **kwargs)
        if len(args) >= 2 and args[1] & phase_a_sweep.os.O_DIRECTORY:
            directory_fds.append(fd)
        return fd

    def _fail_directory_fsync(fd):
        nonlocal fsync_calls
        fsync_calls += 1
        if fsync_calls == 2:
            raise OSError("synthetic directory fsync failure")
        return real_fsync(fd)

    def _fail_directory_close(fd):
        if fd in directory_fds:
            raise OSError("synthetic directory close failure")
        return real_close(fd)

    try:
        with monkeypatch.context() as patch:
            patch.setattr(phase_a_sweep.os, "open", _tracked_open)
            patch.setattr(phase_a_sweep.os, "fsync", _fail_directory_fsync)
            patch.setattr(phase_a_sweep.os, "close", _fail_directory_close)
            with pytest.raises(OSError, match="synthetic directory fsync failure"):
                phase_a_sweep._write_receipt_atomic(
                    tmp_path / "receipt.txt", "receipt"
                )
    finally:
        for fd in directory_fds:
            try:
                real_close(fd)
            except OSError:
                pass


def _assert_campaign_uncovered(db, run_id: str) -> None:
    af = alarm_file(run_id, synthetic=False)
    af.unlink(missing_ok=True)
    result = analyze(db, campaign_run_id=run_id)
    assert result.missed_watchdog == 1
    assert not result.alarm_raised
    try:
        assert run_watchdog(db, campaign_run_id=run_id) == 1
    finally:
        af.unlink(missing_ok=True)


@pytest.mark.parametrize("name,fn", selftest.CHECKS, ids=[c[0] for c in selftest.CHECKS])
def test_selftest_check(name, fn):
    ok, detail = fn()
    assert ok, f"{name}: {detail}"


def test_prediction_scope_is_ngram_component():
    assert PREDICTION_SCOPE == "ngram_component"


def test_freqmodel_top3_is_pure_ngram_ranking():
    """top3(ctx) must be the raw-count-ranked followers — no ensemble anywhere."""
    model = FreqModel.load(lab_corpus_files("json"))
    # find a context with >=4 followers so ranking is observable
    ctx = next(c for c, slot in model.bigrams.items() if len(slot) >= 4)
    top3 = model.top3(ctx)
    followers = model.followers(ctx)
    expected = [w for w, _ in sorted(followers.items(), key=lambda kv: (-kv[1], kv[0]))[:3]]
    assert top3 == expected
    # p_top3 equals the summed conditional mass of exactly those three
    total = model.bigram_totals[ctx]
    manual = sum(followers[w] for w in top3) / total
    assert abs(model.p_top3(ctx, top3) - manual) < 1e-9


def test_no_ensemble_or_pyo3_in_measured_path():
    ok, detail = selftest.check_scope_firewall_static()
    assert ok, detail


def test_corpus_pins_match_expected():
    ok, detail = selftest.check_corpus_pin()
    assert ok, detail


def test_synthetic_sweep_cannot_cover_real_campaign(tmp_path):
    db = tmp_path / "synthetic-cannot-cover.db"
    real_ts = _add_campaign(db, "real-selected", synthetic=False)
    _add_scoped_sweep(
        db,
        "synthetic-foreign",
        synthetic=True,
        event_ts=real_ts,
    )

    _assert_campaign_uncovered(db, "real-selected")


def test_foreign_real_sweep_cannot_cover_selected_run_id(tmp_path):
    db = tmp_path / "foreign-real-cannot-cover.db"
    base_ts = time.time() - 60.0
    selected_ts = _add_campaign(
        db, "real-selected", synthetic=False, ts=base_ts
    )
    foreign_ts = _add_campaign(
        db, "real-foreign", synthetic=False, ts=base_ts
    )
    _add_scoped_sweep(
        db,
        "real-foreign",
        synthetic=False,
        event_ts=foreign_ts,
    )

    _assert_campaign_uncovered(db, "real-selected")
    assert dt.date.fromtimestamp(selected_ts) == dt.date.fromtimestamp(foreign_ts)


def test_legacy_unscoped_sweep_is_migrated_but_not_credited(tmp_path):
    db = tmp_path / "legacy-unscoped.db"
    event_ts = time.time() - 60.0
    legacy = sqlite3.connect(db)
    legacy.execute(
        "CREATE TABLE sweeps ("
        "id INTEGER PRIMARY KEY, sweep_date TEXT NOT NULL, ran_ts REAL NOT NULL, "
        "events INTEGER NOT NULL, resolutions INTEGER NOT NULL, latency_p99_us INTEGER, "
        "over_budget_pct REAL, watchdog_status TEXT NOT NULL, receipt TEXT NOT NULL)"
    )
    legacy.execute(
        "INSERT INTO sweeps (sweep_date, ran_ts, events, resolutions, latency_p99_us, "
        "over_budget_pct, watchdog_status, receipt) VALUES (?,?,?,?,?,?,?,?)",
        (
            dt.date.fromtimestamp(event_ts).isoformat(),
            time.time(),
            1,
            1,
            100,
            0.0,
            "OK",
            "legacy sweep",
        ),
    )
    legacy.commit()
    legacy.close()

    _add_campaign(db, "real-selected", synthetic=False, ts=event_ts)
    for _ in range(2):
        migrated = connect(db)
        migrated.close()

    conn = connect(db)
    columns = [row[1] for row in conn.execute("PRAGMA table_info(sweeps)")]
    legacy_row = conn.execute(
        "SELECT run_id, synthetic FROM sweeps WHERE receipt='legacy sweep'"
    ).fetchone()
    trigger_count = conn.execute(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='trigger' "
        "AND name='sweeps_provenance_required_insert'"
    ).fetchone()[0]
    assert columns.count("run_id") == 1
    assert columns.count("synthetic") == 1
    assert tuple(legacy_row) == (None, None)
    assert trigger_count == 1
    with pytest.raises(sqlite3.IntegrityError, match="sweeps provenance required"):
        conn.execute(
            "INSERT INTO sweeps (sweep_date, ran_ts, events, resolutions, "
            "watchdog_status, receipt) VALUES (?,?,?,?,?,?)",
            ("2026-07-19", time.time(), 0, 0, "OK", "missing provenance"),
        )
    conn.close()

    _assert_campaign_uncovered(db, "real-selected")


def test_watchdog_on_unmigrated_legacy_sweeps_alarms_read_only(tmp_path):
    db = tmp_path / "legacy-watchdog-readonly.db"
    _add_campaign(db, "real-selected", synthetic=False)

    legacy = sqlite3.connect(db)
    legacy.execute("DROP TABLE sweeps")
    legacy.execute(
        "CREATE TABLE sweeps ("
        "id INTEGER PRIMARY KEY, sweep_date TEXT NOT NULL, ran_ts REAL NOT NULL, "
        "events INTEGER NOT NULL, resolutions INTEGER NOT NULL, latency_p99_us INTEGER, "
        "over_budget_pct REAL, watchdog_status TEXT NOT NULL, receipt TEXT NOT NULL)"
    )
    legacy.execute(
        "INSERT INTO sweeps (sweep_date, ran_ts, events, resolutions, latency_p99_us, "
        "over_budget_pct, watchdog_status, receipt) VALUES (?,?,?,?,?,?,?,?)",
        ("2026-08-16", time.time(), 1, 1, 100, 0.0, "OK", "legacy global"),
    )
    legacy.commit()
    legacy.close()

    assert run_watchdog(db, campaign_run_id="real-selected") == 1
    assert alarm_file("real-selected", synthetic=False).is_file()
    readonly = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    columns = {row[1] for row in readonly.execute("PRAGMA table_info(sweeps)")}
    readonly.close()
    assert "run_id" not in columns
    assert "synthetic" not in columns


def test_matching_campaign_sweep_covers_analysis_and_watchdog(tmp_path):
    db = tmp_path / "matching-sweep.db"
    event_ts = _add_campaign(db, "real-selected", synthetic=False)
    _add_scoped_sweep(
        db,
        "real-selected",
        synthetic=False,
        event_ts=event_ts,
    )

    result = analyze(db, campaign_run_id="real-selected")
    assert result.missed_watchdog == 0
    assert run_watchdog(db, campaign_run_id="real-selected") == 0


def test_run_sweep_stamps_and_counts_only_selected_campaign(tmp_path):
    db = tmp_path / "run-sweep-provenance.db"
    _add_campaign(db, "real-selected", synthetic=False)
    _add_campaign(db, "real-foreign", synthetic=False)

    receipt = run_sweep(db, campaign_run_id="real-selected")

    conn = connect(db, readonly=True)
    row = conn.execute(
        "SELECT run_id, synthetic, events, resolutions FROM sweeps "
        "ORDER BY id DESC LIMIT 1"
    ).fetchone()
    conn.close()
    assert tuple(row) == ("real-selected", 0, 1, 1)
    assert "campaign_run_id: real-selected" in receipt


def test_run_sweep_preserves_receipts_for_multiple_campaigns(tmp_path):
    db = tmp_path / "campaign-receipts.db"
    base_ts = time.time() - 60.0
    _add_campaign(db, "real-first", synthetic=False, ts=base_ts)
    _add_campaign(db, "real-second", synthetic=False, ts=base_ts)
    before = set(receipts_dir().glob("sweep-*-real-*.txt"))

    run_sweep(db, campaign_run_id="real-first")
    run_sweep(db, campaign_run_id="real-second")

    created = set(receipts_dir().glob("sweep-*-real-*.txt")) - before
    assert len(created) == 2
    contents = {path.read_text(encoding="utf-8") for path in created}
    assert any("campaign_run_id: real-first" in text for text in contents)
    assert any("campaign_run_id: real-second" in text for text in contents)


def test_watchdog_counts_selected_raw_events_even_when_analysis_excludes_them(
    tmp_path,
):
    db = tmp_path / "raw-event-presence.db"
    _add_campaign(
        db,
        "real-excluded",
        synthetic=False,
        n_candidates=0,
    )
    af = alarm_file("real-excluded", synthetic=False)

    try:
        assert run_watchdog(db, campaign_run_id="real-excluded") == 1
        assert af.exists()
    finally:
        af.unlink(missing_ok=True)


def test_watchdog_rejects_explicit_unknown_campaign(tmp_path):
    db = tmp_path / "unknown-campaign.db"
    _add_campaign(db, "real-known", synthetic=False)

    with pytest.raises(ValueError, match="campaign 'typo' not found in real scope"):
        run_watchdog(db, campaign_run_id="typo")


def test_default_watchdog_selects_latest_campaign_with_events(tmp_path):
    """A fresh empty restart must not hide an older unswept live campaign."""
    db = tmp_path / "latest-nonempty-watchdog.db"
    event_ts = _add_campaign(db, "real-with-events", synthetic=False)
    _add_empty_campaign(
        db,
        "newest-empty-restart",
        synthetic=False,
        ts=event_ts + 30.0,
    )
    af = alarm_file("real-with-events", synthetic=False)
    af.unlink(missing_ok=True)

    try:
        assert run_watchdog(db) == 1
        assert af.exists(), "the unswept non-empty campaign must raise its alarm"
    finally:
        af.unlink(missing_ok=True)


def test_default_selector_skips_newest_metadata_only_campaign(tmp_path):
    db = tmp_path / "latest-nonempty-selector.db"
    event_ts = _add_campaign(db, "real-with-events", synthetic=False)
    _add_empty_campaign(
        db,
        "newest-empty-restart",
        synthetic=False,
        ts=event_ts + 30.0,
    )
    conn = connect(db)
    try:
        selection = select_campaign_rows(conn, synthetic=False)
    finally:
        conn.close()

    assert selection.campaign_run_id == "real-with-events"
    assert len(selection.raw_rows) == 1


def test_watchdog_does_not_materialize_full_campaign_rows(tmp_path, monkeypatch):
    db = tmp_path / "lightweight-watchdog.db"
    event_ts = _add_campaign(db, "real-selected", synthetic=False)
    _add_scoped_sweep(db, "real-selected", synthetic=False, event_ts=event_ts)

    def _forbidden_full_selector(*_args, **_kwargs):
        raise AssertionError("watchdog must not materialize historical event rows")

    monkeypatch.setattr(phase_a_sweep, "select_campaign_rows", _forbidden_full_selector)
    assert phase_a_sweep.run_watchdog(db) == 0


def test_sweep_receipt_failure_does_not_credit_or_clear_alarm(tmp_path, monkeypatch):
    db = tmp_path / "receipt-failure.db"
    _add_campaign(db, "real-receipt-failure", synthetic=False)
    af = alarm_file("real-receipt-failure", synthetic=False)
    af.write_text("existing alarm\n", encoding="utf-8")

    def _fail_receipt(*_args, **_kwargs):
        raise OSError("synthetic receipt failure")

    monkeypatch.setattr(phase_a_sweep, "_write_receipt_atomic", _fail_receipt)
    try:
        with pytest.raises(OSError, match="synthetic receipt failure"):
            phase_a_sweep.run_sweep(db, campaign_run_id="real-receipt-failure")
        conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
        try:
            credited = conn.execute(
                "SELECT COUNT(*) FROM sweeps WHERE run_id=?",
                ("real-receipt-failure",),
            ).fetchone()[0]
        finally:
            conn.close()
        assert credited == 0
        assert af.read_text(encoding="utf-8") == "existing alarm\n"
    finally:
        af.unlink(missing_ok=True)


def test_sweep_cleanup_failure_preserves_the_receipt_error(tmp_path, monkeypatch):
    db = tmp_path / "cleanup-failure.db"
    _add_campaign(db, "real-cleanup-failure", synthetic=False)
    real_connect = phase_a_sweep.connect
    opened = []

    class _CleanupFailingConnection:
        def __init__(self, inner):
            self.inner = inner

        @property
        def row_factory(self):
            return self.inner.row_factory

        @row_factory.setter
        def row_factory(self, value):
            self.inner.row_factory = value

        def rollback(self):
            raise OSError("synthetic rollback cleanup failure")

        def close(self):
            raise OSError("synthetic close cleanup failure")

        def __getattr__(self, name):
            return getattr(self.inner, name)

    def _connect_with_failing_cleanup(*args, **kwargs):
        inner = real_connect(*args, **kwargs)
        opened.append(inner)
        return _CleanupFailingConnection(inner)

    def _fail_receipt(*_args, **_kwargs):
        raise OSError("synthetic receipt root failure")

    monkeypatch.setattr(phase_a_sweep, "connect", _connect_with_failing_cleanup)
    monkeypatch.setattr(phase_a_sweep, "_write_receipt_atomic", _fail_receipt)
    try:
        with pytest.raises(OSError, match="synthetic receipt root failure"):
            phase_a_sweep.run_sweep(
                db, campaign_run_id="real-cleanup-failure"
            )
    finally:
        for conn in opened:
            try:
                conn.rollback()
            finally:
                conn.close()


def test_analyze_rejects_explicit_unknown_campaign(tmp_path):
    db = tmp_path / "unknown-analysis-campaign.db"
    _add_campaign(db, "real-known", synthetic=False)

    with pytest.raises(ValueError, match="campaign 'typo' not found in real scope"):
        analyze(db, campaign_run_id="typo")
