"""Tests for the keystroke diagnostic trace (spec 2026-07-12, Part B).

Integration tests drive the REAL ``do_process_key_event -> _execute_actions``
seam (the false-green lesson) with the trace attached, then read the flushed
JSONL back.  Privacy tests pin the hard guardrails: OFF is a strict no-op,
structural level carries no content, the debug dir can never resolve inside
the repo.
"""

from __future__ import annotations

import json
import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

# Reuse the fake-IBus harness (forces the module's _FakeIBus fallback).
from test_accept_backspace import build_engine, ske  # noqa: E402,F401

import smartkey_debug
from smartkey_debug import (
    LEVEL_FULL,
    LEVEL_OFF,
    LEVEL_STRUCTURAL,
    KeystrokeTrace,
    lang_class,
)

_KEY_TAB = ske.IBus.KEY_Tab
_KEY_BACKSPACE = ske.IBus.KEY_BackSpace
_KEY_ESCAPE = ske.IBus.KEY_Escape


def _attach_trace(eng, tmp_path: Path, level: int) -> KeystrokeTrace:
    trace = KeystrokeTrace(level=level, directory=tmp_path)
    assert trace.enabled
    eng._trace = trace
    return trace


def _read_events(trace: KeystrokeTrace) -> list[dict]:
    trace.flush()
    assert trace._file is not None
    lines = trace._file.read_text(encoding="utf-8").splitlines()
    return [json.loads(line) for line in lines if line]


# --- OFF is a strict no-op -----------------------------------------------------
def test_off_level_creates_nothing_and_emits_nothing(tmp_path):
    trace = KeystrokeTrace(level=LEVEL_OFF, directory=tmp_path / "dbg")
    assert not trace.enabled
    trace.begin_keystroke()
    trace.emit("key_in", keyval=1, payload_text="secret")
    trace.flush()
    assert not (tmp_path / "dbg").exists(), "OFF must create no files/dirs"


def test_off_engine_path_is_noop(tmp_path):
    # Engine with _trace disabled: keystrokes leave zero files behind.
    eng, _rec = build_engine(scripts=[[("composing", "hel\x00lo")]])
    eng._trace = KeystrokeTrace(level=LEVEL_OFF, directory=tmp_path / "dbg")
    assert eng.do_process_key_event(ord("l"), 38, 0) is True
    assert not (tmp_path / "dbg").exists()


# --- The real seam is traced ---------------------------------------------------
def test_trace_records_real_dispatch_seam(tmp_path):
    eng, _rec = build_engine(
        scripts=[
            [("composing", "hel\x00lo")],  # composing keystroke
            [("hide", ""), ("commit", "hello")],  # Tab accept
            [("hide", ""), ("forward", "")],  # backspace forwarded by core
        ]
    )
    trace = _attach_trace(eng, tmp_path, LEVEL_FULL)

    assert eng.do_process_key_event(ord("l"), 38, 0) is True
    eng.do_process_key_event(_KEY_TAB, 23, 0)
    assert eng.do_process_key_event(_KEY_BACKSPACE, 22, 0) is False

    events = _read_events(trace)
    by_seq: dict[int, list[dict]] = {}
    for event in events:
        by_seq.setdefault(event["seq"], []).append(event)
    assert set(by_seq) == {1, 2, 3}, f"one seq group per keystroke, got {by_seq.keys()}"

    seq1 = {e["event"]: e for e in by_seq[1]}
    assert seq1["key_in"]["keyname"] == "l"
    assert seq1["core"]["verdict"] == "consume"
    assert any(
        e["event"] == "action" and e["action_name"] == "composing"
        for e in by_seq[1]
    )

    seq2 = {e["event"]: e for e in by_seq[2]}
    assert seq2["key_in"]["keyname"] == "Tab"
    assert seq2["accept"]["committed_text"] == "hello"
    assert seq2["accept"]["typed_prefix"] == "hel"
    assert seq2["outcome"]["committed_to_app"] == "hello"

    seq3 = {e["event"]: e for e in by_seq[3]}
    assert seq3["core"]["verdict"] == "forward", "core-forwarded backspace"
    assert seq3["outcome"]["committed_to_app_len"] == 0


def test_escape_keystroke_traced(tmp_path):
    eng, _rec = build_engine(
        scripts=[
            [("composing", "hel\x00lo")],
            [("composing", "hel\x00")],  # Escape keeps the typed word
        ]
    )
    trace = _attach_trace(eng, tmp_path, LEVEL_STRUCTURAL)
    eng.do_process_key_event(ord("l"), 38, 0)
    eng.do_process_key_event(_KEY_ESCAPE, 1, 0)

    events = _read_events(trace)
    escape_group = [e for e in events if e["seq"] == 2]
    names = {e["event"] for e in escape_group}
    assert {"key_in", "core", "action", "outcome"} <= names
    key_in = next(e for e in escape_group if e["event"] == "key_in")
    assert key_in["keyname"] == "Escape"


# --- Privacy: structural hides content, full shows it ---------------------------
def test_structural_level_has_no_content_fields(tmp_path):
    eng, _rec = build_engine(
        scripts=[
            [("composing", "привет\x00е")],
            [("hide", ""), ("commit", "привете")],
        ]
    )
    trace = _attach_trace(eng, tmp_path, LEVEL_STRUCTURAL)
    eng._surrounding_text = "тайна"
    eng.do_process_key_event(ord("t"), 20, 0)
    eng.do_process_key_event(_KEY_TAB, 23, 0)

    raw = trace._file  # noqa: SLF001
    events = _read_events(trace)
    text = raw.read_text(encoding="utf-8")
    for verbatim in ("привет", "тайна"):
        assert verbatim not in text, f"structural level leaked content: {verbatim}"
    accept = next(e for e in events if e["event"] == "accept")
    assert "committed_text" not in accept and "typed_prefix" not in accept
    # lang classes + lengths are ALWAYS present (they catch the Space-inject).
    assert accept["committed_lang"] == "cyr"
    assert accept["typed_lang"] == "cyr"
    outcome = [e for e in events if e["event"] == "outcome"][-1]
    assert outcome["committed_to_app_len"] == len("привете")
    assert "committed_to_app" not in outcome


def test_full_level_carries_content(tmp_path):
    eng, _rec = build_engine(
        scripts=[
            [("composing", "hel\x00lo")],
            [("hide", ""), ("commit", "hello")],
        ]
    )
    trace = _attach_trace(eng, tmp_path, LEVEL_FULL)
    eng.do_process_key_event(ord("l"), 38, 0)
    eng.do_process_key_event(_KEY_TAB, 23, 0)
    events = _read_events(trace)
    accept = next(e for e in events if e["event"] == "accept")
    assert accept["committed_text"] == "hello"


def test_lang_class_catches_space_inject_signature(tmp_path):
    # The live bug: Bulgarian typing, an ENGLISH prediction committed.
    # Even at STRUCTURAL level the lang mismatch must be visible.
    eng, _rec = build_engine(
        scripts=[
            [("composing", "прив\x00ет")],
            [("hide", ""), ("commit", "hello"), ("forward", "")],
        ]
    )
    trace = _attach_trace(eng, tmp_path, LEVEL_STRUCTURAL)
    eng.do_process_key_event(ord("p"), 25, 0)
    eng.do_process_key_event(ske.IBus.KEY_space, 57, 0)
    events = _read_events(trace)
    accept = next(e for e in events if e["event"] == "accept")
    assert accept["committed_lang"] == "lat"
    assert accept["typed_lang"] == "cyr"
    assert accept["committed_lang"] != accept["typed_lang"], "inject signature"


# --- Debug dir guardrails --------------------------------------------------------
def test_debug_dir_inside_repo_is_refused(monkeypatch):
    repo = smartkey_debug.repo_root()
    assert repo is not None, "tests run inside the repo"
    monkeypatch.setenv("SMARTKEY_DEBUG_DIR", str(repo / "keystroke-logs"))
    resolved = smartkey_debug.resolve_dir().resolve()
    assert not resolved.is_relative_to(repo), "in-repo debug dir must be refused"
    assert resolved == smartkey_debug.default_dir().resolve()


def test_default_debug_dir_outside_repo_and_gitignored():
    repo = smartkey_debug.repo_root()
    assert repo is not None
    default = smartkey_debug.default_dir().resolve()
    assert not default.is_relative_to(repo)
    gitignore = (repo / ".gitignore").read_text(encoding="utf-8")
    assert "*.jsonl" in gitignore or "keystroke" in gitignore or "debug" in gitignore.lower(), (
        ".gitignore must carry a debug-log safety net"
    )


def test_trace_files_are_0600(tmp_path):
    trace = KeystrokeTrace(level=LEVEL_STRUCTURAL, directory=tmp_path)
    trace.begin_keystroke()
    trace.emit("key_in", keyval=1)
    trace.flush()
    mode = trace._file.stat().st_mode & 0o777  # noqa: SLF001
    assert mode == 0o600
    assert (tmp_path.stat().st_mode & 0o777) == 0o700


# --- Purge / wipe ---------------------------------------------------------------
def test_purge_removes_stale_files(tmp_path):
    stale = tmp_path / "trace-old.jsonl"
    stale.write_text("{}\n", encoding="utf-8")
    old = time.time() - (smartkey_debug.PURGE_AFTER_HOURS + 1) * 3600
    os.utime(stale, (old, old))
    KeystrokeTrace(level=LEVEL_STRUCTURAL, directory=tmp_path)
    assert not stale.exists(), "stale trace files must be purged at startup"


def test_cli_wipe_and_status(tmp_path, monkeypatch, capsys):
    monkeypatch.setenv("SMARTKEY_DEBUG_DIR", str(tmp_path))
    monkeypatch.delenv("SMARTKEY_DEBUG", raising=False)
    (tmp_path / "trace-x.jsonl").write_text("{}\n", encoding="utf-8")
    assert smartkey_debug.main(["wipe"]) == 0
    assert not list(tmp_path.glob("*.jsonl"))
    assert smartkey_debug.main(["status"]) == 0
    out = capsys.readouterr().out
    assert str(tmp_path) in out


# --- Hot-path budget --------------------------------------------------------------
def test_emit_overhead_within_budget(tmp_path):
    off = KeystrokeTrace(level=LEVEL_OFF, directory=tmp_path / "off")
    start = time.perf_counter()
    for _ in range(10_000):
        off.emit("key_in", keyval=1, payload_text="x")
    off_mean = (time.perf_counter() - start) / 10_000
    assert off_mean < 5e-6, f"OFF emit must be a bool check, got {off_mean * 1e6:.2f}µs"

    on = KeystrokeTrace(level=LEVEL_STRUCTURAL, directory=tmp_path / "on")
    start = time.perf_counter()
    for _ in range(10_000):
        on.begin_keystroke()
        on.emit("key_in", keyval=1, payload_len=1, payload_text="x")
    on_mean = (time.perf_counter() - start) / 10_000
    assert on_mean < 100e-6, f"enabled emit too slow: {on_mean * 1e6:.2f}µs"


# --- lang_class unit coverage -----------------------------------------------------
def test_lang_class_basics():
    assert lang_class("привет") == "cyr"
    assert lang_class("hello") == "lat"
    assert lang_class("123") == "digit"
    assert lang_class("?!") == "punct"
    assert lang_class("привet") == "mixed"
    assert lang_class("") == "empty"
    assert lang_class(None) == "empty"
