"""Whole-reading language choices at the IBus boundary.

Fake IBus and a scripted public Core protocol exercise the real adapter.  The
shared Space harness fences native imports and debug sinks; these tests never
construct a native Core, contact a bus, or display a host window.  A separate
private panel witness must establish that the real caps-41 panel route renders.
"""

from __future__ import annotations

from copy import deepcopy
from types import SimpleNamespace

import pytest

from .test_space_accept import FakeCore, FakeO1, build_engine, capture_replay, ske


class _LookupTable:
    def __init__(
        self, page_size=2, cursor_pos=0, cursor_visible=False, round=False
    ) -> None:
        self.candidates = []
        self.cursor_pos = cursor_pos
        self.cursor_visible = cursor_visible

    @classmethod
    def new(cls, page_size, cursor_pos, cursor_visible, round):
        return cls(page_size, cursor_pos, cursor_visible, round)

    def append_candidate(self, text) -> None:
        self.candidates.append(text.s)

    def set_cursor_pos(self, pos) -> None:
        self.cursor_pos = pos

    def get_cursor_pos(self):
        return self.cursor_pos

    def set_cursor_visible(self, visible) -> None:
        self.cursor_visible = visible

    def get_number_of_candidates(self):
        return len(self.candidates)


class _ChoiceCore(FakeCore):
    """The frozen revision-based API, plus the pre-existing key protocol."""

    def __init__(self, scripts, current_language="en") -> None:
        super().__init__(scripts)
        self.supported = False
        self.support_calls = []
        self.rewrite_calls = []
        self.offer_calls = 0
        self.key_calls = []
        self.accept_calls = []
        self.dismiss_calls = []
        self.revision = 17
        self.current_language = current_language
        self.readings = {"en": "draftyt", "bg": "драфтът"}
        self.dismissed = set()
        self.saved = 0

    def set_external_rewrite_supported(self, supported):
        self.rewrite_calls.append(supported)

    def set_language_choice_supported(self, supported):
        self.supported = supported
        self.support_calls.append(supported)

    def language_choice_offer(self):
        self.offer_calls += 1
        if not self.supported or not self.word or self.revision in self.dismissed:
            return None
        return deepcopy(
            {
                "revision": self.revision,
                "current_language": self.current_language,
                **self.readings,
            }
        )

    def dismiss_language_choice(self, revision):
        self.dismiss_calls.append(revision)
        if revision != self.revision or not self.word:
            return False
        self.dismissed.add(revision)
        return True

    def accept_language_choice(self, revision, language):
        self.accept_calls.append((revision, language))
        if (
            revision != self.revision
            or revision in self.dismissed
            or not self.word
            or not self.supported
        ):
            return None
        self.word = ""
        self.revision += 1
        return [("hide", ""), ("commit", self.readings[language])]

    def process_keycode(self, keycode, state):
        self.key_calls.append(("raw", keycode, state))
        return self._next()

    def handle_key(self, keyval, state):
        self.key_calls.append(("semantic", keyval, state))
        return self._next()

    def predictions(self):
        return [("draftytzz", 1.0, 0.9)]

    def set_surrounding_text(self, _text, _cursor):
        return None

    def focus_gained(self):
        return None

    def focus_lost(self):
        return self.reset()

    def reset(self):
        self.word = ""
        self.revision += 1
        return [("hide", "")]

    def save_personal(self):
        self.saved += 1


def _engine(monkeypatch, scripts=None, current_language="en", route=True):
    monkeypatch.setattr(ske, "_HAS_CORE", True)
    monkeypatch.setattr(ske, "_HAS_IBUS", True)
    monkeypatch.setattr(ske.IBus, "LookupTable", _LookupTable, raising=False)
    monkeypatch.setattr(
        ske.IBus, "Orientation", SimpleNamespace(VERTICAL=1), raising=False
    )
    # Stub only the external client/panel reachability observation.  Actual
    # caps-41 panel ownership and rendered rows need the private GTK witness.
    monkeypatch.setattr(
        ske.SmartKeyEngine,
        "_language_choice_ui_available",
        lambda _self: route,
        raising=False,
    )
    eng, rec = build_engine([])
    typed = {"en": "draftyt", "bg": "драфтът"}[current_language]
    eng._core = _ChoiceCore(
        scripts if scripts is not None else [[("composing", typed + "\0")]],
        current_language,
    )
    eng._sensitive = False
    eng._declared_ordinary = True
    eng._content_type_key = (0, 0)
    eng._last_save = float("inf")
    eng._o1 = FakeO1()
    rec.lookups = []
    rec.lookup_hides = 0
    rec.auxiliary = []

    def publish(table, visible):
        rec.lookups.append(
            (tuple(table.candidates), table.cursor_pos, table.cursor_visible, visible)
        )

    def hide():
        rec.lookup_hides += 1

    if route:
        eng.update_lookup_table = publish
        eng.hide_lookup_table = hide
        eng.update_auxiliary_text = lambda text, visible: rec.auxiliary.append(
            (text.s, visible)
        )
        eng.hide_auxiliary_text = lambda: None
    eng.do_set_capabilities(41)  # PREEDIT | FOCUS | SURROUNDING; no LOOKUP_TABLE bit.
    return eng, rec


def _type_final_letter(eng):
    assert eng.do_process_key_event(ord("t"), 20, 0) is True


def test_language_choice_caps41_publishes_two_whole_readings_without_changing_input(
    monkeypatch,
):
    for language, rows in (
        ("en", ("draftyt", "драфтът")),
        ("bg", ("драфтът", "draftyt")),
    ):
        eng, rec = _engine(monkeypatch, current_language=language)
        _type_final_letter(eng)
        assert eng._core.rewrite_calls == [True]
        assert eng._core.support_calls == [True], "caps41 must enable the panel route"
        assert rec.lookups[-1] == (rows, 0, False, True)
        assert eng._core.current_word() == rows[0]
        assert rec.preedits == [(rows[0], True)]
        assert rec.commits == [] and rec.forwarded_keys == []
        assert eng._core.accept_calls == [] and eng._o1.words == []

    eng, rec = _engine(monkeypatch, route=False)
    _type_final_letter(eng)
    assert not eng._core.supported, "no usable renderer must leave policy disabled"
    assert eng._core.offer_calls == 0 and rec.lookups == []


def test_language_choice_selected_arrow_tab_accepts_exact_row_and_preserves_unselected_keys(
    monkeypatch,
):
    for selection_keys in (
        (ske.IBus.KEY_Down,),
        (ske.IBus.KEY_Down, ske.IBus.KEY_Up, ske.IBus.KEY_Down),
    ):
        eng, rec = _engine(monkeypatch)
        events = capture_replay(eng)
        _type_final_letter(eng)
        before = list(eng._core.key_calls)
        for arrow in selection_keys:
            assert eng.do_process_key_event(
                arrow, 108 if arrow == ske.IBus.KEY_Down else 103, 0
            )
        assert eng._core.key_calls == before, "row navigation must not flush the token"
        assert eng._core.current_word() == "draftyt" and rec.commits == []
        assert rec.lookups[-1] == (("draftyt", "драфтът"), 1, True, True)
        assert eng.do_process_key_event(ske.IBus.KEY_Tab, 15, 0) is True
        assert eng._core.accept_calls == [(17, "bg")]
        assert eng._core.key_calls == before, "selected Tab must bypass ghost acceptance"
        assert rec.commits == ["драфтът"] and rec.forwarded_keys == []
        assert eng._core.current_word() == "" and eng._o1.words == ["драфтът"]
        assert not any(kind == "accepted" for kind, _ in events), "not a ghost acceptance"
        assert eng._active_prediction is None and rec.lookup_hides > 0

    eng, rec = _engine(
        monkeypatch,
        [[("composing", "draftyt\0zz")], [("hide", ""), ("commit", "draftytzz")]],
    )
    _type_final_letter(eng)
    assert eng.do_process_key_event(ske.IBus.KEY_Tab, 15, 0) is True
    assert eng._core.accept_calls == [] and rec.commits == ["draftytzz"]
    assert eng._o1.words == ["draftytzz"]

    for dismiss in (False, True):
        scripts = [[("composing", "draftyt\0")]]
        scripts.append([("hide", ""), ("commit", "draftyt"), ("forward", "")])
        eng, rec = _engine(monkeypatch, scripts)
        _type_final_letter(eng)
        if dismiss:
            before = list(eng._core.key_calls)
            assert eng.do_process_key_event(ske.IBus.KEY_Escape, 1, 0) is True
            assert eng._core.dismiss_calls == [17]
            assert eng._core.key_calls == before, "choice Escape must not cancel the word"
            assert eng._core.current_word() == "draftyt"
        assert eng.do_process_key_event(ske.IBus.KEY_space, 57, 0) is False
        assert eng._core.accept_calls == [] and rec.commits == ["draftyt"]
        assert eng._o1.words == ["draftyt"]


def test_language_choice_stale_privacy_and_lifecycle_events_cannot_accept_hidden_rows(
    monkeypatch,
):
    eng, rec = _engine(monkeypatch)
    _type_final_letter(eng)
    assert eng.do_process_key_event(ske.IBus.KEY_Down, 108, 0) is True
    before = list(eng._core.key_calls)
    eng._core.revision += 1  # Asynchronous Core invalidation, cached old table remains.
    assert eng.do_process_key_event(ske.IBus.KEY_Tab, 15, 0) is True
    assert eng._core.accept_calls == [(17, "bg")]
    assert eng._core.key_calls == before, "stale Tab cannot fall through to ghost accept"
    assert rec.commits == [] and eng._o1.words == [] and rec.lookup_hides > 0

    for transition in ("reset", "focus_out", "disable", "sensitive", "caps_drop"):
        eng, rec = _engine(monkeypatch)
        _type_final_letter(eng)
        assert eng.do_process_key_event(ske.IBus.KEY_Down, 108, 0) is True
        if transition == "sensitive":
            eng.do_set_content_type(8, 0)
        elif transition == "caps_drop":
            eng.do_set_capabilities(0)
        else:
            getattr(eng, "do_" + transition)()
        assert rec.lookup_hides > 0, transition
        assert rec.commits == [] and eng._o1.words == [], transition
        assert eng._core.accept_calls == [], transition
        before = len(eng._core.key_calls)
        eng.do_process_key_event(ske.IBus.KEY_Tab, 15, 0)
        assert eng._core.accept_calls == [], transition
        assert rec.commits == [], transition
        if transition == "sensitive":
            assert len(eng._core.key_calls) == before, "privacy stop precedes Core input"
            assert not eng._core.supported


@pytest.mark.parametrize(
    "state",
    [1 << 6, 1 << 26, ske._MOD_CONTROL, ske._MOD_ALT, 1, ske._MOD_RELEASE],
    ids=["physical-super", "super", "ctrl", "alt", "shift", "release"],
)
def test_language_choice_modified_arrows_and_tab_keep_the_normal_key_path(
    monkeypatch, state
):
    for keyval, keycode in ((ske.IBus.KEY_Down, 108), (ske.IBus.KEY_Tab, 15)):
        eng, rec = _engine(monkeypatch)
        _type_final_letter(eng)
        if keyval == ske.IBus.KEY_Tab:
            assert eng.do_process_key_event(ske.IBus.KEY_Down, 108, 0) is True
        before = len(eng._core.key_calls)
        assert eng.do_process_key_event(keyval, keycode, state) is False
        assert eng._core.key_calls[before:] == [("raw", keycode, state)]
        assert eng._core.accept_calls == [] and rec.commits == []


def test_language_choice_requires_an_explicit_ordinary_declaration(monkeypatch):
    eng, rec = _engine(monkeypatch)
    eng._declared_ordinary = False  # Legacy non-sensitive state is not authority.
    eng.do_set_capabilities(41)
    _type_final_letter(eng)
    assert not eng._core.supported
    assert eng._core.offer_calls == 0 and rec.lookups == []
    assert eng._core.current_word() == "draftyt" and rec.commits == []


def test_language_choice_route_cache_avoids_per_letter_bus_queries_and_rechecks_tab(
    monkeypatch,
):
    eng, rec = _engine(
        monkeypatch,
        [[("composing", "draftyt\0")], [("composing", "draftyt\0")]],
    )
    observations = []
    available = [True]

    def observe():
        observations.append(available[0])
        return available[0]

    eng._language_choice_ui_available = observe
    eng.do_set_capabilities(41)
    assert observations == [True]
    _type_final_letter(eng)
    _type_final_letter(eng)
    assert observations == [True], "ordinary letters must not synchronously query D-Bus"
    assert eng.do_process_key_event(ske.IBus.KEY_Down, 108, 0) is True
    assert observations == [True, True], "visible selection rechecks the actual route"
    available[0] = False
    before = list(eng._core.key_calls)
    assert eng.do_process_key_event(ske.IBus.KEY_Tab, 15, 0) is True
    assert observations == [True, True, False]
    assert eng._core.key_calls == before and eng._core.accept_calls == []
    assert not eng._core.supported and rec.commits == [] and rec.lookup_hides > 0
    available[0] = True
    eng.do_focus_in()
    assert observations == [True, True, False, True]


def test_language_choice_renderer_failure_is_consumed_without_losing_the_word(
    monkeypatch,
):
    eng, rec = _engine(monkeypatch)
    _type_final_letter(eng)

    def refuse(_table, _visible):
        raise RuntimeError("synthetic renderer refusal")

    eng.update_lookup_table = refuse
    before = list(eng._core.key_calls)
    assert eng.do_process_key_event(ske.IBus.KEY_Down, 108, 0) is True
    assert eng._core.key_calls == before and eng._core.current_word() == "draftyt"
    assert eng._core.accept_calls == [] and rec.commits == []
    assert not eng._core.supported and rec.lookup_hides > 0


@pytest.mark.parametrize(
    "sensitive, decision_key, expected_choice",
    [(False, None, True), (True, None, False), (False, (0, 0), False)],
    ids=["default-never-declared", "strict-never-declared", "inconsistent-declaration"],
)
def test_language_choice_never_declared_context_follows_existing_privacy_policy(
    monkeypatch, sensitive, decision_key, expected_choice
):
    eng, rec = _engine(monkeypatch)
    eng._sensitive = sensitive
    eng._declared_ordinary = False
    eng._content_type_key = decision_key
    eng.do_set_capabilities(41)
    consumed = eng.do_process_key_event(ord("t"), 20, 0)
    assert consumed is (not sensitive)
    assert eng._core.supported is expected_choice
    if expected_choice:
        assert rec.lookups == [(("draftyt", "драфтът"), 0, False, True)]
        assert eng._core.offer_calls == 1
        assert eng._core.current_word() == "draftyt"
    else:
        assert rec.lookups == [] and eng._core.offer_calls == 0
    if sensitive:
        assert eng._core.key_calls == [] and eng._core.current_word() == ""
    assert rec.commits == [] and eng._core.accept_calls == []
