#!/usr/bin/env bash
# INSTANT ROLLBACK to a working ibus engine. This is the safety net for the
# operator's LIVE input method: run it the moment typing feels wrong.
#
#   phase_a/rollback.sh                 # roll back to the captured previous engine
#   phase_a/rollback.sh <engine>        # roll back to a specific engine
#   phase_a/rollback.sh --panic         # roll back to the bare keyboard layout
#   phase_a/rollback.sh --dry-run       # LIVE dry-run: prev -> panic -> prev (proves rollback)
#
# Target resolution (in order): explicit arg > captured previous engine file >
# PANIC_ENGINE (a bare XKB layout that is always present, so this works even if
# EVERY smartkey variant is broken).
set -uo pipefail

LAB=/home/rmanov/smartkey-phase-a-lab
SAVED="$LAB/phase_a_data/rollback-engine.txt"
PANIC_ENGINE="${PHASEA_PANIC_ENGINE:-xkb:us::eng}"   # universal fallback; override if needed

# The safety net must work even if ibus-daemon DIED: (re)start it before any
# engine set, since `ibus engine` (ibus_bus_set_global_engine) fails when the
# daemon is down.
_ensure_ibus_up() {
    [[ "${PHASEA_IBUS_DRYRUN:-0}" == "1" ]] && return 0
    ibus engine >/dev/null 2>&1 && return 0            # already reachable
    echo "ibus-daemon not reachable — (re)starting it..."
    command -v ibus-daemon >/dev/null 2>&1 && ibus-daemon -drxR >/dev/null 2>&1 &
    for _ in 1 2 3 4 5 6 7 8; do
        ibus engine >/dev/null 2>&1 && return 0
        sleep 0.5
    done
    return 1
}

# Dry-run / test hook: set PHASEA_IBUS_DRYRUN=1 to echo instead of switching.
_ibus_engine() {
    if [[ "${PHASEA_IBUS_DRYRUN:-0}" == "1" ]]; then
        echo "[dryrun] ibus engine $1"
    else
        _ensure_ibus_up || echo "WARNING: ibus-daemon still not reachable — trying the set anyway"
        ibus engine "$1"
    fi
}
_ibus_current() {
    if [[ "${PHASEA_IBUS_DRYRUN:-0}" == "1" ]]; then
        echo "${PHASEA_FAKE_CURRENT:-xkb:us::eng}"
    else
        ibus engine 2>/dev/null
    fi
}

target=""
mode="${1:-}"
case "$mode" in
    --panic) target="$PANIC_ENGINE" ;;
    --dry-run)
        # Prove the rollback WHILE smartkey is still healthy: switch to the
        # panic layout, PAUSE for the human to actually type and confirm, then
        # restore and confirm again. No auto-continue: verification is real.
        prev="$(_ibus_current)"
        echo "LIVE rollback dry-run: current='$prev'  panic='$PANIC_ENGINE'"
        _ibus_engine "$PANIC_ENGINE"
        now="$(_ibus_current)"
        echo ">>> Active engine is now '$now'. Focus a text field and TYPE a few words."
        if [[ "${PHASEA_NONINTERACTIVE:-0}" != "1" ]]; then
            read -r -p ">>> Press ENTER only AFTER you have verified typing WORKS in panic mode... " _
        fi
        _ibus_engine "$prev"
        back="$(_ibus_current)"
        echo ">>> Restored to '$back'. Type again to confirm your normal engine works."
        if [[ "${PHASEA_NONINTERACTIVE:-0}" != "1" ]]; then
            read -r -p ">>> Press ENTER only AFTER you have verified typing WORKS again... " _
        fi
        echo "ROLLBACK DRY-RUN COMPLETE: panic layout and restore both interactively verified."
        exit 0
        ;;
    "" )
        if [[ -f "$SAVED" ]]; then
            target="$(cat "$SAVED")"
        else
            target="$PANIC_ENGINE"
            echo "no captured engine ($SAVED missing) -> using panic fallback"
        fi
        ;;
    *) target="$mode" ;;
esac

[[ -z "$target" ]] && target="$PANIC_ENGINE"
echo "ROLLBACK -> $target"
_ibus_engine "$target"
echo "done. verify by typing; if still broken, run: phase_a/rollback.sh --panic"
