"""O1 / SmartKey Phase-A n-gram top-3 CALIBRATION harness.

Frozen spec: O1-smartkey-calibration.md
  (sha256 48251199ca6fcad07cbda2cbc1d4db2ab7bf7077512641bc6a49290053f7b319,
   vawm-spec @ a158ea1), entering PREDICTIVE-OODA-FORMALIZATION-2026-07-05 §12.4.

Scope (FROZEN): prediction_scope = ngram_component ONLY. The harness wraps and
measures the raw n-gram component of the predictor — NOT the 3-stage ensemble
(no Hedge / calibrator / neural reranker / PPM / BPE in the measured path). The
firewall is structural: nothing under the measured path imports the ensemble or
the ``smartkey_py`` PyO3 binding. Green here is NOT ensemble/live-predictor
calibration and is NOT proof of team tier.

Pipeline: raw n-gram top-3 + p_top3 (counts only) -> append-only Phase-A event
log -> 50/50 ts split -> 5 quantile buckets (min-30 merge) -> isotonic map on
the FIT half -> per-bucket |mean_p - hit_rate| gate on the SCORING half. No LLM,
no embeddings, no neural inference — counts, division, IsotonicRegression only.
"""

from __future__ import annotations

from .constants import (
    FIREWALL_LINE_SCOPE,
    FIREWALL_LINE_TIER,
    FIREWALL_LINES,
    PREDICTION_SCOPE,
    RESOLVER,
)
from .freqmodel import FreqModel
from .harness import PhaseALogger

__all__ = [
    "FIREWALL_LINE_SCOPE",
    "FIREWALL_LINE_TIER",
    "FIREWALL_LINES",
    "PREDICTION_SCOPE",
    "RESOLVER",
    "FreqModel",
    "PhaseALogger",
]
