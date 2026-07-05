#!/usr/bin/env bash
# HARD-GATE preconditions that must ALL be green before the conductor switches
# the operator's live ibus to the lab engine. Non-switching: this script only
# reads and tests. Exit 0 = clear to proceed; non-zero = DO NOT SWITCH.
set -uo pipefail

LAB=/home/rmanov/smartkey-phase-a-lab
PY=/home/rmanov/smartkey/.venv/bin/python3
PREFLIGHT_DATA="$LAB/phase_a_data/preflight"
cd "$LAB"
mkdir -p "$PREFLIGHT_DATA"

ok=1
pass() { echo "  [ OK ] $1"; }
fail() { echo "  [FAIL] $1"; ok=0; }

echo "==================================================================="
echo " PHASE-A PRE-SWITCH PRECONDITION GATE"
echo "==================================================================="

# 1. self-test (incl. forced-FAIL proof + schema rejection)
if SMARTKEY_PHASEA_DATA="$PREFLIGHT_DATA" "$PY" -m phase_a.selftest >/tmp/phasea_selftest.log 2>&1 && \
   grep -q "ALL CHECKS PASS" /tmp/phasea_selftest.log; then
    pass "self-test ALL CHECKS PASS (incl. forced-FAIL, schema invariant, B2/B4 outcome)"
else
    fail "self-test did NOT pass — see /tmp/phasea_selftest.log"
fi

# 2. logging overhead within the 20ms budget
if SMARTKEY_PHASEA_DATA="$PREFLIGHT_DATA" "$PY" -m phase_a.bench --check >/tmp/phasea_bench.log 2>&1; then
    pass "logging overhead within <=20ms budget ($(grep 'latency p99' /tmp/phasea_bench.log | tr -s ' '))"
else
    fail "logging overhead check failed — see /tmp/phasea_bench.log"
fi

# 3. isolation receipt present + push disabled in BOTH configs
if [[ -f "$LAB/isolation-receipt.txt" ]]; then
    pass "isolation-receipt.txt present"
else
    fail "isolation-receipt.txt missing"
fi
lab_push="$(git -C "$LAB" remote get-url --push origin 2>/dev/null || echo '?')"
main_push="$(git -C /home/rmanov/smartkey remote get-url --push origin 2>/dev/null || echo '?')"
if [[ "$lab_push" == "DISABLED" && "$main_push" == "DISABLED" ]]; then
    pass "push DISABLED in both lab and main configs"
else
    fail "push not disabled (lab=$lab_push main=$main_push)"
fi

# 4. ibus reachable + rollback target resolvable (so rollback will work)
if command -v ibus >/dev/null 2>&1 && ibus engine >/dev/null 2>&1; then
    cur="$(ibus engine 2>/dev/null)"
    pass "ibus reachable; current live engine = '$cur'"
    if ibus list-engine 2>/dev/null | grep -qE "${PHASEA_PANIC_ENGINE:-xkb:us::eng}"; then
        pass "panic rollback engine '${PHASEA_PANIC_ENGINE:-xkb:us::eng}' exists"
    else
        fail "panic rollback engine '${PHASEA_PANIC_ENGINE:-xkb:us::eng}' NOT in ibus list-engine"
    fi
else
    fail "ibus not reachable here — run this on the operator's live session"
fi

echo "-------------------------------------------------------------------"
echo " REMAINING MANUAL PRECONDITIONS (conductor confirms live):"
echo "   [ ] rollback dry-run confirmed:  phase_a/rollback.sh --dry-run"
echo "   [ ] lab engine builds/launches cleanly: phase_a/run-lab-engine.sh"
echo "-------------------------------------------------------------------"
if [[ "$ok" == "1" ]]; then
    echo " GATE: automated preconditions GREEN. Do the 2 manual steps, then switch."
else
    echo " GATE: RED — DO NOT SWITCH."
fi
echo "Phase-A зелено ≠ доказателство за team tier."
[[ "$ok" == "1" ]] && exit 0 || exit 1
