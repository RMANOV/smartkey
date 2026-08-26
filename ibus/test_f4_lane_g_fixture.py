"""F4 Phase R — lane G diagnostic fixture (ADVOCATE_CODEX ruling 221af6d0a1c8,
minimum RED matrix row G).

A SYNTHETIC 55-token Bulgarian flow is kept as a PARTITIONED diagnostic
fixture, never as a single "N/45 fixed" gate: every word is tagged with the
lane that owns it (A: delimiter-time exact short word; B: wrong early lock /
full-word crossover; C: zero-support provenance, explicitly NOT fixed) or with
a non-lane class (hyphen probe artefact — keycode 12 ends the word; ordinary
typo; unsupported OOV; Latin token such as ``smartkey``).  Non-lane classes are
never silently reassigned to A/B.  Words are derived into evdev keycode
sequences (BG phonetic) from the fixture text at test time — no keystream
capture and no live trace is used.  Written by the lane G agent only.

Fixture shape: the flow is typed token by token, Space-separated, through ONE
fresh native core with the machine corpora (``~/.config/smartkey/corpus_*.bin``,
read-only), so phrase context and momentum carry across words exactly as in
the registry rows.  The flow has 55 Space-separated tokens: public registry
tokens at fixed positions, neutral high-frequency corpus filler elsewhere; it
contains no operator-authored text (privacy ruling 06055072be3f).
Every partition is its own test:

* ``expected_pass``          — PASS_GUARD: must stay Cyrillic (regression net).
* ``lane_a_short_words``     — RED until lane A lands (но ×2, топ, тн).
* ``lane_b_prefix_lock``     — RED until lane B lands (статия, статии).
* ``lane_c_unsupported``     — diagnostic only (runs, records observed forms, no skip).
* ``probe_artefact_hyphen``  — diagnostic only (keycode 12 splits the token).
* ``latin_token``            — PASS_GUARD: ``smartkey`` stays Latin.

Harness: ``SMARTKEY_NATIVE_MODULE_DIR`` (unzipped worktree wheel, imported
once per process, never pre-imported); ``SMARTKEY_PHASEA_DATA`` = temp dir.
"""

from __future__ import annotations

import importlib
import json
import os
import sys
import tempfile

import pytest

os.environ.setdefault(
    "SMARTKEY_PHASEA_DATA", tempfile.mkdtemp(prefix="smartkey-test-phasea-")
)

_MODULE_DIR = os.environ.get("SMARTKEY_NATIVE_MODULE_DIR")
_CORPUS_DIR = os.path.expanduser("~/.config/smartkey")
_CORPORA = [os.path.join(_CORPUS_DIR, f) for f in ("corpus_en.bin", "corpus_bg.bin")]
_HAS_CORPORA = all(os.path.exists(p) for p in _CORPORA)
if os.environ.get("SMARTKEY_REQUIRE_NATIVE") == "1" and (
    not _MODULE_DIR or not _HAS_CORPORA
):
    # Fail-closed evidence mode (ADVOCATE 4922bcfed82e): never a skip.
    raise RuntimeError(
        "SMARTKEY_REQUIRE_NATIVE=1 but SMARTKEY_NATIVE_MODULE_DIR="
        f"{_MODULE_DIR!r} / corpora present={_HAS_CORPORA}"
    )
pytestmark = pytest.mark.skipif(
    not _MODULE_DIR or not _HAS_CORPORA,
    reason="needs SMARTKEY_NATIVE_MODULE_DIR and the local corpora (audit artifact)",
)

SPACE = 57
HYPHEN = 12  # '-' on the physical keyboard: a non-alphabetic char ends the word
# BG phonetic layout, evdev scancodes.
_BG_KEYS = {
    "я": 16, "в": 17, "е": 18, "р": 19, "т": 20, "ъ": 21, "у": 22, "и": 23,
    "о": 24, "п": 25, "ш": 26, "щ": 27, "а": 30, "с": 31, "д": 32, "ф": 33,
    "г": 34, "х": 35, "й": 36, "к": 37, "л": 38, "ч": 41, "ю": 43, "з": 44,
    "ь": 45, "ц": 46, "ж": 47, "б": 48, "н": 49, "м": 50,
}  # fmt: skip
# Physical keys of the Latin token (same scancodes read through the EN keymap;
# ``y`` is the key that carries ``ъ`` on BG phonetic).
_LATIN_KEYS = {"s": 31, "m": 50, "a": 30, "r": 19, "t": 20, "k": 37, "e": 18, "y": 21}

FLOW = (
    "статия това smartkey но работа на време да се за не от статии в което "
    "може темплейтът да сай-фай бекграунд бъде на всеки топ фичъри ден "
    "по-малко и в този случай има още една част от текста която трябва да "
    "бъде на място и smartkey след това но идва краят на реда фичъри за тн"
).split()

# SYNTHETIC de-identified flow (privacy ruling 06055072be3f): registry tokens sit at the
# same 1-based positions as the original diagnostic; every other token is neutral,
# high-frequency Bulgarian filler from the public corpus. No operator-authored text.
# 1-based token positions; a token belongs to exactly one partition.
PARTITIONS: dict[str, tuple[int, ...]] = {
    "lane_a_short_words": (4, 24, 48, 55),  # но, топ, но, тн
    "lane_b_prefix_lock": (1, 13),  # статия, статии
    "lane_c_unsupported": (17, 20, 25, 53),  # темплейтът, бекграунд, фичъри, фичъри
    "probe_artefact_hyphen": (19, 27),  # сай-фай, по-малко
    "latin_token": (3, 45),  # smartkey, smartkey
}
_TAGGED = {pos for positions in PARTITIONS.values() for pos in positions}
PARTITIONS["expected_pass"] = tuple(
    pos for pos in range(1, len(FLOW) + 1) if pos not in _TAGGED
)

_NATIVE = None
_OBSERVED: list[dict] | None = None


def _native():
    global _NATIVE  # noqa: PLW0603
    if _NATIVE is None:
        sys.path.insert(0, _MODULE_DIR)
        assert "smartkey_py" not in sys.modules, "another smartkey_py already loaded"
        _NATIVE = importlib.import_module("smartkey_py")
        assert _NATIVE.__file__.startswith(_MODULE_DIR), _NATIVE.__file__
    return _NATIVE


def _codes(token: str) -> list[int]:
    codes = []
    for ch in token:
        if ch == "-":
            codes.append(HYPHEN)
        elif ch in _BG_KEYS:
            codes.append(_BG_KEYS[ch])
        else:
            codes.append(_LATIN_KEYS[ch])
    return codes


def _type_token(core, token: str) -> dict:
    commits: list[str] = []
    forwards = 0
    replaces = 0
    for code in [*_codes(token), SPACE]:
        actions = core.process_keycode(code, 0)
        commits += [p for a, p in actions if a == "commit"]
        forwards += sum(1 for a, _ in actions if a == "forward")
        replaces += sum(1 for a, _ in actions if a == "replace")
    return {
        "token": token,
        "commits": commits,
        "forwards": forwards,
        "replaces": replaces,
    }


def _observed() -> list[dict]:
    """Type the whole flow once through one fresh core; cached per process."""
    global _OBSERVED  # noqa: PLW0603
    if _OBSERVED is None:
        smartkey_py = _native()
        core = smartkey_py.PyInputMethodCore(json.dumps({}))
        for path in _CORPORA:
            core.load_corpus_file(path)
        _OBSERVED = [_type_token(core, token) for token in FLOW]
    return _OBSERVED


def _partition(name: str) -> list[tuple[int, dict]]:
    observed = _observed()
    return [(pos, observed[pos - 1]) for pos in PARTITIONS[name]]


def _wrong(name: str) -> dict[str, list[str]]:
    """``"<pos>:<token>" -> commits`` for every token of the partition whose
    committed text is not exactly the token."""
    return {
        f"{pos}:{row['token']}": row["commits"]
        for pos, row in _partition(name)
        if row["commits"] != [row["token"]]
    }


def _forms(name: str) -> dict[str, list[str]]:
    return {f"{pos}:{row['token']}": row["commits"] for pos, row in _partition(name)}


# --- structure -----------------------------------------------------------------


def test_partitions_cover_every_token_exactly_once():
    assert len(FLOW) == 55
    positions = [pos for positions in PARTITIONS.values() for pos in positions]
    assert sorted(positions) == list(range(1, len(FLOW) + 1)), positions
    assert [FLOW[p - 1] for p in PARTITIONS["lane_a_short_words"]] == [
        "но",
        "топ",
        "но",
        "тн",
    ]
    assert [FLOW[p - 1] for p in PARTITIONS["lane_b_prefix_lock"]] == [
        "статия",
        "статии",
    ]
    assert [FLOW[p - 1] for p in PARTITIONS["lane_c_unsupported"]] == [
        "темплейтът", "бекграунд", "фичъри", "фичъри"
    ]  # fmt: skip
    assert [FLOW[p - 1] for p in PARTITIONS["probe_artefact_hyphen"]] == [
        "сай-фай", "по-малко"
    ]  # fmt: skip
    assert [FLOW[p - 1] for p in PARTITIONS["latin_token"]] == ["smartkey", "smartkey"]
    assert len(PARTITIONS["expected_pass"]) == 41


def test_every_plain_token_commits_exactly_once_with_one_delimiter():
    # Structural invariant across ALL non-artefact tokens regardless of script:
    # one commit, one forwarded Space, never a ReplaceWord — the pre-commit
    # resolver may change the text of that commit, never its shape.
    artefacts = set(PARTITIONS["probe_artefact_hyphen"])
    bad = {
        f"{pos}:{row['token']}": row
        for pos, row in enumerate(_observed(), start=1)
        if pos not in artefacts
        and (len(row["commits"]) != 1 or row["forwards"] != 1 or row["replaces"] != 0)
    }
    assert bad == {}, bad


# --- partitions ------------------------------------------------------------------


def test_expected_pass_words_commit_cyrillic():
    # Regression net: the Bulgarian words the baseline already gets right must
    # stay right after any lane lands (report exactly which ones regressed).
    assert _wrong("expected_pass") == {}


def test_lane_a_short_words_commit_cyrillic():
    # RED (lane A): baseline commits the strongest EN prefix completion's
    # script — но→no ×2, топ→top, тн→tn — inside a Bulgarian phrase.
    wrong = _wrong("lane_a_short_words")
    assert wrong != {
        "4:но": ["no"], "24:топ": ["top"], "48:но": ["no"], "55:тн": ["tn"]
    }, f"baseline lane A defect unchanged: {wrong}"  # fmt: skip
    assert wrong == {}, wrong


def test_lane_b_prefix_lock_words_commit_cyrillic():
    # RED (lane B): the EN display lock on ``stat`` at char 4 is committed as
    # statiq / statii instead of the exact Bulgarian words.
    wrong = _wrong("lane_b_prefix_lock")
    assert wrong != {
        "1:статия": ["statiq"], "13:статии": ["statii"]
    }, f"baseline lane B defect unchanged: {wrong}"  # fmt: skip
    assert wrong == {}, wrong


def test_lane_c_unsupported_words_are_diagnostic_only(record_property):
    # Lane C is invariant hardening only; the ruling forbids claiming these
    # words fixed.  The partition RUNS (no skip — ADVOCATE 4922bcfed82e) and
    # the observed forms are recorded as a JUnit property; the only
    # assertion is structural: every token produced exactly one commit.
    forms = _forms("lane_c_unsupported")
    record_property("lane_c_observed_forms", repr(forms))
    assert len(forms) == len(PARTITIONS["lane_c_unsupported"])
    assert all(len(commits) == 1 for commits in forms.values()), forms


def test_probe_artefact_hyphen_words_are_diagnostic_only(record_property):
    # Keycode 12 ('-') is a non-alphabetic boundary: the probe splits each
    # hyphenated token into two commits + two forwards.  Deferred (typo/hyphen
    # correction) — recorded, not gated; no skip.
    forms = _forms("probe_artefact_hyphen")
    record_property("hyphen_observed_forms", repr(forms))
    assert len(forms) == len(PARTITIONS["probe_artefact_hyphen"])
    assert all(len(commits) == 2 for commits in forms.values()), forms


def test_latin_token_stays_latin():
    # ``smartkey`` is typed on the same physical keys; it must remain a Latin
    # token in a Bulgarian phrase (guard against over-eager Cyrillic flips).
    assert _wrong("latin_token") == {}
