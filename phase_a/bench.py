"""Benchmark the Phase-A logging overhead against the >=20ms/event budget,
using the REAL corpus frequency table (not the synthetic stub). Reports corpus
load time / size, per-event latency distribution (the value stored in
``latency_us``), and the fraction over budget.
"""

from __future__ import annotations

import argparse
import random
import time
import tracemalloc
from pathlib import Path

import numpy as np

from .constants import LATENCY_BUDGET_US, MAX_OVER_BUDGET_FRAC
from .freqmodel import FreqModel, default_corpus_files
from .harness import PhaseALogger, connect
from .paths import data_dir


def main() -> int:
    ap = argparse.ArgumentParser(description="Phase-A logging-overhead benchmark")
    ap.add_argument(
        "--check",
        action="store_true",
        help="exit non-zero if the >20ms budget is violated (for preflight gating)",
    )
    ap.add_argument(
        "--realistic",
        action="store_true",
        help="also pace events at human typing cadence (proves tight-loop tail "
        "spikes are a bench artifact, not a real-typing stall)",
    )
    args = ap.parse_args()
    files = default_corpus_files()
    if not files:
        print("no corpus files found")
        return 1

    tracemalloc.start()
    t0 = time.perf_counter()
    fm = FreqModel.load(files)
    load_s = time.perf_counter() - t0
    _cur, peak = tracemalloc.get_traced_memory()
    tracemalloc.stop()

    print("=" * 66)
    print(" PHASE-A LOGGING-OVERHEAD BENCHMARK (real corpus freq table)")
    print("=" * 66)
    for f in fm.files:
        print(f"  corpus: {Path(f['path']).name}  {f['bytes']/1e6:.2f} MB  sha256={f['sha256'][:12]}")
    print(f"  freq-table hash: {fm.table_hash()}")
    print(f"  p-model        : {fm.pmodel}")
    print(f"  load time      : {load_s:.2f} s")
    print(f"  unigrams       : {len(fm.unigrams):,}")
    print(f"  bigram contexts: {len(fm.bigrams):,}")
    print(f"  peak load mem  : ~{peak/1e6:.0f} MB (incl. transient JSON parse)")

    top_words = sorted(fm.unigrams, key=lambda w: fm.unigrams[w], reverse=True)[:200]
    rng = random.Random(0)

    db = data_dir() / "bench_synthetic.db"
    for suffix in ("", "-wal", "-shm"):
        p = Path(str(db) + suffix)
        if p.exists():
            p.unlink()
    logger = PhaseALogger(db, fm, synthetic=True, notes="benchmark")

    # sample p_top3 sanity
    sample_p = [fm.p_top3(rng.choice(top_words), rng.sample(top_words, 3)) for _ in range(5)]
    print(f"  sample p_top3  : {[round(x,4) for x in sample_p]}")

    n = 5000
    # full_hook = external wall time of the ENTIRE log_prediction() call
    # (compute + INSERT + commit + latency write-back + commit) — the TRUE
    # synchronous cost the hook adds to a keystroke (Codex B1).
    full_hook = np.empty(n, dtype=float)
    resolve_lat = []
    for i in range(n):
        ctx = rng.choice(top_words)
        top3 = rng.sample(top_words, 3)
        h0 = time.perf_counter_ns()
        pend = logger.log_prediction(ctx, top3)
        full_hook[i] = (time.perf_counter_ns() - h0) / 1000.0
        if i % 2 == 0:
            r0 = time.perf_counter_ns()
            logger.resolve(pend, top3[rng.randint(0, 2)] if rng.random() < 0.3 else "MISS")
            resolve_lat.append((time.perf_counter_ns() - r0) / 1000.0)
    logger.close()

    conn = connect(db, readonly=True)
    stored = np.array(
        [r[0] for r in conn.execute("SELECT latency_us FROM events").fetchall()],
        dtype=float,
    )
    conn.close()
    resolve_lat = np.array(resolve_lat, dtype=float)

    def _row(label, a):
        print(
            f"  {label:22s} mean {a.mean():6.1f}  p50 {np.percentile(a,50):5.0f}  "
            f"p99 {np.percentile(a,99):5.0f}  max {a.max():6.0f} us  "
            f">20ms {np.mean(a>LATENCY_BUDGET_US):.4%}"
        )

    print("-" * 66)
    print(f"  events logged  : {len(stored):,}   budget 20000 us (20 ms), PASS <=1%")
    _row("stored latency_us", stored)         # what the gate reads (now incl. commit)
    _row("FULL hook (external)", full_hook)   # the true full synchronous path
    _row("resolve() (external)", resolve_lat)
    if args.realistic:
        # Pace at ~human fast typing (30ms/keystroke). Spreads commits so WAL
        # checkpoints don't pile up — the tight-loop tail should vanish.
        m = 400
        paced = np.empty(m, dtype=float)
        logger2 = PhaseALogger(data_dir() / "bench_realistic.db", fm,
                               synthetic=True, notes="benchmark realistic")
        for i in range(m):
            ctx = rng.choice(top_words)
            top3 = rng.sample(top_words, 3)
            h0 = time.perf_counter_ns()
            logger2.log_prediction(ctx, top3)
            paced[i] = (time.perf_counter_ns() - h0) / 1000.0
            time.sleep(0.030)
        logger2.close()
        _row("paced (30ms cadence)", paced)

    print("=" * 66)
    print("Phase-A зелено ≠ доказателство за team tier.")
    # Gate on the TRUE full path (the strictest, honest measure).
    over_full = float(np.mean(full_hook > LATENCY_BUDGET_US))
    over_resolve = float(np.mean(resolve_lat > LATENCY_BUDGET_US))
    if args.check and (over_full > MAX_OVER_BUDGET_FRAC or over_resolve > MAX_OVER_BUDGET_FRAC):
        print(
            f"CHECK FAILED: full-hook >20ms {over_full:.4%} / resolve {over_resolve:.4%} "
            f"> {MAX_OVER_BUDGET_FRAC:.0%}"
        )
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
