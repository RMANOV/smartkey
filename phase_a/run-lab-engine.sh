#!/usr/bin/env bash
# Launch the Phase-A LAB ibus engine (instrumented smartkey_engine.py +
# phase_a harness) with logging ENABLED. Rust is unchanged vs installed, so no
# rebuild is required — the venv's existing smartkey_py is reused.
#
# This runs the lab engine from the isolated worktree. It does NOT touch the
# installed engine's files; it registers the SmartKey component from THIS
# process so you can select it in the ibus menu. Stop it (Ctrl-C / kill) and
# reselect your normal engine to switch back.
set -euo pipefail

LAB=/home/rmanov/smartkey-phase-a-lab
VENV_PY=/home/rmanov/smartkey/.venv/bin/python3

cd "$LAB"
export SMARTKEY_PHASE_A=1
export SMARTKEY_PHASEA_COMMIT="$(git -C "$LAB" rev-parse HEAD 2>/dev/null || echo unknown)"
export PYTHONPATH="$LAB:${PYTHONPATH:-}"
# Optional: uncomment to force a specific corpus dir (defaults to $LAB/corpus).
# export SMARTKEY_CORPUS_DIR="$HOME/.config/smartkey"

echo "Phase-A LAB engine: SMARTKEY_PHASE_A=1  commit=$SMARTKEY_PHASEA_COMMIT"
echo "event DB -> $LAB/phase_a_data/events.db"
exec "$VENV_PY" -m ibus.main "$@"
