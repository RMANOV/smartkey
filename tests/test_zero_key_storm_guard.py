#!/usr/bin/env python3
"""Regression guard for the live keyval=0/keycode=240 IBus storm."""

from __future__ import annotations

import importlib
import importlib.util
import os
import sys
import tempfile
from pathlib import Path

LAB = os.environ.get("SMARTKEY_LAB", "/home/rmanov/smartkey-phase-a-lab")
os.environ["SMARTKEY_PHASEA_DATA"] = tempfile.mkdtemp(prefix="phasea_zero_key_")
sys.path.insert(0, LAB)

from phase_a.paths import data_dir  # noqa: E402


ENGINE_FILE = os.environ.get("SMARTKEY_ENGINE_FILE")


def load_engine_module():
    if ENGINE_FILE:
        spec = importlib.util.spec_from_file_location("skeng_zero_key", ENGINE_FILE)
        module = importlib.util.module_from_spec(spec)
        assert spec.loader is not None
        spec.loader.exec_module(module)
        return module
    return importlib.import_module("ibus.smartkey_engine")


class CoreMustNotBeCalled:
    def process_keycode(self, *_args):
        raise AssertionError("spurious zero-key event must not reach Rust core")

    def handle_key(self, *_args):
        raise AssertionError("spurious zero-key event must not reach Rust core")


def build_engine(module):
    engine = module.SmartKeyEngine.__new__(module.SmartKeyEngine)
    engine._core = CoreMustNotBeCalled()
    engine._phase_a = None
    engine._phase_a_action_trace = data_dir() / "zero_key_action_trace.jsonl"
    engine._phase_a_callback_trace = data_dir() / "zero_key_callback_trace.jsonl"
    engine._phase_a_prediction_trace = None
    engine._preedit_active = False
    engine._preedit_mode = None
    engine._active_prediction = None
    engine._prediction_seq = 0
    engine._caps = 0
    engine._refresh_surrounding_text = lambda: None
    engine._phase_a_observe = lambda: None
    return engine


def main() -> int:
    module = load_engine_module()
    engine = build_engine(module)
    consumed = engine.do_process_key_event(0, 240, 16)
    consumed_release_like = engine.do_process_key_event(0, 240, 272)

    problems: list[str] = []
    if consumed is not True or consumed_release_like is not True:
        problems.append("spurious zero-key events must be consumed")
    if Path(engine._phase_a_action_trace).exists():
        problems.append("spurious zero-key events must not flood action trace")
    if Path(engine._phase_a_callback_trace).exists():
        problems.append("spurious zero-key events must not flood callback trace")

    if problems:
        print("FAIL:")
        for problem in problems:
            print("  -", problem)
        return 1
    print("PASS: zero-key storm is consumed before Rust/core and trace logging.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
