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

- Output only under `~/.local/state/smartkey/debug/` (override:
  `SMARTKEY_DEBUG_DIR`; an override inside the repo is **refused**).
  Dir 0700, files 0600. Never network, never the engine's stdout.
- Bounded: flush off the hot path, 48h age purge + 50MB size cap at start.
- `./smartkey-debug wipe` deletes every trace in one command.

## Repro recipe (accept/backspace class)

1. `./smartkey-debug enable full`
2. Restart the engine (engine recycle / ibus restart) — with env instead:
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
