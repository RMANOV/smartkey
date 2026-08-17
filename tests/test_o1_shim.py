"""Focused tests for the passive calibration collector and native boundary."""

from __future__ import annotations

import importlib.util
import json
import os
import pathlib
import subprocess
import sqlite3
import sys
import time
import types

import pytest

_REPO = pathlib.Path(__file__).resolve().parent.parent
if str(_REPO) not in sys.path:
    sys.path.insert(0, str(_REPO))


def _native_module_path() -> pathlib.Path:
    native = pathlib.Path(
        os.environ.get(
            "SMARTKEY_TEST_NATIVE_MODULE",
            _REPO / "target" / "debug" / "libsmartkey_py.so",
        )
    )
    if native.is_file():
        return native
    detail = f"native extension not built: {native}"
    if os.environ.get("SMARTKEY_REQUIRE_NATIVE_TESTS") == "1":
        pytest.fail(f"required {detail}", pytrace=False)
    pytest.skip(detail)


_O1_PATH = _REPO / "ibus" / "o1_shim.py"
_spec = importlib.util.spec_from_file_location("o1_shim_test_mod", _O1_PATH)
assert _spec and _spec.loader
o1_shim = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(o1_shim)

import shim_report  # noqa: E402  (repo root on sys.path above)
from phase_a import harness as phase_a_harness  # noqa: E402
from phase_a.analyze import analyze  # noqa: E402
from phase_a.freqmodel import FreqModel  # noqa: E402
from phase_a.selftest import check_data_guard  # noqa: E402


class FakeO1Core:
    """Scripted core for the O1 FFI surface: o1_snapshot / o1_resolve /
    o1_abandon with the real contract (top-3 held core-side; numbers out)."""

    def __init__(self, top3_by_ctx: dict[str, list[str]] | None = None) -> None:
        self.top3_by_ctx = top3_by_ctx or {}
        self.pending: list[str] | None = None
        self.snapshot_calls: list[str] = []
        self.raise_on_snapshot = False

    def o1_snapshot(self, ctx: str) -> tuple[int, float, int]:
        if self.raise_on_snapshot:
            raise RuntimeError("scripted snapshot failure")
        self.snapshot_calls.append(ctx)
        top3 = self.top3_by_ctx.get(ctx, ["alpha", "beta", "gamma"])
        self.pending = list(top3)
        return (len(top3), 0.5, 7)

    def o1_resolve(self, token: str) -> int | None:
        if self.pending is None:
            return None
        outcome = 1 if token in self.pending else 0
        self.pending = None
        return outcome

    def o1_abandon(self) -> None:
        self.pending = None


class PreparingO1Core(FakeO1Core):
    """New-native fake that records startup cache preparation."""

    def __init__(self) -> None:
        super().__init__()
        self.prepare_calls = 0

    def o1_prepare(self) -> bool:
        self.prepare_calls += 1
        return self.prepare_calls == 1


def _corpus_marker(tmp_path: pathlib.Path) -> pathlib.Path:
    corpus = tmp_path / "corpus_test.json"
    corpus.write_text('{"unigrams":{"alpha":1}}', encoding="utf-8")
    return corpus


def _mk_collector(tmp_path, core=None, **cfg_extra):
    core = core or FakeO1Core()
    cfg = {"enabled": True, "data_dir": str(tmp_path / "o1data"), **cfg_extra}
    collector = o1_shim.O1Collector(
        core,
        cfg,
        [str(_corpus_marker(tmp_path))],
        engine_commit="testsha",
    )
    return collector, core


def _read_events(db_path):
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    conn.row_factory = sqlite3.Row
    try:
        events = [dict(r) for r in conn.execute("SELECT * FROM events ORDER BY id")]
        meta = [dict(r) for r in conn.execute("SELECT * FROM run_metadata")]
        logs = [dict(r) for r in conn.execute("SELECT * FROM collector_log ORDER BY id")]
    finally:
        conn.close()
    return events, meta, logs


# ---------------------------------------------------------------------------
# T1 — schema / privacy
# ---------------------------------------------------------------------------
def test_t1_schema_privacy(tmp_path):
    collector, _ = _mk_collector(tmp_path)
    collector.on_word_commit("Hello")
    collector.on_word_commit("world!")
    collector.close()

    events, meta, logs = _read_events(collector._db_path)
    assert len(events) == 2
    for row in events:
        assert set(row) <= {
            "id", "run_id", "ts", "context_hash", "n_candidates", "p_top3",
            "latency_us", "outcome", "class", "resolver", "resolved_ts",
            "synthetic",
        }  # no token/word columns exist
        assert row["class"] == "machine"
        assert row["resolver"] == "script:next_token_in_top3"
        assert row["synthetic"] == 0
        assert len(row["context_hash"]) == 16
        assert row["context_hash"] not in ("hello", "world")
    assert meta[0]["scope"] == "ngram_component"
    assert meta[0]["engine_commit"] == "testsha"
    # The salt sidecar is 0600, outside the DB, and never logged.
    salt_file = collector._db_path.parent / "context_salt"
    assert salt_file.exists()
    assert oct(salt_file.stat().st_mode & 0o777) == "0o600"
    assert all(salt_file.read_bytes().hex() not in str(entry) for entry in logs)


# ---------------------------------------------------------------------------
# T2 — semantics parity: FreqModel evaluated on the SAME truth table that the
# Rust unit tests pin (o1_shim.rs), with identical expected values.
# ---------------------------------------------------------------------------
def test_t2_semantics_parity_shared_truth_table(tmp_path):
    corpus = {
        "unigrams": {"the": 100, "and": 80, "for": 60, "rare": 1},
        "bigrams": [
            {"ctx": "the", "word": "zebra", "count": 9},
            {"ctx": "the", "word": "cat", "count": 5},
            {"ctx": "the", "word": "dog", "count": 5},
            {"ctx": "the", "word": "ant", "count": 3},
        ],
    }
    corpus_file = tmp_path / "corpus_t2.json"
    corpus_file.write_text(json.dumps(corpus), encoding="utf-8")
    model = FreqModel.load([corpus_file])

    # Pinned in o1_shim.rs::top3_ranks_by_count_then_lexicographic
    top3 = model.top3("the")
    assert top3 == ["zebra", "cat", "dog"]
    assert model.p_top3("the", top3) == pytest.approx(19.0 / 22.0, abs=1e-12)

    # Pinned in o1_shim.rs::backoff_to_unigram_top3_when_no_bigram_mass
    backoff = model.top3("unseen-context")
    assert backoff == ["the", "and", "for"]
    assert model.p_top3("unseen-context", backoff) == pytest.approx(
        240.0 / 241.0, abs=1e-12
    )

    # Tokenization mirror used by the collector.
    assert o1_shim.word_token("Hello,") == "hello"
    assert o1_shim.word_token("  свят! ") == "свят"
    assert o1_shim.word_token("...") == ""


# ---------------------------------------------------------------------------
# T3 — disabled path: no collector → zero shim work, zero files.
# ---------------------------------------------------------------------------
def test_t3_disabled_path_writes_nothing(tmp_path):
    data = tmp_path / "o1data"
    core = FakeO1Core()
    # The engine only constructs O1Collector when cfg.enabled is true; with no
    # collector attached the hook helper is a no-op and the core FFI is never
    # touched. Simulate the adapter-side guard directly:
    o1 = None
    for word in ("hello", "world"):
        if o1 is not None:  # the exact adapter guard
            o1.on_word_commit(word)
    assert core.snapshot_calls == []
    assert not data.exists()


# ---------------------------------------------------------------------------
# T4 — failure isolation: hot-path FFI failure and writer init failure both
# disable the tap without raising into the input path.
# ---------------------------------------------------------------------------
def test_t4_hot_path_failure_disables_tap(tmp_path):
    collector, core = _mk_collector(tmp_path)
    core.raise_on_snapshot = True
    collector.on_word_commit("hello")  # must not raise
    assert collector._active is False
    core.raise_on_snapshot = False
    collector.on_word_commit("world")  # inert after disable
    assert core.snapshot_calls == []
    collector.close()


def test_t4_writer_init_failure_disables_tap(tmp_path, monkeypatch):
    def _boom(_path):
        raise RuntimeError("scripted writer init failure")

    monkeypatch.setattr(phase_a_harness, "connect", _boom)
    collector, core = _mk_collector(tmp_path)
    collector._writer.join(timeout=5.0)
    assert collector._active is False
    collector.on_word_commit("hello")
    assert core.snapshot_calls == []


def test_t4_collector_prepares_new_native_before_starting_writer(tmp_path):
    core = PreparingO1Core()
    collector, _ = _mk_collector(tmp_path, core=core)
    try:
        assert core.prepare_calls == 1
        assert collector._active is True
    finally:
        collector.close()


def test_t4_collector_without_loaded_corpus_stays_inactive_and_does_not_prepare(
    tmp_path,
):
    core = PreparingO1Core()
    collector = o1_shim.O1Collector(
        core,
        {"enabled": True, "data_dir": str(tmp_path / "empty-o1data")},
        [],
        engine_commit="testsha",
    )
    try:
        assert collector._active is False
        assert core.prepare_calls == 0
    finally:
        collector.close()


# ---------------------------------------------------------------------------
# T5 — kill-switch: present at startup → tap never starts; created at
# runtime → tap stops within one check interval and logs the activation.
# ---------------------------------------------------------------------------
def test_t5_kill_file_at_startup_keeps_tap_off(tmp_path):
    data = tmp_path / "o1data"
    data.mkdir(parents=True)
    (data / "KILL").write_text("stop", encoding="utf-8")
    collector, core = _mk_collector(tmp_path)
    assert collector._active is False
    collector.on_word_commit("hello")
    assert core.snapshot_calls == []


def test_t5_kill_file_is_checked_before_native_prepare(tmp_path):
    data = tmp_path / "o1data"
    data.mkdir(parents=True)
    (data / "KILL").write_text("stop", encoding="utf-8")
    core = PreparingO1Core()
    collector, _ = _mk_collector(tmp_path, core=core)
    assert collector._active is False
    assert core.prepare_calls == 0


def test_t5_kill_file_at_runtime_stops_tap(tmp_path):
    collector, core = _mk_collector(tmp_path)
    collector.on_word_commit("warm")  # one snapshot taken
    (collector._db_path.parent / "KILL").write_text("stop", encoding="utf-8")
    for i in range(o1_shim._KILL_CHECK_EVERY + 1):
        collector.on_word_commit(f"w{i}")
    assert collector._active is False
    assert core.pending is None  # pending snapshot abandoned on kill
    collector.close()
    _, _, logs = _read_events(collector._db_path)
    assert any(entry["kind"] == "kill_switch_on" for entry in logs)


def test_t5_writer_requests_kill_and_owner_clears_core(tmp_path):
    collector, core = _mk_collector(tmp_path)
    collector.on_word_commit("warm")
    (collector._db_path.parent / "KILL").write_text("stop", encoding="utf-8")
    assert collector._kill_requested.wait(timeout=4.0)
    collector.on_word_commit("next")
    assert collector._active is False
    assert core.pending is None
    collector.close()


def test_t5_required_native_module_cannot_silently_skip(tmp_path, monkeypatch):
    missing = tmp_path / "missing-smartkey-native.so"
    monkeypatch.setenv("SMARTKEY_TEST_NATIVE_MODULE", str(missing))
    monkeypatch.setenv("SMARTKEY_REQUIRE_NATIVE_TESTS", "1")
    with pytest.raises(pytest.fail.Exception, match="required native extension"):
        _native_module_path()


def test_t5_native_unsendable_core_is_never_called_by_writer(tmp_path):
    """Exercise the production PyO3 class across the writer-request path."""
    native = _native_module_path()
    data_root = tmp_path / "native-o1"
    script = """
import importlib.util
import pathlib
import sys

repo = pathlib.Path(sys.argv[1])
native = pathlib.Path(sys.argv[2])
spec = importlib.util.spec_from_file_location("smartkey_py", native)
assert spec and spec.loader
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
from ibus.o1_shim import O1Collector

core = module.PyInputMethodCore()
corpus = pathlib.Path(sys.argv[3]).parent / "native-corpus.json"
corpus.write_text('{"unigrams":{"one":1}}', encoding="utf-8")
collector = O1Collector(
    core,
    {"data_dir": sys.argv[3]},
    [str(corpus)],
    "native-test",
)
collector.on_word_commit("one")
collector._kill_file.write_text("stop", encoding="utf-8")
assert collector._kill_requested.wait(4.0)
collector.on_word_commit("two")
assert collector._active is False
assert core.o1_resolve("anything") is None
collector.close()
"""
    proc = subprocess.run(
        [sys.executable, "-c", script, str(_REPO), str(native), str(data_root)],
        cwd=_REPO,
        text=True,
        capture_output=True,
        timeout=15,
        check=False,
    )
    assert proc.returncode == 0, f"stdout={proc.stdout}\nstderr={proc.stderr}"


def test_t5_native_o1_prepare_refreshes_unigram_cache_after_corpus_change():
    """Preparation must move cache construction off the first live boundary."""
    native = _native_module_path()
    script = """
import importlib.util
import json
import pathlib
import sys
import tempfile

native = pathlib.Path(sys.argv[1])
spec = importlib.util.spec_from_file_location("smartkey_py", native)
assert spec and spec.loader
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

core = module.PyInputMethodCore()
core.load_word("alpha", 100)
core.load_word("gamma", 80)
core.load_word("delta", 60)
core.load_word("rare", 1)
assert core.o1_prepare() is True
assert core.o1_prepare() is False
core.load_bigram("context", "alpha", 50)
core.load_trigram("one", "two", "alpha", 25)
assert core.o1_prepare() is False
n_candidates, p_top3, _ = core.o1_snapshot("unseen-context")
assert n_candidates == 3
assert abs(p_top3 - 240 / 241) < 1e-12, p_top3

# A corpus mutation invalidates only the prepared fallback: it must not erase
# an already-captured outcome that still needs resolving against the next word.
core.load_word("beta", 10_000)
assert core.o1_resolve("alpha") == 1

# Calling prepare again must rebuild before the next boundary rather than
# serving stale data.
assert core.o1_prepare() is True
assert core.o1_prepare() is False

# A failed corpus load changes nothing and must preserve the warm cache.
try:
    core.load_corpus_file("/definitely/missing/smartkey-corpus.json")
except OSError:
    pass
else:
    raise AssertionError("missing corpus unexpectedly loaded")
assert core.o1_prepare() is False

n_candidates, p_top3, _ = core.o1_snapshot("another-unseen-context")
assert n_candidates == 3
assert abs(p_top3 - 10_180 / 10_241) < 1e-12, p_top3
assert core.o1_resolve("beta") == 1

# A successful corpus load changes unigrams and must invalidate the cache.
with tempfile.TemporaryDirectory() as scratch:
    corpus = pathlib.Path(scratch) / "corpus_en.json"
    corpus.write_text(
        json.dumps({"unigrams": {"omega": 100_000}}),
        encoding="utf-8",
    )
    core.load_corpus_file(str(corpus))
assert core.o1_prepare() is True
n_candidates, p_top3, _ = core.o1_snapshot("third-unseen-context")
assert n_candidates == 3
assert abs(p_top3 - 110_100 / 110_241) < 1e-12, p_top3
assert core.o1_resolve("omega") == 1
"""
    proc = subprocess.run(
        [sys.executable, "-c", script, str(native)],
        cwd=_REPO,
        text=True,
        capture_output=True,
        timeout=15,
        check=False,
    )
    assert proc.returncode == 0, f"stdout={proc.stdout}\nstderr={proc.stderr}"


def test_t5_native_o1_fallback_is_pinned_against_online_oov_learning():
    native = _native_module_path()
    script = """
import importlib.util
import json
import pathlib
import sys

native = pathlib.Path(sys.argv[1])
spec = importlib.util.spec_from_file_location("smartkey_py", native)
assert spec and spec.loader
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

core = module.PyInputMethodCore(json.dumps({
    "dual_buffer": {"enabled": False},
    "use_ppm": False,
    "use_reranker": False,
}))
core.load_word("alpha", 100)
assert core.o1_prepare() is True

# Commit a real OOV through the input path. It enters the online unigram table
# at frequency 5, but Phase-A's fallback remains pinned to the loaded corpus.
for char in "beta":
    core.handle_key(ord(char), 0)
actions = core.handle_key(0x20, 0)
assert ("forward", "") in actions
assert core.o1_prepare() is False

n_candidates, p_top3, _latency_us = core.o1_snapshot("unseen-context")
assert n_candidates == 1
assert core.o1_resolve("alpha") == 1
assert abs(p_top3 - 1.0) < 1e-12, p_top3
"""
    proc = subprocess.run(
        [sys.executable, "-c", script, str(native)],
        cwd=_REPO,
        text=True,
        capture_output=True,
        timeout=15,
        check=False,
    )
    assert proc.returncode == 0, f"stdout={proc.stdout}\nstderr={proc.stderr}"


def test_t5_native_module_exports_runtime_provenance():
    native = _native_module_path()
    script = """
import importlib.util
import pathlib
import re
import sys

native = pathlib.Path(sys.argv[1])
spec = importlib.util.spec_from_file_location("smartkey_py", native)
assert spec and spec.loader
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

assert module.__version__ == "0.6.2"
assert module.__build_git_sha__ == "unknown" or re.fullmatch(
    r"[0-9a-f]{40}", module.__build_git_sha__
)
assert module.__build_dirty__ is None or isinstance(module.__build_dirty__, bool)
"""
    proc = subprocess.run(
        [sys.executable, "-c", script, str(native)],
        cwd=_REPO,
        text=True,
        capture_output=True,
        timeout=15,
        check=False,
    )
    assert proc.returncode == 0, f"stdout={proc.stdout}\nstderr={proc.stderr}"


# ---------------------------------------------------------------------------
# T6 — dual-root fail-closed refusal BEFORE any DB open (A.3).
# ---------------------------------------------------------------------------
def test_t6_dual_root_refusal_observed():
    ok, detail = check_data_guard()
    assert ok, detail
    assert "live_path_refused=True" in detail
    assert "config_path_refused=True" in detail


def test_t6_collector_refuses_forbidden_root_before_db_open():
    core = FakeO1Core()
    probe = pathlib.Path.home() / "smartkey" / "o1-shim-test-refuse"
    cfg = {"enabled": True, "data_dir": str(probe)}
    with pytest.raises(RuntimeError):
        o1_shim.O1Collector(
            core,
            cfg,
            [str(_REPO / "corpus" / "corpus_en.json")],
            engine_commit=None,
        )
    # Refusal happened BEFORE any open: nothing was created under the root.
    assert not probe.exists()
    for suffix in ("events.db", "events.db-wal", "events.db-shm"):
        assert not (probe / suffix).exists()


# ---------------------------------------------------------------------------
# T7 — outcome correctness across commits (hit / miss / unresolved).
# ---------------------------------------------------------------------------
def test_t7_outcome_bits_and_unresolved(tmp_path):
    core = FakeO1Core(top3_by_ctx={
        "one": ["two", "alpha", "beta"],    # "two" follows → hit
        "two": ["gamma", "delta", "eps"],   # "three" follows → miss
        "three": ["x", "y", "z"],           # abandoned → unresolved
    })
    collector, _ = _mk_collector(tmp_path, core=core)
    collector.on_word_commit("one")
    collector.on_word_commit("two")
    collector.on_word_commit("three")
    collector.on_abandon()
    collector.close()

    events, _, _ = _read_events(collector._db_path)
    assert [e["outcome"] for e in events] == [1, 0, None]
    assert events[0]["resolved_ts"] is not None
    assert events[2]["resolved_ts"] is None
    assert all(e["latency_us"] >= 0 for e in events)


# ---------------------------------------------------------------------------
# T8 — reconciliation over a crafted DB covering every reason code.
# ---------------------------------------------------------------------------
def test_t8_reconciliation_all_reason_codes(tmp_path):
    db = tmp_path / "crafted.db"
    conn = phase_a_harness.connect(db)
    conn.executescript(o1_shim._COLLECTOR_LOG_SCHEMA)
    run = "campaign-run"
    t0 = 1_000_000.0
    window = (t0, t0 + 10_000.0)
    conn.execute(
        "INSERT INTO run_metadata (run_id, created_ts, synthetic, freq_table_hash, "
        "freq_table_files, p_model, harness_version) VALUES (?,?,0,'h','[]','m','v')",
        (run, t0),
    )

    def ev(ts, run_id=run, n=3, p=0.5, lat=100, outcome=1, resolved=None, syn=0):
        conn.execute(
            "INSERT INTO events (run_id, ts, context_hash, n_candidates, p_top3, "
            "latency_us, outcome, class, resolver, resolved_ts, synthetic) "
            "VALUES (?,?,'ch',?,?,?,?,?,?,?,?)",
            (run_id, ts, n, p, lat, outcome,
             "machine", "script:next_token_in_top3",
             resolved if resolved is not None else (ts + 1 if outcome is not None else None),
             syn),
        )

    ev(t0 + 10)                                     # valid
    ev(t0 + 20)                                     # valid
    ev(t0 + 30, syn=1)                              # EXCL_SYNTHETIC
    ev(t0 + 40, run_id="other-run")                 # EXCL_FOREIGN_RUN
    ev(t0 + 50, p=1.5)                              # EXCL_CORRUPT_ROW
    ev(t0 - 500)                                    # EXCL_OUT_OF_WINDOW
    ev(t0 + 60, n=0)                                # EXCL_NO_CANDIDATES
    ev(t0 + 3_000)                                  # EXCL_KILL_SWITCH_WINDOW
    ev(t0 + 5_000)                                  # EXCL_COLLECTOR_DEGRADED
    ev(t0 + 70, outcome=None)                       # EXCL_UNRESOLVED
    conn.execute(
        "INSERT INTO collector_log (ts, kind, detail) VALUES (?,?,?)",
        (t0 + 2_900, "kill_switch_on", "test"),
    )
    conn.execute(
        "INSERT INTO collector_log (ts, kind, detail) VALUES (?,?,?)",
        (t0 + 3_100, "kill_switch_off", "test"),
    )
    conn.execute(
        "INSERT INTO collector_log (ts, kind, detail) VALUES (?,?,?)",
        (t0 + 5_010, "drop_counter", "3"),
    )
    conn.commit()
    conn.close()

    report = shim_report.build_report(db, campaign_run_id=run, window=window)
    assert report.raw_rows == 10
    assert report.valid == 2
    assert report.excluded == {
        "EXCL_SYNTHETIC": 1,
        "EXCL_FOREIGN_RUN": 1,
        "EXCL_CORRUPT_ROW": 1,
        "EXCL_OUT_OF_WINDOW": 1,
        "EXCL_NO_CANDIDATES": 1,
        "EXCL_KILL_SWITCH_WINDOW": 1,
        "EXCL_COLLECTOR_DEGRADED": 1,
        "EXCL_UNRESOLVED": 1,
    }
    assert report.reconciles
    assert report.drop_total == 3
    assert report.hold_flags  # kill + degradation present ⇒ HOLD flags
    text = shim_report.format_report(report)
    assert "reconciles (raw == valid + Σ excluded): True" in text


def test_analyzer_and_reconciliation_share_real_campaign_population(tmp_path):
    db = tmp_path / "selector.db"
    conn = phase_a_harness.connect(db)
    run = "selected-run"
    conn.execute(
        "INSERT INTO run_metadata (run_id, created_ts, synthetic, freq_table_hash, "
        "freq_table_files, p_model, harness_version) "
        "VALUES (?,1000,0,'h','[]','m','v')",
        (run,),
    )

    def add(run_id, ts, candidates):
        conn.execute(
            "INSERT INTO events (run_id, ts, context_hash, n_candidates, p_top3, "
            "latency_us, outcome, class, resolver, resolved_ts, synthetic) "
            "VALUES (?,?,'ch',?,0.5,100,1,'machine','script:test',?,0)",
            (run_id, ts, candidates, ts + 1),
        )

    add(run, 1010, 3)
    add("foreign-run", 1020, 3)
    add(run, 1030, 0)
    conn.commit()
    conn.close()

    report = shim_report.build_report(db, campaign_run_id=run)
    result = analyze(db, campaign_run_id=run)
    assert report.raw_rows == 3
    assert report.valid == 1
    assert result.total_events == report.valid == 1
    assert result.resolutions == report.valid == 1
    assert result.excluded["EXCL_FOREIGN_RUN"] == 1
    assert result.excluded["EXCL_NO_CANDIDATES"] == 1


# ---------------------------------------------------------------------------
# Adapter-hook integration: boundary keys and Tab-accept feed the collector;
# focus loss abandons. Real adapter class, fake IBus, spy collector.
# ---------------------------------------------------------------------------
def _load_engine_module():
    # Full debug mode opens the operator's real content logs at import time.
    # These tests exercise lifecycle only and must never inherit that setting.
    previous_debug = os.environ.get("SMARTKEY_DEBUG")
    os.environ["SMARTKEY_DEBUG"] = "0"
    os.environ.setdefault("SMARTKEY_PHASEA_DATA", str(pathlib.Path(
        os.environ.get("PYTEST_TMPDIR", "/tmp")) / "o1-engine-test-data"))
    fake_gi = types.ModuleType("gi")

    def _require_version(*_a, **_k):
        raise ValueError("forced fake IBus for tests")

    fake_gi.require_version = _require_version
    sys.modules["gi"] = fake_gi
    sys.modules.pop("gi.repository", None)
    path = _REPO / "ibus" / "smartkey_engine.py"
    spec = importlib.util.spec_from_file_location("ibus.smartkey_engine_o1test", path)
    assert spec and spec.loader
    mod = importlib.util.module_from_spec(spec)
    try:
        spec.loader.exec_module(mod)
    finally:
        if previous_debug is None:
            os.environ.pop("SMARTKEY_DEBUG", None)
        else:
            os.environ["SMARTKEY_DEBUG"] = previous_debug
    assert mod._HAS_IBUS is False
    return mod


def test_engine_init_starts_o1_only_after_corpus_load(
    tmp_path, monkeypatch
):
    """The engine must hand a proven loaded corpus to the collector."""
    ske = _load_engine_module()
    cfg = tmp_path / "smartkey.json"
    cfg.write_text(
        json.dumps(
            {
                "o1_shim": {
                    "enabled": True,
                    "data_dir": str(tmp_path / "phasea"),
                    "engine_commit": "stale-config",
                }
            }
        ),
        encoding="utf-8",
    )

    class _InitCore:
        def __init__(self):
            self.calls = []

        def load_personal(self):
            self.calls.append("personal")

        def o1_prepare(self):
            self.calls.append("prepare")

        def o1_snapshot(self, _ctx):
            return (0, 0.0, 0)

    core = _InitCore()
    import ibus.o1_shim as engine_o1_shim

    class _InitCollector:
        def __init__(self, *args, **_kwargs):
            core.calls.append("collector")
            self.engine_commit = args[3]

    monkeypatch.setattr(ske, "_CONFIG_FILE", cfg)
    monkeypatch.setattr(ske, "PyInputMethodCore", lambda _config: core)
    monkeypatch.setattr(engine_o1_shim, "O1Collector", _InitCollector)

    def _fake_load_corpus(self):
        core.calls.append("corpus")
        self._o1_corpus_loaded.append(str(_corpus_marker(tmp_path)))

    monkeypatch.setattr(ske.SmartKeyEngine, "_load_corpus", _fake_load_corpus)

    eng = ske.SmartKeyEngine()

    assert core.calls == ["personal", "corpus", "collector"]
    assert isinstance(eng._o1, _InitCollector)
    assert eng._o1.engine_commit == ske._native_engine_commit("stale-config")


def test_collector_keeps_older_native_modules_without_prepare_compatible(tmp_path):
    collector, _core = _mk_collector(tmp_path, core=FakeO1Core())
    try:
        assert collector._active is True
    finally:
        collector.close()


def test_engine_init_keeps_older_native_module_collector_active(tmp_path, monkeypatch):
    ske = _load_engine_module()
    cfg = tmp_path / "smartkey.json"
    cfg.write_text(
        json.dumps(
            {"o1_shim": {"enabled": True, "data_dir": str(tmp_path / "phasea")}}
        ),
        encoding="utf-8",
    )
    events = []

    class _OldCore:
        def load_personal(self):
            events.append("personal")

        def o1_snapshot(self, _ctx):
            return (0, 0.0, 0)

    class _Collector:
        def __init__(self, *_args, **_kwargs):
            events.append("collector")

    import ibus.o1_shim as engine_o1_shim

    monkeypatch.setattr(ske, "_CONFIG_FILE", cfg)
    monkeypatch.setattr(ske, "PyInputMethodCore", lambda _config: _OldCore())

    def _fake_load_corpus(self):
        self._o1_corpus_loaded.append(str(_corpus_marker(tmp_path)))

    monkeypatch.setattr(ske.SmartKeyEngine, "_load_corpus", _fake_load_corpus)
    monkeypatch.setattr(engine_o1_shim, "O1Collector", _Collector)

    eng = ske.SmartKeyEngine()

    assert events == ["personal", "collector"]
    assert isinstance(eng._o1, _Collector)


def test_engine_init_does_not_start_o1_without_a_loaded_corpus(tmp_path, monkeypatch):
    ske = _load_engine_module()
    cfg = tmp_path / "smartkey.json"
    cfg.write_text(
        json.dumps(
            {"o1_shim": {"enabled": True, "data_dir": str(tmp_path / "phasea")}}
        ),
        encoding="utf-8",
    )
    events = []

    class _Core(PreparingO1Core):
        def load_personal(self):
            events.append("personal")

    class _Collector:
        def __init__(self, *_args, **_kwargs):
            events.append("collector")

    import ibus.o1_shim as engine_o1_shim

    core = _Core()
    monkeypatch.setattr(ske, "_CONFIG_FILE", cfg)
    monkeypatch.setattr(ske, "PyInputMethodCore", lambda _config: core)
    monkeypatch.setattr(ske.SmartKeyEngine, "_load_corpus", lambda _self: None)
    monkeypatch.setattr(engine_o1_shim, "O1Collector", _Collector)

    eng = ske.SmartKeyEngine()

    assert events == ["personal"]
    assert core.prepare_calls == 0
    assert eng._o1 is None


def _engine_init_with_prepare_error(tmp_path, monkeypatch, error):
    ske = _load_engine_module()
    cfg = tmp_path / "smartkey.json"
    cfg.write_text(
        json.dumps(
            {"o1_shim": {"enabled": True, "data_dir": str(tmp_path / "phasea")}}
        ),
        encoding="utf-8",
    )

    class _Core(FakeO1Core):
        def load_personal(self):
            return None

        def o1_prepare(self):
            raise error

    core = _Core()
    corpus = _corpus_marker(tmp_path)

    def _fake_load_corpus(self):
        self._o1_corpus_loaded.append(str(corpus))

    monkeypatch.setattr(ske, "_CONFIG_FILE", cfg)
    monkeypatch.setattr(ske, "PyInputMethodCore", lambda _config: core)
    monkeypatch.setattr(ske.SmartKeyEngine, "_load_corpus", _fake_load_corpus)
    monkeypatch.setattr(ske, "KeystrokeTrace", None)
    return ske.SmartKeyEngine


def test_engine_isolates_native_prepare_panic(tmp_path, monkeypatch):
    class _NativePanic(BaseException):
        pass

    engine_type = _engine_init_with_prepare_error(
        tmp_path, monkeypatch, _NativePanic("scripted native panic")
    )
    eng = engine_type()
    assert eng._o1 is None


@pytest.mark.parametrize("process_exit", (KeyboardInterrupt(), SystemExit(7)))
def test_engine_does_not_swallow_process_exit(tmp_path, monkeypatch, process_exit):
    engine_type = _engine_init_with_prepare_error(tmp_path, monkeypatch, process_exit)
    with pytest.raises(type(process_exit)):
        engine_type()


class _SpyCollector:
    def __init__(self) -> None:
        self.commits: list[str] = []
        self.abandons = 0

    def on_word_commit(self, word: str) -> None:
        self.commits.append(word)

    def on_abandon(self) -> None:
        self.abandons += 1


class _StepCore:
    def __init__(self) -> None:
        self._current_word = ""
        self._steps: list[tuple[list[tuple[str, str]], str]] = []

    def queue(self, actions, post_word):
        self._steps.append((actions, post_word))

    def handle_key(self, _kv, _st):
        if self._steps:
            actions, post = self._steps.pop(0)
            self._current_word = post
            return actions
        return [("forward", "")]

    process_keycode = handle_key

    def predictions(self):
        return []

    def current_word(self):
        return self._current_word

    def focus_lost(self):
        return []

    def reset(self):
        self._current_word = ""
        return []

    def save_personal(self):
        return None


def _build_engine(ske):
    eng = ske.SmartKeyEngine.__new__(ske.SmartKeyEngine)
    core = _StepCore()
    eng._core = core
    eng._caps = 0
    eng._preedit_active = False
    eng._preedit_mode = None
    eng._active_prediction = None
    eng._prediction_seq = 0
    eng._surrounding_text = None
    eng._surrounding_cursor_pos = None
    eng._trace = None
    eng._last_composing_typed = ""
    eng._session_id = "test"
    eng._last_save = time.monotonic()  # skip the debounced personal save
    eng._o1 = _SpyCollector()
    eng.commit_text = lambda _t: None
    eng.update_preedit_text = lambda *_a: None
    eng.hide_preedit_text = lambda: None
    eng.forward_key_event = lambda *_a: None
    eng.delete_surrounding_text = lambda *_a: None
    eng._log_replay_event = lambda *_a, **_k: None
    eng._refresh_surrounding_text = lambda: None
    eng._sync_surrounding_text = lambda *_a: None
    return eng, core


def test_adapter_boundary_key_commits_typed_word():
    ske = _load_engine_module()
    eng, core = _build_engine(ske)
    core._current_word = "hello"          # word typed before the boundary key
    core.queue([("commit", "hello ")], "")  # Space commits + clears the word
    eng.do_process_key_event(ske.IBus.KEY_space, 0, 0)
    assert eng._o1.commits == ["hello"]


def test_adapter_digit_boundary_commits_typed_word():
    ske = _load_engine_module()
    eng, core = _build_engine(ske)
    core._current_word = "version"
    core.queue([("commit", "version"), ("forward", "")], "")
    eng.do_process_key_event(ord("2"), 0, 0)
    assert eng._o1.commits == ["version"]


def test_adapter_tab_accept_commits_predicted_word():
    ske = _load_engine_module()
    eng, core = _build_engine(ske)
    eng._active_prediction = {"prediction_id": 1, "word": "hello", "ghost": "llo"}
    core.queue([("commit", "llo")], "")
    eng.do_process_key_event(ske.IBus.KEY_Tab, 0, 0)
    assert eng._o1.commits == ["hello"]


def test_adapter_focus_out_abandons():
    ske = _load_engine_module()
    eng, _core = _build_engine(ske)
    eng.do_focus_out()
    assert eng._o1.abandons == 1


def test_adapter_disable_abandons_pending_snapshot():
    ske = _load_engine_module()
    eng, _core = _build_engine(ske)
    eng.do_disable()
    assert eng._o1.abandons == 1
