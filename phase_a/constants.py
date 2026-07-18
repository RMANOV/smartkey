"""Hash-committed constants for the O1 / Phase-A n-gram calibration harness.

Single source of truth for the values the frozen spec pins down:

  O1-smartkey-calibration.md
    sha256 48251199ca6fcad07cbda2cbc1d4db2ab7bf7077512641bc6a49290053f7b319
    (vawm-spec @ a158ea1), which enters PREDICTIVE-OODA-FORMALIZATION-2026-07-05
    v0.8 §12.4 (Phase-A value gate).

Nothing here may be reinterpreted after run 1 — the git hash is the receipt
(§4: "Gate-text + p-source version are hash-pinned"; §8: "does not reinterpret
gate text after run 1").
"""

from __future__ import annotations

# --- Frozen scope [freeze-receipt / audit] -----------------------------------
# The harness measures ONLY the n-gram component of the predictor. The 3-stage
# ensemble (Hedge / calibrator / neural reranker / PPM / BPE) is NOT in the
# measured path. This is a structural firewall, enforced by construction: the
# measured modules never import the ensemble / PyO3 binding at all.
PREDICTION_SCOPE = "ngram_component"

# --- Firewall lines: verbatim, end every Phase-A report ----------------------
# (1) O1 scope firewall [task freeze]: a green gate proves n-gram plumbing
#     calibration, NOT that the live 3-stage ensemble/predictor is calibrated.
FIREWALL_LINE_SCOPE = (
    "Phase-A green ≠ ensemble/live-predictor calibration "
    "(prediction_scope=ngram_component ONLY: no Hedge/calibrator/neural-reranker/"
    "PPM/BPE in the measured path)."
)
# (2) Spec §8 tier firewall (verbatim): green is inadmissible as team-tier proof.
FIREWALL_LINE_TIER = "Phase-A green ≠ proof of team tier."

# Every report ends with BOTH lines, tier last.
FIREWALL_LINES = (FIREWALL_LINE_SCOPE, FIREWALL_LINE_TIER)

# --- Schema invariants [G-FEAT §6(ii) no-LLM-in-loop] ------------------------
# Phase-A rows admit resolver only with a 'script:' or 'sql:' prefix; enforced
# by a DB CHECK constraint (structural), not prose.
RESOLVER = "script:next_token_in_top3"
EVENT_CLASS = "machine"
ALLOWED_RESOLVER_PREFIXES = ("script:", "sql:")

# --- Latency budget [§12.4] --------------------------------------------------
LATENCY_BUDGET_US = 20_000  # 20 ms per decision (spec gate boundary)

# --- Gate thresholds [§5 / §12.4, hash-committed] ----------------------------
# PASS = |mean_p - hit_rate| <= 0.05 in EVERY surviving bucket
#        AND resolutions >= 5000 / 14d AND 0 missed watchdog AND <= 1% > 20ms
# FAIL = |mean_p - hit_rate| > 0.10 in >= 3 buckets OR > 5% unresolved
#        OR > 1% > 20ms OR missed watchdog without in-band alarm
# INCONCLUSIVE = otherwise
GATE_PASS_GAP = 0.05
GATE_FAIL_GAP = 0.10
GATE_FAIL_BUCKETS = 3
MIN_RESOLUTIONS = 5000
MAX_UNRESOLVED_FRAC = 0.05
MAX_OVER_BUDGET_FRAC = 0.01

# --- Bucketing [§2 offline scorer, fixed BEFORE run 1] -----------------------
N_QUANTILE_BUCKETS = 5
MIN_BUCKET_RESOLUTIONS = 30  # a score-half bucket with fewer merges left
MIN_SURVIVING_BUCKETS = 2
MAX_SURVIVING_BUCKETS = 5

# --- Split [§2: first 50% by ts = FIT, second 50% = SCORING] -----------------
FIT_FRACTION = 0.50

# --- Watchdog ----------------------------------------------------------------
WATCHDOG_MAX_AGE_H = 48

# --- Volume-rate window [§5: >= 5000 resolutions / 14 days] ------------------
# In LAB conditions there is no live 14-day typing window; the corpus-replay
# driver stamps events across a simulated 14-day span so the >=5000/14d volume
# gate is computed against harness-collected lab data (documented in README-O1).
VOLUME_WINDOW_DAYS = 14

HARNESS_VERSION = "o1-phase-a-harness/1.0.0"
SPEC_SHA256 = "48251199ca6fcad07cbda2cbc1d4db2ab7bf7077512641bc6a49290053f7b319"
