#!/usr/bin/env bash
# Install the Phase-A lab engine as a user-level IBus component.
#
# This does not modify /usr/share or the installed "smartkey" engine. It copies
# the lab component XML into ~/.local/share/ibus/component and refreshes the
# user IBus registry cache so "ibus list-engine" can see "smartkey-phasea".
set -euo pipefail

LAB=/home/rmanov/smartkey-phase-a-lab
COMPONENT_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/ibus/component"
COMPONENT="$COMPONENT_DIR/smartkey-phasea.xml"

cd "$LAB"
chmod +x "$LAB/phase_a/ibus-engine-smartkey-phasea"
mkdir -p "$COMPONENT_DIR"
cp "$LAB/phase_a/smartkey-phasea.xml" "$COMPONENT"
IBUS_COMPONENT_PATH="$COMPONENT_DIR:/usr/share/ibus/component" ibus write-cache

echo "installed: $COMPONENT"
if ibus list-engine | grep -q '^  smartkey-phasea '; then
    echo "OK: smartkey-phasea is visible in ibus list-engine"
else
    echo "WARN: smartkey-phasea not visible yet; run 'ibus restart' or restart ibus-daemon"
    exit 1
fi
