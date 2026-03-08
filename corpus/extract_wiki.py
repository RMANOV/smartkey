#!/usr/bin/env python3
"""Streaming Wikipedia article extractor.

Reads a bz2-compressed Wikipedia XML dump and yields cleaned plain-text
articles, one per line (internal newlines → spaces).  Designed for piping
into build_corpus.py.

Usage:
    python3 extract_wiki.py DUMP.xml.bz2 [--min-words N]

Dependencies:
    mwxml (pip install mwxml) — preferred, robust XML streaming
    Falls back to bz2 + xml.etree.ElementTree.iterparse if unavailable.
"""

import argparse
import bz2
import re
import sys
import xml.etree.ElementTree as ET

# ---------------------------------------------------------------------------
# Try mwxml for robust parsing; fall back to stdlib iterparse.
# ---------------------------------------------------------------------------
try:
    import mwxml  # type: ignore[import-untyped]

    _HAS_MWXML = True
except ImportError:
    _HAS_MWXML = False

# ---------------------------------------------------------------------------
# MediaWiki markup → plain text.
# ---------------------------------------------------------------------------

# Pre-compiled patterns (order matters).
_RE_COMMENT = re.compile(r"<!--.*?-->", re.DOTALL)
_RE_REF = re.compile(r"<ref[^>]*/?>(?:.*?</ref>)?", re.DOTALL | re.IGNORECASE)
_RE_MATH = re.compile(r"<math[^>]*>.*?</math>", re.DOTALL | re.IGNORECASE)
_RE_CODE = re.compile(
    r"<(?:code|syntaxhighlight|source|nowiki)[^>]*>.*?</(?:code|syntaxhighlight|source|nowiki)>",
    re.DOTALL | re.IGNORECASE,
)
_RE_TABLE = re.compile(r"\{\|.*?\|\}", re.DOTALL)
_RE_TEMPLATE = re.compile(r"\{\{[^{}]*\}\}")  # innermost first
_RE_FILE_LINK = re.compile(
    r"\[\[(?:File|Image|Файл|Изображение):[^\]]*\]\]", re.IGNORECASE
)
_RE_CATEGORY = re.compile(
    r"\[\[(?:Category|Категория):[^\]]*\]\]", re.IGNORECASE
)
_RE_PIPED_LINK = re.compile(r"\[\[[^|\]]*\|([^\]]*)\]\]")
_RE_SIMPLE_LINK = re.compile(r"\[\[([^\]]*)\]\]")
_RE_EXTLINK_TEXT = re.compile(r"\[https?://\S+\s+([^\]]+)\]")
_RE_EXTLINK_BARE = re.compile(r"\[https?://[^\]]*\]")
_RE_HTML_TAG = re.compile(r"<[^>]+>")
_RE_BOLD_ITALIC = re.compile(r"'{2,5}")
_RE_HEADING = re.compile(r"^=+\s*(.*?)\s*=+$", re.MULTILINE)
_RE_BULLET_NUM = re.compile(r"^[*#:;]+\s*", re.MULTILINE)
_RE_MULTI_SPACE = re.compile(r"[ \t]+")
_RE_MULTI_NEWLINE = re.compile(r"\n{2,}")

# Redirect / disambiguation detection.
_RE_REDIRECT = re.compile(r"^#(?:REDIRECT|ПРЕНАСОЧВАНЕ)\s*\[\[", re.IGNORECASE)
_DISAMBIG_TEMPLATES = {
    "disambig",
    "disambiguation",
    "disamb",
    "dab",
    "пояснение",
    "многозначност",
}


def _is_redirect(text: str) -> bool:
    return bool(_RE_REDIRECT.match(text))


def _is_disambiguation(text: str) -> bool:
    lower = text.lower()
    return any(f"{{{{{t}" in lower for t in _DISAMBIG_TEMPLATES)


def clean_wikitext(raw: str) -> str:
    """Strip MediaWiki markup and return plain text."""
    text = raw

    # 1. Comments, refs, math, code blocks.
    text = _RE_COMMENT.sub("", text)
    text = _RE_REF.sub("", text)
    text = _RE_MATH.sub("", text)
    text = _RE_CODE.sub("", text)

    # 2. Tables.
    text = _RE_TABLE.sub("", text)

    # 3. Templates — strip from innermost outward (max 8 passes).
    for _ in range(8):
        new = _RE_TEMPLATE.sub("", text)
        if new == text:
            break
        text = new

    # 4. File/Image and Category links (remove entirely).
    text = _RE_FILE_LINK.sub("", text)
    text = _RE_CATEGORY.sub("", text)

    # 5. Wiki links: [[target|display]] → display, [[target]] → target.
    text = _RE_PIPED_LINK.sub(r"\1", text)
    text = _RE_SIMPLE_LINK.sub(r"\1", text)

    # 6. External links.
    text = _RE_EXTLINK_TEXT.sub(r"\1", text)
    text = _RE_EXTLINK_BARE.sub("", text)

    # 7. Remaining HTML tags.
    text = _RE_HTML_TAG.sub("", text)

    # 8. Bold/italic markers, headings, bullets.
    text = _RE_BOLD_ITALIC.sub("", text)
    text = _RE_HEADING.sub(r"\1", text)
    text = _RE_BULLET_NUM.sub("", text)

    # 9. Collapse whitespace.
    text = _RE_MULTI_SPACE.sub(" ", text)
    text = _RE_MULTI_NEWLINE.sub("\n", text)

    return text.strip()


# ---------------------------------------------------------------------------
# Article generators.
# ---------------------------------------------------------------------------


def _iter_articles_mwxml(path: str, min_words: int):
    """Yield cleaned article texts using mwxml (robust)."""
    dump = mwxml.Dump.from_file(bz2.open(path, "rt", encoding="utf-8"))
    count = 0

    for page in dump:
        # Skip non-article namespaces (0 = main article namespace).
        if page.namespace != 0:
            continue

        # Get the latest revision.
        for revision in page:
            text = revision.text or ""
            break
        else:
            continue

        if _is_redirect(text) or _is_disambiguation(text):
            continue

        cleaned = clean_wikitext(text)
        word_count = len(cleaned.split())
        if word_count < min_words:
            continue

        count += 1
        if count % 10_000 == 0:
            print(f"[extract] {count:,} articles processed", file=sys.stderr)

        # One article per line: collapse internal newlines.
        yield cleaned.replace("\n", " ")


def _iter_articles_iterparse(path: str, min_words: int):
    """Fallback: yield articles using stdlib iterparse (zero dependencies)."""
    ns = "http://www.mediawiki.org/xml/export-0.10/"
    count = 0
    current_ns = None
    current_text = None

    with bz2.open(path, "rb") as fh:
        for event, elem in ET.iterparse(fh, events=("end",)):
            tag = elem.tag.replace(f"{{{ns}}}", "")

            if tag == "ns":
                current_ns = elem.text
            elif tag == "text":
                current_text = elem.text
            elif tag == "page":
                # Process complete page.
                if current_ns == "0" and current_text:
                    text = current_text
                    if not _is_redirect(text) and not _is_disambiguation(text):
                        cleaned = clean_wikitext(text)
                        if len(cleaned.split()) >= min_words:
                            count += 1
                            if count % 10_000 == 0:
                                print(
                                    f"[extract] {count:,} articles processed",
                                    file=sys.stderr,
                                )
                            yield cleaned.replace("\n", " ")

                current_ns = None
                current_text = None
                # Free memory — critical for large dumps.
                elem.clear()


def iter_articles(path: str, min_words: int = 100):
    """Yield cleaned article texts from a bz2 Wikipedia dump."""
    if _HAS_MWXML:
        yield from _iter_articles_mwxml(path, min_words)
    else:
        print(
            "[warn] mwxml not installed, using fallback parser",
            file=sys.stderr,
        )
        yield from _iter_articles_iterparse(path, min_words)


# ---------------------------------------------------------------------------
# CLI.
# ---------------------------------------------------------------------------


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Extract plain-text articles from a Wikipedia XML dump (bz2)."
    )
    parser.add_argument("dump", help="Path to the bz2-compressed XML dump.")
    parser.add_argument(
        "--min-words",
        type=int,
        default=100,
        help="Skip articles shorter than N words (default: 100).",
    )
    args = parser.parse_args()

    count = 0
    for article in iter_articles(args.dump, args.min_words):
        print(article)
        count += 1

    print(f"[done] {count:,} articles extracted", file=sys.stderr)


if __name__ == "__main__":
    main()
