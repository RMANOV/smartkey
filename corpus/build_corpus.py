#!/usr/bin/env python3
"""Build n-gram corpus from text files for SmartKey.

Processes one or more UTF-8 text files and emits a JSON corpus containing
unigram frequencies, bigram counts, and trigram counts -- ready for loading
into SmartKeyEngine at startup.

Usage:
    python3 build_corpus.py INPUT [INPUT ...] -o OUTPUT.json [--min-freq N]
"""

import argparse
import json
import re
import sys
from collections import Counter
from pathlib import Path


# ---------------------------------------------------------------------------
# Sentence-aware tokenisation.
# ---------------------------------------------------------------------------

# Abbreviations that should NOT trigger a sentence split when followed by ".".
_ABBREVIATIONS = frozenset(
    "dr mr mrs ms st vs etc jr sr prof gen gov rev vol no"
    " jan feb mar apr jun jul aug sep oct nov dec"
    " al approx dept est inc ltd corp univ assn ave blvd".split()
)

# Matches a potential sentence boundary: .!? followed by whitespace + uppercase.
_SENT_BOUNDARY = re.compile(r"[.!?]\s+(?=[A-ZА-ЯЁ])")

# Words: sequences of word-chars and apostrophes, but strip outer apostrophes.
_WORD_RE = re.compile(r"[\w']+", re.UNICODE)


def _is_abbreviation(text: str, dot_pos: int) -> bool:
    """Check whether the period at *dot_pos* belongs to an abbreviation."""
    if text[dot_pos] != ".":
        return False
    # Walk backward to find the preceding word.
    i = dot_pos - 1
    while i >= 0 and text[i].isalpha():
        i -= 1
    word = text[i + 1 : dot_pos].lower()
    if not word:
        return False
    # Check single-letter abbreviations (U.S., e.g., i.e.) and known list.
    if len(word) == 1:
        return True
    return word in _ABBREVIATIONS


def tokenize_sentences(text: str) -> list[str]:
    """Split *text* into sentences, respecting abbreviations.

    Splits on `.` `!` `?` followed by whitespace + uppercase letter,
    but not after common abbreviations like ``Dr.`` ``Mr.`` ``vs.``.
    """
    if not text or not text.strip():
        return []
    # Find all potential split points and filter out abbreviations.
    splits: list[int] = []
    for m in _SENT_BOUNDARY.finditer(text):
        if not _is_abbreviation(text, m.start()):
            # Split right after the punctuation mark.
            splits.append(m.start() + 1)

    if not splits:
        return [text.strip()] if text.strip() else []

    # Build sentences from the split points.
    result: list[str] = []
    prev = 0
    for pos in splits:
        chunk = text[prev:pos].strip()
        if chunk:
            result.append(chunk)
        prev = pos
    # Last segment.
    chunk = text[prev:].strip()
    if chunk:
        result.append(chunk)
    return result


def tokenize_words(text: str) -> list[str]:
    """Split *text* into lowercase word tokens, preserving contractions.

    - Keeps apostrophes *between* letters (``I'm``, ``don't``).
    - Strips leading/trailing apostrophes.
    - Handles Cyrillic and Latin naturally via Unicode ``\\w``.
    """
    if not text or not text.strip():
        return []
    tokens = _WORD_RE.findall(text.lower())
    # Strip leading/trailing apostrophes from each token.
    return [t.strip("'") for t in tokens if t.strip("'")]


def tokenize(text: str) -> list[str]:
    """Split text into lowercase word tokens (Unicode-aware).

    Uses the sentence-aware tokenize_words() for better quality.
    """
    return tokenize_words(text)


def build_ngrams(
    tokens: list[str],
) -> tuple[Counter, Counter, Counter]:
    """Return (unigram, bigram, trigram) Counters from a token list."""
    unigrams: Counter = Counter(tokens)
    bigrams: Counter = Counter(zip(tokens, tokens[1:]))
    trigrams: Counter = Counter(zip(tokens, tokens[1:], tokens[2:]))
    return unigrams, bigrams, trigrams


def filter_by_freq(counter: Counter, min_freq: int) -> Counter:
    """Drop entries below *min_freq*."""
    return Counter({k: v for k, v in counter.items() if v >= min_freq})


def corpus_to_json(
    unigrams: Counter, bigrams: Counter, trigrams: Counter
) -> dict:
    """Serialise counters into the SmartKey JSON corpus format."""
    return {
        "unigrams": dict(unigrams.most_common()),
        "bigrams": [
            {"ctx": ctx, "word": word, "count": count}
            for (ctx, word), count in bigrams.most_common()
        ],
        "trigrams": [
            {"w1": w1, "w2": w2, "word": w3, "count": count}
            for (w1, w2, w3), count in trigrams.most_common()
        ],
    }


def main(argv: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(
        description="Build n-gram corpus from text files for SmartKey."
    )
    parser.add_argument(
        "inputs",
        metavar="INPUT_FILE",
        nargs="+",
        help="One or more UTF-8 text files to process.",
    )
    parser.add_argument(
        "-o",
        "--output",
        required=True,
        help="Path to write the JSON corpus.",
    )
    parser.add_argument(
        "--min-freq",
        type=int,
        default=2,
        help="Minimum frequency to include an n-gram (default: 2).",
    )
    args = parser.parse_args(argv)

    all_tokens: list[str] = []
    for path_str in args.inputs:
        path = Path(path_str)
        if not path.is_file():
            print(f"Warning: skipping '{path}' (not a file)", file=sys.stderr)
            continue
        text = path.read_text(encoding="utf-8")
        all_tokens.extend(tokenize(text))

    if not all_tokens:
        print("Error: no tokens found in input files.", file=sys.stderr)
        sys.exit(1)

    unigrams, bigrams, trigrams = build_ngrams(all_tokens)

    unigrams = filter_by_freq(unigrams, args.min_freq)
    bigrams = filter_by_freq(bigrams, args.min_freq)
    trigrams = filter_by_freq(trigrams, args.min_freq)

    corpus = corpus_to_json(unigrams, bigrams, trigrams)

    out_path = Path(args.output)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(
        json.dumps(corpus, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )

    print(
        f"Processed {len(all_tokens)} tokens, "
        f"wrote {len(corpus['unigrams'])} unigrams, "
        f"{len(corpus['bigrams'])} bigrams, "
        f"{len(corpus['trigrams'])} trigrams"
    )


if __name__ == "__main__":
    main()
