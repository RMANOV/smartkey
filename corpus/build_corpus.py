#!/usr/bin/env python3
"""Build n-gram corpus from text files for SmartKey.

Processes one or more UTF-8 text files (or stdin) and emits a JSON corpus
containing unigram frequencies, bigram counts, and trigram counts -- ready
for loading into SmartKeyEngine at startup.

Usage:
    python3 build_corpus.py INPUT [INPUT ...] -o OUTPUT.json [--min-freq N]
    python3 extract_wiki.py dump.bz2 | python3 build_corpus.py --stdin -o corpus.json
"""

import argparse
import json
import re
import sys
import typing
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
    unigrams: Counter,
    bigrams: Counter,
    trigrams: Counter,
    max_vocab: int = 0,
) -> dict:
    """Serialise counters into the SmartKey JSON corpus format.

    If *max_vocab* > 0, only the top-N unigrams by frequency are kept,
    and bigrams/trigrams are filtered to only include those words.
    """
    if max_vocab > 0:
        top_words = {w for w, _ in unigrams.most_common(max_vocab)}
        unigrams = Counter({w: c for w, c in unigrams.items() if w in top_words})
        bigrams = Counter(
            {k: c for k, c in bigrams.items() if k[0] in top_words and k[1] in top_words}
        )
        trigrams = Counter(
            {
                k: c
                for k, c in trigrams.items()
                if k[0] in top_words and k[1] in top_words and k[2] in top_words
            }
        )

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


# ---------------------------------------------------------------------------
# Streaming n-gram accumulation.
# ---------------------------------------------------------------------------


def _process_line(
    line: str,
    unigrams: Counter,
    bigrams: Counter,
    trigrams: Counter,
) -> None:
    """Tokenize a single line and update counters in-place."""
    for sentence in tokenize_sentences(line):
        words = tokenize_words(sentence)
        if not words:
            continue
        unigrams.update(words)
        if len(words) >= 2:
            bigrams.update(zip(words, words[1:]))
        if len(words) >= 3:
            trigrams.update(zip(words, words[1:], words[2:]))


def _iter_lines(args) -> tuple[typing.Iterator[str], str]:
    """Yield lines from stdin or input files. Returns (line_iter, source_name)."""
    if args.stdin:
        return sys.stdin, "stdin"
    else:
        def _file_lines():
            for path_str in args.inputs:
                path = Path(path_str)
                if not path.is_file():
                    print(f"Warning: skipping '{path}' (not a file)", file=sys.stderr)
                    continue
                with open(path, encoding="utf-8") as fh:
                    yield from fh

        return _file_lines(), "files"


def main(argv: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(
        description="Build n-gram corpus from text files for SmartKey."
    )
    parser.add_argument(
        "inputs",
        metavar="INPUT_FILE",
        nargs="*",
        help="One or more UTF-8 text files to process.",
    )
    parser.add_argument(
        "-o",
        "--output",
        default=None,
        help="Path to write the JSON corpus (required unless --lang is given).",
    )
    parser.add_argument(
        "--min-freq",
        type=int,
        default=2,
        help="Minimum frequency to include an n-gram (default: 2).",
    )
    parser.add_argument(
        "--max-vocab",
        type=int,
        default=0,
        help="Cap vocabulary to top-N words by frequency (0 = unlimited).",
    )
    parser.add_argument(
        "--stdin",
        action="store_true",
        help="Read input lines from stdin (for piping from extract_wiki.py).",
    )
    parser.add_argument(
        "--lang",
        type=str,
        default="",
        help="Language code (e.g. 'en', 'bg'). Sets default output to "
        "~/.config/smartkey/corpus_{lang}.json.",
    )
    args = parser.parse_args(argv)

    if not args.stdin and not args.inputs:
        parser.error("either provide INPUT_FILE(s) or use --stdin")

    # Default output path when --lang is given and -o is not explicitly set.
    if args.output is None:
        if args.lang:
            config_dir = Path.home() / ".config" / "smartkey"
            args.output = str(config_dir / f"corpus_{args.lang}.json")
        else:
            parser.error("either provide -o/--output or use --lang")

    unigrams: Counter = Counter()
    bigrams: Counter = Counter()
    trigrams: Counter = Counter()
    total_lines = 0
    total_tokens = 0

    line_iter, source = _iter_lines(args)

    for line in line_iter:
        line = line.rstrip("\n")
        if not line:
            continue
        before = sum(unigrams.values())
        _process_line(line, unigrams, bigrams, trigrams)
        after = sum(unigrams.values())
        added = after - before
        if added == 0:
            continue
        total_tokens += added
        total_lines += 1

        if total_lines % 100_000 == 0:
            print(
                f"[build] {total_lines:,} lines, "
                f"{total_tokens:,} tokens, "
                f"{len(unigrams):,} unique words",
                file=sys.stderr,
            )

    if total_tokens == 0:
        print("Error: no tokens found in input.", file=sys.stderr)
        sys.exit(1)

    # Apply frequency filter.
    unigrams = filter_by_freq(unigrams, args.min_freq)
    bigrams = filter_by_freq(bigrams, args.min_freq)
    trigrams = filter_by_freq(trigrams, args.min_freq)

    corpus = corpus_to_json(unigrams, bigrams, trigrams, args.max_vocab)

    out_path = Path(args.output)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(
        json.dumps(corpus, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )

    print(
        f"Processed {total_tokens:,} tokens from {total_lines:,} lines ({source}), "
        f"wrote {len(corpus['unigrams'])} unigrams, "
        f"{len(corpus['bigrams'])} bigrams, "
        f"{len(corpus['trigrams'])} trigrams"
    )


if __name__ == "__main__":
    main()
