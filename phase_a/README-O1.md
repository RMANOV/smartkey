# O1 / Phase-A n-gram top-3 CALIBRATION harness

Frozen spec: `O1-smartkey-calibration.md`
(sha256 `48251199ca6fcad07cbda2cbc1d4db2ab7bf7077512641bc6a49290053f7b319`,
vawm-spec `@a158ea1`), which enters `PREDICTIVE-OODA-FORMALIZATION-2026-07-05`
v0.8 §12.4 (the ratified, hash-committed Phase-A value gate). Only **P1** (this
harness) is a committed move; the P2 log-odds swarm kernel is OPTION-behind-gate
and is **not** built here.

## Scope firewall (FROZEN — read this first)

`prediction_scope = ngram_component` **ONLY**. This harness wraps and measures
the raw **n-gram component** of the predictor. It does **NOT** measure the
3-stage ensemble — no Hedge, no confidence calibrator, no neural reranker, no
PPM, no BPE, no Kneser-Ney is in the measured path.

The firewall is **structural, by construction**: every module on the measured
path (`freqmodel`, `corpus_replay`, `harness`, `analyze`, `sweep`, `paths`,
`constants`) imports *nothing* from the Rust core (`smartkey_py`) or the
ensemble. The self-test (`check 9`) and pytest scan the sources and fail if any
forbidden import token appears. The measured probability therefore cannot smuggle
neural inference, and the calibration test is not circular.

Two firewall lines end **every** report/receipt:

```
Phase-A green ≠ ensemble/live-predictor calibration (prediction_scope=ngram_component ONLY: no Hedge/calibrator/neural-reranker/PPM/BPE in the measured path).
Phase-A green ≠ proof of team tier.
```

### Why FreqModel *is* the n-gram component (equivalence)

The core's own n-gram primitives compute exactly what `FreqModel` computes over
the same corpus files the engine loads:

| measured quantity | FreqModel (this harness) | core n-gram primitive (excluded from the ensemble math) |
|---|---|---|
| raw conditional prob `p(w\|ctx)` | `count(ctx,w) / total(ctx)` (`freqmodel.p_word`) | `MarkovChain::prob_from_followers` = `count/total` (`markov.rs:185`) |
| top-3 candidates | 3 highest-count followers, lexicographic tie-break (`freqmodel.top3`) | `NgramTrie::prefix_search` raw-count-descending ranking (`ngram.rs:75`) |

The core does **not** expose these primitives to Python without the ensemble
(`smartkey_py.predictions()` is the full 3-stage output, post-calibrator). Rather
than add a PyO3 surface (which would touch the shipping crate and risk pulling
ensemble state in), the harness recomputes the n-gram raw frequency directly from
the identical corpus JSON. `p_top3` is thus the dumb-baseline n-gram mass the
spec §2 pins (`p_top3 = raw normalized freq`), not the engine `score`/`confidence`.

## Pipeline

```
raw n-gram top-3 + p_top3  (counts + argmax; NO ensemble)
   -> append-only Phase-A event log (SQLite; blocker-3 schema; no-LLM CHECK)
   -> 50/50 split by ts   (first half = FIT, second half = SCORING; no recut)
   -> 5 quantile buckets on FIT-half raw p_top3; SCORE bucket <30 merges LEFT (min 2)
   -> isotonic map fitted on the FIT half  (sklearn IsotonicRegression)
   -> gate: |mean_p - hit_rate| per surviving bucket on the SCORING half
```

Raw `p_top3` is reported as the baseline; the gate runs on the **calibrated** p.

## Gate (hash-committed §5 / §12.4 — executed, never reinterpreted)

- **PASS** = `|mean_p − hit_rate| ≤ 0.05` in every surviving bucket **AND**
  `≥ 5000` resolutions / 14d **AND** `0` missed watchdog **AND** `≤ 1%` events
  `> 20 ms`.
- **FAIL** = `|mean_p − hit_rate| > 0.10` in `≥ 3` buckets **OR** `> 5%`
  unresolved **OR** `> 1%` events `> 20 ms` **OR** missed watchdog without an
  in-band alarm.
- **INCONCLUSIVE** = otherwise.

FAIL is evaluated **first** (a fatal plumbing condition overrides a superficial
calibration pass). Low raw hit-rate is **not** failure — this tests the plumbing,
not the model. Latency is a budget constraint, never the success signal.

### How ">= 5000 resolutions / 14d" is computed in lab conditions

There is no live 14-day typing window in the lab. `corpus_replay` generates the
resolution stream from the corpus's own bigram distribution and stamps the events
uniformly across a **simulated 14-day span** (`--window-days`, default 14). A
clean gate run generates ≥ 5000 resolutions inside that simulated window, so the
volume/rate gate is computed against harness-collected lab data. A daily `OK`
sweep row is written per simulated day, so the watchdog sees 0 missed days.
This is a **lab** volume check, not a live-typing rate — see the firewall lines.

## Commands

```bash
export PYTHONPATH=<worktree-root>
export SMARTKEY_PHASEA_DATA=<scratch-dir>          # REQUIRED: never a live path

# clean gate run (expect PASS)
python3 -m phase_a.corpus_replay --events 12000 --seed 20260718 --fresh --db "$SMARTKEY_PHASEA_DATA/gate_clean.db"
python3 -m phase_a.analyze --synthetic --db "$SMARTKEY_PHASEA_DATA/gate_clean.db"   # exit 0=PASS 2=FAIL 3=INCONCLUSIVE

# forced-FAIL run (injected overconfidence skew -> expect FAIL)
python3 -m phase_a.corpus_replay --events 8000 --seed 20260719 --fresh --inject-skew 0.35 --db "$SMARTKEY_PHASEA_DATA/gate_red.db"
python3 -m phase_a.analyze --synthetic --db "$SMARTKEY_PHASEA_DATA/gate_red.db"

# daily sweep + 48h watchdog
python3 -m phase_a.sweep sweep    --synthetic --db "$SMARTKEY_PHASEA_DATA/gate_clean.db"
python3 -m phase_a.sweep watchdog --synthetic --db "$SMARTKEY_PHASEA_DATA/gate_clean.db"

# end-to-end machinery self-test (incl. forced-FAIL, firewall, corpus pin)
python3 -m phase_a.selftest

# pytest surface
python3 -m pytest tests/test_o1_phasea.py -q
```

## Isolation invariants (fail-closed)

- **Data out** only under `SMARTKEY_PHASEA_DATA`; `paths.data_dir()` refuses any
  path inside `~/smartkey` (live tree) or `~/.config/smartkey` (operator config).
- **Corpus in** only the pinned `lab_corpus/*.json` copies; `verify_lab_corpus()`
  checks every sha256 against the frozen pins at the start **and** end of a run
  and raises on any mismatch.
- Never touches the live `events.db` / identity; no keystroke logs; never the
  live IBus process. `git remote -v` shows the push URL DISABLED (isolation is
  plumbing, not will).

## Deviations from the verbatim spec (with rationale)

1. **`top3` / `resolved_token` not persisted.** The blocker-3 schema lists them,
   but §4/§7/§8 forbid persisting linguistic content / keystroke logs and mandate
   privacy. The outcome bit (`resolved_token ∈ top3`) is computed **in memory**;
   the DB stores only the candidate **count**, `p_top3`, and a keyed-HMAC
   `context_hash`. This is strictly more privacy-safe and loses no gate input.
   (Same deliberate divergence as the established Phase-A harness.)
2. **p-source = a raw-frequency mirror over the same corpus, not a direct call
   into the Rust `NgramTrie`/`MarkovChain`.** Those structs are not PyO3-exposed
   without the ensemble; recomputing the identical `count/total` raw frequency
   keeps the ensemble structurally out of the measured path (see equivalence
   table) instead of adding a shipping-crate binding.
3. **Forced-FAIL = injected overconfidence skew.** The spec names "probability
   skew" as an example. The injection holds reported `p_top3` honest but depresses
   the SCORING-half realized hit-rate by Δ (default 0.35), so the FIT-half
   isotonic map overshoots by > 0.10 in ≥ 3 buckets — tripping the §5 gap FAIL
   clause. Used only for the forced-FAIL receipt; never in a clean gate run.
4. **Volume window simulated** (see above): lab replay stamps a 14-day span; not
   a live rate.
