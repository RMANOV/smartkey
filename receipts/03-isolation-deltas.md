# O1 / PHASE-A RECEIPT 3 — ISOLATION DELTAS

generated: 2026-07-18T21:48:23+03:00   worktree HEAD: 47acd8a12ec16b9e5aeb60a63f4bf43550f04405
BEFORE snapshot: taken before any file was written (see timestamp within).

## Live process untouched
```
PID 422286 (live IBus smartkey engine):
 422286 S<l   6-00:55:53 /home/rmanov/smartkey/.venv/bin/python3 -m ibus.main --ibus
```
Still running; never signalled, attached, or reconfigured by this work.

## Byte-identical invariants (config + pinned corpus) — BEFORE vs AFTER the entire run

| file | BEFORE sha256 | AFTER sha256 | delta |
|---|---|---|---|
| smartkey.json | 116253280bf9a589... | 116253280bf9a589... | 0 (identical) |
| corpus_bg.bin | 08b1440a6d542d68... | 08b1440a6d542d68... | 0 (identical) |
| corpus_bg.json | 087136b9e3b5cb6e... | 087136b9e3b5cb6e... | 0 (identical) |
| corpus_en.bin | 609c90439257f687... | 609c90439257f687... | 0 (identical) |
| corpus_en.json | 5a59e842c5d58e97... | 5a59e842c5d58e97... | 0 (identical) |
| corpus_tech.bin | 77b57593eab7f3c8... | 77b57593eab7f3c8... | 0 (identical) |
| corpus_tech.json | 5af923c90a9e8dfb... | 5af923c90a9e8dfb... | 0 (identical) |

ALL config + corpus deltas == 0: YES
config/smartkey.json AFTER == frozen expected 116253280bf9...: YES
6 lab_corpus files AFTER == frozen pins: YES (all 6 verified)

## personal.json — live-owned, changes independently of this work
```
BEFORE: acdf2919f1553a4c4ec5c786bc5719a1b738d90146e079f101343424597dc0bc
AFTER : 98a4bbc7b2098629762b3180035bec0c5ae885834d50b7bdb072e3df8499e63f
```
~/.config/smartkey/personal.json is continuously rewritten by the LIVE engine (PID
422286) as the operator types; it is NOT byte-stable through any action of ours, and
we must not stop the live process. Isolation guarantee: this harness NEVER opens or
writes that path. Grep proof — the ONLY references to the live/operator tree in phase_a/
are inside the fail-closed data-dir GUARD that REFUSES such paths (never a write target):
```
phase_a/harness.py:18:``context_hash`` instead. This honors the spec's "never live events.db /
phase_a/paths.py:7:     operator config (~/.config/smartkey) so a run can never write live state.
phase_a/paths.py:73:    """Lab event DB. Never the live events.db (data_dir() guards the root)."""
phase_a/paths.py:75:    return Path(env) if env else data_dir() / "events.db"
--- the guard (paths.py) ---
35:_FORBIDDEN_DATA_PREFIXES = (
36:    Path.home() / "smartkey",
62:    for forbidden in _FORBIDDEN_DATA_PREFIXES:
65:                f"SMARTKEY_PHASEA_DATA={d} resolves inside the live/operator tree "
66:                f"{forbidden} — refusing (data outputs must stay in a scratch dir)."
```

## Data outputs stayed in the scratch dir
SMARTKEY_PHASEA_DATA = /tmp/claude-1000/-home-rmanov-smartkey-phase-a-lab/179f182b-c6aa-4af8-b4ef-df37d19ebd44/scratchpad/o1-phasea-data
(paths.data_dir() fail-closes on any path inside ~/smartkey or ~/.config/smartkey.)

## Git-state precision (watch-audit ADVOCATE_CODEX e6eafac7d369)

State after the O1 commits: **tracked base clean + exactly six AUTHORIZED untracked
corpus copies under lab_corpus/ (`??` in `git status --untracked-files=all`)**.

The six corpus copies and anything under the temp data dir are NEVER committed:
commits stage only `phase_a/`, `tests/`, and `receipts/` by explicit path (never
`git add -A`), and `phase_a_data/` (the in-worktree data-dir fallback) is git-excluded
via `.git/info/exclude`. `lab_corpus/` is deliberately left visible as authorized
untracked (not excluded) so the audit can see the six `??` entries.

`git status --short --branch --untracked-files=all` (after the three O1 commits):
```
## agent/o1-p1-calibration
?? lab_corpus/corpus_bg.bin
?? lab_corpus/corpus_bg.json
?? lab_corpus/corpus_en.bin
?? lab_corpus/corpus_en.json
?? lab_corpus/corpus_tech.bin
?? lab_corpus/corpus_tech.json
```
Tracked working tree is clean (no `M`/`D`/`A` beyond the three commits); the only
untracked entries are the six authorized `lab_corpus/` copies. The scratch data dir
lives outside the repo and never appears here.
