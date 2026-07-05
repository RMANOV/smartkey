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
    for i in range(n):
        ctx = rng.choice(top_words)
        top3 = rng.sample(top_words, 3)
        pend = logger.log_prediction(ctx, top3)
        if i % 2 == 0:
            logger.resolve(pend, top3[rng.randint(0, 2)] if rng.random() < 0.3 else "MISS")
    logger.close()

    conn = connect(db, readonly=True)
    lat = np.array(
        [r[0] for r in conn.execute("SELECT latency_us FROM events").fetchall()],
        dtype=float,
    )
    conn.close()

    print("-" * 66)
    print(f"  events logged  : {len(lat):,}")
    print(f"  latency mean   : {lat.mean():.1f} us")
    print(f"  latency p50    : {np.percentile(lat,50):.0f} us")
    print(f"  latency p99    : {np.percentile(lat,99):.0f} us")
    print(f"  latency max    : {lat.max():.0f} us")
    print(f"  budget         : 20000 us (20 ms)")
    over_frac = float(np.mean(lat > LATENCY_BUDGET_US))
    print(f"  events > 20ms  : {over_frac:.4%}  (PASS <=1%)")
    print("=" * 66)
    print("Phase-A зелено ≠ доказателство за team tier.")
    if args.check and over_frac > MAX_OVER_BUDGET_FRAC:
        print(f"CHECK FAILED: over-budget {over_frac:.4%} > {MAX_OVER_BUDGET_FRAC:.0%}")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
