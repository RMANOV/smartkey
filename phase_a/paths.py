"""Lab-local paths for Phase-A artifacts. Everything stays inside the worktree
so real data never mixes with the operator's main tree or the installed engine.
"""

from __future__ import annotations

import os
from pathlib import Path

LAB_ROOT = Path(__file__).resolve().parent.parent


def data_dir() -> Path:
    d = Path(os.environ.get("SMARTKEY_PHASEA_DATA", str(LAB_ROOT / "phase_a_data")))
    d.mkdir(parents=True, exist_ok=True)
    return d


def default_db_path() -> Path:
    """Real-typing event DB (14-day window)."""
    env = os.environ.get("SMARTKEY_PHASEA_DB")
    return Path(env) if env else data_dir() / "events.db"


def synthetic_db_path() -> Path:
    """Self-test DB — clearly separate, synthetic=1, never mixed with real data."""
    return data_dir() / "selftest_synthetic.db"


def alarm_file() -> Path:
    """Visible watchdog alarm file (present == stale sweep / missed watchdog)."""
    return data_dir() / "WATCHDOG-ALARM.txt"


def receipts_dir() -> Path:
    d = data_dir() / "receipts"
    d.mkdir(parents=True, exist_ok=True)
    return d


def context_salt() -> bytes:
    """Per-data-dir random salt for keyed context hashing, stored in a 0600
    sidecar file OUTSIDE the event DB. This keeps context_hash from being a
    dictionary-recoverable hash of a single corpus word: leaking events.db
    alone no longer reveals context words (the key is not in the DB).
    """
    f = data_dir() / "context_salt"
    if f.exists():
        return f.read_bytes()
    salt = os.urandom(16)
    try:
        fd = os.open(str(f), os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "wb") as fh:
            fh.write(salt)
        return salt
    except FileExistsError:  # concurrent create — reuse the winner's salt
        return f.read_bytes()
