//! Integration tests for the full SmartKey pipeline.
//!
//! These tests exercise the public API from outside the crate, covering
//! the complete flow from corpus loading through key events to predictions.

use smartkey_core::input::{Action, InputConfig, InputMethodCore, Key, KeyEvent, Modifiers};

// ======================================================================
// Helpers
// ======================================================================

fn press(key: Key) -> KeyEvent {
    KeyEvent {
        key,
        modifiers: Modifiers::empty(),
    }
}

fn press_with(key: Key, mods: Modifiers) -> KeyEvent {
    KeyEvent {
        key,
        modifiers: mods,
    }
}

fn has_action(actions: &[Action], target: &Action) -> bool {
    actions.iter().any(|a| a == target)
}

fn ghost_text(actions: &[Action]) -> Option<String> {
    actions.iter().find_map(|a| match a {
        Action::ShowGhost(s) => Some(s.clone()),
        _ => None,
    })
}

fn type_string(core: &mut InputMethodCore, s: &str) {
    for ch in s.chars() {
        core.handle_key(press(Key::Char(ch)));
    }
}

fn load_json_corpus(core: &mut InputMethodCore, json: &str) {
    let corpus = smartkey_core::corpus::Corpus::from_json(json).expect("test corpus should parse");
    // Load via individual calls (public API).
    for w in &corpus.words {
        core.load_word(&w.word, w.frequency);
    }
    for b in &corpus.bigrams {
        core.load_bigram(&b.ctx, &b.word, b.count);
    }
    for t in &corpus.trigrams {
        core.load_trigram(&t.w1, &t.w2, &t.word, t.count);
    }
}

// ======================================================================
// Test 1: Full pipeline
// ======================================================================

#[test]
fn full_pipeline_predict_and_ghost() {
    let mut core = InputMethodCore::new(InputConfig::default());

    let corpus = r#"{
        "unigrams": {"hello": 100, "help": 80, "hero": 60, "world": 90},
        "bigrams": [{"ctx": "say", "word": "hello", "count": 5}],
        "trigrams": []
    }"#;
    load_json_corpus(&mut core, corpus);

    // Type "hel" — should get ghost "lo" (from "hello", top by frequency).
    core.handle_key(press(Key::Char('h')));
    core.handle_key(press(Key::Char('e')));
    let actions = core.handle_key(press(Key::Char('l')));

    let g = ghost_text(&actions);
    assert!(g.is_some(), "should have ghost text for 'hel'");
    assert_eq!(g.unwrap(), "lo", "ghost should be 'lo' from 'hello'");

    // Verify "hello" is top prediction.
    let preds = core.predictions();
    assert!(!preds.is_empty());
    assert_eq!(preds[0].word, "hello");
}

// ======================================================================
// Test 2: Tab acceptance with bigram boost
// ======================================================================

#[test]
fn tab_acceptance_then_bigram_boost() {
    let mut core = InputMethodCore::new(InputConfig::default());

    let corpus = r#"{
        "unigrams": {"hello": 100, "help": 80, "world": 90, "worry": 90},
        "bigrams": [
            {"ctx": "hello", "word": "world", "count": 10}
        ],
        "trigrams": []
    }"#;
    load_json_corpus(&mut core, corpus);

    // Type "hel" → ghost "lo" → Tab commits ghost suffix only (typed chars were forwarded).
    type_string(&mut core, "hel");
    let actions = core.handle_key(press(Key::Tab));
    assert!(
        has_action(&actions, &Action::CommitText("lo".into())),
        "Tab should commit ghost suffix 'lo' (not full word)"
    );

    // Now type "wor" — with "hello" in context, "world" should rank first
    // due to bigram boost.
    type_string(&mut core, "wor");
    let preds = core.predictions();

    assert!(!preds.is_empty(), "should have predictions for 'wor'");
    assert_eq!(
        preds[0].word, "world",
        "'world' should rank first due to bigram from 'hello'"
    );
}

// ======================================================================
// Test 3: Personal profile round-trip
// ======================================================================

#[test]
fn personal_round_trip_persists_boost() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let profile_path = dir.path().join("personal_test.json");

    // Engine 1: learn "help" heavily, then save.
    // Use a small CVM buffer so "help" is highly significant when retained.
    {
        let mut core = InputMethodCore::new(InputConfig::default());
        core.load_word("hello", 100);
        core.load_word("help", 100);

        // Learn "help" 200 times to strongly establish it in CVM.
        for _ in 0..200 {
            type_string(&mut core, "help");
            core.handle_key(press(Key::Space));
        }

        core.save_personal(&profile_path)
            .expect("save should succeed");
        assert!(profile_path.is_file());
    }

    // Engine 2 (baseline): same corpus, NO profile — establish rank without personal boost.
    let mut core2 = InputMethodCore::new(InputConfig::default());
    core2.load_word("hello", 100);
    core2.load_word("help", 100);

    type_string(&mut core2, "hel");
    let baseline_preds = core2.predictions();
    assert!(
        baseline_preds.len() >= 2,
        "baseline should have at least 2 predictions for 'hel'"
    );
    let help_rank_without_profile = baseline_preds
        .iter()
        .position(|p| p.word == "help")
        .expect("'help' should appear in baseline predictions");

    // Engine 3: same corpus, WITH loaded profile — personal CVM boost should elevate "help".
    let mut core3 = InputMethodCore::new(InputConfig::default());
    core3.load_word("hello", 100);
    core3.load_word("help", 100);

    core3
        .load_personal(&profile_path)
        .expect("load should succeed");

    type_string(&mut core3, "hel");
    let preds = core3.predictions();
    assert!(
        preds.iter().any(|p| p.word == "help"),
        "loaded profile should include learned word in predictions"
    );
    let help_rank_with_profile = preds
        .iter()
        .position(|p| p.word == "help")
        .expect("'help' should appear in predictions after loading personal profile");

    assert!(
        help_rank_with_profile <= help_rank_without_profile,
        "loading personal profile should not reduce 'help' rank: \
         baseline={}, with_profile={}",
        help_rank_without_profile,
        help_rank_with_profile
    );
}

// ======================================================================
// Test 4: Kill switch
// ======================================================================

#[test]
fn kill_switch_disables_and_reenables() {
    let mut core = InputMethodCore::new(InputConfig::default());
    core.load_word("hello", 100);
    core.load_word("help", 80);

    assert!(core.is_enabled());

    // Super+Escape → disable.
    let actions = core.handle_key(press_with(Key::Escape, Modifiers::SUPER));
    assert!(!core.is_enabled());
    assert!(has_action(&actions, &Action::HideGhost));

    // Keys while disabled → forwarded, no predictions.
    let actions = core.handle_key(press(Key::Char('h')));
    assert!(has_action(&actions, &Action::ForwardKey));
    assert_eq!(core.current_word(), "");

    let actions = core.handle_key(press(Key::Char('e')));
    assert!(has_action(&actions, &Action::ForwardKey));
    assert!(core.predictions().is_empty());

    // Super+Escape again → re-enable.
    core.handle_key(press_with(Key::Escape, Modifiers::SUPER));
    assert!(core.is_enabled());

    // Now predictions should work again.
    type_string(&mut core, "hel");
    let preds = core.predictions();
    assert!(!preds.is_empty(), "predictions should work after re-enable");
}

// ======================================================================
// Test 5: Multi-language corpus
// ======================================================================

#[test]
fn multi_language_latin_and_cyrillic() {
    let mut core = InputMethodCore::new(InputConfig {
        min_prefix_length: 2,
        ..InputConfig::default()
    });

    // English words.
    core.load_word("hello", 100);
    core.load_word("help", 80);

    // Bulgarian (Cyrillic) words.
    core.load_word("здравей", 100);
    core.load_word("здраве", 80);

    // Latin prefix → English predictions.
    type_string(&mut core, "hel");
    let preds = core.predictions();
    assert!(
        preds.iter().any(|p| p.word == "hello"),
        "Latin prefix 'hel' should yield English predictions"
    );
    assert!(
        !preds.iter().any(|p| p.word == "здравей"),
        "Latin prefix should not yield Cyrillic predictions"
    );

    // Reset state.
    core.handle_key(press(Key::Space));

    // Cyrillic prefix → Bulgarian predictions.
    // "здр" is 3 chars (each is 2 bytes in UTF-8 = 6 bytes, but 3 chars).
    // With min_prefix_length=2, typing 2 Cyrillic chars should trigger predictions.
    core.handle_key(press(Key::Char('з')));
    let actions = core.handle_key(press(Key::Char('д')));

    let preds = core.predictions();
    // After Step 5 char-aware fix, 2 Cyrillic chars >= min_prefix_length.
    assert!(
        !preds.is_empty(),
        "2 Cyrillic chars should be enough for predictions (char-aware prefix length)"
    );
    assert!(
        preds.iter().any(|p| p.word.starts_with("здрав")),
        "Cyrillic prefix 'зд' should yield Bulgarian predictions, got: {:?}",
        preds.iter().map(|p| &p.word).collect::<Vec<_>>()
    );

    // Verify ghost text appears for Cyrillic.
    let ghost = ghost_text(&actions);
    assert!(
        ghost.is_some(),
        "should have ghost text for Cyrillic prefix"
    );
}
