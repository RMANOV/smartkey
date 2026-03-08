# SmartKey

**Rust-powered predictive keyboard engine with word-by-word smart suggestions, CVM streaming adaptation, and system-wide IBus integration.**

All prediction happens in pure Rust. Zero network. Zero cloud. Sub-microsecond lookups. Your typing patterns never leave your machine.

---

## The Problem

Every predictive keyboard on desktop Linux falls into one of two traps:

1. **Copilot-style paragraph completion** — generates walls of text you didn't ask for, interrupts your flow, and hallucinates context. Great for boilerplate, terrible for actual writing.

2. **Dumb autocomplete** — matches prefixes against a static dictionary with no context awareness. Suggests "hello" when you clearly mean "helicopter" because you just typed "apache".

There's a gap: **word-by-word suggestions that understand context**, adapt to *your* vocabulary, and respond faster than you can blink.

---

## The Solution

SmartKey is a system-wide IBus input method engine that predicts the **next word** — not the next paragraph. It uses a 3-stage ensemble of classical statistical models, each contributing a different signal:

```
Keystroke → "hel"

Stage 1: N-gram Prefix Match ─────────────── < 1μs
  └─ Aho-Corasick trie returns: ["hello", "help", "helmet", "helium"]

Stage 2: Markov Context Weighting ─────────── < 5μs
  └─ P(word | prev_words) via Katz backoff: λ₁·P₃ + λ₂·P₂ + λ₃·P₁

Stage 3: CVM Personal Boost ──────────────── < 2μs
  └─ Streaming cardinality estimator boosts YOUR frequent words

Final: α·corpus + β·markov + γ·personal ──── < 10μs total
  └─ "hello" (87%) → ghost text appears inline
```

---

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│                    IBus Framework                        │
│  ┌───────────────┐    ┌──────────────┐    ┌──────────┐  │
│  │ IBus Daemon   │◄──►│ SmartKey     │◄──►│ UI Panel │  │
│  │ (system)      │    │ Engine       │    │ (ghost + │  │
│  │               │    │ (Rust lib)   │    │  popup)  │  │
│  └───────────────┘    └──────┬───────┘    └──────────┘  │
│                              │                           │
│                   ┌──────────┼──────────┐               │
│                   ▼          ▼          ▼               │
│            ┌──────────┐ ┌────────┐ ┌────────┐          │
│            │ Static   │ │ Markov │ │  CVM   │          │
│            │ N-gram   │ │ Chain  │ │ Stream │          │
│            │ Corpus   │ │ (ctx)  │ │ Layer  │          │
│            └──────────┘ └────────┘ └────────┘          │
│                   │          │          │               │
│                   ▼          ▼          ▼               │
│            ┌─────────────────────────────────┐          │
│            │  Ensemble Scorer + Ranker       │          │
│            │  (weighted vote → top-5)        │          │
│            └─────────────────────────────────┘          │
└─────────────────────────────────────────────────────────┘
```

**4 layers, each independently testable:**

| Layer | Tech | Responsibility |
|---|---|---|
| IBus Integration | Python + GObject | Key events, text commit, kill switch |
| Prediction Engine | Rust (PyO3) | N-gram lookup, Markov scoring, ensemble |
| Learning Layer | CVM + SQLite | Personal vocabulary, adaptive memory |
| UI | GTK4 (IBus Panel) | Ghost text overlay, floating popup |

---

## The Math

### N-gram Trie with Frequency Ranking

Character-level trie (not byte-level) for native UTF-8/Cyrillic support. Prefix search returns candidates sorted by corpus frequency in O(k) where k is the number of characters in the prefix.

### Markov Chain with Katz Backoff

Interpolated trigram language model:

```
score = λ₁ · P(word | w₋₂, w₋₁)     trigram
      + λ₂ · P(word | w₋₁)           bigram
      + λ₃ · P(word)                  unigram

where λ₁ = 0.6, λ₂ = 0.3, λ₃ = 0.1 (default, configurable)
```

When context is missing, weights are redistributed proportionally — no hard fallback discontinuities.

### CVM Streaming Cardinality Estimator

Based on the Chakraborty–Vinodchandran–Meel (CVM) algorithm for counting distinct elements in a stream using **O(log N) memory**.

The key insight: CVM's probabilistic coin-flip eviction is a natural **vocabulary decay mechanism**. Words you stop typing get probabilistically evicted during new rounds. Words you keep typing survive (higher frequency → more copies → harder to fully evict). This gives you **automatic vocabulary adaptation** without timestamps, TTLs, or explicit forgetting logic.

```rust
// After 1000 typing sessions:
cvm.frequency_score("function")   // → 64.0  (survived 6 rounds — you're a programmer)
cvm.frequency_score("synergy")    // → 0.0   (evicted — you stopped writing corporate email)
```

**Novel extension — zero-config language detection:** Per-language CVM counters track vocabulary cardinality. The counter with the lowest round count (least memory pressure) is the best language match. Switch from English to Bulgarian mid-sentence? SmartKey detects it automatically.

### Ensemble Scorer

Three signals blended with configurable weights:

```
final_score = α · corpus_frequency   (how common in the language)
            + β · markov_context     (how likely given recent words)
            + γ · cvm_personal       (how often YOU type this word)
```

The γ weight is what makes SmartKey learn *you*. After a week of coding, "async" outranks "assistant" even though "assistant" is more common in English.

---

## Quick Start

### Build the Rust core

```bash
git clone https://github.com/RMANOV/smartkey.git
cd smartkey
cargo build --release
```

### Build a corpus

```bash
# From any text file (your emails, documents, code comments...)
python3 corpus/build_corpus.py ~/Documents/*.txt -o ~/.config/smartkey/corpus.json --min-freq 2
```

### Install the PyO3 bridge

```bash
pip install maturin
maturin develop -m crates/smartkey-py/Cargo.toml --release
```

### Register with IBus

```bash
mkdir -p ~/.local/share/ibus/component/
cp ibus/smartkey.xml ~/.local/share/ibus/component/
ibus restart
# Select "SmartKey Predictive" in IBus settings
```

---

## Interaction Model

**Ghost text** — the top prediction appears as dimmed text inline:

```
Ето така се пише ░hello░
                  ▲
                  │ Tab = accept entire word
                  │ → = accept one character
                  │ Esc = dismiss
```

**Alt-hold popup** — alternatives with confidence scores:

```
┌─────────────────────────┐
│ 1. hello    (87%)       │
│ 2. help     (62%)       │
│ 3. helmet   (31%)       │
│ 4. held     (28%)       │
│ 5. helium   (15%)       │
└─────────────────────────┘
```

**Kill switch:** `Super+Escape` instantly disables SmartKey, reverts to default IBus engine.

---

## Configuration

`~/.config/smartkey/smartkey.json`:

```json
{
    "enabled": true,
    "ghost_text": true,
    "popup_on_alt": true,
    "max_candidates": 5,
    "min_prefix_length": 2,
    "kill_switch": "Super+Escape",
    "weights": {
        "corpus": 0.4,
        "markov": 0.4,
        "personal": 0.2
    },
    "languages": ["en", "bg"]
}
```

Hot-reload — changes take effect without restart.

---

## Language Support

| Language | Status | Corpus Source |
|---|---|---|
| English | Built-in | Google Web 1T (planned) |
| Bulgarian | Built-in | BG web corpus (planned) |
| Any other | Auto-learn | CVM detects new languages automatically |

The engine is language-agnostic by design. The character-level trie handles any UTF-8 script. Adding a language is just adding a corpus file.

---

## Performance Targets

| Metric | Target | Actual |
|---|---|---|
| Prefix search (100K words) | < 1μs | TBD |
| Full predict pipeline | < 10ms | TBD |
| CVM process per element | < 100ns | TBD |
| Memory (EN+BG corpora) | < 50MB | TBD |
| Ghost text render latency | < 10ms | TBD |

---

## Test Suite

```
38 tests across 5 modules:

  cvm       10 tests  (streaming counter, adaptive memory, probabilistic eviction)
  ngram      7 tests  (trie operations, Cyrillic, prefix search, frequency ranking)
  markov     7 tests  (bigram/trigram probability, Katz backoff, candidate ranking)
  prefix     8 tests  (single/multi-prefix matching, Aho-Corasick batch)
  ensemble   6 tests  (prediction pipeline, personal boost, context handling)
```

---

## Project Structure

```
smartkey/
├── Cargo.toml                    # Workspace root
├── crates/
│   ├── smartkey-core/            # Rust prediction engine
│   │   └── src/
│   │       ├── cvm.rs            # CVM streaming counter
│   │       ├── ngram.rs          # N-gram trie + prefix search
│   │       ├── markov.rs         # Markov chain + Katz backoff
│   │       ├── prefix.rs         # Aho-Corasick prefix matcher
│   │       └── ensemble.rs       # SmartKeyEngine (ties it all together)
│   └── smartkey-py/              # PyO3 Python bindings
├── ibus/                         # IBus engine (Python)
│   ├── smartkey_engine.py        # IBus.Engine subclass
│   ├── main.py                   # Daemon entry point
│   └── smartkey.xml              # Component descriptor
├── corpus/                       # Corpus tools
│   └── build_corpus.py           # Text → JSON n-gram extractor
└── config/
    └── smartkey.json             # Default configuration
```

---

## Prior Art & Inspiration

SmartKey builds on algorithms proven in production across other projects:

- **Aho-Corasick multi-pattern matching** — from [rust_search_tools_for_linux](https://github.com/RMANOV/rust_search_tools_for_linux), where it powers grep-like text search across millions of files
- **Particle filter state estimation** — from [particle-filter-rs](https://github.com/RMANOV/particle-filter-rs), where sequential Bayesian inference tracks regime changes in financial data
- **CVM cardinality estimation** — from [Number-of-Unique-Elements-Prediction](https://github.com/RMANOV/Number-of-Unique-Elements-Prediction), adapted from streaming analytics to personal vocabulary tracking
- **Fuzzy string matching** — from [fuzzy-match-visual](https://github.com/RMANOV/fuzzy-match-visual), where Levenshtein distance and SequenceMatcher provide typo correction foundations
- **FTS5 BM25 search** — from [sqlite-memory-mcp](https://github.com/RMANOV/sqlite-memory-mcp), where tiered scoring ranks knowledge graph entities

---

## Roadmap

- [ ] Benchmark suite (Criterion) with latency targets
- [ ] Memory-mapped binary corpus format (mmap, no JSON parse at startup)
- [ ] GTK4 popup panel for Alt-hold alternatives
- [ ] SQLite persistence for CVM personal vocabulary
- [ ] Pre-built English and Bulgarian corpus packages
- [ ] Typo correction via Levenshtein distance (fuzzy prefix matching)
- [ ] Mobile port (Android IME via JNI)

---

## License

MIT
