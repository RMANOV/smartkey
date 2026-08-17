# SmartKey keystroke diagnostics (`smartkey-debug`)

Per-keystroke JSONL trace of the live decision path:
`key_in → core verdict (consume/forward, dual-buffer/lock state, predictions)
→ dispatched actions → accept (committed vs typed + script class) → outcome`.

**Off by default.** Without `SMARTKEY_DEBUG` (env) or an `enable` marker the
trace is a single boolean check per keystroke — zero I/O, production
byte-for-byte identical.

## Levels

| Level | How | What is logged |
|---|---|---|
| off | (default) | nothing |
| structural | `SMARTKEY_DEBUG=1` or `./smartkey-debug enable` | verdicts, state, lengths, **script classes (cyr/lat/…)** — no typed text: printable keys appear only as `key_class=char` + `key_lang`; raw keyname/keyval/keycode are kept for special keys (Tab/BackSpace/…) only |
| full | `SMARTKEY_DEBUG=full` or `./smartkey-debug enable full` | + verbatim typed/ghost/committed/predictions (plaintext banner on start) |

Script classes are always logged: `committed_lang=lat` while `typed_lang=cyr`
is exactly the Space-inject signature, visible even in structural mode.

## Privacy guardrails

- Trace JSONL lives under `~/.local/state/smartkey/debug/` (override:
  `SMARTKEY_DEBUG_DIR`; an override inside the repo is **refused**). Full mode
  also enables `smartkey.log`, `predictions.log`, and `replay.jsonl` under
  `$XDG_DATA_HOME/smartkey/` (normally `~/.local/share/smartkey/`). Both
  directories are 0700 and files 0600. Never network, never engine stdout.
- Trace JSONL is bounded: flush off the hot path, 48h age purge + 50MB size
  cap at start. The three legacy full-mode sinks are not rotated; keep full
  mode brief and use `wipe` when the capture is complete.
- `./smartkey-debug wipe` deletes every trace and all three legacy content
  sinks in one command. Recycle SmartKey afterward if it was still running.

## Repro recipe (accept/backspace class)

1. `./smartkey-debug enable full`
2. Switch to a fallback keyboard and recycle only SmartKey (do not globally
   restart IBus). With an explicit environment instead:
   `SMARTKEY_DEBUG=full <engine start>`.
3. Type the repro phrase in the failing app (e.g. Bulgarian typing where a
   prediction got injected), then switch focus once (flushes the buffer).
4. `./smartkey-debug dump` → prints the path of a merged 0600 JSONL.
5. Read the trace: find the `seq` of the bad keystroke; check `core.verdict`
   (was the key consumed or forwarded?), the `action` list (what was
   dispatched through `_execute_actions`), and `accept`
   (`committed_text` vs `typed_prefix`, `committed_lang` vs `typed_lang`).
6. `./smartkey-debug wipe` when done; `./smartkey-debug disable` to turn off.

`./smartkey-debug status` shows level, dir and file sizes; `tail [N]` prints
the last N events of the newest trace.
