"""S04 accounting characterizations through the REAL Rust core → PyO3 → adapter seam.

The offline S04 nodes in ``test_space_accept.py`` prove the adapter's
commit/forward/preedit accounting against scripted fakes; these nodes prove the
same accounting when the current-checkout ``smartkey_py`` core produces the
actions (S04 native plan v1.1).

Vehicle (as in ``test_input_safety.py``'s native seam nodes): ``build_engine()``
then ``eng._core = ske.PyInputMethodCore(...)``.  Every node is an
expected-PASS characterization of today's seam; any deviation is a STOP with
evidence, never a production fix from here.

N0 is the precondition node: the native core's ``save_personal()`` resolves
``paths::personal_profile_path()`` = ``config_dir()/personal.json`` from the
platform config dir (XDG_CONFIG_HOME, else HOME/.config).  ``SMARTKEY_PHASEA_DATA``
does NOT cover it.  The module-level autouse fixture below redirects both
variables into a temp root BEFORE any engine is built (pytest node order is not a
contract, so the redirect is a fixture, not a node), N0 proves the redirect holds
inside the built extension, and every later node asserts that it created no new
``personal.json`` under the root.  Nothing here ever stats the live config.

N4 (Space after the full native word commits exactly once, consumed False) is
already pinned by ``test_current_native_right_accept_keeps_locked_bg_preedit_synchronized``
in ``test_input_safety.py`` and is cross-referenced, not duplicated.

CI reaches these nodes only when the Test (Linux) job gets to its pytest step;
while the smartkey-core cargo test step is intentionally RED they are NOT RUN.
"""

from __future__ import annotations

import json
import os
import time

import pytest

from test_input_safety import HINT_NONE, PURPOSE_FREE_FORM, build_engine, ske
from test_space_accept import _accepted_events, capture_replay

# evdev codes of the physical keys; the core maps them to з д р а в itself.
_ZDRAW = tuple(zip("zdraw", (44, 32, 19, 30, 17), strict=True))
_EVDEV_TAB = 15
_EVDEV_RIGHT = 106


@pytest.fixture(scope="module", autouse=True)
def config_root(tmp_path_factory):
    """Redirect every platform config lookup into a module-private temp root.

    Both XDG_CONFIG_HOME and HOME are patched, so a redirect miss would require
    the built extension to ignore both; N0 proves it does not.
    """
    root = tmp_path_factory.mktemp("smartkey-native-config")
    (root / "xdg").mkdir()
    (root / "home").mkdir()
    with pytest.MonkeyPatch.context() as mp:
        mp.setenv("XDG_CONFIG_HOME", str(root / "xdg"))
        mp.setenv("HOME", str(root / "home"))
        yield root


def _personal_files(root) -> set[str]:
    return {str(p) for p in root.rglob("personal.json")}


def _require_native() -> None:
    """The existing guard: an absent/stale extension is a FAILURE under
    ``SMARTKEY_REQUIRE_NATIVE_TESTS=1`` (CI sets it), a skip only locally."""
    if not ske._HAS_CORE:
        if os.environ.get("SMARTKEY_REQUIRE_NATIVE_TESTS") == "1":
            pytest.fail("current-checkout smartkey_py is required for the native seam test")
        pytest.skip("smartkey_py is not built in this local Python environment")


def _to_ibus(evdev: int) -> int:
    return evdev if ske._IS_WAYLAND else evdev + 8


def _native_core():
    native = ske.PyInputMethodCore(
        json.dumps(
            {
                "dual_buffer": {"enabled": True, "lock_threshold": 0.85, "min_lock_chars": 4},
                "ghost_text_separation_margin": 0.0,
                "use_ppm": False,
                "use_reranker": False,
            }
        )
    )
    native.load_word("здравей", 1_000_000)
    return native


def _engine(native):
    """Real adapter over the native core; accounting sinks + replay capture."""
    eng, rec = build_engine()
    eng._core = native
    eng._last_save = time.monotonic()  # "just saved": focus-out must not save now
    eng._o1 = None
    eng._keys_since_content_type = 0
    events = capture_replay(eng)
    eng.do_set_content_type(PURPOSE_FREE_FORM, HINT_NONE)
    return eng, rec, events


def _type_zdraw(eng, native) -> None:
    for keyval, evdev in _ZDRAW:
        assert eng.do_process_key_event(ord(keyval), _to_ibus(evdev), 0) is True
    assert native.current_word() == "здрав", "premise: the native dual buffer composed здрав"
    assert native.debug_state() == (True, True, False), "premise: locked BG composition"
    assert eng._last_composing_typed == "здрав"
    assert eng._active_prediction is not None, "premise: the adapter tracks the native ghost"
    assert eng._active_prediction["word"] == "здравей", "premise: predictions()[0] is здравей"


# ── N0 precondition ──────────────────────────────────────────────────────────


def test_n0_native_profile_save_lands_under_the_redirected_config_root(config_root):
    """The redirect holds inside the built extension: one explicit save_personal()
    creates personal.json under the temp root (parent dirs are created by the
    core) and nowhere else this module can see."""
    _require_native()
    before = _personal_files(config_root)
    native = _native_core()
    native.save_personal()
    created = _personal_files(config_root) - before
    assert created, "save_personal() wrote nothing under the redirected root"
    assert all(path.startswith(str(config_root)) for path in created)
    assert all(path.endswith("/smartkey/personal.json") for path in created), created


# ── N1 Tab accept with a ghost ───────────────────────────────────────────────


def test_n1_tab_accept_commits_the_full_native_word_once(config_root):
    """Tab on a locked BG composition with ghost ``ей``: the core emits
    ``[hide, commit("здравей")]`` (full word, not the suffix), the adapter defers
    the hide because a composing commit follows, replaces the composing preedit
    with the empty invisible one, commits exactly once, and logs one acceptance
    (reason "tab") whose tracked word equals the committed payload."""
    _require_native()
    before = _personal_files(config_root)
    native = _native_core()
    eng, rec, events = _engine(native)
    _type_zdraw(eng, native)
    commits_before = list(rec.commits)
    hides_before = rec.hide_preedit_calls

    assert eng.do_process_key_event(ske.IBus.KEY_Tab, _to_ibus(_EVDEV_TAB), 0) is True
    assert rec.commits == [*commits_before, "здравей"], rec.commits
    assert rec.hide_preedit_calls == hides_before, "the hide before a composing commit is deferred"
    assert rec.preedits[-1] == ("", False), rec.preedits[-1]
    assert rec.forwarded_keys == []
    assert eng._preedit_active is False
    assert eng._active_prediction is None
    accepted = _accepted_events(events)
    assert [(e["word"], e["reason"]) for e in accepted] == [("здравей", "tab")], accepted
    assert native.current_word() == ""
    assert _personal_files(config_root) == before, "no profile write during a Tab accept"


# ── N2 partial Right, completing Right, then Right on an empty core ──────────


def test_n2_partial_right_stays_in_preedit_then_completes_once_then_passes_through(
    config_root,
):
    """The native counterpart of
    ``test_client_partial_right_updates_preedit_then_completes_once_then_forwards``:
    the first Right takes one ghost character into the BG preedit (composing
    only, no client text); the Right that exhausts the ghost is the shared
    full-preedit accept (``[hide, commit("здравей")]``, one acceptance, reason
    "right", consumed); a further Right on the now-empty core is a cursor move:
    the core emits ``[hide, forward]``, the adapter returns False and issues no
    forward_key_event and no commit."""
    _require_native()
    before = _personal_files(config_root)
    native = _native_core()
    eng, rec, events = _engine(native)
    _type_zdraw(eng, native)
    commits_before = list(rec.commits)

    assert eng.do_process_key_event(ske.IBus.KEY_Right, _to_ibus(_EVDEV_RIGHT), 0) is True
    assert rec.commits == commits_before, "partial Right accept stays in the BG preedit"
    assert native.current_word() == "здраве"
    assert eng._last_composing_typed == "здраве"
    assert eng._preedit_active is True
    assert eng._active_prediction is not None and eng._active_prediction["word"] == "здравей"
    assert _accepted_events(events) == []

    hides_before = rec.hide_preedit_calls
    assert eng.do_process_key_event(ske.IBus.KEY_Right, _to_ibus(_EVDEV_RIGHT), 0) is True
    assert rec.commits == [*commits_before, "здравей"], rec.commits
    assert rec.hide_preedit_calls == hides_before, "the hide before a composing commit is deferred"
    assert rec.preedits[-1] == ("", False), rec.preedits[-1]
    assert rec.forwarded_keys == []
    assert eng._preedit_active is False
    accepted = _accepted_events(events)
    assert [(e["word"], e["reason"]) for e in accepted] == [("здравей", "right")], accepted
    assert native.current_word() == ""
    assert native.debug_state()[0] is False, "the dual buffer is gone after the accept"

    consumed = eng.do_process_key_event(ske.IBus.KEY_Right, _to_ibus(_EVDEV_RIGHT), 0)
    assert consumed is False, "no composition, no ghost: Right is the client's key"
    assert rec.forwarded_keys == [], "pass-through is return False, never forward_key_event"
    assert rec.commits == [*commits_before, "здравей"], "a cursor move commits nothing"
    assert rec.hide_preedit_calls == hides_before + 1, "the bare hide of a cursor move lands"
    assert _accepted_events(events) == accepted, "a cursor move is not an acceptance"
    assert _personal_files(config_root) == before


# ── N3 focus-out mid-word ────────────────────────────────────────────────────


def test_n3_focus_out_flushes_the_native_composition_exactly_once(config_root):
    """Native ``focus_lost()`` emits ``[commit("здрав"), hide]`` — the flush
    commit FIRST, the trailing HideGhost last (the offline fixture in
    ``test_client_focus_out_flushes_the_composed_word_exactly_once`` models the
    opposite order).  The commit count is the same either way: the composing
    preedit is replaced by the empty invisible one and the displayed reading is
    committed exactly once with no forwarded key.  The order shows in the hide
    count only: the trailing bare hide arrives after the commit has already
    cleared the composing preedit, so ``hide_preedit_text`` is called once here
    where the offline node sees zero.  With a recent save the personal profile
    is not written."""
    _require_native()
    before = _personal_files(config_root)
    native = _native_core()
    eng, rec, events = _engine(native)
    _type_zdraw(eng, native)
    commits_before = list(rec.commits)
    hides_before = rec.hide_preedit_calls

    eng.do_focus_out()
    assert rec.commits == [*commits_before, "здрав"], rec.commits
    assert rec.preedits[-1] == ("", False), rec.preedits[-1]
    assert rec.hide_preedit_calls == hides_before + 1
    assert rec.forwarded_keys == []
    assert native.current_word() == ""
    assert eng._active_prediction is None
    assert eng._preedit_active is False
    assert eng._keys_since_content_type == 0
    assert _accepted_events(events) == [], "a focus flush is never an acceptance"
    assert _personal_files(config_root) == before, "focus-out with a recent save must not write"


# ── N5 the guard itself ──────────────────────────────────────────────────────


def test_n5_native_guard_fails_when_required_and_skips_otherwise(monkeypatch):
    """Characterization of the guard every node above uses: under
    SMARTKEY_REQUIRE_NATIVE_TESTS=1 an absent extension is a FAILURE (CI's
    setting), otherwise a skip — so a stale/absent build can never pass silently
    in CI.  Per-file native isolation (D2) is a separate operator decision."""
    monkeypatch.setattr(ske, "_HAS_CORE", False)
    monkeypatch.setenv("SMARTKEY_REQUIRE_NATIVE_TESTS", "1")
    with pytest.raises(pytest.fail.Exception):
        _require_native()
    monkeypatch.delenv("SMARTKEY_REQUIRE_NATIVE_TESTS", raising=False)
    with pytest.raises(pytest.skip.Exception):
        _require_native()
