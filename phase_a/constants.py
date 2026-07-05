"""Shared constants for the smartkey Phase-A predictive-loop harness.

Single source of truth for the values that the hash-committed spec
(PREDICTIVE-OODA-FORMALIZATION-2026-07-05.md §12.4, git 6403096b) pins down.
Nothing here may be reinterpreted after run 1 — the git hash is the receipt.
"""

from __future__ import annotations

# --- Firewall line [amendment d]: verbatim, ends every Phase-A report. --------
FIREWALL_LINE = "Phase-A зелено ≠ доказателство за team tier."

# --- Schema invariants [blocker 3 / Codex major] ------------------------------
# resolver is fixed for Phase-A; the no-LLM schema invariant only admits
# resolvers with a 'script:' or 'sql:' prefix (enforced by a DB CHECK and
# re-verified by the nightly audit — not by prose).
RESOLVER = "script:next_token_in_top3"
EVENT_CLASS = "machine"
ALLOWED_RESOLVER_PREFIXES = ("script:", "sql:")

# --- Latency budget -----------------------------------------------------------
LATENCY_BUDGET_US = 20_000  # 20 ms per event (spec gate boundary)

# --- Gate thresholds [blocker 1] ---------------------------------------------
# PASS   : |mean_p - hit_rate| <= 0.05 in EVERY surviving bucket
#          AND resolutions >= 5000 AND 0 missed watchdog/sweep
#          AND <= 1% events > 20 ms
# FAIL   : |mean_p - hit_rate| > 0.10 in >= 3 buckets
#          OR > 5% unresolved OR > 1% > 20 ms
#          OR missed watchdog without in-band alarm
# INCONCLUSIVE : everything else
GATE_PASS_GAP = 0.05
GATE_FAIL_GAP = 0.10
GATE_FAIL_BUCKETS = 3
MIN_RESOLUTIONS = 5000
MAX_UNRESOLVED_FRAC = 0.05
MAX_OVER_BUDGET_FRAC = 0.01

# --- Bucketing [blocker 2] ----------------------------------------------------
N_QUANTILE_BUCKETS = 5
MIN_BUCKET_RESOLUTIONS = 30  # a score-half bucket with fewer merges left
MIN_SURVIVING_BUCKETS = 2
MAX_SURVIVING_BUCKETS = 5

# --- Split --------------------------------------------------------------------
# First 50% of resolutions by ts = calibration fit; second 50% = scoring.
# No post-hoc re-splitting (git hash = receipt).
FIT_FRACTION = 0.50

# --- Watchdog -----------------------------------------------------------------
WATCHDOG_MAX_AGE_H = 48  # a sweep receipt older than this raises a visible alarm

HARNESS_VERSION = "phase-a-harness/1.0.0"
