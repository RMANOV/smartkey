#!/usr/bin/env python3
"""Regression guard for live doubled-word commits from composing preedit.

The live failure mode was visible as doubled words on word boundary.  The risky
sequence is a full-word composing preedit followed by a Rust action batch:

    HideGhost, CommitText(word), ForwardKey

Hiding a composing preedit does not clear its payload in the IBus client. Some
clients can materialize that retained payload as well as CommitText(word),
doubling the token. The adapter must replace composing preedit with an empty,
invisible buffer before committing. Ghost-only preedit keeps the old order.
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
    engine.update_preedit_text = lambda text, cursor, visible: events.append(
        ("preedit", text, cursor, visible)
    )
    engine.commit_text = lambda text: events.append(("commit", text))
    return engine, events


def main() -> int:
    module = load_engine_module()
    original_text = module.IBus.Text
    original_attr_list = module.IBus.AttrList
    original_attribute = module.IBus.Attribute

    # Exercise the real GI enum/attribute surface before replacing it with
    # deterministic fakes. This is the live regression: older code referenced
    # module-level ATTR_* names that modern IBus GI does not expose.
    native = module.SmartKeyEngine.__new__(module.SmartKeyEngine)
    native_preedits = []
    native.update_preedit_text = (
        lambda text, cursor, visible: native_preedits.append((cursor, visible))
    )
    native._show_composing("контролирай", "те")

    class DummyText:
        def __init__(self, text: str) -> None:
            self.text = text
            self.attrs = None

        @staticmethod
        def new_from_string(text: str) -> "DummyText":
            return DummyText(text)

        def set_attributes(self, attrs) -> None:
            self.attrs = list(attrs)

        def __eq__(self, other) -> bool:
            return isinstance(other, str) and self.text == other

    class DummyAttrList(list):
        pass

    class DummyAttribute:
        @staticmethod
        def new(attr_type, value, start, end):
            return attr_type, value, start, end

    module.IBus.Text = DummyText
    module.IBus.AttrList = DummyAttrList
    module.IBus.Attribute = DummyAttribute
    try:
        composing, composing_events = build_engine(module, "composing")
        consumed = composing._execute_actions(
            [("hide", ""), ("commit", "word"), ("forward", "")]
        )

        ghost, ghost_events = build_engine(module, "ghost")
        ghost._execute_actions([("hide", ""), ("commit", "suffix")])

        offsets = module.SmartKeyEngine.__new__(module.SmartKeyEngine)
        preedits = []
        offsets.update_preedit_text = (
            lambda text, cursor, visible: preedits.append(
                (text.text, cursor, visible, text.attrs)
            )
        )
        offsets._show_composing("контролирай", "те")
        offsets._show_ghost("свят")
    finally:
        module.IBus.Text = original_text
        module.IBus.AttrList = original_attr_list
        module.IBus.Attribute = original_attribute

    problems: list[str] = []
    if native_preedits != [(11, True)]:
        problems.append(f"native IBus composing call failed: {native_preedits!r}")
    if composing_events != [
        ("preedit", "", 0, False),
        ("commit", "word"),
    ]:
        problems.append(
            "composing boundary should clear payload then commit once, got "
            f"{composing_events!r}"
        )
    if consumed is not False:
        problems.append("forward delimiter should still make the key unconsumed")
    if composing._preedit_active or getattr(composing, "_preedit_mode", None) is not None:
        problems.append("composing preedit state should be cleared after commit")
    if ghost_events != ["hide", ("commit", "suffix")]:
        problems.append(f"ghost order should remain hide->commit, got {ghost_events!r}")
    composing_text, composing_cursor, _, composing_attrs = preedits[0]
    if composing_text != "контролирайте" or composing_cursor != 11:
        problems.append(
            "composing cursor must use Unicode characters, got "
            f"text={composing_text!r} cursor={composing_cursor}"
        )
    if any(start > 13 or end > 13 for _, _, start, end in composing_attrs):
        problems.append(f"composing attributes exceed character length: {composing_attrs!r}")
    ghost_text, ghost_cursor, _, ghost_attrs = preedits[1]
    if ghost_text != "свят" or ghost_cursor != 4:
        problems.append(
            f"ghost cursor must use Unicode characters, got {ghost_text!r}/{ghost_cursor}"
        )
    if any(start > 4 or end > 4 for _, _, start, end in ghost_attrs):
        problems.append(f"ghost attributes exceed character length: {ghost_attrs!r}")

    if problems:
        print("FAIL:")
        for problem in problems:
            print("  -", problem)
        return 1
    print(
        "PASS: composing clears before one commit; ghost order and Unicode offsets are valid."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
