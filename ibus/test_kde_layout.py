"""Metadata-only KDE layout contract; no GI, bus or keyboard is used."""
import importlib.util
from pathlib import Path

import pytest

spec = importlib.util.spec_from_file_location(
    "smartkey_kde_layout_contract", Path(__file__).with_name("kde_layout.py"))
kde = importlib.util.module_from_spec(spec)
spec.loader.exec_module(kde)
LAYOUTS = [("us", "", "English (US)"),
           ("bg", "", "Bulgarian (traditional phonetic)")]


class Backend:
    def __init__(self, index=0, layouts=LAYOUTS):
        self.value = index, layouts
        self.error = None

    def start(self, changed):
        self.changed = changed
        changed()

    def snapshot(self):
        if self.error:
            raise self.error
        return self.value

    def emit(self, index, layouts=LAYOUTS):
        self.value = index, layouts
        self.error = None
        self.changed()

    def close(self):
        pass


@pytest.mark.parametrize("index,wanted", [(0, False), (1, True)])
def test_verified_layout_id_selects_manual_bg(index, wanted):
    assert kde.KdeLayoutObserver(Backend(index)).manual_bg is wanted


@pytest.mark.parametrize("index,layouts", [
    (-1, LAYOUTS), (2, LAYOUTS), (True, LAYOUTS), ("1", LAYOUTS),
    (None, LAYOUTS), (0, []), (0, None), (0, ["bg"]),
    (0, [("bg", "missing field")]), (0, [(b"bg", "", "BG")]),
])
def test_invalid_unverified_snapshot_does_not_claim_manual_bg(index, layouts):
    assert kde.KdeLayoutObserver(Backend(index, layouts)).manual_bg is False


def test_id_is_authority_not_human_label_or_cyrillic_text():
    backend = Backend(0, [("us", "bg", "Български")])
    observer = kde.KdeLayoutObserver(backend)
    assert observer.manual_bg is False
    backend.emit(0, [("bg", "en", "English label")])
    assert observer.manual_bg is True


def test_layout_and_list_changes_update_only_verified_metadata():
    backend = Backend()
    observer = kde.KdeLayoutObserver(backend)
    backend.emit(1)
    assert observer.manual_bg is True
    backend.emit(0, list(reversed(LAYOUTS)))
    assert observer.manual_bg is True
    backend.emit(1, list(reversed(LAYOUTS)))
    assert observer.manual_bg is False


def test_owner_loss_read_error_and_invalid_index_hold_last_verified_bg():
    backend = Backend(1)
    observer = kde.KdeLayoutObserver(backend)
    assert observer.manual_bg is True
    backend.error = RuntimeError("owner absent / read timeout")
    backend.changed()
    assert observer.manual_bg is True
    backend.emit(-1)
    assert observer.manual_bg is True
    backend.emit(0)
    assert observer.manual_bg is False


def test_initial_absence_is_optional_and_later_owner_can_supply_layout():
    backend = Backend()
    backend.error = RuntimeError("no KDE owner")
    observer = kde.KdeLayoutObserver(backend)
    assert observer.manual_bg is False
    backend.emit(1)
    assert observer.manual_bg is True


def test_close_ignores_late_metadata_callbacks():
    backend = Backend(1)
    observer = kde.KdeLayoutObserver(backend)
    assert observer.manual_bg is True
    observer.close()
    backend.emit(0)
    assert observer.manual_bg is True


def test_optional_factory_returns_none_when_backend_cannot_start(monkeypatch):
    def unavailable():
        raise ImportError("GI is not installed")
    monkeypatch.setattr(kde, "_GioBackend", unavailable)
    assert kde.create_kde_layout_observer() is None


def test_factory_retains_subscribed_observer_for_later_owner_without_focus(monkeypatch):
    from types import SimpleNamespace

    class Variant:
        def __init__(self, value):
            self.value = value

        def unpack(self):
            return self.value

    class Connection:
        def __init__(self):
            self.available = False
            self.callbacks = {}
            self.calls = []

        def signal_subscribe(self, sender, interface, member, path, arg0,
                             flags, callback):
            handle = len(self.callbacks) + 1
            self.callbacks[handle] = callback
            return handle

        def signal_unsubscribe(self, handle):
            del self.callbacks[handle]

        def call_sync(self, name, path, interface, method, params, result_type,
                      flags, timeout, cancellable):
            assert (name, path, interface) == (
                "org.kde.keyboard", "/Layouts", "org.kde.KeyboardLayouts")
            assert flags == 1 and timeout == 250
            self.calls.append(method)
            if not self.available:
                raise RuntimeError("KDE owner unavailable during initial read")
            assert method in ("getLayoutsList", "getLayout")
            return Variant((LAYOUTS,) if method == "getLayoutsList" else (1,))

    gio = SimpleNamespace(DBusCallFlags=SimpleNamespace(NO_AUTO_START=1),
                          DBusSignalFlags=SimpleNamespace(NONE=0))
    connection = Connection()
    backend = kde._GioBackend(gio, connection)
    monkeypatch.setattr(kde, "_GioBackend", lambda: backend)
    observer = kde.create_kde_layout_observer()
    assert observer is not None
    assert observer.manual_bg is False and observer._verified is False

    connection.available = True
    connection.callbacks[2](connection, ":1.0", "/org/freedesktop/DBus",
                            "org.freedesktop.DBus", "NameOwnerChanged",
                            Variant(("org.kde.keyboard", "", ":1.42")))
    assert observer.manual_bg is True and observer._verified is True
    calls = list(connection.calls)
    connection.callbacks[1](connection, ":1.42", "/Layouts",
                            "org.kde.KeyboardLayouts", "layoutChanged",
                            Variant((0,)))
    assert observer.manual_bg is False
    assert connection.calls == calls  # Cached layout signal needs no focus/query.
    observer.close()
    assert connection.callbacks == {}


def test_unverified_factory_observer_can_refresh_with_same_owner(monkeypatch):
    backend = Backend(1)
    backend.error = RuntimeError("temporary initial read failure")
    monkeypatch.setattr(kde, "_GioBackend", lambda: backend)
    observer = kde.create_kde_layout_observer()
    assert observer is not None
    assert getattr(observer, "verified", None) is False
    assert observer.manual_bg is False
    backend.error = None  # Owner did not change and emitted no layout signal.
    observer.refresh()
    assert observer.verified is True and observer.manual_bg is True
    with pytest.raises(AttributeError):
        observer.verified = False


def test_partial_subscription_failure_releases_owned_handler(monkeypatch):
    from types import SimpleNamespace

    class Connection:
        def __init__(self):
            self.handlers = {}
            self.subscribe_calls = 0
            self.metadata_calls = 0

        def signal_subscribe(self, *args):
            self.subscribe_calls += 1
            if self.subscribe_calls == 2:
                raise RuntimeError("second subscription unavailable")
            self.handlers[1] = args[-1]
            return 1

        def signal_unsubscribe(self, handle):
            del self.handlers[handle]

        def call_sync(self, *args):
            self.metadata_calls += 1
            raise AssertionError("partial startup must not query metadata")

    gio = SimpleNamespace(DBusSignalFlags=SimpleNamespace(NONE=0))
    connection = Connection()
    backend = kde._GioBackend(gio, connection)
    monkeypatch.setattr(kde, "_GioBackend", lambda: backend)
    assert kde.create_kde_layout_observer() is None
    assert connection.subscribe_calls == 2
    assert connection.metadata_calls == 0
    assert connection.handlers == {} and backend.handles == []


def test_direct_dbus_is_readonly_bounded_and_layout_signal_needs_no_query():
    from types import SimpleNamespace

    class Variant:
        def __init__(self, value):
            self.value = value

        def unpack(self):
            return self.value

    class Connection:
        def __init__(self):
            self.callbacks = {}
            self.calls = []

        def call_sync(self, name, path, interface, method, params, result_type,
                      flags, timeout, cancellable):
            assert (name, path, interface) == (
                "org.kde.keyboard", "/Layouts", "org.kde.KeyboardLayouts")
            assert flags == 1 and timeout == 250
            assert method in ("getLayoutsList", "getLayout")
            self.calls.append(method)
            return Variant((LAYOUTS,) if method == "getLayoutsList" else (1,))

        def signal_subscribe(self, sender, interface, member, path, arg0,
                             flags, callback):
            self.callbacks[len(self.callbacks) + 1] = callback
            return len(self.callbacks)

        def signal_unsubscribe(self, handle):
            del self.callbacks[handle]

    gio = SimpleNamespace(DBusCallFlags=SimpleNamespace(NO_AUTO_START=1),
                          DBusSignalFlags=SimpleNamespace(NONE=0))
    connection = Connection()
    observer = kde.KdeLayoutObserver(kde._GioBackend(gio, connection))
    assert observer.manual_bg is True  # Initial query completed, no ready race.
    assert connection.calls == ["getLayoutsList", "getLayout"]
    callback = connection.callbacks[1]
    callback(connection, ":1.42", "/Layouts", "org.kde.KeyboardLayouts",
             "layoutChanged", Variant((0,)))
    assert observer.manual_bg is False
    assert connection.calls == ["getLayoutsList", "getLayout"]
    observer.close()
