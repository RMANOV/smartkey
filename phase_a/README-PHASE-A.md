# Phase-A harness — quick reference

Spec §12.4 (git 6403096b, ~/vawm-spec), ticket 25d391a0ba2f. No LLM, no
embeddings, no neural inference in the machinery (§12.3): counts, division, and
sklearn `IsotonicRegression` only.

## Modules

| module | role |
|---|---|
| `constants.py` | hash-committed thresholds, resolver, firewall line |
| `freqmodel.py` | raw normalised-frequency model (dumb baseline p_top3); corpus hash pin |
| `harness.py` | SQLite schema + `PhaseALogger` (INSERT@prediction, UPDATE@resolution) |
| `engine_adapter.py` | IBus-free wiring (`note_context`/`on_next_word_prediction`/`on_commit`/`on_reset`) |
| `analyze.py` | 50/50 split → 5 quantile buckets → isotonic → mechanical PASS/FAIL/INCONCLUSIVE |
| `sweep.py` | daily receipt + 48h watchdog |
| `selftest.py` | end-to-end proof on synthetic data (incl. forced FAIL) |
| `bench.py` | logging-overhead benchmark vs the 20 ms budget |
| `run-lab-engine.sh` | launch the instrumented lab ibus engine with logging ON |
| `preflight.sh` | hard precondition gate before any live ibus switch |
| `rollback.sh` | instant rollback to the live engine (`--panic` = bare keyboard) |
| `switch-to-lab.sh` | conductor-only gated switch (`--confirm`) |

The ibus switch is performed by the **conductor** (the operator delegated it —
smartkey is his live IME); see `../CONDUCTOR-ACTIVATION.md`.

## Commands

```bash
python -m phase_a.selftest                    # prove the machinery (exit 0 = all pass)
python -m phase_a.bench                        # logging overhead (real corpus)
python -m phase_a.analyze                      # real DB: report + verdict (exit 0/2/3)
python -m phase_a.analyze --synthetic          # analyse a synthetic DB
python -m phase_a.sweep sweep                  # daily receipt
python -m phase_a.sweep watchdog               # 48h watchdog (exit 1 + alarm if stale)
```

## The gate (blocker 1 — verbatim, not reinterpreted after run 1)

- **PASS** — `|mean_p − hit_rate| ≤ 0.05` in every surviving bucket **and**
  ≥5000 resolutions **and** 0 missed watchdog/sweep **and** ≤1% events > 20 ms.
- **FAIL** — `|mean_p − hit_rate| > 0.10` in ≥3 buckets **or** >5% unresolved
  **or** >1% > 20 ms **or** missed watchdog without in-band alarm.
- **INCONCLUSIVE** — everything else → exactly one adjudication round
  (diagnosis + pre-declared fix + new 14-day window); a second INCONCLUSIVE = FAIL.

`mean_p` is the **calibrated** p (isotonic map fit on the first-half); raw
`p_top3` is reported as a **baseline** and does not gate. Precedence:
**FAIL is checked before PASS** (a fatal plumbing condition overrides a
superficial calibration pass) — the only safe deterministic reading of the two
independent condition sets.

Low raw hit-rate is **not** failure (~20–35% top-3 expected for BG). Phase-A
tests the plumbing, not the model.

## Report template

Every generated report (analyze, sweep, watchdog, self-test) ends with the
verbatim firewall line:

```
Phase-A зелено ≠ доказателство за team tier.
```

## No-LLM schema invariant (Codex major)

`events.resolver` admits only `script:`/`sql:` prefixes and `events.class` must
be `machine` — enforced by a SQLite `CHECK` **and** re-verified by the nightly
sweep. The self-test proves both a `llm:`-prefixed resolver and a non-`machine`
class are rejected at insert time.
