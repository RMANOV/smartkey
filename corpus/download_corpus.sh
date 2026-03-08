#!/usr/bin/env bash
# Downloads Wikipedia article dumps for specified languages.
# Usage: ./download_corpus.sh [--lang en] [--lang bg] [--output-dir ./raw]

set -euo pipefail

MIRROR="https://dumps.wikimedia.org"
OUTPUT_DIR="./raw"
LANGS=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --lang) LANGS+=("$2"); shift 2 ;;
        --output-dir) OUTPUT_DIR="$2"; shift 2 ;;
        *) echo "Unknown: $1"; exit 1 ;;
    esac
done

[[ ${#LANGS[@]} -eq 0 ]] && LANGS=("en" "bg")

mkdir -p "$OUTPUT_DIR"

for lang in "${LANGS[@]}"; do
    DUMP_URL="${MIRROR}/${lang}wiki/latest/${lang}wiki-latest-pages-articles.xml.bz2"
    DEST="${OUTPUT_DIR}/${lang}wiki-latest-pages-articles.xml.bz2"

    if [[ -f "$DEST" ]]; then
        echo "[skip] $DEST already exists"
    else
        echo "[download] $lang → $DEST"
        curl -L --retry 3 --continue-at - -o "$DEST" "$DUMP_URL"
    fi
done

echo "[done] Raw dumps in $OUTPUT_DIR"
