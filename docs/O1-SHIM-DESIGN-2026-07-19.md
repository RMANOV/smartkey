# O1 LIVE-RUN SHIM — DESIGN ARTIFACT (2026-07-19)

Frozen-spec binding: `O1-P1-LIVE-RUN-SHIM-SPEC-2026-07-19.md`
sha256 `68a343b463b61dd05a6ed672829ccdf77c9d6c6ab25bfdfdcc5364b91b3d7d9c`
(vawm-spec @ 6ca0e57, FOLD-GATE PASS f97b70580a21). Shim base:
`b415a9503199782b777675fa90d48220bb9afc92` = merge of the double-gated UX
cluster tip `0de644d` (PASS 02ebe263be73 + aec92e020cc6) and the double-gated
O1/P1 calibration harness `5ef79cc` (PASS 16cab66b11cb + addendum
465110c2018f). This document FREEZES the Δ2 latency semantics and the Δ3
validity predicate + exclusion reason-code taxonomy; per Δ3 they are
pre-registered IN the shim-diff gate, not at analysis time.

## 1. Architecture (spec §2 — minimal surface, passive tap)

Measured event = one NEXT-WORD prediction of the **ngram component only**,
taken at each word-commit boundary in the IBus adapter:

1. At commit of word `W_prev` the adapter (new module `ibus/o1_shim.py`)
   calls the new PyO3 accessor `o1_snapshot(ctx)` with
   `ctx = last_context_word(W_prev)` (tokenization mirrors
   `phase_a/freqmodel.py::last_context_word`: trailing `\w+` token,
   lowercased). The core computes, from its OWN loaded raw n-gram/frequency
   tables (never the ensemble):
   - `top3(ctx)`: 3 highest-raw-count followers of `ctx`, ties broken
     lexicographically; global unigram top-3 back-off when `ctx` has no
     bigram mass — the exact `FreqModel.top3` semantics;
   - `p_top3`: raw normalised frequency mass of the deduplicated top-3 set,
     `p_word = count(ctx,w)/total(ctx)` with unigram back-off `u/U`,
     clamped to [0,1] — the exact `FreqModel.p_top3` semantics.
   The top-3 words stay in core memory (a single `Option` slot); the FFI
   returns **numbers only**: `(n_candidates, p_top3, core_latency_us)`.
2. At commit of the NEXT word `W_next` the adapter calls
   `o1_resolve(next_ctx_token)` → the core compares against the stored
   top-3, clears the slot, and returns only the `{0,1}` outcome bit.
   No candidate words ever cross the FFI, are logged, or are persisted.
3. The adapter enqueues (in-memory, bounded queue) a prediction row at
   step 1 and a resolution update at step 2; a background writer thread
   drains the queue into the events DB (schema §5). The hot path performs
   **zero** DB I/O.

A.1 passive-only: the accessors read raw count tables and one `Option`
slot; they never call ensemble/reranker/calibrator code, never mutate
predictor state, never touch ranking/UX/config/watchdog. With the shim
disabled (§6) the adapter makes no PyO3 shim calls at all — the disabled
path is behaviorally identical to base by construction.

Ensemble firewall: `phase_a/` measured modules remain byte-identical
(check 9 untouched). The shim is a NEW surface outside the measured-path
module set; the p it records comes from raw counts only.

## 2. Δ2 — LATENCY SEMANTICS (FROZEN)

`latency_us` for a live row = monotonic elapsed time
(`time.perf_counter_ns()`, integer µs, adapter-side) from immediately
BEFORE the `o1_snapshot(ctx)` FFI call to immediately AFTER the prediction
row (including this latency value) is fully constructed — i.e. up to the
point just before the queue push.

- Interval CONTAINS: PyO3 crossing, core raw-table query (top3 + p_top3),
  context-hash HMAC computation, row construction.
- Interval EXCLUDES: the queue push itself (sub-µs; excluded so the row
  can carry its own final latency without a hot-path read-back race), the
  background writer's SQLite INSERT/UPDATE/commit (async, outside the hot
  path per Δ2), and the resolution step.
- Measurement point: the word-commit boundary in the adapter — the only
  place the shim adds hot-path work; this is the full cost the shim adds
  to a live keystroke path per event, measured conservatively
  (FFI included).
- Known deviation from the LAB semantics (declared, not silent): the lab
  harness stored the pipelined compute+INSERT+commit cost of the previous
  event (`phase_a/harness.py`). The live shim excludes durable-write cost
  because Δ2 mandates the collector write out of the measured hot path.
  The §12.4 budget clause (≤1% of valid observations >20ms,
  `LATENCY_BUDGET_US=20_000`) applies to the live semantics above.
- `core_latency_us` (core-internal query time) is returned by the FFI and
  logged to `collector_log` for diagnostics only; it is NOT the gated
  `latency_us` and never substitutes for it.

## 3. Δ3 — VALIDITY PREDICATE + EXCLUSION REASON CODES (FROZEN)

A row is a **valid resolution** iff ALL of:
  V1 `synthetic = 0`;
  V2 `run_id` = the single campaign run_id written at deploy
     (fresh dedicated DB, §5 — foreign run_ids are structurally absent);
  V3 `outcome IN (0,1)` and `resolved_ts IS NOT NULL`;
  V4 `ts` and `resolved_ts` inside `[deploy_ts, campaign_end]`;
  V5 `n_candidates >= 1`;
  V6 row passes range checks: `p_top3 ∈ [0,1]`, `latency_us >= 0`,
     `resolved_ts >= ts`;
  V7 row not inside any `collector_log` kill-switch or degradation window.

Exclusion reason codes (machine-readable; each non-valid row carries
exactly one, first-match in this order):
  `EXCL_SYNTHETIC`        — V1 fails (impossible in a campaign DB; counted if seen)
  `EXCL_FOREIGN_RUN`      — V2 fails
  `EXCL_CORRUPT_ROW`      — V6 fails
  `EXCL_OUT_OF_WINDOW`    — V4 fails
  `EXCL_NO_CANDIDATES`    — V5 fails (ngram had no candidates; recorded, never dropped)
  `EXCL_KILL_SWITCH_WINDOW` — V7 fails (kill-switch interval)
  `EXCL_COLLECTOR_DEGRADED` — V7 fails (queue-overflow/writer-error interval;
                              dropped-event counts reported alongside)
  `EXCL_UNRESOLVED`       — V3 fails (outcome NULL; feeds the §12.4
                            >5%-unresolved FAIL clause via the unchanged
                            analyze path — reported, never deleted)

B.3 reconciliation: `raw_rows = valid + Σ(excluded per code)` computed by a
NEW reporting module (`shim_report.py`, outside `phase_a/`); it reports and
reconciles ONLY. The verdict engine remains `phase_a/analyze.py` UNCHANGED
(B.4) over the campaign DB; no row surgery, no post-hoc bucket exclusion
(B.1). Any non-empty `EXCL_KILL_SWITCH_WINDOW`/`EXCL_COLLECTOR_DEGRADED`
class ⇒ HOLD + CONDUCTOR adjudication, not silent filtering.

## 4. Δ1 — HMAC KEY GOVERNANCE

`context_hash` = keyed HMAC-SHA256, first 16 hex chars, exactly
`phase_a/harness.py::context_hash`. The key is the `context_salt` sidecar
(0600, `os.urandom(16)`) created at FIRST collector start after deploy,
in the live data root (§6) — outside the events DB, outside any git repo,
outside any push/sync path; never printed, never in receipts/logs/journal.
Rotation = new campaign only.

## 5. Storage (schema reuse, additive only)

- DB: `phase_a` SCHEMA verbatim via `phase_a.harness.connect()` — tables
  `run_metadata`, `events`, `sweeps` untouched (analyze path compatibility
  by construction). All rows `synthetic=0`, `class='machine'`,
  `resolver='script:next_token_in_top3'`.
- `run_metadata` at deploy: `engine_commit` = exact deployed shim SHA
  (B.1), `scope='ngram_component'`,
  `p_model='live-ngram-component/bigram+unigram-backoff'`,
  `freq_table_hash` computed over the engine's loaded corpus files in the
  `FreqModel.table_hash` format.
- ADDITIVE table `collector_log(id, ts, kind, detail)` created by the shim
  module after `connect()` — `kind ∈ {start, stop, kill_switch_on,
  kill_switch_off, queue_overflow, writer_error, drop_counter}`.
  `phase_a/` never reads it; `shim_report.py` does.
- Live data root: config-driven, default `~/.local/share/smartkey-phasea/`
  — outside both forbidden roots. The collector sets
  `SMARTKEY_PHASEA_DATA` to it and calls `phase_a.paths.data_dir()` so the
  fail-closed guard (A.3) executes BEFORE any DB open; no `-wal`/`-shm` is
  touched on the refusal path (guard raises before `connect()`).

## 6. A.4 — KILL-SWITCH + FAILURE ISOLATION

- Config gate: `smartkey.json` → `"o1_shim": {"enabled": false, "data_dir":
  "~/.local/share/smartkey-phasea"}`. DEFAULT `false` — the shim ships
  dark; the campaign deploy enables it explicitly (Δ4: that deploy starts
  the single-configuration window).
- Runtime kill-switch: presence of `<data_root>/KILL` — polled by the
  writer thread each drain cycle and by the adapter at most once per
  boundary event (single `os.path.exists`); on trigger the tap stops
  (no more snapshots/enqueues), `kill_switch_on` is logged, predictor
  untouched. Removing the file + `kill_switch_off` resumes only on
  explicit operator action (engine restart) — no silent resume.
- Failure isolation: bounded queue (drop-newest + `drop_counter` on
  overflow — never blocks input); writer-thread exception ⇒ tap
  auto-disables, `writer_error` logged, engine/input path unaffected;
  no partial rows (prediction INSERT and resolution UPDATE are each a
  single atomic statement executed only by the writer thread).

## 7. A.3 — DUAL-ROOT FAIL-CLOSED (inherited 465110c2018f(a))

- `phase_a/selftest.py` check 11 EXTENDED to probe BOTH forbidden roots —
  `~/smartkey` AND `~/.config/smartkey` — each via an OBSERVED refusal
  (env set → `data_dir()` raises) before any DB open. This is the only
  `phase_a/` change in the shim diff; the analyze path is untouched.
- The A.5 receipt bundle includes an executed run of both probes
  (declaration/import-guard ≠ proof).

## 8. A.5 — RECEIPT BUNDLE + FOCUSED TESTS (plan)

Bundle: exact base (`b415a950…`) + head SHA, bounded diff stat, frozen
spec hash `68a343b4…`, this design artifact's hash, focused test receipts,
clean runtime log with zero forbidden-root DB attempts, dual-root refusal
probe output, G-FEAT threshold anchor (issued by primary ADVOCATE at
registration — f97b70580a21 §5).

Deploy-procedure condition (pre-registered by primary ADVOCATE
d9c2df7e8c50 after the UX-deploy SIGBUS shutdown coredumps; folds into
the G-FEAT threshold): the shim `.so` swap MUST use (a) atomic
write+rename — new inode, the old process keeps the old inode until exit
(preferred) — OR (b) a confirmed-dead prior engine PID BEFORE the `.so`
write. The A.5 receipt declares which was used with evidence. No new code
is required for this; it binds the operator deploy procedure.

Focused tests (all with `SMARTKEY_PHASEA_DATA` in an isolated temp dir):
  T1 schema/privacy: live rows carry no token columns; resolver/class
     CHECKs hold; `context_hash` ≠ plaintext, salt not in DB.
  T2 semantics parity: Rust `o1_snapshot` top3/p_top3 == `FreqModel`
     top3/p_top3 on identical loaded tables (property test over corpus
     samples, incl. back-off + tie-break + clamp).
  T3 disabled path: `enabled:false` ⇒ zero shim FFI calls, zero DB/file
     writes, adapter action stream byte-identical to base on the
     regression key sequences.
  T4 failure isolation: writer killed mid-run ⇒ input path unaffected,
     tap disabled, `writer_error` logged, no partial rows.
  T5 kill-switch: KILL file ⇒ tap stops within one boundary event;
     predictor output unchanged; log records the interval.
  T6 dual-root refusal: both probes raise BEFORE DB open; no `-wal`/`-shm`
     artifacts appear under either root.
  T7 outcome correctness: scripted commit sequences produce the expected
     outcome bits (hit / miss / unresolved-at-session-end).
  T8 validity/reconciliation: `shim_report` reproduces
     `raw = valid + Σ excluded` on a crafted DB covering every reason code.

## 9. Sequencing / Δ4 (unchanged by this artifact)

Shim deploy is SEPARATE and later than the UX deploy (operator GO
eb47509b959e); the 14-day single-configuration window counts from the shim
deploy (constant engine SHA for the whole window, declared in the final
receipt per Δ4 + fold-gate note (б)). Conjunctive eligibility B.2 and the
integrity-only T+7 stand as folded. Nothing in this lane touches
`~/smartkey` (live), `~/smartkey-ux-cluster` (deploy vehicle), origin, or
any push path.
