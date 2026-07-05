# Phase-A ARCHITECTURE NOTE — smartkey prediction hook points

Ticket 25d391a0ba2f · spec §12.4 (git 6403096b in ~/vawm-spec) · lab branch `phase-a-lab`.
This note records what the codebase exploration found and where the Phase-A
harness hooks in. No Rust was modified; only Python logging was added.

## 1. Stack shape

- **Language:** Rust workspace (`crates/smartkey-core`, `-py`, `-win`, `-mac`,
  `-playground`) + a thin **Python IBus adapter** (`ibus/smartkey_engine.py`,
  `ibus/main.py`). Bindings via **PyO3** (`crates/smartkey-py`, module
  `smartkey_py`, class `PyInputMethodCore`). The venv already has `smartkey_py`
  built (`/home/rmanov/smartkey/.venv`).
- All prediction logic lives in Rust `MasterLoop` → `InputMethodCore`
  (`master_loop.rs`, `input.rs`). Python only translates `Action` tuples
  (`ghost`, `hide`, `commit`, `replace`, `composing`, `forward`) into IBus calls.

## 2. Where candidates + frequencies are computed

- **Candidate ranking:** `SmartKeyEngine` (Rust, `ensemble.rs`) blends four
  signals — corpus frequency (α), Markov context (β), personal CVM (γ), tech
  vocab (δ) — then an optional **neural reranker** (`reranker.rs`, a tiny
  feed-forward net) reorders, and an internal **isotonic calibrator**
  (`calibration.rs`) maps the ratio to a UI `confidence`.
- **Python-visible surface:** `PyInputMethodCore.predictions()` →
  `list[(word: str, score: f64, confidence: f64)]`. `score` is the *ensemble*
  score (post-reranker); `confidence` is smartkey's *own* calibrated value.
- **The raw n-gram / frequency table** is the corpus: `corpus/corpus_*.json`
  with shape `{"unigrams": {w: freq}, "bigrams": [{ctx, word, count}],
  "trigrams": [{w1, w2, word, count}]}`. The engine loads these via
  `load_corpus_file` (`corpus.rs`).

### Why the harness does NOT reuse `score`/`confidence`

The spec pins `p_top3` to a **raw normalised frequency** from the frequency
table (blocker 3), a deliberately *dumb baseline* (value-gate-first). Using
`score`/`confidence` would (a) smuggle **neural inference** into the measured p
(violating the §12.3 no-LLM/no-neural principle for harness machinery) and (b)
make the calibration test **circular** (calibrating smartkey's calibration).
So `phase_a/freqmodel.py` reads the *same* corpus files independently and
computes `p_top3` purely from counts:

```
P_raw(w|ctx) = count(ctx, w) / Σ_w' count(ctx, w')     # bigram-conditional
             = freq(w) / Σ freq                          # unigram back-off
p_top3       = clamp(Σ_{w∈top3} P_raw(w|ctx), 0, 1)
```

Measured (real corpus): load 1.55 s; 189,851 unigrams; 17,979 bigram contexts;
~100 MB peak. The corpus files are SHA-256'd and the combined hash is pinned in
`run_metadata` (frequency-table version pin).

## 3. Hook points (all in the Python layer — zero Rust change)

The engine already tracks a prediction lifecycle for its replay log
(`_track_prediction_shown` → `_finalize_prediction_outcome`). Phase-A piggybacks
the same two moments, guarded by `SMARTKEY_PHASE_A=1` (no-op otherwise):

| Phase-A event | smartkey_engine.py site | what it captures |
|---|---|---|
| **candidate-generation** (next-word prediction) | `_execute_actions` → `ghost` branch → `_phase_a_ghost()` | context (surrounding text), top3 words, p_top3, latency, INSERT row (outcome NULL) |
| **resolution** (next token committed) | `_execute_actions` → `commit` & `replace` branches → `_phase_a_commit()` | resolved token → UPDATE outcome = token ∈ top3 |
| **drop in-flight** | `do_reset`, `do_focus_out` → `on_reset()` | pending stays unresolved |
| **flush/close** | `do_disable` → `close()` | commit + close DB |

### Resolver semantics = `next_token_in_top3`

We log at the **`ghost` action** (ShowGhost carries *no typed prefix* → the
cursor is at a word boundary and the whole next token is being anticipated).
The event resolves to the **next token committed to the application**. In-word
completions (`composing`, which carry a typed prefix) are deliberately **not**
logged: they are a different semantic and would supersede one another per
keystroke. One clean event per predicted word position — exactly what the
resolver name promises. This is the one engine-coupled choice, documented here.

- Top3 words and the resolved token are **lowercased** before membership/lookup
  so they align with the (lowercase) corpus keys and each other.
- If the engine emits no `commit`/`replace` for a word (e.g. some fallback input
  modes forward characters directly), that event stays **unresolved**. A high
  unresolved rate is therefore a *real* plumbing signal — and `>5% unresolved`
  is a spec FAIL condition, which is correct: the harness would be unable to
  observe outcomes and must say so.

## 4. Isolation

Work is confined to the `phase-a-lab` git worktree
(`/home/rmanov/smartkey-phase-a-lab`) with **push disabled** in both the main
repo and worktree configs. See `isolation-receipt.txt` (captured before any code
was written) and `CONDUCTOR-ACTIVATION.md`. The installed/live ibus engine and
the main working tree of `~/smartkey` are untouched; activating the lab engine
for real typing is the **conductor's** guarded step (the operator delegated the
switch, since smartkey is his live IME), gated by `phase_a/preflight.sh` and a
pre-proven `phase_a/rollback.sh`.
