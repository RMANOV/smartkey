"""Optional read-only KDE layout metadata; importing this module loads no GI."""

_NAME, _PATH, _IFACE = "org.kde.keyboard", "/Layouts", "org.kde.KeyboardLayouts"


class _GioBackend:
    def __init__(self, gio=None, connection=None):
        if gio is None:
            from gi.repository import Gio
            gio = Gio
        self.gio = gio
        self.connection = connection or gio.bus_get_sync(gio.BusType.SESSION, None)
        self.handles = []
        self.layouts = None

    def start(self, changed):
        self.changed = changed
        subscribe = self.connection.signal_subscribe
        self.handles.append(subscribe(
            _NAME, _IFACE, None, _PATH, None,
            self.gio.DBusSignalFlags.NONE, self._signal))
        self.handles.append(subscribe(
            "org.freedesktop.DBus", "org.freedesktop.DBus", "NameOwnerChanged",
            "/org/freedesktop/DBus", _NAME, self.gio.DBusSignalFlags.NONE,
            self._owner))
        changed()  # Initial metadata is verified before the observer returns.

    def snapshot(self):
        def read(method):
            return self.connection.call_sync(
                _NAME, _PATH, _IFACE, method, None, None,
                self.gio.DBusCallFlags.NO_AUTO_START, 250, None).unpack()[0]
        layouts = read("getLayoutsList")
        index = read("getLayout")
        self.layouts = layouts
        return index, layouts

    def _signal(self, _connection, _sender, _path, _interface, name, parameters):
        if name == "layoutChanged" and self.layouts is not None:
            try:
                self.changed((parameters.unpack()[0], self.layouts))
            except Exception:
                pass  # Malformed metadata cannot replace a verified reading.
        elif name in ("layoutChanged", "layoutListChanged"):
            self.changed()

    def _owner(self, *_args):
        self.layouts = None
        self.changed()

    def close(self):
        for handle in self.handles:
            self.connection.signal_unsubscribe(handle)
        self.handles.clear()


class KdeLayoutObserver:
    def __init__(self, backend=None):
        self.manual_bg = False
        self._verified = False
        self._closed = False
        self._backend = backend if backend is not None else _GioBackend()
        try:
            self._backend.start(self.refresh)
        except Exception:
            self.close()
            raise

    @property
    def verified(self):
        return self._verified

    def refresh(self, snapshot=None):
        if self._closed:
            return
        try:
            index, layouts = snapshot if snapshot is not None else self._backend.snapshot()
            if type(index) is not int or not isinstance(layouts, (list, tuple)):
                return
            if not 0 <= index < len(layouts):
                return
            row = layouts[index]
            if (not isinstance(row, (list, tuple)) or len(row) != 3
                    or not all(type(value) is str for value in row)):
                return
            self.manual_bg = row[0] == "bg"
            self._verified = True
        except Exception:
            pass  # Startup unknown stays False; later errors hold the last proof.

    def close(self):
        self._closed = True
        self._backend.close()


def create_kde_layout_observer():
    try:
        # Keep successful subscriptions even when the first read is unavailable;
        # owner/layout signals can verify metadata without another focus event.
        return KdeLayoutObserver()
    except Exception:
        pass
    return None
