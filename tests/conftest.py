"""Ensure the repository root is importable (so `import phase_a` works)
regardless of pytest's import mode / invocation directory."""

from __future__ import annotations

import sys
from pathlib import Path

_ROOT = Path(__file__).resolve().parent.parent
if str(_ROOT) not in sys.path:
    sys.path.insert(0, str(_ROOT))
