"""Client rewrite capability must reach Core before learning or processing.

Reuse the native/debug-fenced fake-IBus harness: no host engine, native import,
personal profile, display, or bus is reachable through these tests.
"""

from __future__ import annotations

import pytest

from .test_space_accept import ske


def _constructor_fixture(monkeypatch, core):
    events = []
    monkeypatch.setattr(ske, "_HAS_CORE", True)
    monkeypatch.setattr(ske, "_load_json", lambda _path: {})
    monkeypatch.setattr(ske, "_sensitive_until_declared", lambda: False)
    monkeypatch.setattr(ske, "KeystrokeTrace", None)

    def create_core(_config_json):
        events.append("create")
        return core

    monkeypatch.setattr(ske, "PyInputMethodCore", create_core)
    monkeypatch.setattr(
        ske.SmartKeyEngine, "_load_corpus", lambda _self: events.append("corpus")
    )
    core.load_personal = lambda: events.append("personal")
    return events


def test_constructor_disables_external_rewrite_before_profile_or_corpus(monkeypatch):
    class Core:
        def handoff_preedit_for_native_completion(self):
            raise AssertionError("startup must not hand off text")

        def reconcile_native_completion_word(self, caret_collapsed=False):
            raise AssertionError("startup must not reconcile text")

        def set_external_rewrite_supported(self, supported):
            events.append(("external_rewrite", supported))

        def flush_preedit_for_layout(self):
            raise AssertionError("startup must validate layout handoff without flushing")

    events = _constructor_fixture(monkeypatch, Core())
    engine = ske.SmartKeyEngine()
    assert engine._caps == 0
    assert events == ["create", ("external_rewrite", False), "personal", "corpus"]


@pytest.mark.parametrize("api", ["missing", "noncallable"])
def test_incompatible_layout_handoff_refuses_startup_before_loading_profile(
    monkeypatch, api
):
    class Core:
        def set_external_rewrite_supported(self, supported):
            events.append(("external_rewrite", supported))

    core = Core()
    if api == "noncallable":
        core.flush_preedit_for_layout = None
    events = _constructor_fixture(monkeypatch, core)

    with pytest.raises(RuntimeError, match="layout handoff"):
        ske.SmartKeyEngine()
    assert events == ["create", ("external_rewrite", False)]


@pytest.mark.parametrize("api", ["missing", "noncallable", "raising"])
def test_incompatible_native_refuses_startup_before_loading_profile(monkeypatch, api):
    class Core:
        pass

    core = Core()
    if api == "noncallable":
        core.set_external_rewrite_supported = None
    elif api == "raising":
        def fail(_supported):
            raise ValueError("synthetic incompatible native")

        core.set_external_rewrite_supported = fail
    events = _constructor_fixture(monkeypatch, core)

    with pytest.raises(RuntimeError, match="external rewrite"):
        ske.SmartKeyEngine()
    assert events == ["create"]


def test_client_capability_updates_enable_only_surrounding_text_rewrites():
    supported_values = []

    class Core:
        def set_external_rewrite_supported(self, supported):
            supported_values.append(supported)

    engine = ske.SmartKeyEngine.__new__(ske.SmartKeyEngine)
    engine._core = Core()
    engine._caps = 0
    for caps in (9, 41, 0, 63, 9):
        engine.do_set_capabilities(caps)
        assert engine._caps == caps
    assert supported_values == [False, True, False, True, False]


@pytest.mark.parametrize("text,cursor", [("", 0), (" \t\n", 3), (None, None), ("hello", 5)])
def test_surrounding_report_preserves_known_empty_and_missing_distinction(text, cursor):
    reports = []

    class Core:
        def set_surrounding_text(self, reported_text, reported_cursor):
            reports.append((reported_text, reported_cursor))

    engine = ske.SmartKeyEngine.__new__(ske.SmartKeyEngine)
    engine._core = Core()
    engine._sync_surrounding_text(text, cursor)
    assert reports == [(text, cursor)]
    assert engine._surrounding_text == text
    assert engine._surrounding_cursor_pos == cursor


@pytest.mark.parametrize("missing", ["handoff_preedit_for_native_completion", "reconcile_native_completion_word"])
@pytest.mark.parametrize("noncallable", [False, True])
def test_incompatible_native_completion_refuses_startup_before_profile(monkeypatch, missing, noncallable):
    class Core:
        def set_external_rewrite_supported(self, supported):
            events.append(("external_rewrite", supported))

        def flush_preedit_for_layout(self):
            raise AssertionError("startup must not deliver text")

    core = Core()
    for api in ("handoff_preedit_for_native_completion", "reconcile_native_completion_word"):
        if api != missing:
            setattr(core, api, lambda *args: None)
    if noncallable:
        setattr(core, missing, None)
    events = _constructor_fixture(monkeypatch, core)
    with pytest.raises(RuntimeError, match="native completion handoff"):
        ske.SmartKeyEngine()
    assert events == ["create", ("external_rewrite", False)]
