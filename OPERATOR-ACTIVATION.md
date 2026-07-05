# OPERATOR ACTIVATION — Phase-A smartkey harness (14-day real-typing window)

Ticket 25d391a0ba2f · lab worktree `/home/rmanov/smartkey-phase-a-lab` (branch
`phase-a-lab`, push DISABLED). Rust is **unchanged** vs the installed engine, so
**no maturin rebuild is required** — the existing `smartkey_py` in the venv is
reused. Activation is manual and reversible; the harness is observational and
OFF unless `SMARTKEY_PHASE_A=1`.

Everything below is the operator's call. The executor prepared and verified the
machinery (see the self-test evidence in the final report); it did not activate
real typing and never pushed.

---

## 0. One-time smoke check (no typing needed)

```bash
cd /home/rmanov/smartkey-phase-a-lab
/home/rmanov/smartkey/.venv/bin/python3 -m phase_a.selftest        # expect: ALL CHECKS PASS
/home/rmanov/smartkey/.venv/bin/python3 -m phase_a.bench           # logging overhead vs 20ms
```

## 1. Activate the lab engine (start the 14-day window)

Ensure ibus is running, then launch the instrumented lab engine:

```bash
ibus-daemon -drxR                                   # if not already running
/home/rmanov/smartkey-phase-a-lab/phase_a/run-lab-engine.sh
```

This registers the **SmartKey (SK)** component from the lab process with
`SMARTKEY_PHASE_A=1`. Select **SmartKey** in the ibus menu (tray / Super-Space)
and type normally. Events log to
`/home/rmanov/smartkey-phase-a-lab/phase_a_data/events.db` (`synthetic=0`).

> If the installed `smartkey` engine is registered system-wide and collides,
> temporarily switch your input source away from it first; the lab process
> provides its own SmartKey component for the window. Do **not** edit the
> installed engine's files.

Sanity after a few minutes of typing:

```bash
cd /home/rmanov/smartkey-phase-a-lab
/home/rmanov/smartkey/.venv/bin/python3 -m phase_a.sweep sweep      # should show events > 0
```

## 2. Daily automation (document only — DO NOT let anyone auto-install)

Add these two lines with `crontab -e` (paths are absolute; real DB is the
default, no `--synthetic`):

```cron
# Phase-A daily sweep (receipt) at 23:55, and 48h watchdog check at 09:00
55 23 * * * cd /home/rmanov/smartkey-phase-a-lab && /home/rmanov/smartkey/.venv/bin/python3 -m phase_a.sweep sweep    >> /home/rmanov/smartkey-phase-a-lab/phase_a_data/cron.log 2>&1
0  9  * * * cd /home/rmanov/smartkey-phase-a-lab && /home/rmanov/smartkey/.venv/bin/python3 -m phase_a.sweep watchdog >> /home/rmanov/smartkey-phase-a-lab/phase_a_data/cron.log 2>&1
```

- The **watchdog** exits non-zero and writes a visible
  `phase_a_data/WATCHDOG-ALARM.txt` if no sweep ran in the last 48h. A
  successful sweep clears the alarm automatically.
- Daily receipts are also written to `phase_a_data/receipts/sweep-YYYY-MM-DD.txt`.

## 3. Read the verdict (end of the 14-day window)

```bash
cd /home/rmanov/smartkey-phase-a-lab
/home/rmanov/smartkey/.venv/bin/python3 -m phase_a.analyze          # text report + verdict
/home/rmanov/smartkey/.venv/bin/python3 -m phase_a.analyze --json   # machine-readable
```

Exit code encodes the verdict: `0` PASS, `2` FAIL, `3` INCONCLUSIVE. The gate
text is hash-committed and is not reinterpreted after run 1. One `INCONCLUSIVE`
buys exactly one adjudication round (diagnosis + pre-declared fix + a new 14-day
window); a second `INCONCLUSIVE` is a FAIL.

## 4. Deactivate + switch back

1. Select a different ibus input source (your normal engine / keyboard).
2. Stop the lab engine process: `Ctrl-C` in its terminal, or
   `pkill -f 'smartkey-phase-a-lab.*ibus.main'`.
3. Remove the two cron lines (`crontab -e`) when the window is closed.

The installed engine and `~/smartkey` main tree were never modified, so there is
nothing to restore there.

## 5. Restore push (after the lab is retired)

Push was disabled in **both** configs for the window. Re-enable when done:

```bash
git -C /home/rmanov/smartkey            remote set-url --push origin https://github.com/RMANOV/smartkey.git
git -C /home/rmanov/smartkey-phase-a-lab remote set-url --push origin https://github.com/RMANOV/smartkey.git
```

(Original push URL is also recorded in `isolation-receipt.txt`.)

## 6. Privacy note

`events.db` stores, per prediction: a SHA-1 **context hash** (not raw text),
the top-3 candidate words, a raw frequency, latency, and the resolved token.
The resolved token and candidate words are plaintext. Treat `phase_a_data/` as
sensitive; it is git-ignored and stays inside the lab worktree.

---
Phase-A зелено ≠ доказателство за team tier.
