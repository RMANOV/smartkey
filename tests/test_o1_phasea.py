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
import sqlite3
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


from phase_a import selftest  # noqa: E402  (after fixture so env is ready at call time)
from phase_a.analyze import analyze  # noqa: E402
from phase_a.constants import PREDICTION_SCOPE  # noqa: E402
from phase_a.freqmodel import FreqModel  # noqa: E402
from phase_a.harness import connect  # noqa: E402
from phase_a.paths import alarm_file, lab_corpus_files, receipts_dir  # noqa: E402
from phase_a.sweep import run_sweep, run_watchdog  # noqa: E402


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


def test_analyze_rejects_explicit_unknown_campaign(tmp_path):
    db = tmp_path / "unknown-analysis-campaign.db"
    _add_campaign(db, "real-known", synthetic=False)

    with pytest.raises(ValueError, match="campaign 'typo' not found in real scope"):
        analyze(db, campaign_run_id="typo")
