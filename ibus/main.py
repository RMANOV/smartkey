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

# Engine module import (side-effect: registers the GType).
try:
    from . import smartkey_engine as _smartkey_engine  # type: ignore[attr-defined]
except ImportError:
    import smartkey_engine as _smartkey_engine  # type: ignore[import-not-found]

_ENGINE_MODULE = _smartkey_engine

# ---------------------------------------------------------------------------
# Constants.
# ---------------------------------------------------------------------------
_BUS_NAME = "org.freedesktop.IBus.SmartKey"
_OBJECT_PATH = "/org/freedesktop/IBus/Engine/SmartKey"
_ENGINE_NAME = "smartkey"
_ENGINE_VERSION = _ENGINE_MODULE._NATIVE_VERSION
_ENGINE_LICENSE = "GPL-3.0-only"
_ENGINE_AUTHOR = "SmartKey Contributors"
_ENGINE_DESCRIPTION = "Predictive text input with ghost text completion"


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
            f"  <version>{_ENGINE_VERSION}</version>\n"
            f"  <license>{_ENGINE_LICENSE}</license>\n"
            f"  <author>{_ENGINE_AUTHOR}</author>\n"
            "  <homepage>https://github.com/RMANOV/smartkey</homepage>\n"
            "  <engines>\n"
            "    <engine>\n"
            f"      <name>{_ENGINE_NAME}</name>\n"
            "      <language>en</language>\n"
            f"      <license>{_ENGINE_LICENSE}</license>\n"
            f"      <author>{_ENGINE_AUTHOR}</author>\n"
            "      <icon>preferences-desktop-keyboard</icon>\n"
            "      <layout>default</layout>\n"
            "      <longname>SmartKey Predictive Input</longname>\n"
            f"      <description>{_ENGINE_DESCRIPTION}</description>\n"
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
    log_path = _ENGINE_MODULE._prepare_private_log_path(log_dir / "smartkey.log")
    # logging.DEBUG makes the engine log verbatim action payloads to
    # smartkey.log — content capture, so it is gated on the CONTENT level
    # only ("full"), same as the legacy content logs.  SMARTKEY_DEBUG=1
    # (structural keystroke trace) must stay content-free end to end.
    log_level = (
        logging.DEBUG
        if os.environ.get("SMARTKEY_DEBUG") == "full"
        else logging.WARNING
    )
    logging.basicConfig(
        level=log_level,
        format="%(asctime)s smartkey: %(message)s",
        filename=str(log_path),
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
            _ENGINE_VERSION,
            _ENGINE_LICENSE,
            _ENGINE_AUTHOR,
            "https://github.com/RMANOV/smartkey",
            "",
            "smartkey",
        )
        engine_desc = IBus.EngineDesc.new(
            _ENGINE_NAME,
            "SmartKey Predictive Input",
            _ENGINE_DESCRIPTION,
            "en",
            _ENGINE_LICENSE,
            _ENGINE_AUTHOR,
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
