# O1 / Phase-A build receipts

All receipts are for the **P1 n-gram calibration harness** in this worktree
(`agent/o1-p1-calibration`, base `47acd8a`). Frozen spec:
`O1-smartkey-calibration.md` sha256
`48251199ca6fcad07cbda2cbc1d4db2ab7bf7077512641bc6a49290053f7b319`.

**Scope firewall (on every receipt):** `prediction_scope = ngram_component` ONLY.
The harness measures the raw n-gram component; the 3-stage ensemble (Hedge /
calibrator / neural reranker / PPM / BPE) is NOT in the measured path.

> Phase-A green ≠ ensemble/live-predictor calibration.
> Phase-A green ≠ proof of team tier.

| # | file | one-line description |
|---|---|---|
| 1 | `01-forced-fail-red.log` | Injected overconfidence skew (Δ=0.35) drives the gate **RED — verdict FAIL, exit 2** (≥3 buckets >0.10); the same harness with no injection goes PASS/exit 0. Proof the gate is not rubber-stamping. |
| 2 | `02-clean-build.log` | Clean base build: `cargo build --workspace` exit 0, `cargo clippy --all-targets` **0 warnings**, `cargo test --workspace` **445 passed / 0 failed** exit 0; `py_compile` OK; `pytest` 15 passed. (Harness is pure Python — Rust untouched.) |
| 3 | `03-isolation-deltas.md` | config/smartkey.json + all 6 lab_corpus files **byte-identical before/after (delta 0)**; config == frozen `116253280bf9…`; live PID 422286 still running/untouched; personal.json documented as live-owned (harness never references it — grep proof). **Watch-audit git-state precision**: tracked base clean + exactly six authorized untracked corpus copies (`??` in `git status --untracked-files=all`). |
| 4 | `04-rollback-dry-run.log` | **Exact executable** rollback (a)-(d) with validated absolute paths: `worktree remove --force` (subsumes the lab_corpus `rm -rf`), `branch -D`, scratch-dir `rm -rf` — each path validated, **NOT executed**. Push-URL restore is out of rollback scope (operator-gated). |
| 5 | `05-feature-flag-manifest.md` | Effective env/constants/corpus-source manifest — nothing points at live data; `git remote -v` push **DISABLED**; zero ensemble/PyO3 imports in the measured path. |
| 6 | `06-latency-watchdog.log` | Lab run over the corpus copies: latency **p99 = 96 µs**, **0 events > 20 ms (0.0000%)** (≤1% gate), **0 missed watchdog days**, watchdog exit 0. |

## Fail-closed statement

The forced-FAIL receipt (1) demonstrates the gate going RED under injected
miscalibration — the required proof that a GREEN verdict is earned, not assumed.
No receipt was fabricated; every log is captured command output.

## What the harness is (and is not)

It wraps the **n-gram top-3 path only**: raw-count candidates + raw normalized
frequency `p_top3`, computed with zero ensemble/neural inference, over the pinned
`lab_corpus/`. It does **not** measure or validate the live 3-stage predictor. A
green Phase-A gate proves the n-gram plumbing's calibration discipline on the
safe lab substrate — nothing about the ensemble, the live predictor, or team tier.
