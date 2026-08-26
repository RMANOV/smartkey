"""F4 Phase R — lane F RED tests (ADVOCATE_CODEX ruling 221af6d0a1c8, minimum
RED matrix row F): PyO3/native controlled-corpus artifact and the raw IBus
flow — preedit crossover (``stat`` shown EN, ``статия`` committed BG), exactly
one commit and one forwarded delimiter, no stale/duplicate action; all B9/F1
English and Bulgarian guards retained.

Harness pattern (see ``test_native_lang_prior.py`` / ``test_native_space_accept.py``):
``SMARTKEY_NATIVE_MODULE_DIR`` names the directory holding an unzipped
worktree wheel; ``smartkey_py`` is imported once per process and must not be
pre-imported; corpora come from the machine's ``~/.config/smartkey/corpus_*.bin``
(read-only); ``SMARTKEY_PHASEA_DATA`` points at a fresh temp dir.  Without the
module dir and corpora the tests are SKIPPED.  Written by the lane F agent only.

Two corpus sources are used deliberately:

* ``controlled`` — a tiny JSON corpus pair written to a temp dir and loaded via
  ``PyInputMethodCore.load_corpus_file`` (``corpus_en.json`` / ``corpus_bg.json``;
  the stem suffix selects the language model, the loader also drops a ``.bin``
  cache next to it, inside the temp dir).  It reproduces the ruling's evidence
  shape deterministically: ``stat`` has strong EN *prefix* support (status /
  static / station) and weak BG prefix support, so the display locks EN at
  char 4 although ``statiq`` is no English word and ``статия`` is an exact
  Bulgarian entry; ``li``/``no`` have strong EN prefix completions (like / not)
  while ``ли``/``но`` are exact Bulgarian words.
* ``real`` — the machine corpora, only for the retained B9/F1 guards and for the
  operator-reported short-word cases in their original evidence.

RED discipline: every failing test fails on an assertion about baseline
behaviour (the EN keymap text is committed); guards that already hold are kept
as PASS_GUARD.  Nothing here calls the default-off post-commit autocorrect path.
"""

from __future__ import annotations

import importlib
import importlib.util
import json
import os
import pathlib
import sys
import tempfile
import types

import pytest

os.environ.setdefault(
    "SMARTKEY_PHASEA_DATA", tempfile.mkdtemp(prefix="smartkey-test-phasea-")
)

_MODULE_DIR = os.environ.get("SMARTKEY_NATIVE_MODULE_DIR")
_CORPUS_DIR = os.path.expanduser("~/.config/smartkey")
_REAL_CORPORA = [
    os.path.join(_CORPUS_DIR, f) for f in ("corpus_en.bin", "corpus_bg.bin")
]
_HAS_REAL_CORPORA = all(os.path.exists(p) for p in _REAL_CORPORA)

_REQUIRE_NATIVE = os.environ.get("SMARTKEY_REQUIRE_NATIVE") == "1"
if _REQUIRE_NATIVE and (not _MODULE_DIR or not _HAS_REAL_CORPORA):
    # Fail-closed evidence mode (ADVOCATE 4922bcfed82e): with
    # SMARTKEY_REQUIRE_NATIVE=1 a missing module dir or missing corpora is a
    # collection ERROR, never a skip, so an audit run can prove zero skips.
    raise RuntimeError(
        "SMARTKEY_REQUIRE_NATIVE=1 but SMARTKEY_NATIVE_MODULE_DIR="
        f"{_MODULE_DIR!r} / real corpora present={_HAS_REAL_CORPORA}"
    )
pytestmark = pytest.mark.skipif(
    not _MODULE_DIR, reason="SMARTKEY_NATIVE_MODULE_DIR not set (audit artifact only)"
)
needs_real_corpora = pytest.mark.skipif(
    not _HAS_REAL_CORPORA, reason="needs the local corpora (read-only audit input)"
)

SPACE = 57
# BG phonetic layout, evdev scancodes.
_BG_KEYS = {
    "я": 16, "в": 17, "е": 18, "р": 19, "т": 20, "ъ": 21, "у": 22, "и": 23,
    "о": 24, "п": 25, "ш": 26, "щ": 27, "а": 30, "с": 31, "д": 32, "ф": 33,
    "г": 34, "х": 35, "й": 36, "к": 37, "л": 38, "ч": 41, "ю": 43, "з": 44,
    "ь": 45, "ц": 46, "ж": 47, "б": 48, "н": 49, "м": 50,
}  # fmt: skip
_LATIN_KEYS = {"t": 20, "h": 35, "e": 18, "i": 23, "s": 31, "g": 34}


def _codes(word: str) -> list[int]:
    table = _BG_KEYS if any(ch in _BG_KEYS for ch in word) else _LATIN_KEYS
    return [table[ch] for ch in word]


LATIN_CONTEXT = "cd ~/smartkey && cargo test -p smartkey-core "

# Controlled corpora: frequencies chosen so that the PREFIX evidence favours EN
# (the baseline display/lock signal) while the EXACT-word evidence is Bulgarian
# only (li) or both-supported (no/но, to/то) — the lane A/B distinction.
CONTROLLED_EN = {
    "status": 600_000, "static": 300_000, "station": 200_000, "state": 150_000,
    "start": 100_000, "like": 900_000, "list": 400_000, "line": 300_000,
    "not": 1_000_000, "now": 700_000, "no": 600_000, "the": 2_000_000,
    "is": 1_500_000, "to": 1_800_000, "it": 1_400_000, "top": 50_000,
    "topic": 40_000,
}  # fmt: skip
CONTROLLED_BG = {
    "статия": 55_000, "статии": 10_000, "стативи": 500, "ли": 400_000,
    "лично": 200_000, "но": 800_000, "нов": 300_000, "искаш": 50_000,
    "искам": 100_000, "то": 300_000, "това": 900_000, "топ": 30_000,
    "топло": 20_000,
}  # fmt: skip

_NATIVE = None
_CONTROLLED_PATHS: list[str] | None = None
_ENGINE_MODULE = None


def _native():
    # A compiled extension can be imported only once per process: load it
    # exactly once from the audited directory and reuse it.
    global _NATIVE  # noqa: PLW0603
    if _NATIVE is None:
        sys.path.insert(0, _MODULE_DIR)
        assert "smartkey_py" not in sys.modules, "another smartkey_py already loaded"
        _NATIVE = importlib.import_module("smartkey_py")
        assert _NATIVE.__file__.startswith(_MODULE_DIR), _NATIVE.__file__
    return _NATIVE


def _controlled_paths() -> list[str]:
    global _CONTROLLED_PATHS  # noqa: PLW0603
    if _CONTROLLED_PATHS is None:
        base = pathlib.Path(os.environ["SMARTKEY_PHASEA_DATA"]) / "f4-controlled-corpus"
        base.mkdir(parents=True, exist_ok=True)
        paths = []
        for lang, unigrams in (("en", CONTROLLED_EN), ("bg", CONTROLLED_BG)):
            path = base / f"corpus_{lang}.json"
            path.write_text(json.dumps({"unigrams": unigrams}), encoding="utf-8")
            paths.append(str(path))
        _CONTROLLED_PATHS = paths
    return _CONTROLLED_PATHS


def _core(corpora: list[str], surrounding: str | None = None):
    smartkey_py = _native()
    core = smartkey_py.PyInputMethodCore(json.dumps({}))
    for path in corpora:
        core.load_corpus_file(path)
    if surrounding is not None:
        core.set_surrounding_text(surrounding, len(surrounding))
    return core


def _controlled_core(surrounding: str | None = None):
    return _core(_controlled_paths(), surrounding)


def _real_core(surrounding: str | None = None):
    return _core(_REAL_CORPORA, surrounding)


def _type_word(core, word: str) -> dict:
    """Type ``word`` on the physical keys, then Space.  Returns the boundary
    accounting: the composing display after each key, the delimiter batch and
    the committed texts across the whole word."""
    displays: list[str] = []
    commits: list[str] = []
    forwards = 0
    replaces = 0
    for code in _codes(word):
        actions = core.process_keycode(code, 0)
        displays.append(core.current_word())
        commits += [p for a, p in actions if a == "commit"]
        forwards += sum(1 for a, _ in actions if a == "forward")
        replaces += sum(1 for a, _ in actions if a == "replace")
    delimiter = core.process_keycode(SPACE, 0)
    commits += [p for a, p in delimiter if a == "commit"]
    forwards += sum(1 for a, _ in delimiter if a == "forward")
    replaces += sum(1 for a, _ in delimiter if a == "replace")
    return {
        "displays": displays,
        "delimiter": delimiter,
        "commits": commits,
        "forwards": forwards,
        "replaces": replaces,
    }


def _committed(core, word: str) -> str:
    result = _type_word(core, word)
    assert len(result["commits"]) == 1, result
    return result["commits"][0]


# --- F.1 controlled corpus: native core, direct -----------------------------


@pytest.mark.parametrize(
    ("word", "baseline"), [("статия", "statiq"), ("статии", "statii")]
)
def test_native_controlled_stat_lock_is_display_only_and_commit_is_cyrillic(
    word: str, baseline: str
):
    # Lane B on the native artifact: the prefix evidence for ``stat`` locks the
    # DISPLAY to EN at char 4 (min_lock_chars) — that hysteresis is allowed to
    # stay.  The exact-word evidence (статия/статии are corpus entries, statiq/
    # statii are not) must decide the ONE commit at the delimiter.
    core = _controlled_core()
    result = _type_word(core, word)

    assert result["displays"][3] == "stat", result["displays"]
    assert result["replaces"] == 0, result
    assert result["forwards"] == 1, result
    assert len(result["commits"]) == 1, result
    assert result["commits"] != [baseline], (
        f"baseline EN keymap text committed: {result}"
    )
    assert result["commits"] == [word], result


def test_native_controlled_li_after_bulgarian_word_is_bulgarian():
    # Lane A: exact ``li`` has no EN support in the corpus (only the prefix
    # completion ``like``), exact ``ли`` is a Bulgarian word — the only
    # supported exact reading may win at the delimiter.
    core = _controlled_core()
    assert _committed(core, "искаш") == "искаш"

    got = _committed(core, "ли")

    assert got != "li", "baseline: strongest prefix completion (like) wins → 'li'"
    assert got == "ли", got


def test_native_controlled_no_after_bulgarian_phrase_is_bulgarian():
    # Lane A, both exact readings supported (no / но): the Bulgarian phrase
    # context supplies the explicit margin the ruling requires.
    core = _controlled_core()
    for word in ("искаш", "ли"):
        _type_word(core, word)

    got = _committed(core, "но")

    assert got != "no", "baseline: prefix completion (not) wins → 'no'"
    assert got == "но", got


def test_native_controlled_exactly_one_commit_and_one_delimiter_for_statia():
    # Pre-commit resolution must never leave a stale action behind: no
    # ReplaceWord, no second commit, the Space forwarded exactly once and the
    # delimiter batch shaped [hide, commit, forward].
    core = _controlled_core()
    result = _type_word(core, "статия")

    kinds = [a for a, _ in result["delimiter"]]
    assert kinds.count("commit") == 1 and kinds.count("forward") == 1, result
    assert "replace" not in kinds, result
    assert core.current_word() == "", "the word must be fully consumed by the commit"
    assert result["commits"] == ["статия"], result


# --- F.1 guards on the controlled corpus (PASS_GUARD) ------------------------


def test_native_controlled_english_short_words_after_latin_context_stay_english():
    # Genuine English no / li / to / is after English context remain English
    # even though li/ли, no/но, to/то have Bulgarian exact or prefix support.
    core = _controlled_core(LATIN_CONTEXT)
    got = {w: _committed(core, w) for w in ("но", "ли", "то", "ис")}
    assert got == {"но": "no", "ли": "li", "то": "to", "ис": "is"}, got


def test_native_controlled_english_word_stays_english_after_english_word():
    core = _controlled_core(LATIN_CONTEXT)
    assert _committed(core, "the") == "the"
    got = _committed(core, "ис")
    assert got == "is", got


def test_native_controlled_unresolved_ambiguity_keeps_displayed_reading():
    # no/но are both supported and there is NO phrase or prior context at all:
    # the ruling forbids a silent flip, so the displayed reading commits.
    core = _controlled_core()
    got = _committed(core, "но")
    assert got == "no", got


def test_native_controlled_bulgarian_word_with_exact_support_is_cyrillic():
    core = _controlled_core()
    assert _committed(core, "искаш") == "искаш"
    assert _committed(core, "това") == "това"


# --- F.1 real corpora: operator-reported cases in their original evidence ---


@needs_real_corpora
def test_native_real_corpora_statia_commits_cyrillic_once():
    core = _real_core()
    result = _type_word(core, "статия")

    assert result["displays"][3] == "stat", result["displays"]
    assert len(result["commits"]) == 1 and result["forwards"] == 1, result
    assert result["replaces"] == 0, result
    assert result["commits"] != ["statiq"], "baseline: EN lock at char 4 is committed"
    assert result["commits"] == ["статия"], result


@needs_real_corpora
def test_native_real_corpora_short_words_in_bulgarian_phrase_are_cyrillic():
    core = _real_core()
    assert _committed(core, "искаш") == "искаш"
    got = {"ли": _committed(core, "ли"), "но": _committed(core, "но")}

    assert got != {"ли": "li", "но": "no"}, f"baseline EN keymap text: {got}"
    assert got == {"ли": "ли", "но": "но"}, got


@needs_real_corpora
def test_native_real_corpora_english_short_words_after_latin_context_stay_english():
    core = _real_core(LATIN_CONTEXT)
    got = {w: _committed(core, w) for w in ("но", "ли", "то", "ис")}
    assert got == {"но": "no", "ли": "li", "то": "to", "ис": "is"}, got


# --- F.3 retained B9/F1 guards (mirror of test_native_lang_prior.py) --------

_B9_WORDS = ("имаш", "отговор", "след", "превкл")
_B9_ENGLISH = ("the", "is", "git")


@needs_real_corpora
def test_native_guard_bulgarian_words_with_latin_surrounding_are_cyrillic():
    core = _real_core(LATIN_CONTEXT)
    got = {w: _committed(core, w) for w in _B9_WORDS}
    assert got == {w: w for w in _B9_WORDS}, got


@needs_real_corpora
def test_native_guard_bulgarian_words_without_context_are_cyrillic():
    core = _real_core()
    got = {w: _committed(core, w) for w in _B9_WORDS}
    assert got == {w: w for w in _B9_WORDS}, got


@needs_real_corpora
def test_native_guard_english_words_with_latin_surrounding_stay_english():
    core = _real_core(LATIN_CONTEXT)
    got = {w: _committed(core, w) for w in _B9_ENGLISH}
    assert got == {w: w for w in _B9_ENGLISH}, got


@needs_real_corpora
def test_native_guard_english_word_after_bulgarian_words_is_english():
    core = _real_core()
    for word in _B9_WORDS:
        _committed(core, word)
    assert _committed(core, "the") == "the"


# --- F.2 raw IBus flow: smartkey_engine.py + the REAL native core -----------
#
# The adapter is driven exactly like ``test_space_accept.py`` (fake ``gi``, the
# engine instantiated without IBus) but with the audited native core instead
# of a scripted one, so the assertions cover the real action batches crossing
# the FFI and the adapter's dispatch of them.


def _engine_module():
    global _ENGINE_MODULE  # noqa: PLW0603
    if _ENGINE_MODULE is None:
        _native()  # the engine's ``import smartkey_py`` must resolve to the artifact
        fake_gi = types.ModuleType("gi")

        def _require_version(*_a, **_k):  # noqa: ANN002, ANN003
            raise ValueError("forced fake IBus for tests")

        fake_gi.require_version = _require_version  # type: ignore[attr-defined]
        sys.modules["gi"] = fake_gi
        sys.modules.pop("gi.repository", None)
        engine_path = pathlib.Path(__file__).resolve().parent / "smartkey_engine.py"
        spec = importlib.util.spec_from_file_location(
            "smartkey_engine_f4_lane_f", engine_path
        )
        assert spec and spec.loader
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        assert module._HAS_IBUS is False
        assert module._HAS_CORE is True
        assert module.PyInputMethodCore is _native().PyInputMethodCore

        def _mk_text(s: str):
            t = types.SimpleNamespace(s=s)
            t.set_attributes = lambda *_a, **_k: None
            return t

        class _AttrList:
            def append(self, *_a) -> None:  # noqa: ANN002
                return None

        module.IBus.Text.new_from_string = staticmethod(_mk_text)
        module.IBus.AttrList = _AttrList
        _ENGINE_MODULE = module
    return _ENGINE_MODULE


class _RecordingCore:
    """Transparent proxy over the native core that keeps every action batch."""

    def __init__(self, core) -> None:
        self._core = core
        self.batches: list[list[tuple[str, str]]] = []

    def process_keycode(self, keycode: int, state: int) -> list[tuple[str, str]]:
        actions = self._core.process_keycode(keycode, state)
        self.batches.append(list(actions))
        return actions

    def __getattr__(self, name: str):
        return getattr(self._core, name)


class _Recorder:
    def __init__(self) -> None:
        self.commits: list[str] = []
        self.preedits: list[tuple[str, bool]] = []
        self.forwarded_keys: list[tuple[int, int, int]] = []


def _build_engine(core):
    ske = _engine_module()
    eng = ske.SmartKeyEngine.__new__(ske.SmartKeyEngine)
    eng._core = _RecordingCore(core)
    eng._caps = 0
    eng._preedit_active = False
    eng._preedit_mode = None
    eng._active_prediction = None
    eng._prediction_seq = 0
    eng._surrounding_text = None
    eng._surrounding_cursor_pos = None
    eng._trace = None
    eng._last_composing_typed = ""
    eng._o1 = None
    rec = _Recorder()
    eng.commit_text = lambda text_obj: rec.commits.append(text_obj.s)
    eng.update_preedit_text = lambda text_obj, _cursor, visible: rec.preedits.append(
        (text_obj.s, visible)
    )
    eng.hide_preedit_text = lambda: None
    eng.forward_key_event = lambda kv, kc, st: rec.forwarded_keys.append((kv, kc, st))
    eng.delete_surrounding_text = lambda off, n: None
    return eng, rec


def _ibus_keycode(evdev: int) -> int:
    # The adapter normalises X11 (evdev + 8) to evdev; Wayland sends evdev.
    return evdev if _engine_module()._IS_WAYLAND else evdev + 8


def _ibus_type_word(eng, rec, word: str) -> dict:
    ske = _engine_module()
    consumed = []
    for code in _codes(word):
        consumed.append(eng.do_process_key_event(ord("x"), _ibus_keycode(code), 0))
    visible = [text for text, shown in rec.preedits if shown]
    delimiter_consumed = eng.do_process_key_event(
        ske.IBus.KEY_space, _ibus_keycode(SPACE), 0
    )
    return {
        "letters_consumed": consumed,
        "visible_preedits": visible,
        "delimiter_consumed": delimiter_consumed,
    }


def test_ibus_flow_statia_preedit_crossover_one_commit_one_delimiter():
    eng, rec = _build_engine(_controlled_core())

    flow = _ibus_type_word(eng, rec, "статия")

    # Composing keys are consumed; the char-4 preedit shows the EN display lock.
    assert flow["letters_consumed"] == [True] * 6, flow
    assert any(t.startswith("stat") for t in flow["visible_preedits"]), flow
    # Exactly one delimiter: the core forwards Space once, the adapter returns
    # False so IBus delivers it once, and no synthetic key is injected.
    assert flow["delimiter_consumed"] is False, flow
    assert rec.forwarded_keys == [], rec.forwarded_keys
    delimiter_batch = eng._core.batches[-1]
    kinds = [a for a, _ in delimiter_batch]
    assert kinds.count("forward") == 1 and kinds.count("commit") == 1, delimiter_batch
    assert not any(a == "replace" for batch in eng._core.batches for a, _ in batch)
    assert eng._preedit_active is False
    assert rec.preedits[-1] == ("", False), "composing preedit cleared before commit"
    # The single commit carries the Bulgarian reading (baseline: 'statiq').
    assert len(rec.commits) == 1, rec.commits
    assert rec.commits != ["statiq"], "baseline: adapter commits the EN keymap text"
    assert rec.commits == ["статия"], rec.commits


def test_ibus_flow_bulgarian_phrase_short_words_commit_cyrillic():
    eng, rec = _build_engine(_controlled_core())

    flows = [_ibus_type_word(eng, rec, w) for w in ("искаш", "ли", "но")]

    assert [f["delimiter_consumed"] for f in flows] == [False, False, False], flows
    assert rec.forwarded_keys == []
    assert len(rec.commits) == 3, rec.commits
    assert rec.commits != ["искаш", "li", "no"], f"baseline: {rec.commits}"
    assert rec.commits == ["искаш", "ли", "но"], rec.commits


def test_ibus_flow_english_short_words_after_latin_context_stay_english():
    eng, rec = _build_engine(_controlled_core(LATIN_CONTEXT))

    flows = [_ibus_type_word(eng, rec, w) for w in ("но", "ли", "то", "ис")]

    assert [f["delimiter_consumed"] for f in flows] == [False] * 4, flows
    assert rec.commits == ["no", "li", "to", "is"], rec.commits
    assert rec.forwarded_keys == []
