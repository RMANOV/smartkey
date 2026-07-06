"""Offline predictive OODA test mode for SmartKey Phase-A.

This module makes the loop inspectable without switching the live IBus engine:

* Observe: capture the current token/context.
* Orient: decide whether the token is known or looks like a motor-key slip.
* Decide: choose a test-only recommendation.
* Act: report what would be shown/logged; never mutate live text.

No LLMs, embeddings, network calls, or live keyboard actions are involved.
"""

from __future__ import annotations

import argparse
import json
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Iterable, Mapping

from .freqmodel import FreqModel, _WORD_RE, default_corpus_files
from .keyboard_distance import (
    CandidateDistance,
    nearest_keyboard_candidates,
)


@dataclass(frozen=True)
class OodaSnapshot:
    observe: dict
    orient: dict
    decide: dict
    act: dict

    def to_json(self) -> str:
        return json.dumps(asdict(self), ensure_ascii=False, indent=2, sort_keys=True)


def _last_token(text: str) -> str:
    toks = _WORD_RE.findall(text or "")
    return toks[-1].lower() if toks else ""


def _context_tail(text: str, n: int = 6) -> list[str]:
    toks = [t.lower() for t in _WORD_RE.findall(text or "")]
    if toks and (not text or not text[-1].isspace()):
        toks = toks[:-1]
    return toks[-n:]


def _candidate_payload(c: CandidateDistance) -> dict:
    return {
        "candidate": c.candidate,
        "frequency": c.frequency,
        "distance": round(c.distance.distance, 4),
        "normalized": round(c.distance.normalized, 4),
        "layout": c.distance.layout,
        "adjacent_substitutions": c.distance.adjacent_substitutions,
        "edits": [
            {
                "op": e.op,
                "source": e.source,
                "target": e.target,
                "source_index": e.source_index,
                "target_index": e.target_index,
                "cost": e.cost,
            }
            for e in c.distance.edits
        ],
    }


def lexicon_from_corpus(
    corpus_files: list[Path] | None = None,
    *,
    top_n: int = 50_000,
) -> dict[str, int]:
    """Load a bounded frequency lexicon from SmartKey corpus JSON files."""

    files = corpus_files if corpus_files is not None else default_corpus_files()
    if not files:
        return {}
    model = FreqModel.load(files, pmodel="unigram")
    items = sorted(model.unigrams.items(), key=lambda item: (-item[1], item[0]))
    return dict(items[:top_n])


def snapshot(
    *,
    text: str = "",
    token: str | None = None,
    lexicon: Iterable[str] | Mapping[str, int] = (),
    layout: str = "auto",
    max_distance: float = 1.0,
    limit: int = 5,
) -> OodaSnapshot:
    """Build a single offline OODA snapshot for a token."""

    if isinstance(lexicon, Mapping):
        lex: Mapping[str, int] = {str(k).lower(): int(v) for k, v in lexicon.items()}
    else:
        lex = {str(w).lower(): 0 for w in lexicon}

    observed_token = (token or _last_token(text)).lower()
    known = bool(observed_token) and observed_token in lex
    nearest = (
        []
        if known or not observed_token
        else nearest_keyboard_candidates(
            observed_token,
            lex,
            layout=layout,
            max_distance=max_distance,
            limit=limit,
        )
    )
    nearest_payload = [_candidate_payload(c) for c in nearest]
    best = nearest_payload[0] if nearest_payload else None

    observe = {
        "mode": "offline_test_only",
        "token": observed_token,
        "context_tail": _context_tail(text),
        "layout": layout,
        "lexicon_size": len(lex),
    }

    orient = {
        "token_known": known,
        "motor_error_hypothesis": bool(
            best and best["distance"] <= max_distance and best["adjacent_substitutions"] > 0
        ),
        "nearest_keyboard_candidates": nearest_payload,
    }

    if not observed_token:
        decision = "hold_no_token"
        reason = "No current token was observed."
    elif known:
        decision = "accept_known_token"
        reason = "The token is already in the lexicon."
    elif best and best["adjacent_substitutions"] > 0 and best["distance"] <= 0.75:
        decision = "suggest_adjacent_key_candidate"
        reason = (
            "The closest known candidate is explained by adjacent physical-key "
            "substitution, consistent with motor drift."
        )
    elif best:
        decision = "candidate_review_only"
        reason = "A nearby candidate exists, but the motor-error evidence is weak."
    else:
        decision = "hold_no_safe_correction"
        reason = "No close keyboard-neighbour candidate found under the threshold."

    decide = {
        "decision": decision,
        "reason": reason,
        "candidate": best["candidate"] if best else None,
    }
    act = {
        "effect": "none",
        "live_engine_mutation": False,
        "would_surface": decide["candidate"] if decision.startswith("suggest_") else None,
        "guardrail": "test-mode only; do not commit/replace live text from this snapshot",
    }
    return OodaSnapshot(observe=observe, orient=orient, decide=decide, act=act)


def _parse_candidates(value: str | None) -> dict[str, int]:
    if not value:
        return {}
    out: dict[str, int] = {}
    for raw in value.split(","):
        word = raw.strip().lower()
        if word:
            out[word] = 1
    return out


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="SmartKey offline predictive OODA test mode")
    ap.add_argument("--text", default="", help="Text/context to inspect")
    ap.add_argument("--token", default=None, help="Current token override")
    ap.add_argument("--candidates", default=None, help="Comma-separated candidate lexicon")
    ap.add_argument("--corpus", action="store_true", help="Use SmartKey corpus JSON lexicon")
    ap.add_argument("--top-n", type=int, default=50_000, help="Corpus lexicon cap")
    ap.add_argument("--layout", choices=("auto", "en", "bg"), default="auto")
    ap.add_argument("--max-distance", type=float, default=1.0)
    ap.add_argument("--limit", type=int, default=5)
    args = ap.parse_args(argv)

    lex = _parse_candidates(args.candidates)
    if args.corpus:
        lex.update(lexicon_from_corpus(top_n=args.top_n))
    snap = snapshot(
        text=args.text,
        token=args.token,
        lexicon=lex,
        layout=args.layout,
        max_distance=args.max_distance,
        limit=args.limit,
    )
    print(snap.to_json())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
