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
        def set_external_rewrite_supported(self, supported):
            events.append(("external_rewrite", supported))

    events = _constructor_fixture(monkeypatch, Core())
    engine = ske.SmartKeyEngine()
    assert engine._caps == 0
    assert events == ["create", ("external_rewrite", False), "personal", "corpus"]


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
