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

# Dry-run / test hook: set PHASEA_IBUS_DRYRUN=1 to echo instead of switching.
_ibus_engine() {
    if [[ "${PHASEA_IBUS_DRYRUN:-0}" == "1" ]]; then
        echo "[dryrun] ibus engine $1"
    else
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
        prev="$(_ibus_current)"
        echo "LIVE rollback dry-run: current='$prev'  panic='$PANIC_ENGINE'"
        _ibus_engine "$PANIC_ENGINE"
        echo ">>> Type a few characters now to confirm the bare layout works. <<<"
        _ibus_engine "$prev"
        echo "restored to '$prev'. If both switches printed cleanly, rollback is proven."
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
