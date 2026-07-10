#!/usr/bin/env python3
"""Regression guard for plaintext-free live callback error receipts."""

from __future__ import annotations

import importlib
import json
import os
import sys
import tempfile
from pathlib import Path

LAB = os.environ.get("SMARTKEY_LAB", "/home/rmanov/smartkey-phase-a-lab")
os.environ["SMARTKEY_PHASEA_DATA"] = tempfile.mkdtemp(prefix="phasea_error_trace_")
sys.path.insert(0, LAB)


def main() -> int:
    module = importlib.import_module("ibus.smartkey_engine")
    engine = module.SmartKeyEngine.__new__(module.SmartKeyEngine)
    path = Path(os.environ["SMARTKEY_PHASEA_DATA"]) / "error_trace.jsonl"
    engine._phase_a_error_trace = path

    secret = "must-not-enter-error-trace"

    def fail(*_args):
        raise RuntimeError(secret)

    engine._process_key_event = fail
    result = engine.do_process_key_event(module.IBus.KEY_space, 57, 0)

    problems: list[str] = []
    if result is not False:
        problems.append("callback failure must return False so raw typing can continue")
    if not path.exists():
        problems.append("error trace was not written")
        rows = []
    else:
        raw = path.read_text(encoding="utf-8")
        rows = [json.loads(line) for line in raw.splitlines() if line.strip()]
        if secret in raw:
            problems.append("exception message leaked into error trace")
    if len(rows) != 1:
        problems.append(f"expected one error row, got {len(rows)}")
    elif rows[0].get("error_type") != "RuntimeError" or not rows[0].get("frames"):
        problems.append(f"incomplete error receipt: {rows[0]!r}")

    if problems:
        print("FAIL:")
        for problem in problems:
            print("  -", problem)
        return 1
    print("PASS: callback failure preserves raw typing and stores plaintext-free frames.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
