#!/usr/bin/env python3
"""Regression guard for live doubled-word commits from composing preedit.

The live failure mode was visible as doubled words on word boundary.  The risky
sequence is a full-word composing preedit followed by a Rust action batch:

    HideGhost, CommitText(word), ForwardKey

If the Python adapter hides the composing preedit before CommitText(word), some
clients can materialize the preedit and then receive the same committed word,
doubling the token.  Ghost-only preedit keeps the old order: hide first, then
commit the accepted suffix.
"""

from __future__ import annotations

import importlib
import importlib.util
import os
import sys
import tempfile

LAB = os.environ.get("SMARTKEY_LAB", "/home/rmanov/smartkey-phase-a-lab")
os.environ["SMARTKEY_PHASEA_DATA"] = tempfile.mkdtemp(prefix="phasea_commit_order_")
sys.path.insert(0, LAB)


ENGINE_FILE = os.environ.get("SMARTKEY_ENGINE_FILE")


def load_engine_module():
    if ENGINE_FILE:
        spec = importlib.util.spec_from_file_location("skeng_commit_order", ENGINE_FILE)
        module = importlib.util.module_from_spec(spec)
        assert spec.loader is not None
        spec.loader.exec_module(module)
        return module
    return importlib.import_module("ibus.smartkey_engine")


def build_engine(engine_module, mode: str):
    engine = engine_module.SmartKeyEngine.__new__(engine_module.SmartKeyEngine)
    events: list[tuple[str, str] | str] = []
    engine._preedit_active = True
    engine._preedit_mode = mode
    engine._phase_a = None
    engine._active_prediction = None
    engine._prediction_seq = 0
    engine._caps = 0
    engine.hide_preedit_text = lambda: events.append("hide")
    engine.commit_text = lambda text: events.append(("commit", text))
    return engine, events


def main() -> int:
    module = load_engine_module()
    original_text = module.IBus.Text

    class DummyText:
        @staticmethod
        def new_from_string(text: str) -> str:
            return text

    module.IBus.Text = DummyText
    try:
        composing, composing_events = build_engine(module, "composing")
        consumed = composing._execute_actions(
            [("hide", ""), ("commit", "word"), ("forward", "")]
        )

        ghost, ghost_events = build_engine(module, "ghost")
        ghost._execute_actions([("hide", ""), ("commit", "suffix")])
    finally:
        module.IBus.Text = original_text

    problems: list[str] = []
    if composing_events != [("commit", "word"), "hide"]:
        problems.append(
            "composing order should be commit->hide, got "
            f"{composing_events!r}"
        )
    if consumed is not False:
        problems.append("forward delimiter should still make the key unconsumed")
    if composing._preedit_active or getattr(composing, "_preedit_mode", None) is not None:
        problems.append("composing preedit state should be cleared after commit")
    if ghost_events != ["hide", ("commit", "suffix")]:
        problems.append(f"ghost order should remain hide->commit, got {ghost_events!r}")

    if problems:
        print("FAIL:")
        for problem in problems:
            print("  -", problem)
        return 1
    print("PASS: composing commit is ordered commit->hide; ghost commit remains hide->commit.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
