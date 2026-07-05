"""smartkey Phase-A predictive-loop harness (spec §12.4, git 6403096b).

Isolated, observational instrumentation: logs one calibration event per
next-word prediction, splits/buckets/isotonically-calibrates offline, and gates
PASS/FAIL/INCONCLUSIVE mechanically. No LLM, no embeddings, no neural inference
(§12.3) — counts, division, and sklearn IsotonicRegression only.
"""

from __future__ import annotations

from .constants import FIREWALL_LINE, RESOLVER
from .engine_adapter import PhaseAAdapter
from .freqmodel import FreqModel
from .harness import PhaseALogger

__all__ = ["FIREWALL_LINE", "RESOLVER", "PhaseAAdapter", "FreqModel", "PhaseALogger"]
