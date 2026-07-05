"""Raw frequency-table probability model for the Phase-A baseline.

Spec §12.4 blocker 3 pins ``p_top3`` to the *raw normalised frequency* drawn
from smartkey's own n-gram / frequency table — deliberately a **dumb baseline**
(value-gate-first discipline). It is emphatically NOT the engine's ensemble
``score`` nor its ``confidence``: those blend Markov / personal / tech signals
and pass through a neural reranker plus an internal isotonic calibrator, which
would (a) smuggle neural inference into the measured p and (b) make the
calibration test circular. This module therefore reads the *same* corpus files
the engine loads and computes p purely from counts.

``p_top3(context, top3)`` = clamp(Σ_w P_raw(w | context), 0, 1), where P_raw is
the bigram-conditional frequency when the context word has bigram mass, else a
unigram back-off. The corpus files are content-hashed so the frequency-table
version is pinned in the run metadata (blocker 3: "версия пината в лога").

No LLM, no embeddings, no neural inference (§12.3): only counts and division.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
from dataclasses import dataclass, field
from pathlib import Path

_WORD_RE = re.compile(r"\w+", re.UNICODE)


def sha256_file(path: Path) -> str:
    """Streaming SHA-256 of a file's bytes (version pin for the freq table)."""
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def last_context_word(text: str | None) -> str:
    """Extract the trailing word token of *text* (the bigram context word).

    Returns '' when there is no usable context (start of input / punctuation).
    Lowercased so the context is stable and matches the corpus casing convention.
    """
    if not text:
        return ""
    tokens = _WORD_RE.findall(text)
    return tokens[-1].lower() if tokens else ""


@dataclass
class FreqModel:
    """In-memory raw-frequency model built from smartkey corpus_*.json files."""

    unigrams: dict[str, int] = field(default_factory=dict)
    unigram_total: int = 0
    # context word -> {next word -> count}
    bigrams: dict[str, dict[str, int]] = field(default_factory=dict)
    bigram_totals: dict[str, int] = field(default_factory=dict)
    files: list[dict] = field(default_factory=list)  # [{path, sha256, bytes}]
    pmodel: str = "bigram+unigram-backoff"

    # ---- construction --------------------------------------------------------
    @classmethod
    def load(
        cls,
        corpus_files: list[Path],
        pmodel: str | None = None,
        max_bigrams: int | None = None,
    ) -> "FreqModel":
        """Load and merge the given corpus files into one raw-frequency model.

        *pmodel*: 'bigram+unigram-backoff' (default) or 'unigram' (low-memory).
        *max_bigrams*: optional cap on bigram entries per file (memory guard).
        """
        pmodel = pmodel or os.environ.get(
            "SMARTKEY_PHASEA_PMODEL", "bigram+unigram-backoff"
        )
        if max_bigrams is None:
            env_cap = os.environ.get("SMARTKEY_PHASEA_MAX_BIGRAMS")
            max_bigrams = int(env_cap) if env_cap else None

        model = cls(pmodel=pmodel)
        for path in corpus_files:
            path = Path(path)
            raw = path.read_bytes()
            data = json.loads(raw.decode("utf-8"))
            model.files.append(
                {
                    "path": str(path),
                    "sha256": hashlib.sha256(raw).hexdigest(),
                    "bytes": len(raw),
                }
            )
            for word, freq in (data.get("unigrams") or {}).items():
                model.unigrams[word] = model.unigrams.get(word, 0) + int(freq)
            if pmodel != "unigram":
                loaded = 0
                for entry in data.get("bigrams") or []:
                    ctx = entry.get("ctx")
                    word = entry.get("word")
                    count = int(entry.get("count", 0))
                    if not ctx or not word or count <= 0:
                        continue
                    slot = model.bigrams.setdefault(ctx, {})
                    slot[word] = slot.get(word, 0) + count
                    model.bigram_totals[ctx] = model.bigram_totals.get(ctx, 0) + count
                    loaded += 1
                    if max_bigrams is not None and loaded >= max_bigrams:
                        break
        model.unigram_total = sum(model.unigrams.values())
        return model

    # ---- version pin ---------------------------------------------------------
    def table_hash(self) -> str:
        """Single combined hash pinning the frequency-table version."""
        joined = "\n".join(
            f"{f['path']}:{f['sha256']}:{f['bytes']}"
            for f in sorted(self.files, key=lambda f: f["path"])
        )
        return hashlib.sha256(joined.encode("utf-8")).hexdigest()

    # ---- probability ---------------------------------------------------------
    def p_word(self, word: str, context: str) -> float:
        """Raw normalised frequency P_raw(word | context)."""
        if context and self.pmodel != "unigram":
            ctx_total = self.bigram_totals.get(context, 0)
            if ctx_total > 0:
                c = self.bigrams.get(context, {}).get(word, 0)
                if c > 0:
                    return c / ctx_total
        # unigram back-off
        if self.unigram_total > 0:
            u = self.unigrams.get(word, 0)
            if u > 0:
                return u / self.unigram_total
        return 0.0

    def p_top3(self, context: str, top3: list[str]) -> float:
        """Raw normalised frequency mass of the (deduplicated) top-3 SET."""
        seen: set[str] = set()
        total = 0.0
        for w in top3[:3]:
            if w in seen:
                continue
            seen.add(w)
            total += self.p_word(w, context)
        if total < 0.0:
            return 0.0
        return total if total <= 1.0 else 1.0


def default_corpus_files() -> list[Path]:
    """Resolve the corpus files the engine would load, in priority order.

    Mirrors smartkey_engine._load_corpus search order but returns the *repo*
    corpus/ set as the stable default for the lab (env override respected).
    """
    dirs: list[Path] = []
    env = os.environ.get("SMARTKEY_CORPUS_DIR")
    if env:
        dirs.append(Path(env).expanduser())
    dirs.append(Path.home() / ".config" / "smartkey")
    dirs.append(Path(__file__).resolve().parent.parent / "corpus")
    for d in dirs:
        files = sorted(d.glob("corpus_*.json"))
        if files:
            return files
    return []
