"""Local paths and hard isolation guards for calibration artifacts.

Two invariants are enforced fail-closed:

  1. Data outputs go ONLY under ``SMARTKEY_PHASEA_DATA`` (a temp/scratch dir).
     ``data_dir()`` refuses any path inside the live tree (~/smartkey) or the
     operator config (~/.config/smartkey) so a run can never write live state.

  2. Corpus inputs are only local, gitignored copies in ``lab_corpus/``.
     ``lab_corpus_files()`` verifies each file's sha256 against local pins
     and raises if any byte differs — the harness will not run on tampered or
     substituted corpus data.
"""

from __future__ import annotations

import hashlib
import os
from pathlib import Path

LAB_ROOT = Path(__file__).resolve().parent.parent

# --- Pinned local corpus (verified at start and end of every run) ------------
LAB_CORPUS_DIR = LAB_ROOT / "lab_corpus"
PUBLIC_CORPUS_DIR = LAB_ROOT / "corpus"
PINNED_CORPUS_SHA256 = {
    "corpus_bg.bin": "08b1440a6d542d68ff1a1e80d9478292e240531272e8632522bf40a6fc3fcb1c",
    "corpus_bg.json": "087136b9e3b5cb6e7750b31e8837e32b2ea7e92b88ffbed227329f2b82700fb3",
    "corpus_en.bin": "609c90439257f68794a28224da53999ce63d82589b25800b24b494dcb7afccde",
    "corpus_en.json": "5a59e842c5d58e9797790c4ad2921233c45679dccab0cbfbfa9256e4365f7445",
    "corpus_tech.bin": "77b57593eab7f3c884fd3c3f332fbd7add9bb8e3e131fdcb8e44631b940925b6",
    "corpus_tech.json": "5af923c90a9e8dfb5011bcaa4c1a0b11c7e58e7770d6a496474d328b24fb5845",
}

# Paths the harness must never write into (live state / operator identity).
_FORBIDDEN_DATA_PREFIXES = (
    Path.home() / "smartkey",
    Path.home() / ".config" / "smartkey",
)


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def _is_relative_to(child: Path, parent: Path) -> bool:
    try:
        child.resolve().relative_to(parent.resolve())
        return True
    except ValueError:
        return False


def data_dir() -> Path:
    """Root of all Phase-A artifacts. SMARTKEY_PHASEA_DATA if set, else a
    worktree-local ``phase_a_data/`` (git-excluded). Fail-closed if it resolves
    inside the live tree or operator config."""
    d = Path(os.environ.get("SMARTKEY_PHASEA_DATA", str(LAB_ROOT / "phase_a_data")))
    for forbidden in _FORBIDDEN_DATA_PREFIXES:
        if _is_relative_to(d, forbidden):
            raise RuntimeError(
                f"SMARTKEY_PHASEA_DATA={d} resolves inside the live/operator tree "
                f"{forbidden} — refusing (data outputs must stay in a scratch dir)."
            )
    d.mkdir(parents=True, exist_ok=True)
    return d


def default_db_path() -> Path:
    """Lab event DB. Never the live events.db (data_dir() guards the root)."""
    env = os.environ.get("SMARTKEY_PHASEA_DB")
    return Path(env) if env else data_dir() / "events.db"


def synthetic_db_path() -> Path:
    return data_dir() / "selftest_synthetic.db"


def alarm_file() -> Path:
    return data_dir() / "WATCHDOG-ALARM.txt"


def receipts_dir() -> Path:
    d = data_dir() / "receipts"
    d.mkdir(parents=True, exist_ok=True)
    return d


def context_salt() -> bytes:
    """Per-data-dir random salt for keyed context hashing, in a 0600 sidecar
    OUTSIDE the event DB — so a leaked DB never reveals a context word."""
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


def _corpus_dir(kind: str) -> Path:
    """Prefer the isolated local copy; JSON can fall back to tracked fixtures."""
    suffix = ".json" if kind == "json" else ".bin"
    expected = [name for name in PINNED_CORPUS_SHA256 if name.endswith(suffix)]
    if all((LAB_CORPUS_DIR / name).is_file() for name in expected):
        return LAB_CORPUS_DIR
    if kind == "json" and all((PUBLIC_CORPUS_DIR / name).is_file() for name in expected):
        return PUBLIC_CORPUS_DIR
    return LAB_CORPUS_DIR


def verify_lab_corpus(kind: str = "json") -> dict[str, str]:
    """Verify pinned files of one format and fail closed on any mismatch."""
    suffix = ".json" if kind == "json" else ".bin"
    corpus_dir = _corpus_dir(kind)
    seen: dict[str, str] = {}
    for name, pinned in PINNED_CORPUS_SHA256.items():
        if not name.endswith(suffix):
            continue
        p = corpus_dir / name
        if not p.exists():
            raise RuntimeError(f"pinned corpus file missing: {p}")
        got = sha256_file(p)
        if got != pinned:
            raise RuntimeError(
                f"corpus pin mismatch for {name}: expected {pinned}, got {got}"
            )
        seen[name] = got
    return seen


def lab_corpus_files(kind: str = "json") -> list[Path]:
    """Return the pinned corpus files of the given kind ('json' for FreqModel,
    'bin' for the Rust core), AFTER verifying all pins. FreqModel reads .json."""
    verify_lab_corpus(kind)
    suffix = ".json" if kind == "json" else ".bin"
    return sorted(_corpus_dir(kind).glob(f"corpus_*{suffix}"))
