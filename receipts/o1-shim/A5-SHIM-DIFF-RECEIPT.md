# O1 SHIM DIFF — A.5 RECEIPT BUNDLE (2026-07-19)

## Exact binding
- Frozen spec: `O1-P1-LIVE-RUN-SHIM-SPEC-2026-07-19.md`
  sha256 `68a343b463b61dd05a6ed672829ccdf77c9d6c6ab25bfdfdcc5364b91b3d7d9c`
  @ vawm-spec `6ca0e57` (FOLD-GATE PASS f97b70580a21).
- Shim base: `b415a9503199782b777675fa90d48220bb9afc92` = merge --no-ff of
  UX tip `0de644d` (double PASS 02ebe263be73 + aec92e020cc6; DEPLOYED live
  8d1c05bb270b) ← O1/P1 harness `5ef79cc` (double gate 16cab66b11cb +
  addendum 465110c2018f; on origin). Base-objection window closed:
  e95dd30a36f8 (CONDUCTOR) + 0c7fd55b181b (primary ADVOCATE).
- CODE HEAD (gate target): `1cc92d1d5dcc882e46807b20fe7d3e8d934d9c2a`.
  This receipt's own commit sits ABOVE the code head and is excluded from
  the code diff metric (stable accounting rule; receipt-only content).
- Bounded code diff `b415a95..1cc92d1`: 12 files, +1677/−8.
  Commits: cce1a24 design → 3687659 Rust surface → 62284d1 design fold →
  d512966 collector+hooks+check-11 → 1cc92d1 report+tests.
- Design artifact (Δ2 latency semantics + Δ3 validity predicate/reason
  codes FROZEN): `docs/O1-SHIM-DESIGN-2026-07-19.md`
  sha256 `fd62b704b25510fe3033b05e7fd4f7880993858de72826dc8db31073f0bd9c1f`.
- Key artifact hashes:
  `ibus/o1_shim.py` 04aaa266c21c9e1505e96b6c75d98ec00b167427333e7b0131c43c1f5fd02350
  `shim_report.py`  b417d8cd005a6cc7af3c1abe6b15e851f4e4c02fc048fec0eaede70ed6fc9c4f
  `crates/smartkey-core/src/o1_shim.rs` 9871a26a9179ce51b602999f5ba66f03ffe29f9999d03c0cc8b1885760736870
  `tests/test_o1_shim.py` 1b860e51f96836c277d94e1280223306fdc2f394dcb7cc3100b555cf805b933a
- Embargo: no remote branch contains the HEAD (`git branch -r --contains`
  = empty); zero push/merge-to-main; live tree `~/smartkey` and deploy
  vehicle worktree `~/smartkey-ux-cluster` untouched.

## Bar coverage
- A.1 passive-only: numbers-only FFI (`o1_snapshot`/`o1_resolve`/
  `o1_abandon`); raw count tables read-only; no ensemble/reranker/
  calibrator import reachable from the shim (check 9 untouched); config
  gate `o1_shim.enabled` DEFAULT FALSE — ships dark; disabled path makes
  zero shim calls (T3) and the UX regression suites are unchanged-green.
- A.2 minimization: persisted record = {ts, HMAC-keyed context_hash(16),
  n_candidates, p_top3, latency_us, outcome∈{0,1}|NULL, resolved_ts};
  candidate words never cross the FFI, never persisted (T1); schema CHECKs
  (resolver `script:`/`sql:`, class `machine`) inherited verbatim.
- A.3 fail-closed: collector guards the data root via
  `phase_a.paths.data_dir()` BEFORE any DB open; refusal creates nothing —
  no db/-wal/-shm (T6); selftest check 11 EXTENDED to dual-root observed
  refusal (`~/smartkey` AND `~/.config/smartkey`) — executed, exit 0.
- A.4 operability: KILL file stops the tap at startup and at runtime
  (T5) without touching the predictor; hot-path or writer failure disables
  the tap, never raises into input (T4); bounded queue drops+counts, no
  partial rows (single-statement INSERT/UPDATE in one writer thread).
- Δ1 HMAC governance: salt = 0600 sidecar created at first collector start
  (deploy), outside DB/repo/push paths, never logged (T1 asserts).
- Δ2 latency semantics: frozen in design §2 (adapter-side monotonic,
  pre-enqueue interval; async write excluded; core_latency_us diagnostics
  only in collector_log).
- Δ3 validity predicate V1–V7 + 8 EXCL_* codes frozen in design §3;
  `shim_report.py` reconciles raw = valid + Σ excluded (T8) and raises
  HOLD flags for kill/degradation classes — analyze path UNCHANGED (B.4:
  `phase_a/analyze.py` byte-identical to the double-gated 5ef79cc).
- Δ4 + deploy procedure: campaign starts at the SHIM deploy (constant
  engine SHA window). Pre-registered SIGBUS condition (d9c2df7e8c50)
  folded: the shim `.so` swap uses (a) atomic write+rename — preferred —
  OR (b) confirmed-dead prior PID before write; the deploy receipt will
  declare which, with evidence. HMAC key generation + `o1_shim.enabled=
  true` + `engine_commit=<deployed SHA>` config stamping happen at the
  operator deploy, not before.

## Test receipts (this worktree, isolated SMARTKEY_PHASEA_DATA temp roots)
- Rust: `cargo test --workspace` = 472 passed / 0 failed across 15 result
  lines (466 base + 6 o1_shim parity units); `cargo clippy --workspace
  --all-targets -- -D warnings` = clean. Log: `03-rust-suite.log`.
- Python: 69 passed / 0 failed = 40 UX+corpus base + 15 O1 harness +
  14 new shim tests (T1–T8 + adapter hook integration).
  Log: `02-python-suite.log`.
- Selftest: `python3 -m phase_a.selftest` exit 0, ALL CHECKS PASS incl.
  `11 data guard refuses live + operator config roots`
  (`live_path_refused=True config_path_refused=True`).
  Log: `01-selftest-dual-root.log`.
- Parity pinning: Rust unit tests and Python T2 evaluate the SAME truth
  table with identical pinned expectations (top3 ordering incl.
  lexicographic ties, p mass 19/22, back-off 240/241); full FFI parity on
  the exact HEAD is exercised by the gate's independent rebuild.

## Declared deviations / judgment calls (honest, none silent)
1. Live latency semantics differ from the LAB pipelined commit-inclusive
   metric — required by Δ2 (async collector); frozen in design §2.
2. OOV learning can add unigram mass to the live tables over a campaign;
   the measured component is the engine's ngram tables AS-IS at prediction
   moment; `freq_table_hash` pins the corpus FILE inputs; the global
   unigram back-off top-3 is compute-once (FreqModel parity).
3. Word-commit coverage: boundary keys (Space/Return/punctuation) + Tab
   accepts; every commit path feeds the collector so a pending snapshot
   resolves only against the immediately next word; focus-out/reset
   abandon (row stays unresolved — counted, never guessed).
4. `collector_log` is an ADDITIVE table created by the shim module;
   `phase_a/` never reads it; the only `phase_a/` change in this diff is
   the check-11 dual-root extension (A.3, inherited 465110c2018f(a)).

## Pending anchors
- G-FEAT threshold: issued by primary ADVOCATE AT THIS REGISTRATION
  (f97b70580a21 §5); expected content = canonical A.1–A.5 + Δ1/Δ2 over
  this receipt + frozen-hash check + SIGBUS deploy-procedure condition.
- After double gate PASS: operator-hand shim deploy (separate GO) →
  14-day conjunctive campaign (≥14 full wall-clock days AND ≥5000 valid
  resolutions from the SAME audited deploy; T+7 integrity-only).
