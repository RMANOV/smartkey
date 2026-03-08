#!/usr/bin/env bash
set -euo pipefail

echo "=== SmartKey Corpus Pipeline — Integration Test ==="

# Use a small sample text instead of full Wikipedia
SAMPLE=$(mktemp)
cat > "$SAMPLE" << 'EOF'
The quick brown fox jumps over the lazy dog. The fox is quick and brown.
I'm going to the store. Don't forget the milk. Can't wait to see you.
Здравей свят. Как си днес? Аз съм добре. Благодаря за въпроса.
EOF

cd "$(dirname "$0")"

# Test tokenizer
echo "[1/4] Testing tokenizer..."
python3 -c "
from build_corpus import tokenize_sentences, tokenize_words
text = open('$SAMPLE').read()
sents = tokenize_sentences(text)
assert len(sents) >= 6, f'Expected >=6 sentences, got {len(sents)}'
words = tokenize_words(\"I'm don't can't\")
assert \"i'm\" in words, f'Contraction failed: {words}'
cyr = tokenize_words('Здравей свят')
assert cyr == ['здравей', 'свят'], f'Cyrillic failed: {cyr}'
print(f'  ✓ {len(sents)} sentences, contractions OK, Cyrillic OK')
"

# Test streaming build
echo "[2/4] Testing streaming corpus build..."
OUTPUT=$(mktemp --suffix=.json)
python3 build_corpus.py "$SAMPLE" -o "$OUTPUT" --min-freq 1
python3 -c "
import json
d = json.load(open('$OUTPUT'))
assert len(d['unigrams']) > 0, 'No unigrams'
assert len(d['bigrams']) > 0, 'No bigrams'
print(f'  ✓ {len(d[\"unigrams\"])} unigrams, {len(d[\"bigrams\"])} bigrams')
"

# Test stdin piping
echo "[2b/4] Testing stdin piping..."
PIPE_OUTPUT=$(mktemp --suffix=.json)
cat "$SAMPLE" | python3 build_corpus.py --stdin -o "$PIPE_OUTPUT" --min-freq 1
python3 -c "
import json
d = json.load(open('$PIPE_OUTPUT'))
assert len(d['unigrams']) > 0, 'Stdin mode produced no unigrams'
print(f'  ✓ stdin mode: {len(d[\"unigrams\"])} unigrams')
"

# Test quality metrics
echo "[3/4] Testing corpus stats..."
python3 corpus_stats.py "$OUTPUT"

# Test Rust engine loads corpus
echo "[4/4] Testing Rust engine integration..."
cd "$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
cargo test --quiet 2>&1

echo ""
echo "=== All pipeline tests passed ==="
rm -f "$SAMPLE" "$OUTPUT" "$PIPE_OUTPUT"
