#!/usr/bin/env bash
# CONDUCTOR-ONLY: switch the operator's live ibus to the lab engine. Guarded:
# it runs the precondition gate, captures the current engine for rollback,
# switches, and then FAILS CLOSED (auto-rollback) unless it can mechanically
# prove the selected engine is THIS lab process. Requires --confirm.
#
#   phase_a/switch-to-lab.sh --confirm
#
# PREREQUISITE: the lab engine process must already be running:
#   phase_a/run-lab-engine.sh   (in its own terminal/session)
set -uo pipefail

LAB=/home/rmanov/smartkey-phase-a-lab
PY=/home/rmanov/smartkey/.venv/bin/python3
SAVED="$LAB/phase_a_data/rollback-engine.txt"
IDENT="$LAB/phase_a_data/ENGINE-IDENTITY.json"
LAB_ENGINE="${PHASEA_LAB_ENGINE:-smartkey-phasea}"

_ibus_engine() { [[ "${PHASEA_IBUS_DRYRUN:-0}" == "1" ]] && echo "[dryrun] ibus engine $1" >&2 || ibus engine "$1"; }
_ibus_current() { [[ "${PHASEA_IBUS_DRYRUN:-0}" == "1" ]] && echo "${PHASEA_FAKE_CURRENT:-$LAB_ENGINE}" || ibus engine 2>/dev/null; }

fail_closed() {
    echo "!! SWITCH VERIFICATION FAILED: $1"
    echo "!! FAILING CLOSED — rolling back to the captured engine NOW."
    "$LAB/phase_a/rollback.sh" >/dev/null 2>&1 || "$LAB/phase_a/rollback.sh" --panic
    echo "rolled back. Investigate before retrying. (was: $1)"
    exit 1
}

if [[ "${1:-}" != "--confirm" ]]; then
    echo "refusing: pass --confirm (and only after preflight + manual dry-run are green)."
    exit 2
fi

echo "== running precondition gate (mandatory; no skip) =="
if ! "$LAB/phase_a/preflight.sh"; then
    echo "PREFLIGHT RED — aborting switch (no engine change made)."
    exit 1
fi

# Capture the current live engine FIRST — this is what rollback restores.
mkdir -p "$LAB/phase_a_data"
cur="$(_ibus_current)"
[[ -z "$cur" ]] && { echo "could not read current engine; aborting (rollback target unknown)."; exit 1; }
echo "$cur" > "$SAVED"
echo "captured current engine for rollback: '$cur' -> $SAVED"
echo ""
echo ">>> INSTANT ROLLBACK (keep ready):  $LAB/phase_a/rollback.sh"
echo ">>> PANIC ROLLBACK  (bare keyboard):  $LAB/phase_a/rollback.sh --panic"
echo ""

switch_ts="$(date +%s)"
echo "== switching to lab engine '$LAB_ENGINE' =="
_ibus_engine "$LAB_ENGINE"

# (c) active engine must equal the lab engine, else FAIL CLOSED.
now="$(_ibus_current)"
[[ "$now" == "$LAB_ENGINE" ]] || fail_closed "active engine is '$now', expected '$LAB_ENGINE'"

# Ask the operator to activate the engine so it heartbeats the identity receipt.
echo ">>> Focus a text field and type a few words so the lab engine activates."
if [[ "${PHASEA_NONINTERACTIVE:-0}" != "1" ]]; then
    read -r -p ">>> Press ENTER after you have typed and confirmed predictions appear... " _
fi

# MAJOR: mechanically prove the SELECTED engine is THIS lab process — a fresh
# identity heartbeat, phase_a=1, lab commit match, live PID — NOT the installed
# component (which has no harness and never writes this receipt).
lab_head="$(git -C "$LAB" rev-parse HEAD 2>/dev/null || echo unknown)"
verdict="$(PHASEA_IDENT="$IDENT" PHASEA_SWITCH_TS="$switch_ts" PHASEA_LAB_HEAD="$lab_head" "$PY" - <<'PYEOF'
import json, os, time, sys
ident = os.environ["PHASEA_IDENT"]
switch_ts = float(os.environ["PHASEA_SWITCH_TS"])
lab_head = os.environ["PHASEA_LAB_HEAD"]
try:
    d = json.load(open(ident))
except Exception as e:
    print(f"NO_IDENTITY:{e}"); sys.exit(0)
probs = []
if d.get("phase_a") != 1: probs.append("phase_a!=1")
hb = float(d.get("heartbeat_ts", 0))
if hb < switch_ts - 2: probs.append(f"stale heartbeat ({time.time()-hb:.0f}s old, before switch)")
lc = str(d.get("lab_commit"))
# When HEAD is known, the receipt MUST carry the real commit — 'unknown'/'None'
# are NOT acceptable (that would defeat the fail-closed identity proof).
if lab_head != "unknown" and lc not in (lab_head, lab_head[:12]):
    probs.append(f"lab_commit {lc[:12]} != HEAD {lab_head[:12]}")
pid = d.get("pid")
alive = False
try:
    os.kill(int(pid), 0); alive = True
except Exception:
    pass
if not alive: probs.append(f"pid {pid} not alive")
print("OK" if not probs else "BAD:" + "; ".join(probs))
PYEOF
)"
[[ "$verdict" == OK ]] || fail_closed "engine-identity receipt: $verdict"

echo "== SWITCH VERIFIED =="
echo "  active engine   : $now"
echo "  lab process PID : $($PY -c "import json;print(json.load(open('$IDENT'))['pid'])" 2>/dev/null)"
echo "  lab commit      : $lab_head"
echo "  identity receipt: fresh, phase_a=1, pid alive"
echo "If typing ever feels wrong: $LAB/phase_a/rollback.sh"
echo "Phase-A зелено ≠ доказателство за team tier."
