#!/usr/bin/env python3
"""Replay the failed live smoke through the real core and IBus adapter."""

from __future__ import annotations

import importlib
import importlib.util
import os
import sys
import tempfile
from pathlib import Path

LAB = Path(os.environ.get("SMARTKEY_LAB", "/home/rmanov/smartkey-phase-a-lab"))
os.environ["SMARTKEY_PHASEA_DATA"] = tempfile.mkdtemp(prefix="phasea_phrase_")
sys.path.insert(0, str(LAB))

from smartkey_py import PyInputMethodCore  # noqa: E402


def load_engine_module():
    engine_file = os.environ.get("SMARTKEY_ENGINE_FILE")
    if not engine_file:
        return importlib.import_module("ibus.smartkey_engine")
    spec = importlib.util.spec_from_file_location("skeng_phrase_replay", engine_file)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


def ibus_text(value: object) -> str:
    if isinstance(value, str):
        return value
    get_text = getattr(value, "get_text", None)
    if get_text is not None:
        text = get_text()
        if isinstance(text, str):
            return text
    raise TypeError(f"unsupported IBus text object: {type(value).__name__}")


class Client:
    def __init__(self) -> None:
        self.text = ""
        self.preedit = ""
        self.deletes: list[tuple[int, int]] = []

    def update_preedit(self, value: object, _cursor: int, visible: bool) -> None:
        self.preedit = ibus_text(value) if visible else ""

    def hide_preedit(self) -> None:
        self.preedit = ""

    def commit(self, value: object) -> None:
        self.text += ibus_text(value)
        self.preedit = ""

    def delete(self, offset: int, length: int) -> None:
        self.deletes.append((offset, length))
        if offset != -length or length > len(self.text):
            raise AssertionError(f"invalid surrounding delete: {offset}/{length}")
        self.text = self.text[:-length]


def build_engine(module, core: PyInputMethodCore, client: Client):
    engine = module.SmartKeyEngine.__new__(module.SmartKeyEngine)
    engine._core = core
    engine._phase_a = None
    engine._phase_a_action_trace = None
    engine._phase_a_prediction_trace = None
    engine._preedit_active = False
    engine._preedit_mode = None
    engine._active_prediction = None
    engine._prediction_seq = 0
    engine._caps = 0x20  # SURROUNDING_TEXT
    engine.update_preedit_text = client.update_preedit
    engine.hide_preedit_text = client.hide_preedit
    engine.commit_text = client.commit
    engine.delete_surrounding_text = client.delete
    engine.forward_key_event = lambda *_args: None
    engine._track_prediction_shown = lambda *_args: None
    return engine


def load_live_corpus(core: PyInputMethodCore) -> None:
    config_dir = Path.home() / ".config" / "smartkey"
    files = sorted(config_dir.glob("corpus_*.json"))
    if not files:
        files = sorted((LAB / "corpus").glob("corpus_*.json"))
    for path in files:
        core.load_corpus_file(str(path))


def main() -> int:
    module = load_engine_module()
    core = PyInputMethodCore(None)
    load_live_corpus(core)
    client = Client()
    engine = build_engine(module, core, client)

    # Exact key-press sequence from the 2026-07-10 failed smoke. It includes
    # the operator's initial correction and the retyped beginning of "вече".
    keycodes = [
        37, 24, 38, 37, 24, 57, 57, 14, 18, 57, 14, 14, 14,
        25, 21, 20, 23, 57,
        17, 18, 14, 14, 17, 18, 41, 18, 57,
        25, 19, 24, 48, 17, 30, 50, 18, 57,
        32, 30, 57,
        33, 23, 37, 31, 49, 18, 50, 57,
        37, 38, 30, 17, 23, 30, 20, 22, 19, 30, 20, 30, 57,
    ]

    for keycode in keycodes:
        preedit_was_active = engine._preedit_active
        consumed = engine._execute_actions(core.process_keycode(keycode, 0))
        if keycode == 14 and preedit_was_active:
            consumed = True
        if consumed:
            continue
        if keycode == 57:
            client.text += " "
        elif keycode == 14:
            client.text = client.text[:-1]
        else:
            raise AssertionError(f"unexpected forwarded keycode: {keycode}")

    expected = "колко пъти вече пробваме да фикснем клавиатурата "
    problems: list[str] = []
    if client.text != expected:
        problems.append(f"committed text mismatch: {client.text!r}")
    if client.deletes:
        problems.append(f"composing replay deleted committed text: {client.deletes!r}")

    if problems:
        print("FAIL:")
        for problem in problems:
            print("  -", problem)
        return 1
    print("PASS: failed live phrase replays exactly without deletion or duplication.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
