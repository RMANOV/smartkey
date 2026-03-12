# SmartKey

**Rust-powered predictive keyboard engine with word-by-word smart suggestions, CVM streaming adaptation, and cross-platform input method integration.**

All prediction happens in pure Rust. Zero network. Zero cloud. Sub-millisecond predictions. Your typing patterns never leave your machine.

## Platform Support

| Platform | Framework | Status | Crate |
|----------|-----------|--------|-------|
| Linux | IBus | Production | `smartkey-py` (PyO3) |
| Windows | TSF (Text Services Framework) | In Development | `smartkey-win` |
| macOS | Input Method Kit | In Development | `smartkey-mac` |

The Rust core (`smartkey-core`) is platform-agnostic — all prediction logic and key event state machine are shared. Each platform crate is a thin adapter (~200-400 LoC) that bridges OS input events to the shared `InputMethodCore`.

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

Stage 1: N-gram Prefix Match ─────────────── ~140μs (1K corpus)
  └─ Character trie returns: ["hello", "help", "helmet", "helium"]

Stage 2: Markov Context Weighting ─────────── < 130ns
  └─ P(word | prev_words) via Katz backoff: λ₁·P₃ + λ₂·P₂ + λ₃·P₁

Stage 3: CVM Personal Boost ──────────────── < 200ns
  └─ Streaming cardinality estimator boosts YOUR frequent words

Final: α·corpus + β·markov + γ·personal ──── < 200μs (1K corpus)
  └─ "hello" (87%) → ghost text appears inline
```

---

## Architecture

```
┌─────────────────────────────────────────────────────┐
│                  smartkey-core (Apache 2.0)          │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌────────┐ │
│  │ NgramTrie│ │ Markov   │ │ CVM      │ │Ensemble│ │
│  └──────────┘ └──────────┘ └──────────┘ └────────┘ │
│  ┌────────────────────────────────────────────────┐ │
│  │ InputMethodCore                                │ │
│  │  - key event state machine                     │ │
│  │  - ghost text lifecycle                        │ │
│  │  - word commit + context tracking              │ │
│  │  - returns Action list (platform-neutral)      │ │
│  └────────────────────────────────────────────────┘ │
└─────────────┬──────────────┬──────────────┬─────────┘
              │              │              │
    ┌─────────▼───┐  ┌──────▼──────┐  ┌───▼──────────┐
    │ smartkey-py  │  │ smartkey-win│  │ smartkey-mac  │
    │ (PyO3+IBus)  │  │ (TSF/COM)  │  │ (IMK/Swift)  │
    │ GPL 3.0      │  │ GPL 3.0    │  │ GPL 3.0      │
    │ Linux        │  │ Windows    │  │ macOS         │
    └──────────────┘  └────────────┘  └───────────────┘
```

**5 layers, each independently testable:**

| Layer | Tech | Responsibility |
|---|---|---|
| InputMethodCore | Rust | Key dispatch, ghost text lifecycle, context tracking |
| Platform Adapters | PyO3 / COM / C FFI | Translate OS events ↔ Action list |
| Prediction Engine | Rust | N-gram lookup, Markov scoring, ensemble |
| Learning Layer | CVM (JSON persistence) | Personal vocabulary, adaptive memory |
| UI | IBus Panel / TSF / IMK | Ghost text overlay, floating popup |

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

**Planned extension — zero-config language detection:** Per-language CVM counters could track vocabulary cardinality. The counter with the lowest round count (least memory pressure) would be the best language match — enabling automatic mid-sentence language switching.

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

**Alt-hold popup** (planned) — alternatives with confidence scores:

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

Changes require engine restart.

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

| Metric | Target | Actual (Criterion) |
|---|---|---|
| Full predict (1K corpus) | < 10ms | **137 µs** |
| Full predict (10K corpus) | < 10ms | **1.74 ms** |
| Markov trigram scoring | < 1μs | **125 ns** |
| CVM frequency score | < 100ns | **151 ns** |
| Fuzzy search (1K, 1-edit) | < 1ms | **505 µs** |
| Personal profile save/load | < 1ms | **62.5 µs** |
| Ghost text render latency | < 10ms | Platform-dependent |

---

## Test Suite

```
94 tests across 10 modules:

  cvm       22 tests  (streaming counter, decay, snapshots, probabilistic eviction)
  ngram     17 tests  (trie operations, Cyrillic, prefix search, fuzzy matching)
  markov     7 tests  (bigram/trigram probability, Katz backoff, candidate ranking)
  prefix     8 tests  (single/multi-prefix matching, Aho-Corasick batch)
  ensemble  11 tests  (prediction pipeline, personal boost, fuzzy discount, config)
  input     16 tests  (key dispatch, ghost text, kill switch, focus lifecycle)
  corpus     3 tests  (JSON round-trip, load into engine, dropped entries)
  paths      4 tests  (cross-platform config/corpus path resolution)
  mac        4 tests  (keycode mapping, FFI lifecycle, trigram, corpus file)
  integration 5 tests (full pipeline, kill switch, multi-language, personal round-trip)
```

---

## Project Structure

```
smartkey/
├── Cargo.toml                    # Workspace root (4 crates)
├── crates/
│   ├── smartkey-core/            # Rust prediction engine + state machine
│   │   └── src/
│   │       ├── input.rs          # InputMethodCore (cross-platform state machine)
│   │       ├── paths.rs          # Platform-aware config/corpus paths
│   │       ├── cvm.rs            # CVM streaming counter
│   │       ├── ngram.rs          # N-gram trie + prefix search
│   │       ├── markov.rs         # Markov chain + Katz backoff
│   │       ├── prefix.rs         # Aho-Corasick prefix matcher
│   │       └── ensemble.rs       # SmartKeyEngine (ties it all together)
│   ├── smartkey-py/              # PyO3 Python bindings (Linux)
│   ├── smartkey-win/             # Windows TSF adapter
│   │   └── src/
│   │       ├── tsf.rs            # ITfTextInputProcessorEx + ITfKeyEventSink
│   │       ├── display.rs        # Ghost text display attributes
│   │       ├── config.rs         # Windows paths + COM GUIDs
│   │       └── register.rs       # COM/TIP registration helper
│   └── smartkey-mac/             # macOS IMK adapter
│       ├── src/lib.rs            # C FFI exports for Swift
│       └── swift/SmartKeyIME/    # Swift IMKInputController
├── ibus/                         # IBus engine (thin Python adapter)
│   ├── smartkey_engine.py        # IBus.Engine → InputMethodCore bridge
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

### Cross-platform
- [x] **InputMethodCore** — extract platform-agnostic state machine to Rust
- [x] **Windows TSF scaffold** — COM skeleton with ITfTextInputProcessorEx
- [x] **macOS IMK scaffold** — C FFI + Swift IMKInputController
- [ ] Windows TSF full implementation (composition, display attributes)
- [ ] macOS IMK full implementation (marked text, candidate window)
- [ ] Windows installer (MSI)
- [ ] macOS .app bundle + DMG

### Engine
- [x] Benchmark suite (Criterion) with latency targets
- [ ] Memory-mapped binary corpus format (mmap, no JSON parse at startup)
- [ ] GTK4 popup panel for Alt-hold alternatives
- [ ] SQLite persistence for CVM personal vocabulary
- [x] Pre-built English and Bulgarian corpus packages
- [x] Typo correction via Levenshtein distance (fuzzy prefix matching)
- [ ] Mobile port (Android IME via JNI)

---

## License

Dual-licensed:

- **`smartkey-core`** (Rust library) — [Apache 2.0](LICENSE-APACHE). Embed it in your own keyboard, IDE plugin, or WASM app.
- **Everything else** (IBus engine, corpus tools, PyO3 bridge) — [GPL 3.0](LICENSE-GPL). Derivative works must stay open.

See [LICENSE](LICENSE) for details.
