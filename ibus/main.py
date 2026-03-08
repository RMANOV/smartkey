#!/usr/bin/env python3
"""SmartKey IBus daemon entry point.

Connects to the IBus bus, registers a factory for the ``smartkey`` engine,
and enters the GLib main loop.

Usage::

    ibus-daemon -drxR            # ensure IBus is running
    python3 -m ibus.main --ibus  # start the SmartKey engine process

The ``--ibus`` flag tells the process it was launched by IBus and should
connect to the existing bus rather than spawning a new one.
"""

from __future__ import annotations

import sys

# ---------------------------------------------------------------------------
# IBus GObject introspection -- graceful fallback if not installed.
# ---------------------------------------------------------------------------
try:
    import gi

    gi.require_version("IBus", "1.0")
    gi.require_version("GLib", "2.0")
    from gi.repository import GLib, IBus  # type: ignore[attr-defined]

    _HAS_IBUS = True
except (ValueError, ImportError):
    _HAS_IBUS = False

# Engine class import (side-effect: registers the GType).
from smartkey_engine import SmartKeyEngine  # noqa: E402, F401

# ---------------------------------------------------------------------------
# Constants.
# ---------------------------------------------------------------------------
_BUS_NAME = "org.freedesktop.IBus.SmartKey"
_OBJECT_PATH = "/org/freedesktop/IBus/Engine/SmartKey"
_ENGINE_NAME = "smartkey"


# ---------------------------------------------------------------------------
# Main.
# ---------------------------------------------------------------------------
def main() -> None:
    if not _HAS_IBUS:
        print(
            "ERROR: IBus GObject introspection bindings not found.\n"
            "Install the 'ibus' package and 'python3-gi' (or equivalent) first.",
            file=sys.stderr,
        )
        sys.exit(1)

    # Parse IBus-specific flags (--ibus).
    is_ibus = "--ibus" in sys.argv

    # Initialise IBus.
    IBus.init()

    # Connect to the IBus bus.
    bus = IBus.Bus()
    if not bus.is_connected():
        print("ERROR: Cannot connect to IBus bus. Is ibus-daemon running?", file=sys.stderr)
        sys.exit(1)

    # Request the well-known bus name.
    bus.request_name(_BUS_NAME, 0)

    # Create the engine factory.
    factory = IBus.Factory.new(bus.get_connection())
    factory.add_engine(_ENGINE_NAME, GLib.type_from_name("SmartKeyEngine"))

    if is_ibus:
        bus.request_name(_BUS_NAME, 0)
    else:
        # When launched manually (not by IBus), create the engine component
        # so the user can switch to it.
        component = IBus.Component.new(
            _BUS_NAME,
            "SmartKey Predictive Input",
            "0.1.0",
            "MIT",
            "SmartKey Contributors",
            "https://github.com/RMANOV/smartkey",
            "",
            "smartkey",
        )
        engine_desc = IBus.EngineDesc.new(
            _ENGINE_NAME,
            "SmartKey Predictive Input",
            "Predictive text input with ghost text completion",
            "en",
            "MIT",
            "SmartKey Contributors",
            "preferences-desktop-keyboard",
            "default",
        )
        component.add_engine(engine_desc)
        bus.register_component(component)

    # Enter the GLib main loop.
    loop = GLib.MainLoop()
    loop.run()


if __name__ == "__main__":
    main()
