# CONDUCTOR ACTIVATION — Phase-A smartkey harness (14-day real-typing window)

Ticket 25d391a0ba2f · lab worktree `/home/rmanov/smartkey-phase-a-lab` (branch
`phase-a-lab`, push DISABLED).

**Who switches:** the operator delegates the ibus switch/restart to the
**CONDUCTOR** ("ще рестартираш ти когато новата версия е готова"), not to
himself. **smartkey is the operator's LIVE input method right now** — a broken
switch would lock him out of typing entirely. So the switch is guarded: it
happens only after a hard precondition gate is green **and** a rollback to the
current live engine has been proven to work first.

Rust is unchanged vs the installed engine, so **no maturin rebuild is required**
— the existing `smartkey_py` in the venv is reused. The harness is observational
and OFF unless `SMARTKEY_PHASE_A=1`.

Scripts (all in `phase_a/`): `preflight.sh` (gate), `rollback.sh` (instant
rollback + `--dry-run`), `switch-to-lab.sh --confirm` (gated switch),
`run-lab-engine.sh` (launch the lab engine).

---

## The switch, in one line
```bash
/home/rmanov/smartkey-phase-a-lab/phase_a/switch-to-lab.sh --confirm
```
It refuses unless the gate below is green, captures the current engine for
rollback first, then switches ibus to the lab `smartkey` engine.

## The instant rollback, in one line (keep it ready the whole window)
```bash
/home/rmanov/smartkey-phase-a-lab/phase_a/rollback.sh            # back to the captured live engine
/home/rmanov/smartkey-phase-a-lab/phase_a/rollback.sh --panic   # back to a bare keyboard layout
```
`rollback.sh` restores the engine captured at switch time. `--panic` falls back
to a plain XKB layout (`xkb:us::eng` by default) that is always present — so it
works **even if every smartkey variant is broken**. Override the panic target
with `PHASEA_PANIC_ENGINE=<name>` if the operator prefers e.g.
`xkb:bg:phonetic:bul`.

---

## Hard precondition gate — ALL must be green BEFORE switching

Run the gate (non-switching; safe to run anytime):
```bash
cd /home/rmanov/smartkey-phase-a-lab && phase_a/preflight.sh
```
It checks and must report `[ OK ]` for every line:

1. **self-test 8/8** — logging / split / bucketing / isotonic / gate / sweep /
   watchdog, **including the forced-FAIL proof** and the no-LLM schema-invariant
   rejection.
2. **≤20ms logging budget met** — `bench --check` (measured p99 ≈ 55–60 µs,
   0.00% over 20 ms with the real corpus).
3. **isolation receipt present** (`isolation-receipt.txt`).
4. **push DISABLED** in both the lab and the main repo configs.
5. **ibus reachable + panic rollback engine exists** in `ibus list-engine`
   (this only passes on the operator's live session — run the gate there).

Then two **manual** preconditions the conductor confirms live:

6. **rollback dry-run confirmed — do this while smartkey is still HEALTHY:**
   ```bash
   phase_a/rollback.sh --dry-run     # current -> panic layout -> back to current
   ```
   Type a few characters at the panic layout to confirm real typing works, then
   confirm you are restored. This proves the safety net **before** you ever touch
   the lab engine.
7. **lab engine builds/launches cleanly:**
   ```bash
   phase_a/run-lab-engine.sh          # in its own terminal; leave it running
   ```
   It must register the SmartKey component without error (Rust unchanged → no
   build step; a launch error here is a hard stop).

**Only if 1–7 are ALL green do you run `switch-to-lab.sh --confirm`.** If any
line is red: DO NOT SWITCH.

---

## After the switch

- Confirm `ibus engine` reports `smartkey` and type a sentence. If anything is
  wrong, run `rollback.sh` immediately.
- Events log to `phase_a_data/events.db` (`synthetic=0`). **Privacy:** no
  plaintext typing is stored — only `context_hash` (keyed HMAC), a candidate
  count, `p_top3`, latency, and the `outcome` bit. Candidate words and the
  resolved token never leave memory. The HMAC key is in a `0600` sidecar
  (`phase_a_data/context_salt`), not in the DB.
- Daily automation (document only — do NOT auto-install; `crontab -e`):
  ```cron
  55 23 * * * cd /home/rmanov/smartkey-phase-a-lab && /home/rmanov/smartkey/.venv/bin/python3 -m phase_a.sweep sweep    >> /home/rmanov/smartkey-phase-a-lab/phase_a_data/cron.log 2>&1
  0  9  * * * cd /home/rmanov/smartkey-phase-a-lab && /home/rmanov/smartkey/.venv/bin/python3 -m phase_a.sweep watchdog >> /home/rmanov/smartkey-phase-a-lab/phase_a_data/cron.log 2>&1
  ```
  The watchdog writes a visible `phase_a_data/WATCHDOG-ALARM.txt` and exits
  non-zero if no sweep ran in 48h; a successful sweep clears it.

## End of window — read the verdict
```bash
cd /home/rmanov/smartkey-phase-a-lab
/home/rmanov/smartkey/.venv/bin/python3 -m phase_a.analyze          # report + verdict
/home/rmanov/smartkey/.venv/bin/python3 -m phase_a.analyze --json   # machine-readable
```
Exit code: `0` PASS, `2` FAIL, `3` INCONCLUSIVE. The gate text is hash-committed
and is not reinterpreted after run 1. One INCONCLUSIVE buys exactly one
adjudication round (diagnosis + pre-declared fix + a new 14-day window); a second
INCONCLUSIVE is a FAIL.

## Deactivate + switch back
1. `phase_a/rollback.sh` (back to the captured live engine), or select it in the
   ibus menu.
2. Stop the lab engine process: `Ctrl-C` / `pkill -f 'smartkey-phase-a-lab.*ibus.main'`.
3. Remove the two cron lines when the window closes.

The installed engine and the `~/smartkey` main tree were never modified.

## Restore push (after the lab is retired)
```bash
git -C /home/rmanov/smartkey            remote set-url --push origin https://github.com/RMANOV/smartkey.git
git -C /home/rmanov/smartkey-phase-a-lab remote set-url --push origin https://github.com/RMANOV/smartkey.git
```
(Original push URL is also in `isolation-receipt.txt`.)

---

## OPERATOR MANUAL ROLLBACK (if typing ever feels wrong)

You do not need to wait for the conductor. Any of these restores typing instantly:

- **From the ibus tray/menu:** pick any non-SmartKey input source (your normal
  keyboard layout). Immediate.
- **From a terminal:**
  ```bash
  /home/rmanov/smartkey-phase-a-lab/phase_a/rollback.sh --panic   # bare keyboard, always works
  ```
- **If a terminal is hard to reach:** `Super-Space` (or your configured ibus
  hotkey) cycles input sources away from SmartKey.

The panic path uses a plain keyboard layout, so it does not depend on any
smartkey component being healthy.

---
Phase-A зелено ≠ доказателство за team tier.
