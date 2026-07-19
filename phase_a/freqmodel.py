"""Raw n-gram frequency model — the only predictor in the measured path.

``p_top3`` is the raw normalised
frequency* of smartkey's own n-gram / frequency table — deliberately the dumb
baseline, and emphatically NOT the engine's ensemble ``score`` / ``confidence``
(which blend Markov/personal/tech, then pass a neural reranker + an internal
isotonic calibrator). Using the ensemble output would (a) smuggle neural
inference into the measured p and (b) make the calibration test circular.

Scope-firewall by construction: this module imports NOTHING from the Rust core
(``smartkey_py``) or the ensemble — it reads the same corpus JSON the engine
loads and computes both the top-3 candidates AND their probability purely from
counts + argmax. It is a faithful mirror of the core's n-gram primitives:

    p_word(w, ctx) = count(ctx, w) / total(ctx)      # == MarkovChain raw
                                                      #    prob_from_followers
                                                      #    (markov.rs: count/total)
    top3(ctx)      = 3 highest-count followers of ctx # == NgramTrie::prefix_search
                                                      #    ranking (raw count desc)

No LLM, no embeddings, no neural inference: only counts and division.
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
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def last_context_word(text: str | None) -> str:
    """Trailing word token of *text* (the bigram context word). '' when none."""
    if not text:
        return ""
    tokens = _WORD_RE.findall(text)
    return tokens[-1].lower() if tokens else ""


@dataclass
class FreqModel:
    """In-memory raw-frequency model built from smartkey corpus_*.json files."""

    unigrams: dict[str, int] = field(default_factory=dict)
    unigram_total: int = 0
    bigrams: dict[str, dict[str, int]] = field(default_factory=dict)  # ctx -> {w: count}
    bigram_totals: dict[str, int] = field(default_factory=dict)
    files: list[dict] = field(default_factory=list)  # [{path, sha256, bytes}]
    pmodel: str = "bigram+unigram-backoff"
    _unigram_top3: list[str] = field(default_factory=list)  # cached global back-off

    # ---- construction --------------------------------------------------------
    @classmethod
    def load(
        cls,
        corpus_files: list[Path],
        pmodel: str | None = None,
        max_bigrams: int | None = None,
    ) -> "FreqModel":
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
        # Precompute the global unigram top-3 once (back-off when ctx has no mass).
        model._unigram_top3 = [
            w for w, _ in sorted(model.unigrams.items(), key=lambda kv: (-kv[1], kv[0]))[:3]
        ]
        return model

    # ---- version pin ---------------------------------------------------------
    def table_hash(self) -> str:
        joined = "\n".join(
            f"{f['path']}:{f['sha256']}:{f['bytes']}"
            for f in sorted(self.files, key=lambda f: f["path"])
        )
        return hashlib.sha256(joined.encode("utf-8")).hexdigest()

    # ---- candidate generation (n-gram-only, NO ensemble) ---------------------
    def followers(self, context: str) -> dict[str, int]:
        """Raw follower->count map for *context* (empty when no bigram mass)."""
        if context and self.pmodel != "unigram":
            return self.bigrams.get(context, {})
        return {}

    def top3(self, context: str) -> list[str]:
        """Pure n-gram top-3 next-word candidates for *context*, ranked by raw
        count (ties broken lexicographically for determinism). Unigram back-off
        (global top-3) when the context word has no bigram mass. This mirrors
        NgramTrie::prefix_search's raw-count ranking — no ensemble, no neural."""
        slot = self.followers(context)
        if slot:
            ranked = sorted(slot.items(), key=lambda kv: (-kv[1], kv[0]))
            return [w for w, _ in ranked[:3]]
        return list(self._unigram_top3)

    # ---- probability ---------------------------------------------------------
    def p_word(self, word: str, context: str) -> float:
        """Raw normalised frequency P_raw(word | context)."""
        if context and self.pmodel != "unigram":
            ctx_total = self.bigram_totals.get(context, 0)
            if ctx_total > 0:
                c = self.bigrams.get(context, {}).get(word, 0)
                if c > 0:
                    return c / ctx_total
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

    def predict(self, context: str) -> tuple[list[str], float]:
        """Convenience: (n-gram top-3, p_top3) in one call — the full measured
        decision, computed entirely from counts."""
        t3 = self.top3(context)
        return t3, self.p_top3(context, t3)
