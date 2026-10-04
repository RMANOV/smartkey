#!/usr/bin/env python3
"""Keep ordinary keyboard input alive while an older Rust module is installed.

The Python adapter and the native extension can be upgraded independently in a
local IBus session.  A stale extension may have ``handle_key`` but not the
newer ``process_keycode``/FFI helpers; that must degrade to normal keyval input,
not turn every browser key event into an exception.
"""

from __future__ import annotations

import importlib.util
from pathlib import Path


LAB = Path(__file__).resolve().parents[1]
ENGINE = LAB / "ibus" / "smartkey_engine.py"


def load_engine():
    spec = importlib.util.spec_from_file_location("smartkey_legacy_fallback", ENGINE)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class LegacyCore:
    def __init__(self) -> None:
        self.keys: list[tuple[int, int]] = []

    def handle_key(self, _keyval: int, _state: int):
        self.keys.append((_keyval, _state))
        return [("forward", "")]


def main() -> int:
    module = load_engine()
    problems: list[str] = []

    if module._decode_replace_payload_fallback("4\x1fTEST") != (4, "TEST"):
        problems.append("ReplaceWord fallback decoder failed")
    if module._decode_composing_payload_fallback("hel\x00lo") != ("hel", "lo"):
        problems.append("ShowComposing fallback decoder failed")

    engine = module.SmartKeyEngine.__new__(module.SmartKeyEngine)
    legacy = LegacyCore()
    engine._core = legacy
    engine._phase_a = None
    engine._phase_a_action_trace = None
    engine._phase_a_prediction_trace = None
    engine._phase_a_callback_trace = None
    engine._phase_a_error_trace = None
    engine._preedit_active = False
    engine._preedit_mode = None
    engine._active_prediction = None
    engine._prediction_seq = 0
    engine._caps = 0
    engine._refresh_surrounding_text = lambda: None
    seen: list[list[tuple[str, str]]] = []
    engine._execute_actions = lambda actions: (seen.append(actions), False)[1]

    # Hardware keycode is present, but the legacy core has no process_keycode.
    # The adapter must use handle_key(keyval, state) and let the browser receive
    # the ordinary key event.
    for char in "a 3":
        result = engine._process_key_event(ord(char), 30, 0)
        if result is not False:
            problems.append(f"legacy keyval path returned {result!r}, expected False")
    if seen != [[("forward", "")]] * 3:
        problems.append(f"legacy keyval path actions were {seen!r}")
    if legacy.keys != [(ord("a"), 0), (ord(" "), 0), (ord("3"), 0)]:
        problems.append(f"legacy core received {legacy.keys!r}")

    if problems:
        print("FAIL:")
        for problem in problems:
            print("  -", problem)
        return 1
    print("PASS: stale core falls back to ordinary keyval typing without consuming the browser key.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
