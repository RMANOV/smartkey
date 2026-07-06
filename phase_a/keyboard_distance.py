"""Offline keyboard-neighbour distance for Phase-A predictive OODA tests.

This is a motor-error model, not a language model.  It captures a common human
typing failure mode: the intended key is known by motor memory, but fatigue,
stress, or low blood sugar shifts the finger to an adjacent physical key.

The module is deliberately offline/test-mode friendly.  It does not touch the
live IBus path, does not persist typed words, and does not use LLMs,
embeddings, or neural inference.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Iterable, Mapping


ADJACENT_SUBSTITUTION_COST = 0.35
NON_ADJACENT_SUBSTITUTION_COST = 1.0
INSERT_DELETE_COST = 1.0


@dataclass(frozen=True)
class EditStep:
    """One edit in the weighted keyboard-distance path."""

    op: str
    source: str
    target: str
    source_index: int
    target_index: int
    cost: float


@dataclass(frozen=True)
class DistanceResult:
    """Weighted edit distance plus an explanation path."""

    source: str
    target: str
    layout: str
    distance: float
    normalized: float
    edits: tuple[EditStep, ...]

    @property
    def adjacent_substitutions(self) -> int:
        return sum(1 for e in self.edits if e.op == "adjacent_substitution")


@dataclass(frozen=True)
class CandidateDistance:
    """A candidate word ranked by keyboard-distance fit."""

    candidate: str
    distance: DistanceResult
    frequency: int = 0


_EN_ROWS = (
    (0.0, "qwertyuiop"),
    (0.5, "asdfghjkl"),
    (1.0, "zxcvbnm"),
)

_BG_ROWS = (
    (0.0, "яшертъуиоп"),
    (0.5, "асдфгхйкл"),
    (1.0, "зьцвбнм"),
)


def _positions(rows: tuple[tuple[float, str], ...]) -> dict[str, tuple[int, float]]:
    pos: dict[str, tuple[int, float]] = {}
    for row, (offset, chars) in enumerate(rows):
        for col, ch in enumerate(chars):
            pos[ch] = (row, offset + col)
    return pos


_POSITIONS = {
    "en": _positions(_EN_ROWS),
    "bg": _positions(_BG_ROWS),
}


def _has_cyrillic(text: str) -> bool:
    return any("\u0400" <= ch <= "\u04ff" for ch in text)


def resolve_layout(source: str, target: str, layout: str = "auto") -> str:
    """Resolve ``auto`` to BG when either word contains Cyrillic, else EN."""

    if layout in ("en", "bg"):
        return layout
    if layout != "auto":
        raise ValueError("layout must be 'auto', 'en', or 'bg'")
    return "bg" if _has_cyrillic(source) or _has_cyrillic(target) else "en"


def are_adjacent_keys(a: str, b: str, layout: str = "auto") -> bool:
    """Return True when two characters sit on neighbouring physical keys."""

    if not a or not b or a == b:
        return False
    a = a.lower()
    b = b.lower()
    layout = resolve_layout(a, b, layout)
    pos = _POSITIONS[layout]
    if a not in pos or b not in pos:
        return False
    row_a, col_a = pos[a]
    row_b, col_b = pos[b]
    return abs(row_a - row_b) <= 1 and abs(col_a - col_b) <= 1.1


def substitution_cost(a: str, b: str, layout: str = "auto") -> float:
    """Low cost for adjacent-key substitutions, full cost otherwise."""

    if a == b:
        return 0.0
    if are_adjacent_keys(a, b, layout):
        return ADJACENT_SUBSTITUTION_COST
    return NON_ADJACENT_SUBSTITUTION_COST


def keyboard_weighted_distance(
    source: str,
    target: str,
    *,
    layout: str = "auto",
) -> DistanceResult:
    """Weighted Levenshtein distance with cheap adjacent-key substitution.

    ``source`` is what the human typed; ``target`` is a candidate intended word.
    The path is useful for OODA explanation: a single adjacent substitution is a
    very different hypothesis from an arbitrary non-word.
    """

    source = (source or "").lower()
    target = (target or "").lower()
    resolved_layout = resolve_layout(source, target, layout)
    s = list(source)
    t = list(target)
    rows = len(s) + 1
    cols = len(t) + 1
    dp = [[0.0] * cols for _ in range(rows)]
    back: list[list[tuple[str, int, int, float] | None]] = [
        [None] * cols for _ in range(rows)
    ]

    for i in range(1, rows):
        dp[i][0] = i * INSERT_DELETE_COST
        back[i][0] = ("delete", i - 1, -1, INSERT_DELETE_COST)
    for j in range(1, cols):
        dp[0][j] = j * INSERT_DELETE_COST
        back[0][j] = ("insert", -1, j - 1, INSERT_DELETE_COST)

    for i in range(1, rows):
        for j in range(1, cols):
            sub = substitution_cost(s[i - 1], t[j - 1], resolved_layout)
            sub_op = (
                "match"
                if sub == 0.0
                else "adjacent_substitution"
                if sub == ADJACENT_SUBSTITUTION_COST
                else "substitution"
            )
            choices = [
                (dp[i - 1][j] + INSERT_DELETE_COST, "delete", i - 1, -1, INSERT_DELETE_COST),
                (dp[i][j - 1] + INSERT_DELETE_COST, "insert", -1, j - 1, INSERT_DELETE_COST),
                (dp[i - 1][j - 1] + sub, sub_op, i - 1, j - 1, sub),
            ]
            best = min(choices, key=lambda item: (item[0], item[1] != "match"))
            dp[i][j] = best[0]
            back[i][j] = best[1:]

    edits: list[EditStep] = []
    i, j = len(s), len(t)
    while i > 0 or j > 0:
        step = back[i][j]
        if step is None:
            break
        op, si, tj, cost = step
        src = s[si] if si >= 0 else ""
        dst = t[tj] if tj >= 0 else ""
        if op != "match":
            edits.append(EditStep(op, src, dst, si, tj, cost))
        if op in ("match", "substitution", "adjacent_substitution"):
            i -= 1
            j -= 1
        elif op == "delete":
            i -= 1
        elif op == "insert":
            j -= 1
        else:
            raise AssertionError(f"unknown edit op {op!r}")
    edits.reverse()

    denom = max(len(s), len(t), 1)
    distance = dp[len(s)][len(t)]
    return DistanceResult(
        source=source,
        target=target,
        layout=resolved_layout,
        distance=distance,
        normalized=distance / denom,
        edits=tuple(edits),
    )


def _candidate_items(
    candidates: Iterable[str] | Mapping[str, int],
) -> Iterable[tuple[str, int]]:
    if isinstance(candidates, Mapping):
        return candidates.items()
    return ((c, 0) for c in candidates)


def nearest_keyboard_candidates(
    token: str,
    candidates: Iterable[str] | Mapping[str, int],
    *,
    layout: str = "auto",
    max_distance: float = 1.0,
    limit: int = 5,
    max_len_delta: int = 2,
) -> list[CandidateDistance]:
    """Rank candidate intended words by adjacent-key weighted distance."""

    token_norm = (token or "").lower()
    ranked: list[CandidateDistance] = []
    for candidate, freq in _candidate_items(candidates):
        cand = (candidate or "").lower()
        if not cand or cand == token_norm:
            continue
        if abs(len(cand) - len(token_norm)) > max_len_delta:
            continue
        dist = keyboard_weighted_distance(token_norm, cand, layout=layout)
        if dist.distance <= max_distance:
            ranked.append(CandidateDistance(cand, dist, int(freq or 0)))

    ranked.sort(
        key=lambda item: (
            item.distance.distance,
            item.distance.normalized,
            -item.frequency,
            item.candidate,
        )
    )
    return ranked[:limit]
