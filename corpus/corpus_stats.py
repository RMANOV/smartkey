#!/usr/bin/env python3
"""Corpus quality metrics reporter for SmartKey.

Reads a corpus JSON file and prints key quality metrics to help assess
whether the corpus is suitable for prediction.

Usage:
    python3 corpus_stats.py CORPUS.json
"""

import argparse
import json
import math
import sys


def _zipf_exponent(freqs: list[int]) -> float:
    """Estimate Zipf exponent α via log-log linear regression.

    Uses least-squares fit: log(freq) = -α·log(rank) + c.
    Returns α (expected ~1.0 for natural language).
    """
    n = min(len(freqs), 10_000)  # fit on top-10K to avoid long tail noise
    if n < 10:
        return 0.0

    log_ranks = [math.log(i + 1) for i in range(n)]
    log_freqs = [math.log(f) for f in freqs[:n]]

    # Least-squares: α = -cov(log_rank, log_freq) / var(log_rank)
    mean_r = sum(log_ranks) / n
    mean_f = sum(log_freqs) / n

    cov = sum((r - mean_r) * (f - mean_f) for r, f in zip(log_ranks, log_freqs)) / n
    var_r = sum((r - mean_r) ** 2 for r in log_ranks) / n

    if var_r == 0:
        return 0.0
    return -cov / var_r


def _coverage(freqs: list[int], total: int, top_n: int) -> float:
    """Return what fraction of total tokens the top-N words cover."""
    return sum(freqs[:top_n]) / total * 100 if total > 0 else 0.0


def report(corpus: dict) -> None:
    """Print a quality report for the given corpus dict."""
    unigrams = corpus.get("unigrams", {})
    bigrams = corpus.get("bigrams", [])
    trigrams = corpus.get("trigrams", [])

    vocab_size = len(unigrams)
    freqs = sorted(unigrams.values(), reverse=True)
    total_tokens = sum(freqs)
    hapax = sum(1 for f in freqs if f == 1)
    hapax_pct = hapax / vocab_size * 100 if vocab_size > 0 else 0.0

    bigram_count = len(bigrams)
    trigram_count = len(trigrams)
    avg_bigrams = bigram_count / vocab_size if vocab_size > 0 else 0.0

    zipf_alpha = _zipf_exponent(freqs)

    print("=== Corpus Quality Report ===")
    print(f"Vocabulary size:     {vocab_size:>12,} words")
    print(f"Total tokens:        {total_tokens:>12,}")
    print(
        f"Hapax legomena:      {hapax:>12,} "
        f"({hapax_pct:.1f}% — words appearing once)"
    )
    print()
    for n in (10, 100, 1_000):
        if vocab_size >= n:
            cov = _coverage(freqs, total_tokens, n)
            print(f"Top-{n:<4} coverage:   {cov:>12.1f}% of all tokens")
    print()
    print(f"Bigram count:        {bigram_count:>12,}")
    print(f"Trigram count:       {trigram_count:>12,}")
    print(f"Avg bigrams/word:    {avg_bigrams:>12.1f}")
    print(f"Zipf exponent:       {'~' + f'{zipf_alpha:.2f}':>12} (expect ~1.0 for natural language)")


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Print quality metrics for a SmartKey corpus JSON file."
    )
    parser.add_argument("corpus", help="Path to the corpus JSON file.")
    args = parser.parse_args()

    try:
        with open(args.corpus, encoding="utf-8") as fh:
            corpus = json.load(fh)
    except (OSError, json.JSONDecodeError) as e:
        print(f"Error reading corpus: {e}", file=sys.stderr)
        sys.exit(1)

    report(corpus)


if __name__ == "__main__":
    main()
