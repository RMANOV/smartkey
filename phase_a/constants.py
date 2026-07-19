"""Single source of truth for n-gram calibration thresholds."""

from __future__ import annotations

# --- Measured scope ----------------------------------------------------------
# The harness measures ONLY the n-gram component of the predictor. The 3-stage
# ensemble (Hedge / calibrator / neural reranker / PPM / BPE) is NOT in the
# measured path. This is a structural firewall, enforced by construction: the
# measured modules never import the ensemble / PyO3 binding at all.
PREDICTION_SCOPE = "ngram_component"

# --- Scope warnings appended to every report --------------------------------
# A green gate proves n-gram plumbing
#     calibration, NOT that the live 3-stage ensemble/predictor is calibrated.
FIREWALL_LINE_SCOPE = (
    "Phase-A green ≠ ensemble/live-predictor calibration "
    "(prediction_scope=ngram_component ONLY: no Hedge/calibrator/neural-reranker/"
    "PPM/BPE in the measured path)."
)
FIREWALL_LINE_TIER = "Phase-A green ≠ proof of team tier."

# Every report ends with BOTH lines, tier last.
FIREWALL_LINES = (FIREWALL_LINE_SCOPE, FIREWALL_LINE_TIER)

# --- Schema invariants -------------------------------------------------------
# Phase-A rows admit resolver only with a 'script:' or 'sql:' prefix; enforced
# by a DB CHECK constraint (structural), not prose.
RESOLVER = "script:next_token_in_top3"
EVENT_CLASS = "machine"
ALLOWED_RESOLVER_PREFIXES = ("script:", "sql:")

# --- Latency budget ----------------------------------------------------------
LATENCY_BUDGET_US = 20_000

# --- Gate thresholds ---------------------------------------------------------
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

# --- Bucketing ---------------------------------------------------------------
N_QUANTILE_BUCKETS = 5
MIN_BUCKET_RESOLUTIONS = 30  # a score-half bucket with fewer merges left
MIN_SURVIVING_BUCKETS = 2
MAX_SURVIVING_BUCKETS = 5

# --- Split -------------------------------------------------------------------
FIT_FRACTION = 0.50

# --- Watchdog ----------------------------------------------------------------
WATCHDOG_MAX_AGE_H = 48

# --- Volume-rate window ------------------------------------------------------
# In LAB conditions there is no live 14-day typing window; the corpus-replay
# driver stamps events across a simulated 14-day span so the >=5000/14d volume
# gate is computed against harness-collected local data.
VOLUME_WINDOW_DAYS = 14

HARNESS_VERSION = "o1-phase-a-harness/1.0.0"
