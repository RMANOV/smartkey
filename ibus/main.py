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

import logging
import os
import signal
import sys
from pathlib import Path

# ---------------------------------------------------------------------------
# IBus GObject introspection -- graceful fallback if not installed.
# ---------------------------------------------------------------------------
try:
    import gi

    gi.require_version("IBus", "1.0")
    gi.require_version("GLib", "2.0")
    from gi.repository import GLib, GObject, IBus  # type: ignore[attr-defined]

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
    # --xml: print component XML for ibus write-cache discovery, then exit.
    if "--xml" in sys.argv:
        _xml = (
            '<?xml version="1.0" encoding="utf-8"?>\n'
            "<component>\n"
            f"  <name>{_BUS_NAME}</name>\n"
            "  <description>SmartKey Predictive Input</description>\n"
            "  <version>0.1.0</version>\n"
            "  <license>MIT</license>\n"
            "  <author>SmartKey Contributors</author>\n"
            "  <homepage>https://github.com/RMANOV/smartkey</homepage>\n"
            "  <engines>\n"
            "    <engine>\n"
            f"      <name>{_ENGINE_NAME}</name>\n"
            "      <language>en</language>\n"
            "      <license>MIT</license>\n"
            "      <author>SmartKey Contributors</author>\n"
            "      <icon>preferences-desktop-keyboard</icon>\n"
            "      <layout>default</layout>\n"
            "      <longname>SmartKey Predictive Input</longname>\n"
            "      <description>Predictive text input with ghost text</description>\n"
            "      <rank>80</rank>\n"
            "      <symbol>SK</symbol>\n"
            "    </engine>\n"
            "  </engines>\n"
            "</component>\n"
        )
        print(_xml)
        return

    if not _HAS_IBUS:
        print(
            "ERROR: IBus GObject introspection bindings not found.\n"
            "Install the 'ibus' package and 'python3-gi' (or equivalent) first.",
            file=sys.stderr,
        )
        sys.exit(1)

    # Parse IBus-specific flags (--ibus).
    is_ibus = "--ibus" in sys.argv

    # Set up logging to XDG-compliant location.
    log_dir = (
        Path(os.environ.get("XDG_DATA_HOME", str(Path.home() / ".local" / "share")))
        / "smartkey"
    )
    log_dir.mkdir(parents=True, exist_ok=True)
    log_level = logging.DEBUG if os.environ.get("SMARTKEY_DEBUG") else logging.WARNING
    logging.basicConfig(
        level=log_level,
        format="%(asctime)s smartkey: %(message)s",
        filename=str(log_dir / "smartkey.log"),
    )

    # Initialise IBus.
    IBus.init()

    # Connect to the IBus bus.
    bus = IBus.Bus()
    if not bus.is_connected():
        print(
            "ERROR: Cannot connect to IBus bus. Is ibus-daemon running?",
            file=sys.stderr,
        )
        sys.exit(1)

    # Create the engine factory.
    factory = IBus.Factory.new(bus.get_connection())
    factory.add_engine(_ENGINE_NAME, GObject.type_from_name("SmartKeyEngine"))

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

    # Graceful shutdown: quit GLib loop on SIGTERM/SIGINT.

    GLib.unix_signal_add(GLib.PRIORITY_HIGH, signal.SIGTERM, loop.quit)
    GLib.unix_signal_add(GLib.PRIORITY_HIGH, signal.SIGINT, loop.quit)

    loop.run()


if __name__ == "__main__":
    main()
