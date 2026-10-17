//! Native client completion owns the delivered token, not an IBus preedit.
//! Real public Core contracts only; synthetic corpus, no UI or profile I/O.

use smartkey_core::input::{Action, InputConfig, Key, KeyEvent, Modifiers};
use smartkey_core::lang_detect::LangId;
use smartkey_core::MasterLoop;

fn event(key: Key, modifiers: Modifiers) -> KeyEvent {
    KeyEvent { key, modifiers }
}

fn press(master: &mut MasterLoop, key: Key) -> Vec<Action> {
    master.handle_key(event(key, Modifiers::empty()))
}

fn fixture() -> MasterLoop {
    let mut master = MasterLoop::new(InputConfig {
        ghost_text_separation_margin: 0.0,
        use_ppm: false,
        use_reranker: false,
        space_accept: true,
        ..InputConfig::default()
    });
    master.load_word_lang("здравей", 1_000_000, LangId::Bg);
    for code in [44, 32, 19, 30] {
        press(&mut master, Key::RawCode(code));
    }
    assert_eq!(master.current_word(), "здра");
    assert_eq!(
        master.ghost_text(),
        "вей",
        "fixture needs a real ghost to retire"
    );
    master
}

fn commits(actions: &[Action]) -> String {
    actions
        .iter()
        .filter_map(|action| match action {
            Action::CommitText(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn no_preedit_or_rewrite(master: &MasterLoop, actions: &[Action]) {
    assert!(
        !actions.iter().any(|action| matches!(
            action,
            Action::ShowComposing { .. } | Action::ShowGhost(_) | Action::ReplaceWord { .. }
        )),
        "client-owned input must not create an IBus preedit or rewrite: {actions:?}"
    );
    assert!(!master.has_ghost());
    assert!(master.ghost_text().is_empty());
}

fn learned_counts(master: &MasterLoop) -> (u64, u64) {
    let metrics = master.metrics().summary();
    (metrics.total_commits, metrics.accepted_predictions)
}

fn report(master: &mut MasterLoop, document: &str) {
    master.set_surrounding_text(Some(document.into()), Some(document.chars().count()));
}

fn handoff(master: &mut MasterLoop) {
    let before = learned_counts(master);
    let actions = master.handoff_preedit_for_native_completion();
    assert_eq!(
        actions,
        vec![Action::HideGhost, Action::CommitText("здра".into())]
    );
    assert_eq!(master.current_word(), "здра");
    assert!(!master.debug_state().0, "typed prefix is now in the client");
    no_preedit_or_rewrite(master, &actions);
    assert_eq!(
        learned_counts(master),
        before,
        "handoff is delivery, not learning"
    );
}

fn assert_next_word_composes(master: &mut MasterLoop) {
    let actions = press(master, Key::RawCode(44));
    assert!(
        master.debug_state().0,
        "client ownership must not leak to the next token"
    );
    assert!(actions
        .iter()
        .any(|action| matches!(action, Action::ShowComposing { .. })));
    assert!(
        commits(&actions).is_empty(),
        "new raw prefix belongs only to preedit"
    );
}

#[test]
fn handoff_delivers_prefix_once_and_keeps_word_language_without_learning() {
    let mut master = fixture();
    let language = master.detected_language();
    let prior = master.light_profile().lang_prior;
    handoff(&mut master);
    assert_eq!(master.detected_language().lang, language.lang);
    assert_eq!(master.light_profile().lang_prior, prior);
    assert!(master.language_choice_offer().is_none());
    let repeated = master.handoff_preedit_for_native_completion();
    no_preedit_or_rewrite(&master, &repeated);
    assert!(
        commits(&repeated).is_empty(),
        "repeated shortcut must not duplicate the prefix"
    );
    assert_eq!(master.current_word(), "здра");
}

#[test]
fn multiple_bg_refinements_under_us_commit_only_selected_glyphs() {
    let mut master = fixture();
    handoff(&mut master);
    let prior = master.light_profile().lang_prior;
    let mut document = String::from("😀 seed здра");
    for (code, glyph, word) in [(17, "в", "здрав"), (18, "е", "здраве")] {
        report(&mut master, &document);
        assert!(!master.reconcile_native_completion_word(true));
        let actions = press(&mut master, Key::RawCode(code));
        assert_eq!(
            commits(&actions),
            glyph,
            "raw US key must deliver its retained BG reading"
        );
        assert!(
            !actions.contains(&Action::ForwardKey),
            "forwarding would insert US Latin"
        );
        no_preedit_or_rewrite(&master, &actions);
        document.push_str(glyph);
        assert_eq!(master.current_word(), word);
        assert_eq!(learned_counts(&master), (0, 0));
    }
    assert_eq!(document, "😀 seed здраве");
    assert_eq!(master.light_profile().lang_prior, prior);
}

#[test]
fn tab_right_and_space_cannot_accept_a_hidden_core_completion() {
    for key in [Key::Tab, Key::Right, Key::Space] {
        let mut master = fixture();
        handoff(&mut master);
        report(&mut master, "здра");
        assert!(!master.reconcile_native_completion_word(true));
        let actions = press(&mut master, key.clone());
        no_preedit_or_rewrite(&master, &actions);
        assert!(
            commits(&actions).is_empty(),
            "native completion owns {key:?}: {actions:?}"
        );
        assert!(actions.contains(&Action::ForwardKey));
        assert_eq!(master.metrics().summary().accepted_predictions, 0);
    }
}

#[test]
fn empty_handoff_does_not_arm_future_input() {
    let mut master = fixture();
    master.reset();
    for _ in 0..2 {
        let actions = master.handoff_preedit_for_native_completion();
        no_preedit_or_rewrite(&master, &actions);
        assert!(commits(&actions).is_empty());
    }
    assert_next_word_composes(&mut master);
}

#[test]
fn retained_bg_layout_applies_caps_per_key_including_physical_brackets() {
    for (modifiers, e, bracket) in [
        (Modifiers::empty(), "е", "ш"),
        (Modifiers::SHIFT, "Е", "Ш"),
        (Modifiers::CAPS_LOCK, "Е", "Ш"),
        (Modifiers::SHIFT | Modifiers::CAPS_LOCK, "е", "ш"),
    ] {
        let mut master = fixture();
        handoff(&mut master);
        for (code, expected) in [(18, e), (26, bracket)] {
            let document = master.current_word().to_owned();
            report(&mut master, &document);
            assert!(!master.reconcile_native_completion_word(true));
            let actions = master.handle_key(event(Key::RawCode(code), modifiers));
            assert_eq!(commits(&actions), expected, "{modifiers:?}, code={code}");
            assert!(!actions.contains(&Action::ForwardKey));
            no_preedit_or_rewrite(&master, &actions);
        }
        assert_eq!(learned_counts(&master), (0, 0));
    }
}

#[test]
fn backspace_edits_delivered_word_and_restores_composition_at_empty() {
    let mut master = fixture();
    handoff(&mut master);
    let mut document = String::from("здра");
    for expected in ["здр", "зд", "з", ""] {
        report(&mut master, &document);
        assert!(!master.reconcile_native_completion_word(true));
        let actions = press(&mut master, Key::RawCode(14));
        no_preedit_or_rewrite(&master, &actions);
        assert!(commits(&actions).is_empty());
        assert!(
            actions.contains(&Action::ForwardKey),
            "client must delete its own character"
        );
        document.pop();
        assert_eq!(document, expected);
        assert_eq!(master.current_word(), expected);
    }
    assert_eq!(learned_counts(&master), (0, 0));
    assert_next_word_composes(&mut master);
}

#[test]
fn delivered_boundary_learns_once_without_recommitting_or_accepting() {
    let mut master = fixture();
    handoff(&mut master);
    report(&mut master, "здра");
    assert!(!master.reconcile_native_completion_word(true));
    let actions = press(&mut master, Key::Space);
    no_preedit_or_rewrite(&master, &actions);
    assert!(commits(&actions).is_empty());
    assert!(actions.contains(&Action::ForwardKey));
    assert_eq!(learned_counts(&master), (1, 0));
    let prior = master.light_profile().lang_prior;
    let actions = press(&mut master, Key::Space);
    assert!(commits(&actions).is_empty());
    assert_eq!(learned_counts(&master), (1, 0));
    assert_eq!(master.light_profile().lang_prior, prior);
    assert_next_word_composes(&mut master);
}

#[test]
fn missed_space_or_native_replacement_retires_word_without_learning() {
    for document in ["здра ", "здрасинтетична", "prefix аздра"] {
        let mut master = fixture();
        handoff(&mut master);
        let prior = master.light_profile().lang_prior;
        report(&mut master, document);
        assert!(
            master.reconcile_native_completion_word(true),
            "external token change: {document}"
        );
        assert!(master.current_word().is_empty());
        assert_eq!(learned_counts(&master), (0, 0));
        assert_eq!(master.light_profile().lang_prior, prior);
        assert!(
            !master.reconcile_native_completion_word(true),
            "retirement is one-shot"
        );
        assert_next_word_composes(&mut master);
    }
}

#[test]
fn missing_invalid_or_moved_cursor_cannot_keep_native_word_owned() {
    for (text, cursor) in [
        (None, None),
        (Some("здра"), None),
        (Some("здра"), Some(99)),
        (Some("😀 здра tail"), Some(1)),
        (Some("здра"), Some(0)),
    ] {
        let mut master = fixture();
        handoff(&mut master);
        master.set_surrounding_text(text.map(str::to_owned), cursor);
        assert!(master.reconcile_native_completion_word(true));
        assert!(master.current_word().is_empty());
        assert_eq!(learned_counts(&master), (0, 0));
        assert_next_word_composes(&mut master);
    }
}

#[test]
fn noncollapsed_or_unknown_selection_retires_even_matching_suffix() {
    let mut master = fixture();
    handoff(&mut master);
    report(&mut master, "😀 seed здра");
    assert!(master.reconcile_native_completion_word(false));
    assert!(master.current_word().is_empty());
    assert_eq!(learned_counts(&master), (0, 0));
    assert_next_word_composes(&mut master);
}

#[test]
fn ordinary_preedit_survives_external_reports_and_reconcile_calls() {
    let mut master = fixture();
    report(&mut master, "unrelated client field ");
    assert!(!master.reconcile_native_completion_word(false));
    assert_eq!(master.current_word(), "здра");
    assert_eq!(master.ghost_text(), "вей");
    assert!(master.debug_state().0);
    let actions = press(&mut master, Key::RawCode(17));
    assert_eq!(master.current_word(), "здрав");
    assert!(actions
        .iter()
        .any(|action| matches!(action, Action::ShowComposing { .. })));
    assert!(commits(&actions).is_empty());
}

#[test]
fn native_owned_focus_and_reset_retire_without_learning_or_recommitting() {
    for boundary in [0, 1, 2] {
        let mut master = fixture();
        handoff(&mut master);
        match boundary {
            0 => {
                let actions = master.reset();
                assert!(commits(&actions).is_empty());
            }
            1 => {
                let actions = master.focus_lost();
                assert!(commits(&actions).is_empty());
            }
            _ => master.focus_gained(),
        }
        assert!(master.current_word().is_empty());
        assert_eq!(learned_counts(&master), (0, 0));
        assert_next_word_composes(&mut master);
    }
}
