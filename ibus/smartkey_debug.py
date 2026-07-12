"""SmartKey keystroke diagnostic trace + `smartkey-debug` CLI.

Turns "try and guess" into "type the repro, read the trace": one grouped
``seq`` of JSONL events per keystroke records the live decision path
(key_in -> core verdict -> dispatched actions -> accept -> outcome).

Levels (env ``SMARTKEY_DEBUG`` wins; else a LEVEL marker file written by
``smartkey-debug enable`` and read once at engine start):
  unset/0/off  OFF        every emit() is a single bool check; zero I/O.
  1            structural everything EXCEPT verbatim content (lang classes,
                          lengths, verdicts and state are always kept).
  full         content    + verbatim typed/ghost/committed/predictions.
                          Prints a one-time plaintext-keystrokes banner.

HARD privacy guardrails (these are the operator's keystrokes):
  * Output only under $SMARTKEY_DEBUG_DIR (default
    ~/.local/state/smartkey/debug/), dir 0700, files 0600.  A dir override
    that resolves INSIDE the git repo is refused and falls back to the
    default — a keystroke log must never be committable.
  * Never network, never stdout of the engine; ring-buffered appends with
    flush OFF the hot path; bounded total size + age auto-purge;
    ``smartkey-debug wipe`` one-command purge.
"""

from __future__ import annotations

import json
import os
import sys
import time
from collections import deque
from pathlib import Path

LEVEL_OFF = 0
LEVEL_STRUCTURAL = 1
LEVEL_FULL = 2

# Verbatim-content fields — dropped unless level=full.  Everything else
# (lang classes, lengths, verdicts, state booleans) is always logged.
CONTENT_FIELDS = frozenset(
    {
        "surrounding_text",
        "payload_text",
        "predictions",
        "ghost",
        "committed_text",
        "typed_prefix",
        "committed_to_app",
    }
)

RING_SIZE = 5000
FLUSH_EVERY = 50
PURGE_AFTER_HOURS = 48
MAX_TOTAL_BYTES = 50 * 1024 * 1024  # cap the debug dir, oldest files first

_BANNER = (
    "SMARTKEY_DEBUG=full logs your keystrokes in PLAINTEXT to {path} — "
    "local only; wipe with `smartkey-debug wipe`"
)


def repo_root() -> Path | None:
    """The git repo containing this module, if any (module lives in repo/ibus)."""
    here = Path(__file__).resolve().parent
    for candidate in (here, *here.parents):
        if (candidate / ".git").exists():
            return candidate
    return None


def default_dir() -> Path:
    state = os.environ.get("XDG_STATE_HOME") or os.path.expanduser("~/.local/state")
    return Path(state) / "smartkey" / "debug"


def resolve_dir() -> Path:
    """Debug output dir; an override inside the repo tree is REFUSED."""
    override = os.environ.get("SMARTKEY_DEBUG_DIR")
    path = Path(override).expanduser() if override else default_dir()
    repo = repo_root()
    if repo is not None:
        try:
            inside = path.resolve().is_relative_to(repo)
        except (OSError, ValueError):
            inside = False
        if inside:
            path = default_dir()
    return path


def resolve_level() -> int:
    """Env SMARTKEY_DEBUG wins; else the LEVEL marker file; else OFF."""
    raw = os.environ.get("SMARTKEY_DEBUG")
    if raw is None:
        try:
            marker = resolve_dir() / "LEVEL"
            raw = marker.read_text(encoding="utf-8").strip() if marker.exists() else None
        except OSError:
            raw = None
    if raw is None or raw in ("", "0", "off"):
        return LEVEL_OFF
    if raw == "full":
        return LEVEL_FULL
    return LEVEL_STRUCTURAL


def lang_class(text: object) -> str:
    """Cheap script class: cyr/lat/digit/punct/mixed/empty.

    Always logged (even structural) — a committed_lang=lat while
    typed_lang=cyr is exactly the Space-inject signature.
    """
    if not isinstance(text, str) or not text:
        return "empty"
    classes = set()
    for ch in text:
        if "а" <= ch <= "я" or "А" <= ch <= "Я" or ch in "ёЁ":
            classes.add("cyr")
        elif ("a" <= ch <= "z") or ("A" <= ch <= "Z"):
            classes.add("lat")
        elif ch.isdigit():
            classes.add("digit")
        elif not ch.isspace():
            classes.add("punct")
    if not classes:
        return "empty"
    if len(classes) == 1:
        return next(iter(classes))
    return "mixed"


class KeystrokeTrace:
    """Per-keystroke JSONL trace.  OFF => a single bool guard, zero I/O."""

    def __init__(
        self,
        level: int | None = None,
        directory: Path | None = None,
        ring_size: int = RING_SIZE,
        schedule_flush=None,
    ) -> None:
        """``schedule_flush(callback)`` defers ``callback`` OFF the hot path
        (the engine passes ``GLib.idle_add``).  Without a scheduler the ring
        is only written by explicit ``flush()`` calls at idle points
        (focus-out/reset/disable); ``emit()`` itself NEVER touches the file
        system — that is the flush-outside-hot-path invariant."""
        self.level = resolve_level() if level is None else level
        self.enabled = self.level != LEVEL_OFF
        self._seq = 0
        if not self.enabled:
            return

        self.dir = Path(directory) if directory is not None else resolve_dir()
        self._ring: deque = deque(maxlen=ring_size)
        self._since_flush = 0
        self._schedule_flush = schedule_flush
        self._flush_pending = False
        self._file: Path | None = None
        try:
            self.dir.mkdir(parents=True, exist_ok=True)
            os.chmod(self.dir, 0o700)
            self._purge_old()
            stamp = time.strftime("%Y%m%d-%H%M%S")
            self._file = self.dir / f"trace-{stamp}-{os.getpid()}.jsonl"
            self._file.touch()
            os.chmod(self._file, 0o600)
        except OSError:
            # The trace must never take the engine down.
            self.enabled = False
            return
        if self.level == LEVEL_FULL:
            print(_BANNER.format(path=self.dir), file=sys.stderr)

    # -- hot path -----------------------------------------------------------
    def begin_keystroke(self) -> int:
        if not self.enabled:
            return 0
        self._seq += 1
        return self._seq

    def emit(self, event: str, **fields: object) -> None:
        if not self.enabled:
            return
        if self.level < LEVEL_FULL:
            fields = {k: v for k, v in fields.items() if k not in CONTENT_FIELDS}
        record = {"ts": round(time.time(), 4), "seq": self._seq, "event": event}
        record.update(fields)
        self._ring.append(record)
        self._since_flush += 1
        # NO file I/O here — emit only appends to the in-memory ring.  Once
        # the buffer grows, REQUEST a deferred flush (e.g. GLib.idle_add),
        # which runs after the key event has been fully handled.
        if (
            self._since_flush >= FLUSH_EVERY
            and not self._flush_pending
            and self._schedule_flush is not None
        ):
            self._flush_pending = True
            try:
                self._schedule_flush(self._deferred_flush)
            except Exception:  # noqa: BLE001 — tracing must never break input
                self._flush_pending = False

    # -- off the hot path ---------------------------------------------------
    def _deferred_flush(self) -> bool:
        """Scheduler callback (GLib.idle_add compatible: returns False)."""
        self._flush_pending = False
        self.flush()
        return False

    def flush(self) -> None:
        """Append buffered events to the session file (idle points only)."""
        if not self.enabled or not self._ring or self._file is None:
            return
        lines = []
        while self._ring:
            lines.append(json.dumps(self._ring.popleft(), ensure_ascii=False))
        self._since_flush = 0
        try:
            with self._file.open("a", encoding="utf-8") as fh:
                fh.write("\n".join(lines) + "\n")
        except OSError:
            self.enabled = False

    def _purge_old(self) -> None:
        """Age purge + total-size cap (oldest first).  Best-effort."""
        try:
            files = sorted(
                self.dir.glob("*.jsonl"), key=lambda p: p.stat().st_mtime
            )
            cutoff = time.time() - PURGE_AFTER_HOURS * 3600
            kept = []
            for path in files:
                if path.stat().st_mtime < cutoff:
                    path.unlink(missing_ok=True)
                else:
                    kept.append(path)
            total = sum(p.stat().st_size for p in kept)
            for path in kept:
                if total <= MAX_TOTAL_BYTES:
                    break
                total -= path.stat().st_size
                path.unlink(missing_ok=True)
        except OSError:
            pass


# ===========================================================================
# CLI: smartkey-debug enable [full] | disable | status | dump | tail | wipe
# ===========================================================================
def _cli_files(directory: Path) -> list[Path]:
    return sorted(directory.glob("*.jsonl"), key=lambda p: p.stat().st_mtime)


def main(argv: list[str]) -> int:
    directory = resolve_dir()
    cmd = argv[0] if argv else "status"

    if cmd == "enable":
        level = "full" if len(argv) > 1 and argv[1] == "full" else "1"
        directory.mkdir(parents=True, exist_ok=True)
        os.chmod(directory, 0o700)
        marker = directory / "LEVEL"
        marker.write_text(level + "\n", encoding="utf-8")
        os.chmod(marker, 0o600)
        print(f"debug level set to {level!r} (marker {marker})")
        print("restart the engine (ibus restart / engine recycle) to apply")
        return 0

    if cmd == "disable":
        (directory / "LEVEL").unlink(missing_ok=True)
        print("debug disabled (marker removed); restart the engine to apply")
        return 0

    if cmd == "status":
        env = os.environ.get("SMARTKEY_DEBUG")
        marker = directory / "LEVEL"
        marker_val = (
            marker.read_text(encoding="utf-8").strip() if marker.exists() else None
        )
        files = _cli_files(directory) if directory.exists() else []
        total = sum(p.stat().st_size for p in files)
        print(f"dir:    {directory}")
        print(f"env:    SMARTKEY_DEBUG={env!r}")
        print(f"marker: {marker_val!r}")
        print(f"level:  {resolve_level()} (0=off 1=structural 2=full)")
        print(f"files:  {len(files)} ({total} bytes)")
        for path in files[-5:]:
            print(f"  {path.name}  {path.stat().st_size}B")
        return 0

    if cmd == "dump":
        files = _cli_files(directory) if directory.exists() else []
        if not files:
            print("no trace files")
            return 1
        stamp = time.strftime("%Y%m%d-%H%M%S")
        out = directory / f"dump-{stamp}.jsonl"
        with out.open("w", encoding="utf-8") as fh:
            for path in files:
                if path.name.startswith("dump-"):
                    continue
                fh.write(path.read_text(encoding="utf-8"))
        os.chmod(out, 0o600)
        print(out)
        return 0

    if cmd == "tail":
        count = int(argv[1]) if len(argv) > 1 else 20
        files = [
            p for p in (_cli_files(directory) if directory.exists() else [])
            if not p.name.startswith("dump-")
        ]
        if not files:
            print("no trace files")
            return 1
        lines = files[-1].read_text(encoding="utf-8").splitlines()
        for line in lines[-count:]:
            print(line)
        return 0

    if cmd == "wipe":
        removed = 0
        if directory.exists():
            for path in directory.glob("*.jsonl"):
                path.unlink(missing_ok=True)
                removed += 1
        print(f"wiped {removed} file(s) from {directory}")
        return 0

    print(__doc__)
    print("usage: smartkey-debug enable [full] | disable | status | dump | tail [N] | wipe")
    return 2


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
