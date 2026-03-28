# SmartKey Algorithmic Advances — Design Spec

**Date**: 2026-03-28
**Status**: Approved
**Goal**: Implement top 3 algorithmic directions identified from 2024-2026 research preprints to improve prediction accuracy while maintaining sub-millisecond latency.

---

## Context

SmartKey v0.5.0 achieves 137µs prediction latency with a 4-signal ensemble (corpus trie + Katz backoff Markov + CVM personal + tech vocab). Six recent papers were analyzed through a triple filter (correctness, optimality, safety):

- **Infini-gram** (ICML 2024, UW/AI2) — ∞-gram LM with longest-suffix backoff, 47% accuracy on 5T tokens
- **Olifant** (Oct 2025, Utrecht) — Memory-based LM with gain-ratio feature ordering on prefix tries, CPU-only
- **Hybrid RNN-Transformer** (Feb 2025, Bamberg) — Recurrent layers replacing attention, 61M params matches 150M+ transformer
- **WSTypist** (Feb 2026, Saarland) — RL typing simulation, validates ghost text UI, 60-70% accuracy sweet spot
- **MobileFineTuner** (Dec 2025, Duke Kunshan) — GPT-2/Gemma fine-tuning on commodity mobile phones
- **NGPU-LM** (May 2025, NVIDIA) — GPU-accelerated n-gram for ASR context biasing

Research files: `/home/rmanov/smartkey/.firecrawl/` (6 paper scrapes + 4 search JSONs)

---

## Phase 1: ∞-gram Extended Context Backoff

**Source**: Infini-gram (arXiv:2401.17377)

**What**: Extend Markov chain from trigram (3-word context) to 7-gram with longest-suffix-match backoff. Instead of fixed λ=[0.6, 0.3, 0.1] for [P₃, P₂, P₁], use the longest available context match and backoff to shorter only when count=0.

**Changes**:
- `markov.rs`: Extend `MarkovChain` to store up to 7-gram contexts (currently stores bigrams + trigrams)
- `markov.rs`: New `score_infini_backoff()` — start from 7-gram, backoff to 6, 5, ... 1-gram until count > 0, then interpolate with next-lower
- `ensemble.rs`: Wire `score_infini_backoff()` into the scoring pipeline
- `corpus.rs`: Extract 4-gram through 7-gram during corpus loading
- `input.rs`: Expand context window tracking from 2 words to 6 words

**Metrics**: +8-15% Top-1 accuracy (estimated). 0µs latency impact. +2-5MB memory.

**Risk**: Zero — pure extension of existing algorithm.

---

## Phase 2: Information Gain Feature Ordering

**Source**: Olifant / Memory-based LMs (arXiv:2510.22317)

**What**: Weight Markov context positions by their information gain (gain ratio) computed from corpus data, instead of fixed positional weights. The paper proves that w₋₁ (immediately preceding word) carries most information, but the relative value of w₋₂ vs w₋₃ etc. depends on the language and domain.

**Changes**:
- `markov.rs`: New `compute_gain_ratios(&self) -> Vec<f64>` — compute gain ratio per context position from stored n-gram data at corpus load time
- `markov.rs`: Modify `score_infini_backoff()` to use gain-ratio weights instead of fixed λ values
- `ensemble.rs`: Call `compute_gain_ratios()` after corpus loading, store result
- Config: Optional override in `smartkey.json` for manual λ tuning

**Metrics**: +5-10% accuracy from smarter use of existing data. 0µs inference impact. +1ms startup.

**Risk**: Zero — precomputed at startup, not on hot path.

---

## Phase 3: Tiny Hybrid Reranker (Future — after eval framework)

**Source**: Hybrid RNN-Transformer (arXiv:2502.00617) + MobileFineTuner (arXiv:2512.08211)

**What**: 1-3M param hybrid model (2 QRNN layers + 1 attention layer) reranks top-10 candidates from ensemble.

**Deferred**: Requires eval framework (A/B testing), model training pipeline, and `tract`/`candle` dependency. Implement after Phase 1+2 are validated with metrics.

**Estimated**: +15-25% accuracy for ambiguous cases. +200-500µs latency. +2-4MB model size.

---

## Verification

1. `cargo test --workspace` — all existing tests pass
2. New unit tests for extended backoff and gain ratio computation
3. `smartkey-eval` scenarios: compare MRR@5 before/after each phase
4. Manual testing via IBus on running system
5. Criterion benchmarks: verify latency stays sub-500µs
