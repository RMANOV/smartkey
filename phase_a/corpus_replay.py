"""Generate calibration resolution events from a pinned local corpus.

In lab conditions there are no live keystrokes. This driver replays the corpus's
own bigram distribution as the ground-truth next-token stream:

  * pick a context ctx, weighted by its total bigram mass (how often the
    predictor is actually queried after that word);
  * the n-gram predictor's decision = FreqModel.predict(ctx) -> (top3, p_top3),
    computed purely from raw counts (NO ensemble, NO PyO3 — scope firewall);
  * the realized next token is sampled from ctx's follower distribution
    (weighted by count) -> the SAME distribution p_top3 is computed from, so in
    the clean run E[outcome] = p_top3 exactly (the n-gram is calibrated by
    construction; the gate proves the plumbing survives the 50/50 out-of-sample
    split; it does not grade the model);
  * outcome (realized in top3) is computed in memory; only the {0,1} bit + the
    candidate COUNT are persisted (privacy; no plaintext token stored).

Events are ts-stamped uniformly across a simulated VOLUME_WINDOW_DAYS (14d) span
so the ">=5000 resolutions / 14d" volume gate is computed against harness lab
data. A daily 'OK' sweep row is written per simulated day for watchdog coverage.

Forced-FAIL mode (``--inject-skew DELTA``): in the SCORING half (second 50% by
ts) the realized hit-rate is depressed by DELTA (outcome ~ Bernoulli(clip(p_top3
- DELTA))) while the reported p_top3 stays honest — a non-stationary
OVERCONFIDENCE probability skew. The isotonic map fitted on the honest FIT half
then overshoots the SCORING half by > 0.10 in >= 3 buckets, driving the gate
red. The injected skew is never used in a clean gate run.

Hard isolation: reads ONLY the pinned ``lab_corpus/`` (sha256-verified at start
and end); writes ONLY under SMARTKEY_PHASEA_DATA (data_dir() guards the root).
"""

from __future__ import annotations

import argparse
import datetime as _dt
import itertools
import random
import time
from pathlib import Path

from .constants import HARNESS_VERSION, PREDICTION_SCOPE, VOLUME_WINDOW_DAYS
from .freqmodel import FreqModel
from .harness import PhaseALogger, connect
from .paths import (
    data_dir,
    lab_corpus_files,
    verify_lab_corpus,
)


def _fresh(path: Path) -> Path:
    for suffix in ("", "-wal", "-shm"):
        p = Path(str(path) + suffix)
        if p.exists():
            p.unlink()
    return path


def _sim_daily_sweeps(db_path: Path, first_ts: float, last_ts: float) -> int:
    """Insert one 'OK' sweep row per date in the event window (watchdog cover).
    Returns the number of sweep-days written."""
    conn = connect(db_path)
    d = _dt.date.fromtimestamp(first_ts)
    d1 = _dt.date.fromtimestamp(last_ts)
    n = 0
    while d <= d1:
        noon = time.mktime(d.timetuple()) + 43200
        conn.execute(
            "INSERT INTO sweeps (sweep_date, ran_ts, events, resolutions, "
            "latency_p99_us, over_budget_pct, watchdog_status, receipt) "
            "VALUES (?,?,?,?,?,?,?,?)",
            (d.isoformat(), noon, 0, 0, 0, 0.0, "OK", "simulated daily sweep (lab)"),
        )
        d += _dt.timedelta(days=1)
        n += 1
    conn.commit()
    conn.close()
    return n


class _Sampler:
    """Frequency-weighted context sampler + per-context follower sampler, built
    once from the FreqModel. All randomness flows through one seeded RNG."""

    def __init__(self, model: FreqModel, rng: random.Random) -> None:
        self.model = model
        self.rng = rng
        self.contexts = [c for c, t in model.bigram_totals.items() if t > 0]
        if not self.contexts:
            raise RuntimeError("corpus has no bigram contexts to replay")
        weights = [model.bigram_totals[c] for c in self.contexts]
        self.cum_weights = list(itertools.accumulate(weights))
        self._decision: dict[str, tuple[list[str], float]] = {}
        self._followers: dict[str, tuple[list[str], list[int]]] = {}

    def sample_contexts(self, n: int) -> list[str]:
        return self.rng.choices(self.contexts, cum_weights=self.cum_weights, k=n)

    def decision(self, ctx: str) -> tuple[list[str], float]:
        d = self._decision.get(ctx)
        if d is None:
            d = self.model.predict(ctx)  # (top3, p_top3) — n-gram only
            self._decision[ctx] = d
        return d

    def realized_token(self, ctx: str) -> str:
        f = self._followers.get(ctx)
        if f is None:
            slot = self.model.followers(ctx)
            f = (list(slot.keys()), list(slot.values()))
            self._followers[ctx] = f
        words, counts = f
        return self.rng.choices(words, weights=counts, k=1)[0]


def run_replay(
    db_path: Path,
    corpus_files: list[Path],
    n_events: int,
    seed: int,
    inject_skew: float | None = None,
    window_days: int = VOLUME_WINDOW_DAYS,
    synthetic: bool = True,
    notes: str | None = None,
) -> dict:
    """Replay *n_events* corpus-driven resolution events into *db_path*.

    Returns a small summary dict (counts, hashes, sweep-days). ``synthetic``
    defaults to True: all lab-replay data is marked synthetic=1 and lives in the
    scratch data dir — it is never real-typing evidence."""
    rng = random.Random(seed)
    model = FreqModel.load(corpus_files)
    sampler = _Sampler(model, rng)

    span = window_days * 86400
    now = time.time()
    ts0 = now - span

    logger = PhaseALogger(
        db_path,
        model,
        synthetic=synthetic,
        notes=notes or f"corpus-replay seed={seed} skew={inject_skew}",
        scope=PREDICTION_SCOPE,
    )

    ctx_stream = sampler.sample_contexts(n_events)
    half = n_events // 2
    hits = 0
    for i, ctx in enumerate(ctx_stream):
        top3, p = sampler.decision(ctx)
        realized = sampler.realized_token(ctx)
        ts = ts0 + (i / max(n_events, 1)) * span
        honest_outcome = 1 if realized in set(top3) else 0
        if inject_skew is not None and i >= half:
            # scoring-half overconfidence skew: realized reliability drops DELTA
            skewed_p = max(0.0, min(1.0, p - inject_skew))
            outcome = 1 if rng.random() < skewed_p else 0
        else:
            outcome = honest_outcome
        pend = logger.log_prediction(ctx, top3, ts=ts, p_top3=p)
        logger.resolve_bit(pend, outcome, ts=ts)
        hits += outcome
    logger.close()

    conn = connect(db_path, readonly=True)
    lo, hi = conn.execute(
        "SELECT MIN(ts), MAX(ts) FROM events WHERE synthetic=?",
        (1 if synthetic else 0,),
    ).fetchone()
    conn.close()
    sweep_days = _sim_daily_sweeps(db_path, lo, hi)

    return {
        "db_path": str(db_path),
        "n_events": n_events,
        "resolutions": n_events,
        "raw_hit_rate": hits / max(n_events, 1),
        "distinct_contexts_used": len(sampler._decision),
        "freq_table_hash": model.table_hash(),
        "corpus_files": [f["path"] for f in model.files],
        "sweep_days": sweep_days,
        "window_days": window_days,
        "inject_skew": inject_skew,
        "harness_version": HARNESS_VERSION,
        "prediction_scope": PREDICTION_SCOPE,
    }


def main() -> int:
    ap = argparse.ArgumentParser(description="O1 corpus-replay event generator (n-gram only)")
    ap.add_argument("--db", default=None, help="event DB (default: data_dir()/replay.db)")
    ap.add_argument("--events", type=int, default=12000, help="number of resolution events")
    ap.add_argument("--seed", type=int, default=20260718)
    ap.add_argument("--window-days", type=int, default=VOLUME_WINDOW_DAYS)
    ap.add_argument(
        "--inject-skew",
        type=float,
        default=None,
        metavar="DELTA",
        help="forced-FAIL: depress scoring-half hit-rate by DELTA (overconfidence skew)",
    )
    ap.add_argument("--fresh", action="store_true", help="delete the DB first")
    args = ap.parse_args()

    # Verify corpus pins BEFORE any work (fail-closed).
    verify_lab_corpus("json")
    corpus_files = lab_corpus_files("json")

    db_path = Path(args.db) if args.db else data_dir() / "replay.db"
    if args.fresh:
        _fresh(db_path)

    summary = run_replay(
        db_path,
        corpus_files,
        n_events=args.events,
        seed=args.seed,
        inject_skew=args.inject_skew,
        window_days=args.window_days,
    )

    # Verify corpus pins AGAIN at the end (unchanged by the run).
    verify_lab_corpus("json")

    import json as _json

    print(_json.dumps(summary, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
