#!/usr/bin/env python3
"""Hybrid corpus generator: wordfreq unigrams + Wikipedia bigrams/trigrams.

Combines two data sources for optimal SmartKey prediction quality:
- wordfreq library: accurate Zipf-based unigram frequencies from real-world data
- Wikipedia dumps: natural bigram/trigram contexts from running text

Usage:
    python3 generate_full_corpus.py --lang en --unigrams 20000
    python3 generate_full_corpus.py --lang bg --unigrams 10000 --max-articles 0
    python3 generate_full_corpus.py --lang en --unigrams 20000 --wiki-url URL
"""

import argparse
import json
import os
import re
import subprocess
import sys
from collections import Counter
from pathlib import Path

# Add corpus/ to path for imports.
sys.path.insert(0, str(Path(__file__).parent))

from build_corpus import tokenize, filter_by_freq
from extract_wiki import iter_articles

# Wiki dump URLs per language.
_WIKI_URLS = {
    "en": "https://dumps.wikimedia.org/enwiki/latest/enwiki-latest-pages-articles.xml.bz2",
    "bg": "https://dumps.wikimedia.org/bgwiki/latest/bgwiki-latest-pages-articles.xml.bz2",
}

# Default dump filenames.
_DUMP_FILENAMES = {
    "en": "enwiki-latest-pages-articles.xml.bz2",
    "bg": "bgwiki-latest-pages-articles.xml.bz2",
}


def get_wordfreq_unigrams(lang: str, count: int) -> dict[str, int]:
    """Get top-N unigrams with integer frequencies from wordfreq.

    Converts Zipf frequency (log10 scale) to integer counts:
    zipf 7.0 → 10_000_000, zipf 3.0 → 1_000, zipf 1.0 → 10
    """
    from wordfreq import top_n_list, zipf_frequency

    words = top_n_list(lang, count)
    unigrams = {}
    for word in words:
        zipf = zipf_frequency(word, lang)
        # Convert Zipf to integer frequency. Floor at 1.
        freq = max(1, int(10**zipf))
        unigrams[word] = freq

    print(
        f"[wordfreq] {len(unigrams):,} unigrams for '{lang}' "
        f"(top freq: {max(unigrams.values()):,}, "
        f"min freq: {min(unigrams.values()):,})",
        file=sys.stderr,
    )
    return unigrams


def ensure_dump(lang: str, raw_dir: Path, wiki_url: str | None = None) -> Path:
    """Download Wikipedia dump if not cached. Returns path to dump file."""
    filename = _DUMP_FILENAMES.get(lang, f"{lang}wiki-latest-pages-articles.xml.bz2")
    dump_path = raw_dir / filename

    if dump_path.exists():
        size_mb = dump_path.stat().st_size / (1024 * 1024)
        print(f"[cache] Using existing dump: {dump_path} ({size_mb:.0f} MB)", file=sys.stderr)
        return dump_path

    url = wiki_url or _WIKI_URLS.get(lang)
    if not url:
        print(f"Error: no wiki URL for language '{lang}'. Use --wiki-url.", file=sys.stderr)
        sys.exit(1)

    raw_dir.mkdir(parents=True, exist_ok=True)
    print(f"[download] {url} → {dump_path}", file=sys.stderr)

    # Use download_corpus.sh for consistency.
    script = Path(__file__).parent / "download_corpus.sh"
    if script.exists():
        subprocess.run(
            ["bash", str(script), "--lang", lang, "--output-dir", str(raw_dir)],
            check=True,
        )
    else:
        # Fallback: direct curl.
        subprocess.run(
            ["curl", "-L", "--retry", "3", "--continue-at", "-", "-o", str(dump_path), url],
            check=True,
        )

    if not dump_path.exists():
        print(f"Error: download failed, {dump_path} not found.", file=sys.stderr)
        sys.exit(1)

    return dump_path


def supplement_unigrams_from_wiki(
    dump_path: Path,
    existing: dict[str, int],
    target: int,
    max_articles: int = 0,
) -> dict[str, int]:
    """Extract additional unigrams from Wikipedia to reach target count.

    Streams articles, counts word frequencies, and merges the top-N missing
    words into the existing unigram dict (existing entries are never overwritten).
    """
    if len(existing) >= target:
        return existing

    need = target - len(existing)
    vocab = set(existing.keys())
    word_counts: Counter = Counter()
    article_count = 0

    print(
        f"[supplement] Need {need:,} more unigrams from Wikipedia "
        f"(have {len(existing):,}, target {target:,})",
        file=sys.stderr,
    )

    for article_text in iter_articles(str(dump_path)):
        words = tokenize(article_text)
        for w in words:
            if w not in vocab and len(w) >= 2:
                word_counts[w] += 1

        article_count += 1
        if article_count % 25_000 == 0:
            print(
                f"[supplement] {article_count:,} articles, "
                f"{len(word_counts):,} candidate words",
                file=sys.stderr,
            )

        if 0 < max_articles <= article_count:
            break

    # Take top-N by frequency, require at least 3 occurrences.
    top = word_counts.most_common()
    added = 0
    merged = dict(existing)
    for word, count in top:
        if count < 3:
            break
        if added >= need:
            break
        merged[word] = count
        added += 1

    print(
        f"[supplement] Added {added:,} unigrams from Wikipedia "
        f"(total: {len(merged):,})",
        file=sys.stderr,
    )
    return merged


def extract_ngrams_from_wiki(
    dump_path: Path,
    vocab: set[str],
    max_articles: int,
    min_bigram_freq: int = 3,
    min_trigram_freq: int = 2,
) -> tuple[Counter, Counter]:
    """Stream Wikipedia articles and collect bigrams/trigrams.

    Only keeps n-grams where ALL words are in the vocabulary set.
    Returns (bigrams_counter, trigrams_counter).
    """
    bigrams: Counter = Counter()
    trigrams: Counter = Counter()
    article_count = 0

    print(
        f"[wiki] Streaming articles from {dump_path.name} "
        f"(limit: {'unlimited' if max_articles <= 0 else f'{max_articles:,}'})",
        file=sys.stderr,
    )

    for article_text in iter_articles(str(dump_path)):
        words = tokenize(article_text)
        if not words:
            continue

        # Collect bigrams — filter to vocab words.
        for w1, w2 in zip(words, words[1:]):
            if w1 in vocab and w2 in vocab:
                bigrams[(w1, w2)] += 1

        # Collect trigrams — filter to vocab words.
        for w1, w2, w3 in zip(words, words[1:], words[2:]):
            if w1 in vocab and w2 in vocab and w3 in vocab:
                trigrams[(w1, w2, w3)] += 1

        article_count += 1
        if article_count % 10_000 == 0:
            print(
                f"[wiki] {article_count:,} articles, "
                f"{len(bigrams):,} bigram types, "
                f"{len(trigrams):,} trigram types",
                file=sys.stderr,
            )

        if 0 < max_articles <= article_count:
            print(f"[wiki] Reached article limit ({max_articles:,}), stopping.", file=sys.stderr)
            break

    print(
        f"[wiki] Done: {article_count:,} articles → "
        f"{len(bigrams):,} bigram types, {len(trigrams):,} trigram types",
        file=sys.stderr,
    )

    # Apply frequency filters.
    bigrams = filter_by_freq(bigrams, min_bigram_freq)
    trigrams = filter_by_freq(trigrams, min_trigram_freq)

    print(
        f"[wiki] After freq filter: "
        f"{len(bigrams):,} bigrams (min {min_bigram_freq}), "
        f"{len(trigrams):,} trigrams (min {min_trigram_freq})",
        file=sys.stderr,
    )

    return bigrams, trigrams


def build_corpus_json(
    unigrams: dict[str, int],
    bigrams: Counter,
    trigrams: Counter,
    max_bigrams: int = 0,
    max_trigrams: int = 0,
) -> dict:
    """Build SmartKey corpus JSON structure from components.

    If max_bigrams/max_trigrams > 0, keep only top-N by frequency.
    """
    # Sort unigrams by frequency descending.
    sorted_unigrams = dict(sorted(unigrams.items(), key=lambda x: -x[1]))

    # Convert bigrams to list of dicts, sorted by count descending.
    top_bigrams = bigrams.most_common(max_bigrams if max_bigrams > 0 else None)
    bigram_list = [
        {"ctx": ctx, "word": word, "count": count}
        for (ctx, word), count in top_bigrams
    ]

    # Convert trigrams to list of dicts, sorted by count descending.
    top_trigrams = trigrams.most_common(max_trigrams if max_trigrams > 0 else None)
    trigram_list = [
        {"w1": w1, "w2": w2, "word": w3, "count": count}
        for (w1, w2, w3), count in top_trigrams
    ]

    return {
        "unigrams": sorted_unigrams,
        "bigrams": bigram_list,
        "trigrams": trigram_list,
    }


def main(argv: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(
        description="Generate hybrid corpus: wordfreq unigrams + Wikipedia n-grams."
    )
    parser.add_argument(
        "--lang",
        required=True,
        help="Language code (e.g. 'en', 'bg').",
    )
    parser.add_argument(
        "--unigrams",
        type=int,
        default=20000,
        help="Number of unigrams to extract from wordfreq (default: 20000).",
    )
    parser.add_argument(
        "--max-articles",
        type=int,
        default=0,
        help="Max Wikipedia articles to process (0 = unlimited, default: 0).",
    )
    parser.add_argument(
        "--min-bigram-freq",
        type=int,
        default=3,
        help="Minimum frequency for bigrams (default: 3).",
    )
    parser.add_argument(
        "--min-trigram-freq",
        type=int,
        default=2,
        help="Minimum frequency for trigrams (default: 2).",
    )
    parser.add_argument(
        "--max-bigrams",
        type=int,
        default=10000,
        help="Keep only top-N bigrams by frequency (default: 10000).",
    )
    parser.add_argument(
        "--max-trigrams",
        type=int,
        default=3000,
        help="Keep only top-N trigrams by frequency (default: 3000).",
    )
    parser.add_argument(
        "--wiki-url",
        default=None,
        help="Override Wikipedia dump URL.",
    )
    parser.add_argument(
        "--raw-dir",
        default=None,
        help="Directory for cached Wiki dumps (default: corpus/raw/).",
    )
    parser.add_argument(
        "--dump-path",
        default=None,
        help="Direct path to a bz2 Wikipedia dump (skips download).",
    )
    parser.add_argument(
        "-o",
        "--output",
        default=None,
        help="Output path (default: ~/.config/smartkey/corpus_{lang}.json).",
    )
    args = parser.parse_args(argv)

    if not re.fullmatch(r'[a-z]{2,5}', args.lang):
        print(f"Error: invalid language code '{args.lang}' (expected 2-5 lowercase letters)", file=sys.stderr)
        sys.exit(1)

    raw_dir = Path(args.raw_dir) if args.raw_dir else Path(__file__).parent / "raw"
    output = Path(args.output) if args.output else (
        Path.home() / ".config" / "smartkey" / f"corpus_{args.lang}.json"
    )

    # Step 1: Unigrams from wordfreq.
    print(f"\n{'='*60}", file=sys.stderr)
    print(f"  Generating corpus for '{args.lang}'", file=sys.stderr)
    print(f"{'='*60}\n", file=sys.stderr)

    unigrams = get_wordfreq_unigrams(args.lang, args.unigrams)

    # Step 1b: Supplement from Wikipedia if wordfreq fell short.
    if args.dump_path:
        supplement_dump = Path(args.dump_path)
    else:
        supplement_dump = ensure_dump(args.lang, raw_dir, args.wiki_url)

    if len(unigrams) < args.unigrams:
        unigrams = supplement_unigrams_from_wiki(
            supplement_dump, unigrams, args.unigrams, args.max_articles,
        )

    vocab = set(unigrams.keys())

    # Step 2: Bigrams/trigrams from Wikipedia (reuse resolved dump).
    bigrams, trigrams = extract_ngrams_from_wiki(
        supplement_dump,
        vocab,
        args.max_articles,
        args.min_bigram_freq,
        args.min_trigram_freq,
    )

    # Step 3: Build and write corpus.
    corpus = build_corpus_json(
        unigrams, bigrams, trigrams,
        max_bigrams=args.max_bigrams,
        max_trigrams=args.max_trigrams,
    )

    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(
        json.dumps(corpus, ensure_ascii=False, separators=(",", ":")) + "\n",
        encoding="utf-8",
    )

    size_kb = output.stat().st_size / 1024
    print(
        f"\n[done] Wrote {output} ({size_kb:.0f} KB)\n"
        f"  Unigrams: {len(corpus['unigrams']):,}\n"
        f"  Bigrams:  {len(corpus['bigrams']):,}\n"
        f"  Trigrams: {len(corpus['trigrams']):,}",
        file=sys.stderr,
    )


if __name__ == "__main__":
    main()
