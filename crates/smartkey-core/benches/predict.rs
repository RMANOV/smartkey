//! Criterion benchmark suite for smartkey-core prediction latency.
//!
//! Covers the full ensemble `predict()` path as well as isolated measurements
//! for each scoring component (trie, Markov, CVM).

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use smartkey_core::cvm::{CvmCounter, CvmSnapshot};
use smartkey_core::markov::MarkovChain;
use smartkey_core::ngram::NgramTrie;
use smartkey_core::SmartKeyEngine;

// ---------------------------------------------------------------------------
// Corpus helpers
// ---------------------------------------------------------------------------

/// Build a `SmartKeyEngine` loaded with `corpus_size` synthetic words and
/// bigrams derived from them. The corpus uses the pattern `word_NNNN` so
/// that prefix searches with `"wor"` or `"word_0"` hit many candidates.
fn build_engine(corpus_size: usize) -> SmartKeyEngine {
    let mut engine = SmartKeyEngine::new();
    for i in 0..corpus_size {
        let word = format!("word_{i:04}");
        engine.load_word(&word, (corpus_size - i) as u32);
    }
    // Train bigrams on a sliding window over the first 500 words.
    for i in 0..corpus_size.min(500) {
        let w1 = format!("word_{i:04}");
        let w2 = format!("word_{:04}", (i + 1) % corpus_size);
        engine.load_bigram(&w1, &w2, ((corpus_size - i) / 10).max(1) as u32);
    }
    // Train a few trigrams for richer Markov context.
    for i in 0..corpus_size.min(200) {
        let w1 = format!("word_{i:04}");
        let w2 = format!("word_{:04}", (i + 1) % corpus_size);
        let w3 = format!("word_{:04}", (i + 2) % corpus_size);
        engine.load_trigram(&w1, &w2, &w3, ((corpus_size - i) / 20).max(1) as u32);
    }
    engine
}

/// Build a standalone `NgramTrie` with `n` words.
fn build_trie(n: usize) -> NgramTrie {
    let mut trie = NgramTrie::new();
    for i in 0..n {
        trie.insert(&format!("word_{i:04}"), (n - i) as u32);
    }
    trie
}

/// Build a standalone `MarkovChain` trained on `n` bigrams and `n/2` trigrams.
fn build_markov(n: usize) -> MarkovChain {
    let mut chain = MarkovChain::new();
    for i in 0..n {
        let ctx = format!("word_{i:04}");
        let nxt = format!("word_{:04}", (i + 1) % n);
        chain.train_bigram(&ctx, &nxt, ((n - i) / 10).max(1) as u32);
    }
    for i in 0..n / 2 {
        let w1 = format!("word_{i:04}");
        let w2 = format!("word_{:04}", (i + 1) % n);
        let w3 = format!("word_{:04}", (i + 2) % n);
        chain.train_trigram(&w1, &w2, &w3, ((n - i) / 20).max(1) as u32);
    }
    chain
}

// ---------------------------------------------------------------------------
// Benchmark: full predict() by prefix length
// ---------------------------------------------------------------------------

fn bench_predict_by_prefix(c: &mut Criterion) {
    let engine = build_engine(1000);
    let context: Vec<&str> = vec!["word_0010", "word_0011"];

    let mut group = c.benchmark_group("predict_latency_by_prefix_length");
    for prefix_len in [1, 2, 3, 5, 8] {
        // Construct a prefix of the requested length from "word_0005".
        let full = "word_0005";
        let prefix: String = full.chars().take(prefix_len).collect();
        group.bench_with_input(
            BenchmarkId::new("prefix_len", prefix_len),
            &prefix,
            |b, pfx| {
                b.iter(|| {
                    black_box(engine.predict(black_box(pfx), black_box(&context), 5, None));
                });
            },
        );
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark: full predict() by corpus size
// ---------------------------------------------------------------------------

fn bench_predict_by_corpus(c: &mut Criterion) {
    let mut group = c.benchmark_group("predict_latency_by_corpus_size");
    for &size in &[100, 1000, 10_000] {
        let engine = build_engine(size);
        let context: Vec<&str> = vec!["word_0001", "word_0002"];
        group.bench_with_input(BenchmarkId::new("corpus_size", size), &size, |b, _| {
            b.iter(|| {
                black_box(engine.predict(black_box("wor"), black_box(&context), 5, None));
            });
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark: full predict() by candidate limit
// ---------------------------------------------------------------------------

fn bench_predict_by_limit(c: &mut Criterion) {
    let engine = build_engine(1000);
    let context: Vec<&str> = vec!["word_0010"];

    let mut group = c.benchmark_group("predict_latency_by_candidate_count");
    for &limit in &[1, 3, 5, 10] {
        group.bench_with_input(BenchmarkId::new("limit", limit), &limit, |b, &lim| {
            b.iter(|| {
                black_box(engine.predict(black_box("word_0"), black_box(&context), lim, None));
            });
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark: raw trie prefix search (no ensemble overhead)
// ---------------------------------------------------------------------------

fn bench_trie_prefix_search(c: &mut Criterion) {
    let trie = build_trie(5000);

    let mut group = c.benchmark_group("trie_prefix_search");
    for prefix_len in [1, 3, 5] {
        let prefix: String = "word_0005".chars().take(prefix_len).collect();
        group.bench_with_input(
            BenchmarkId::new("prefix_len", prefix_len),
            &prefix,
            |b, pfx| {
                b.iter(|| {
                    black_box(trie.prefix_search(black_box(pfx), 10));
                });
            },
        );
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark: Markov score_with_backoff
// ---------------------------------------------------------------------------

fn bench_markov_score(c: &mut Criterion) {
    let chain = build_markov(1000);

    let mut group = c.benchmark_group("markov_score_with_backoff");

    // No context (unigram only).
    group.bench_function("no_context", |b| {
        b.iter(|| {
            black_box(chain.score_with_backoff(
                black_box("word_0042"),
                black_box(None),
                black_box(None),
            ));
        });
    });

    // Bigram context.
    group.bench_function("bigram_context", |b| {
        b.iter(|| {
            black_box(chain.score_with_backoff(
                black_box("word_0042"),
                black_box(Some("word_0041")),
                black_box(None),
            ));
        });
    });

    // Full trigram context.
    group.bench_function("trigram_context", |b| {
        b.iter(|| {
            black_box(chain.score_with_backoff(
                black_box("word_0042"),
                black_box(Some("word_0041")),
                black_box(Some("word_0040")),
            ));
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark: CVM process + frequency_score cycle
// ---------------------------------------------------------------------------

fn bench_cvm_process_and_score(c: &mut Criterion) {
    let mut group = c.benchmark_group("cvm_process_and_score");

    // Insert-then-score for a fresh element.
    group.bench_function("fresh_insert", |b| {
        b.iter_batched(
            || CvmCounter::new(256, 1024),
            |mut cvm| {
                cvm.process(black_box("word_new"));
                black_box(cvm.frequency_score("word_new"));
            },
            criterion::BatchSize::SmallInput,
        );
    });

    // Score against a pre-populated CVM.
    group.bench_function("score_existing", |b| {
        let mut cvm = CvmCounter::new(256, 1024);
        for i in 0..200 {
            cvm.process(&format!("word_{i:04}"));
        }
        b.iter(|| {
            black_box(cvm.frequency_score(black_box("word_0050")));
        });
    });

    // Process cycle on a populated counter (triggers potential round advancement).
    group.bench_function("process_on_populated", |b| {
        b.iter_batched(
            || {
                let mut cvm = CvmCounter::new(256, 1024);
                for i in 0..200 {
                    cvm.process(&format!("word_{i:04}"));
                }
                cvm
            },
            |mut cvm| {
                cvm.process(black_box("word_0050"));
                black_box(cvm.frequency_score("word_0050"));
            },
            criterion::BatchSize::SmallInput,
        );
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark: fuzzy search by corpus size × edit distance × prefix length
// ---------------------------------------------------------------------------

fn bench_fuzzy_search(c: &mut Criterion) {
    let mut group = c.benchmark_group("fuzzy_search");
    for &corpus_size in &[1_000, 10_000] {
        let trie = build_trie(corpus_size);
        for &max_edits in &[1u8, 2] {
            for &prefix_len in &[3usize, 5] {
                let prefix: String = "word_0005".chars().take(prefix_len).collect();
                let id = format!("corpus_{corpus_size}/edits_{max_edits}/prefix_{prefix_len}");
                group.bench_function(&id, |b| {
                    b.iter(|| {
                        black_box(trie.fuzzy_search(black_box(&prefix), max_edits, 10));
                    });
                });
            }
        }
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark: CvmSnapshot serialization round-trip
// ---------------------------------------------------------------------------

fn bench_personal_serialization(c: &mut Criterion) {
    let mut group = c.benchmark_group("personal_serialization");

    group.bench_function("cvm_snapshot_json_round_trip", |b| {
        b.iter_batched(
            || {
                let mut cvm = CvmCounter::new(256, 1024);
                for i in 0..200 {
                    cvm.process(&format!("word_{i:04}"));
                }
                cvm.to_snapshot()
            },
            |snap| {
                let json = serde_json::to_string(black_box(&snap)).unwrap();
                let restored: CvmSnapshot = serde_json::from_str(black_box(&json)).unwrap();
                black_box(restored);
            },
            criterion::BatchSize::SmallInput,
        );
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Criterion harness
// ---------------------------------------------------------------------------

criterion_group!(
    benches,
    bench_predict_by_prefix,
    bench_predict_by_corpus,
    bench_predict_by_limit,
    bench_trie_prefix_search,
    bench_markov_score,
    bench_cvm_process_and_score,
    bench_fuzzy_search,
    bench_personal_serialization,
);
criterion_main!(benches);
