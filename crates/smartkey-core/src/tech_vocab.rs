// Tech vocabulary layer — prefix search over a hardcoded set of tech terms.
//
// Used as a fourth scoring signal in the ensemble (δ weight), active only
// when the regime detector reports FastCoding. Keeps its own term table with
// relative frequencies so it can be queried via prefix search independently
// of the corpus trie (which may not include technical identifiers).

use std::collections::HashMap;

/// Dedicated tech vocabulary with prefix search and frequency-based scoring.
///
/// The `score()` method returns candidates sorted by a blend of frequency
/// and prefix-match strength, normalised to `[0.0, 1.0]`.
pub struct TechVocab {
    /// word → relative frequency (higher = more common tech term).
    terms: HashMap<String, u32>,
    /// Cached maximum frequency for normalisation.
    max_freq: u32,
}

impl TechVocab {
    /// Create and populate the tech vocabulary with the top 300+ terms.
    pub fn new() -> Self {
        let raw = Self::build_terms();
        let max_freq = raw.values().copied().max().unwrap_or(1);
        Self {
            terms: raw,
            max_freq,
        }
    }

    /// Prefix search: return up to `limit` candidates matching `prefix`,
    /// sorted by score descending. Scores are in `[0.0, 1.0]`.
    ///
    /// Returns `Vec<(word, score)>`.
    pub fn score(&self, prefix: &str, limit: usize) -> Vec<(String, f64)> {
        if limit == 0 || prefix.is_empty() && self.terms.is_empty() {
            return Vec::new();
        }

        let prefix_lower = prefix.to_lowercase();
        let max_freq = self.max_freq.max(1) as f64;

        let mut candidates: Vec<(String, f64)> = self
            .terms
            .iter()
            .filter(|(word, _)| {
                if prefix.is_empty() {
                    true
                } else {
                    word.starts_with(&prefix_lower)
                }
            })
            .map(|(word, &freq)| {
                let freq_score = freq as f64 / max_freq;
                // Boost exact-match and very-short-prefix candidates.
                let length_bonus = if word == &prefix_lower {
                    0.3 // Exact match
                } else if word.len() - prefix_lower.len() <= 3 {
                    0.1 // Almost complete
                } else {
                    0.0
                };
                (word.clone(), (freq_score + length_bonus).min(1.0))
            })
            .collect();

        candidates.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        candidates.truncate(limit);
        candidates
    }

    /// Returns true if the word (case-insensitive) is in the tech vocabulary.
    pub fn contains(&self, word: &str) -> bool {
        self.terms.contains_key(&word.to_lowercase())
    }

    // ── Vocabulary data ─────────────────────────────────────────────────

    fn build_terms() -> HashMap<String, u32> {
        // Format: (term, relative_frequency)
        // Frequency scale: 1000 = most common, lower = less common.
        // Sources: GitHub top-N identifier lists, Stack Overflow tag frequencies,
        // npm download counts, crates.io download counts, TIOBE index.
        let entries: &[(&str, u32)] = &[
            // ── Infrastructure & cloud ──────────────────────────────────
            ("docker", 980),
            ("kubernetes", 940),
            ("terraform", 900),
            ("ansible", 860),
            ("nginx", 920),
            ("redis", 930),
            ("postgres", 910),
            ("postgresql", 870),
            ("mysql", 890),
            ("mongodb", 880),
            ("elasticsearch", 840),
            ("grafana", 820),
            ("prometheus", 800),
            ("jenkins", 810),
            ("gitlab", 830),
            ("github", 970),
            ("bitbucket", 760),
            ("aws", 950),
            ("gcp", 820),
            ("azure", 850),
            ("linux", 960),
            ("ubuntu", 900),
            ("debian", 850),
            ("centos", 800),
            ("fedora", 780),
            ("homebrew", 820),
            ("systemd", 790),
            ("bash", 930),
            ("zsh", 870),
            ("ssh", 940),
            ("curl", 920),
            ("wget", 880),
            ("grep", 910),
            ("awk", 860),
            ("sed", 870),
            ("vim", 890),
            ("neovim", 840),
            ("emacs", 810),
            // ── Languages & runtimes ────────────────────────────────────
            ("python", 990),
            ("javascript", 985),
            ("typescript", 970),
            ("rust", 940),
            ("golang", 920),
            ("java", 960),
            ("kotlin", 890),
            ("swift", 880),
            ("ruby", 870),
            ("php", 860),
            ("csharp", 850),
            ("fsharp", 780),
            ("scala", 820),
            ("haskell", 800),
            ("elixir", 790),
            ("erlang", 770),
            ("clojure", 760),
            ("ocaml", 750),
            ("lua", 810),
            ("perl", 820),
            ("r", 870),
            ("julia", 790),
            ("zig", 760),
            ("nim", 740),
            ("v", 720),
            ("dart", 840),
            ("flutter", 860),
            ("wasm", 800),
            ("webassembly", 780),
            ("node", 950),
            ("nodejs", 940),
            ("deno", 830),
            ("bun", 810),
            // ── Frontend ────────────────────────────────────────────────
            ("react", 980),
            ("vue", 950),
            ("angular", 920),
            ("svelte", 880),
            ("nextjs", 900),
            ("nuxt", 860),
            ("remix", 820),
            ("vite", 870),
            ("webpack", 890),
            ("rollup", 830),
            ("esbuild", 800),
            ("parcel", 790),
            ("tailwind", 880),
            ("css", 950),
            ("scss", 880),
            ("sass", 870),
            ("html", 960),
            ("jsx", 930),
            ("tsx", 900),
            ("graphql", 880),
            ("apollo", 850),
            ("redux", 870),
            ("mobx", 790),
            ("zustand", 800),
            ("jotai", 770),
            // ── Backend & APIs ───────────────────────────────────────────
            ("fastapi", 880),
            ("django", 890),
            ("flask", 870),
            ("express", 900),
            ("nestjs", 840),
            ("spring", 870),
            ("rails", 830),
            ("laravel", 820),
            ("gin", 800),
            ("fiber", 790),
            ("actix", 810),
            ("axum", 800),
            ("rocket", 790),
            ("grpc", 840),
            ("rest", 920),
            ("json", 960),
            ("yaml", 930),
            ("toml", 870),
            ("protobuf", 820),
            ("avro", 760),
            ("kafka", 870),
            ("rabbitmq", 830),
            ("nats", 780),
            ("celery", 800),
            // ── Data & ML ────────────────────────────────────────────────
            ("numpy", 920),
            ("pandas", 910),
            ("sklearn", 880),
            ("scikit", 860),
            ("tensorflow", 880),
            ("pytorch", 890),
            ("keras", 850),
            ("huggingface", 840),
            ("transformers", 830),
            ("langchain", 810),
            ("openai", 830),
            ("matplotlib", 870),
            ("seaborn", 820),
            ("polars", 810),
            ("spark", 840),
            ("hadoop", 800),
            ("dask", 790),
            ("airflow", 820),
            ("dbt", 810),
            ("snowflake", 800),
            ("databricks", 790),
            ("mlflow", 790),
            // ── Package managers & tools ─────────────────────────────────
            ("npm", 960),
            ("yarn", 930),
            ("pnpm", 880),
            ("pip", 940),
            ("cargo", 900),
            ("poetry", 870),
            ("pipenv", 830),
            ("conda", 850),
            ("gradle", 840),
            ("maven", 830),
            ("cmake", 820),
            ("make", 900),
            ("bazel", 790),
            ("nix", 780),
            ("brew", 840),
            ("apt", 870),
            ("yum", 820),
            ("dnf", 800),
            // ── Databases & query ────────────────────────────────────────
            ("sqlite", 910),
            ("cockroachdb", 760),
            ("cassandra", 790),
            ("dynamodb", 800),
            ("firestore", 790),
            ("supabase", 820),
            ("prisma", 840),
            ("diesel", 800),
            ("sqlalchemy", 850),
            ("sequelize", 810),
            ("typeorm", 800),
            ("drizzle", 780),
            ("sql", 960),
            ("nosql", 870),
            ("orm", 880),
            // ── Version control ──────────────────────────────────────────
            ("git", 980),
            ("commit", 960),
            ("branch", 950),
            ("merge", 940),
            ("rebase", 890),
            ("cherry", 830),
            ("stash", 840),
            ("diff", 900),
            ("blame", 810),
            ("clone", 890),
            ("push", 950),
            ("pull", 950),
            ("fetch", 920),
            ("checkout", 910),
            ("origin", 900),
            ("upstream", 860),
            ("remote", 880),
            ("HEAD", 880),
            ("main", 940),
            ("master", 920),
            // ── Testing ──────────────────────────────────────────────────
            ("jest", 900),
            ("vitest", 850),
            ("mocha", 830),
            ("chai", 800),
            ("pytest", 890),
            ("unittest", 860),
            ("cypress", 850),
            ("playwright", 840),
            ("selenium", 820),
            ("mock", 900),
            ("stub", 870),
            ("spy", 830),
            ("fixture", 810),
            ("assert", 920),
            ("expect", 910),
            ("describe", 900),
            ("beforeEach", 860),
            ("afterEach", 850),
            ("beforeAll", 840),
            ("afterAll", 830),
            // ── Common programming keywords (cross-language) ─────────────
            ("function", 990),
            ("class", 990),
            ("struct", 960),
            ("impl", 940),
            ("interface", 980),
            ("abstract", 940),
            ("override", 920),
            ("static", 970),
            ("const", 990),
            ("let", 990),
            ("var", 970),
            ("mut", 950),
            ("pub", 950),
            ("async", 990),
            ("await", 990),
            ("yield", 900),
            ("return", 990),
            ("import", 990),
            ("export", 990),
            ("require", 970),
            ("module", 980),
            ("package", 970),
            ("namespace", 930),
            ("type", 990),
            ("enum", 960),
            ("trait", 940),
            ("extend", 940),
            ("implements", 930),
            ("constructor", 970),
            ("super", 950),
            ("this", 990),
            ("self", 980),
            ("new", 990),
            ("delete", 960),
            ("null", 990),
            ("undefined", 980),
            ("none", 970),
            ("true", 990),
            ("false", 990),
            ("if", 999),
            ("else", 999),
            ("for", 999),
            ("while", 990),
            ("match", 970),
            ("switch", 970),
            ("case", 970),
            ("break", 970),
            ("continue", 960),
            ("throw", 950),
            ("try", 960),
            ("catch", 950),
            ("finally", 940),
            ("raise", 940),
            ("except", 940),
            ("with", 970),
            ("as", 990),
            ("in", 999),
            ("of", 999),
            ("from", 990),
            ("where", 970),
            ("when", 960),
            ("then", 960),
            ("do", 970),
            ("fn", 960),
            ("def", 970),
            ("val", 960),
            ("ref", 950),
            ("ptr", 900),
            ("box", 920),
            ("arc", 900),
            ("rc", 890),
            ("option", 960),
            ("result", 960),
            ("ok", 950),
            ("err", 940),
            ("some", 950),
            ("vec", 940),
            ("map", 960),
            ("set", 960),
            ("list", 960),
            ("dict", 950),
            ("array", 970),
            ("string", 990),
            ("str", 970),
            ("int", 990),
            ("float", 970),
            ("bool", 970),
            ("char", 960),
            ("byte", 950),
            ("usize", 930),
            ("isize", 910),
            ("u8", 900),
            ("u16", 890),
            ("u32", 900),
            ("u64", 890),
            ("i32", 900),
            ("i64", 900),
            ("f32", 890),
            ("f64", 900),
            // ── Async / concurrency ──────────────────────────────────────
            ("future", 940),
            ("promise", 940),
            ("thread", 950),
            ("mutex", 910),
            ("channel", 910),
            ("sender", 880),
            ("receiver", 880),
            ("spawn", 900),
            ("tokio", 880),
            ("rayon", 850),
            ("arc", 900),
            ("atomic", 870),
            ("lock", 880),
            ("rwlock", 830),
            ("semaphore", 800),
            ("condvar", 790),
            // ── Misc common patterns ─────────────────────────────────────
            ("config", 970),
            ("env", 950),
            ("debug", 940),
            ("error", 980),
            ("warn", 920),
            ("info", 940),
            ("trace", 900),
            ("log", 960),
            ("logger", 930),
            ("handler", 940),
            ("middleware", 900),
            ("router", 900),
            ("endpoint", 880),
            ("request", 950),
            ("response", 950),
            ("params", 940),
            ("query", 930),
            ("body", 930),
            ("header", 900),
            ("auth", 910),
            ("token", 920),
            ("jwt", 880),
            ("oauth", 860),
            ("session", 890),
            ("cookie", 870),
            ("cache", 910),
            ("queue", 890),
            ("worker", 880),
            ("job", 870),
            ("task", 900),
            ("event", 930),
            ("listener", 890),
            ("callback", 900),
            ("closure", 880),
            ("lambda", 890),
            ("iterator", 900),
            ("generator", 870),
            ("stream", 900),
            ("buffer", 890),
            ("socket", 880),
            ("http", 950),
            ("https", 940),
            ("tcp", 890),
            ("udp", 860),
            ("url", 930),
            ("uri", 890),
            ("path", 960),
            ("file", 960),
            ("dir", 920),
            ("fs", 900),
            ("io", 910),
            ("read", 970),
            ("write", 970),
            ("open", 960),
            ("close", 950),
            ("flush", 900),
            ("seek", 870),
            ("parse", 930),
            ("serialize", 890),
            ("deserialize", 880),
            ("encode", 890),
            ("decode", 890),
            ("hash", 900),
            ("sign", 870),
            ("verify", 870),
            ("encrypt", 860),
            ("decrypt", 860),
            ("compress", 840),
            ("decompress", 830),
        ];

        let mut map = HashMap::with_capacity(entries.len());
        for &(term, freq) in entries {
            map.insert(term.to_lowercase(), freq);
        }
        map
    }
}

impl Default for TechVocab {
    fn default() -> Self {
        Self::new()
    }
}

// ======================================================================
// Tests
// ======================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_contains_known_terms() {
        let tv = TechVocab::new();
        assert!(tv.contains("docker"));
        assert!(tv.contains("async"));
        assert!(tv.contains("sqlite"));
        assert!(tv.contains("kubernetes"));
        assert!(tv.contains("TypeScript")); // case-insensitive
    }

    #[test]
    fn test_contains_unknown_term() {
        let tv = TechVocab::new();
        assert!(!tv.contains("supercalifragilistic"));
        assert!(!tv.contains("zyzzyva"));
    }

    #[test]
    fn test_score_returns_prefix_matches() {
        let tv = TechVocab::new();
        let results = tv.score("doc", 5);
        assert!(!results.is_empty(), "should find matches for 'doc'");
        assert!(
            results.iter().any(|(w, _)| w == "docker"),
            "'docker' should appear for prefix 'doc'"
        );
        // All results must start with the prefix.
        for (word, _score) in &results {
            assert!(
                word.starts_with("doc"),
                "result '{word}' does not start with 'doc'"
            );
        }
    }

    #[test]
    fn test_score_sorted_by_score_descending() {
        let tv = TechVocab::new();
        let results = tv.score("as", 10);
        for w in results.windows(2) {
            assert!(
                w[0].1 >= w[1].1,
                "results not sorted: {} ({}) before {} ({})",
                w[0].0,
                w[0].1,
                w[1].0,
                w[1].1
            );
        }
    }

    #[test]
    fn test_score_limit_respected() {
        let tv = TechVocab::new();
        let results = tv.score("a", 3);
        assert!(
            results.len() <= 3,
            "limit not respected: got {}",
            results.len()
        );
    }

    #[test]
    fn test_score_empty_prefix_returns_everything_up_to_limit() {
        let tv = TechVocab::new();
        let results = tv.score("", 10);
        assert_eq!(
            results.len(),
            10,
            "should return exactly limit entries for empty prefix"
        );
    }

    #[test]
    fn test_score_unknown_prefix_returns_empty() {
        let tv = TechVocab::new();
        let results = tv.score("zzzzzz", 5);
        assert!(results.is_empty(), "no tech terms start with 'zzzzzz'");
    }

    #[test]
    fn test_score_values_in_range() {
        let tv = TechVocab::new();
        let results = tv.score("f", 20);
        for (word, score) in &results {
            assert!(
                *score >= 0.0 && *score <= 1.0,
                "score out of range for '{word}': {score}"
            );
        }
    }

    #[test]
    fn test_vocab_has_minimum_size() {
        let tv = TechVocab::new();
        assert!(
            tv.terms.len() >= 200,
            "tech vocab should have at least 200 entries, got {}",
            tv.terms.len()
        );
    }

    #[test]
    fn test_case_insensitive_prefix_match() {
        let tv = TechVocab::new();
        // Prefix in uppercase should still find lowercase entries.
        let lower = tv.score("doc", 5);
        let upper = tv.score("DOC", 5);
        assert_eq!(
            lower.len(),
            upper.len(),
            "prefix search should be case-insensitive"
        );
    }
}
