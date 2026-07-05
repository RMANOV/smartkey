#!/usr/bin/env bash
# CONDUCTOR-ONLY: switch the operator's live ibus to the lab engine — but only
# after the precondition gate is green and the current engine has been captured
# for rollback. Requires --confirm so it can never fire accidentally.
#
#   phase_a/switch-to-lab.sh --confirm
#
# PREREQUISITE: the lab engine process must already be running:
#   phase_a/run-lab-engine.sh   (in its own terminal/session)
set -uo pipefail

LAB=/home/rmanov/smartkey-phase-a-lab
SAVED="$LAB/phase_a_data/rollback-engine.txt"
LAB_ENGINE="${PHASEA_LAB_ENGINE:-smartkey}"   # ibus engine name the lab process registers

_ibus_engine() { [[ "${PHASEA_IBUS_DRYRUN:-0}" == "1" ]] && echo "[dryrun] ibus engine $1" || ibus engine "$1"; }
_ibus_current() { [[ "${PHASEA_IBUS_DRYRUN:-0}" == "1" ]] && echo "${PHASEA_FAKE_CURRENT:-xkb:us::eng}" || ibus engine 2>/dev/null; }

if [[ "${1:-}" != "--confirm" ]]; then
    echo "refusing: pass --confirm (and only after preflight + manual dry-run are green)."
    exit 2
fi

echo "== running precondition gate =="
if [[ "${PHASEA_SKIP_PREFLIGHT:-0}" != "1" ]]; then
    if ! "$LAB/phase_a/preflight.sh"; then
        echo "PREFLIGHT RED — aborting switch."
        exit 1
    fi
fi

# Capture the current live engine FIRST — this is what rollback restores.
mkdir -p "$LAB/phase_a_data"
cur="$(_ibus_current)"
if [[ -z "$cur" ]]; then
    echo "could not read current engine; aborting (rollback target unknown)."
    exit 1
fi
echo "$cur" > "$SAVED"
echo "captured current engine for rollback: '$cur' -> $SAVED"

echo ""
echo ">>> INSTANT ROLLBACK (keep this ready):  $LAB/phase_a/rollback.sh"
echo ">>> PANIC ROLLBACK  (bare keyboard)   :  $LAB/phase_a/rollback.sh --panic"
echo ""

echo "== switching to lab engine '$LAB_ENGINE' =="
_ibus_engine "$LAB_ENGINE"
now="$(_ibus_current)"
echo "engine now reports: '$now'  (expected '$LAB_ENGINE')"
echo "Type a sentence to confirm. If anything is wrong, run rollback IMMEDIATELY."
echo "Phase-A зелено ≠ доказателство за team tier."
