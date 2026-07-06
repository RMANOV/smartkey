#!/usr/bin/env python3
"""Standalone tests for offline predictive OODA + keyboard-neighbour distance."""

from __future__ import annotations

import json
import os
import sys
import tempfile

LAB = os.environ.get("SMARTKEY_LAB", "/home/rmanov/smartkey-phase-a-lab")
os.environ["SMARTKEY_PHASEA_DATA"] = tempfile.mkdtemp(prefix="phasea_ooda_")
sys.path.insert(0, LAB)

from phase_a.keyboard_distance import (  # noqa: E402
    are_adjacent_keys,
    keyboard_weighted_distance,
    nearest_keyboard_candidates,
)
from phase_a.predictive_ooda import snapshot  # noqa: E402


def main() -> int:
    problems: list[str] = []

    if not are_adjacent_keys("g", "h", layout="en"):
        problems.append("EN adjacency failed: g/h should be neighbours")
    if not are_adjacent_keys("л", "к", layout="bg"):
        problems.append("BG adjacency failed: л/к should be neighbours")

    dist = keyboard_weighted_distance("gello", "hello", layout="en")
    if dist.distance >= 0.5 or dist.adjacent_substitutions != 1:
        problems.append(
            f"keyboard distance failed for gello->hello: "
            f"distance={dist.distance} adj={dist.adjacent_substitutions}"
        )

    nearest = nearest_keyboard_candidates("лолко", {"колко": 100, "много": 90}, layout="bg")
    if not nearest or nearest[0].candidate != "колко":
        problems.append(f"BG nearest failed: got {[n.candidate for n in nearest]}")

    snap = snapshot(token="gello", lexicon={"hello": 100, "yellow": 10}, layout="en")
    if snap.decide["decision"] != "suggest_adjacent_key_candidate":
        problems.append(f"OODA EN decision failed: {snap.decide}")
    if snap.act["live_engine_mutation"] is not False:
        problems.append("OODA act guardrail failed: live_engine_mutation must be false")

    known = snapshot(token="hello", lexicon={"hello": 100}, layout="en")
    if known.decide["decision"] != "accept_known_token":
        problems.append(f"OODA known-token decision failed: {known.decide}")

    weak = snapshot(token="zzzzz", lexicon={"hello": 100}, layout="en", max_distance=1.0)
    if weak.decide["decision"] != "hold_no_safe_correction":
        problems.append(f"OODA weak-candidate guard failed: {weak.decide}")

    # JSON serialisation is part of the CLI contract.
    json.loads(snap.to_json())

    if problems:
        print("FAIL:")
        for p in problems:
            print("  -", p)
        return 1
    print("PASS: predictive OODA test mode ranks adjacent-key motor errors without live mutation.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
