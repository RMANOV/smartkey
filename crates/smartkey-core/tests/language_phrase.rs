//! An inferred prefix lock must not hide the surviving physical interpretation.

use smartkey_core::input::{Action, InputConfig, Key, KeyEvent, Modifiers};
use smartkey_core::lang_detect::LangId;
use smartkey_core::master_loop::MasterLoop;

fn raw(ml: &mut MasterLoop, code: u16) {
    let actions = ml.handle_key(KeyEvent {
        key: Key::RawCode(code),
        modifiers: Modifiers::empty(),
    });
    assert!(actions.iter().any(|action| matches!(
        action,
        Action::ShowComposing { typed, .. } if typed == ml.current_word()
    )));
    assert!(!actions.iter().any(|action| matches!(
        action,
        Action::CommitText(_) | Action::ForwardKey | Action::ReplaceWord { .. }
    )));
    assert_eq!(ml.metrics().summary().total_commits, 0);
}

#[test]
fn inferred_prefix_lock_recovers_when_its_current_prefix_has_no_support() {
    let mut ml = MasterLoop::new(InputConfig::default());
    // Real public-corpus frequencies. This reduced fixture retains the actual
    // prefix leaders through lis/listo/listow, without loading a profile.
    ml.load_word_lang("list", 190_546, LangId::En);
    ml.load_word_lang("liston", 457, LangId::En);
    ml.load_word_lang("листа", 47_863, LangId::Bg);
    ml.load_word_lang("листове", 4_466, LangId::Bg);
    ml.load_word_lang("листовете", 95, LangId::Bg);
    let context = "I want to thank ";
    ml.set_surrounding_text(Some(context.into()), Some(context.chars().count()));
    ml.set_language_choice_supported(true);

    for code in [38, 23, 31] {
        raw(&mut ml, code);
    }
    assert_eq!(ml.current_word(), "lis");
    assert_eq!(ml.debug_state(), (true, true, false));

    // Both live prefixes still have support: preserve the current lock.
    raw(&mut ml, 20);
    raw(&mut ml, 24);
    assert_eq!(ml.current_word(), "listo");
    assert_eq!(ml.debug_state(), (true, true, false));

    // EN listow has no supported completion, while BG листов still has
    // листове/листовете. No explicit acceptance pinned the earlier reading.
    raw(&mut ml, 17);
    assert_eq!(ml.current_word(), "листов");
    assert!(ml
        .predictions()
        .iter()
        .any(|prediction| prediction.word.to_lowercase().starts_with("листов")));
    // Existing quality gates may abstain when a fuzzy top candidate does not
    // extend this prefix. Recovery must restore BG candidates; any displayed
    // completion must still conserve one of the actual predicted words.
    if !ml.ghost_text().is_empty() {
        let displayed = format!("{}{}", ml.current_word(), ml.ghost_text());
        assert!(ml
            .predictions()
            .iter()
            .any(|prediction| prediction.word.to_lowercase() == displayed.to_lowercase()));
    }
}

fn list_fixture() -> MasterLoop {
    let mut ml = MasterLoop::new(InputConfig::default());
    ml.load_word_lang("list", 190_546, LangId::En);
    ml.load_word_lang("liston", 457, LangId::En);
    ml.load_word_lang("листа", 47_863, LangId::Bg);
    ml.load_word_lang("листове", 4_466, LangId::Bg);
    ml.load_word_lang("листовете", 95, LangId::Bg);
    let context = "I want to thank ";
    ml.set_surrounding_text(Some(context.into()), Some(context.chars().count()));
    ml.set_language_choice_supported(true);
    ml
}

fn boundary(ml: &mut MasterLoop, expected: &str) {
    assert_eq!(
        ml.handle_key(KeyEvent {
            key: Key::RawCode(57),
            modifiers: Modifiers::empty(),
        }),
        vec![
            Action::HideGhost,
            Action::CommitText(expected.into()),
            Action::ForwardKey,
        ]
    );
    assert_eq!(ml.current_word(), "");
    assert_eq!(ml.debug_state(), (false, false, false));
    assert_eq!(ml.metrics().summary().total_commits, 1);
    assert_eq!(ml.metrics().summary().accepted_predictions, 0);
}

#[test]
fn inferred_prefix_lock_recovers_from_bg_to_en_as_well() {
    let mut ml = MasterLoop::new(InputConfig::default());
    ml.load_word_lang("liston", 457, LangId::En);
    ml.load_word_lang("листове", 4_466, LangId::Bg);
    let context = "Искам да благодаря ";
    ml.set_surrounding_text(Some(context.into()), Some(context.chars().count()));

    for code in [38, 23, 31] {
        raw(&mut ml, code);
    }
    assert_eq!(ml.current_word(), "лис");
    assert_eq!(ml.debug_state(), (true, true, false));
    for code in [20, 24] {
        raw(&mut ml, code);
    }
    assert_eq!(ml.current_word(), "листо");

    raw(&mut ml, 49);
    assert_eq!(ml.current_word(), "liston");
    assert!(ml
        .predictions()
        .iter()
        .any(|prediction| prediction.word.to_lowercase() == "liston"));
    boundary(&mut ml, "liston");
}

#[test]
fn recovered_inferred_lock_retracts_with_its_new_evidence_then_retypes() {
    let mut ml = list_fixture();
    for code in [38, 23, 31, 20, 24, 17] {
        raw(&mut ml, code);
    }
    assert_eq!(ml.current_word(), "листов");
    assert_eq!(ml.debug_state(), (true, true, false));

    raw(&mut ml, 14);
    raw(&mut ml, 14);
    assert_eq!(ml.current_word(), "list");
    assert_eq!(ml.debug_state(), (true, false, true));
    for code in [24, 17] {
        raw(&mut ml, code);
    }
    assert_eq!(ml.current_word(), "листов");
    for code in [18, 20, 18] {
        raw(&mut ml, code);
    }
    boundary(&mut ml, "листовете");
}

#[test]
fn accepted_partial_right_keeps_its_reading_when_later_prefix_support_ends() {
    let mut ml = list_fixture();
    raw(&mut ml, 38);
    raw(&mut ml, 23);
    assert_eq!(ml.current_word(), "li");
    assert_eq!(ml.ghost_text(), "st");
    raw(&mut ml, 106); // Explicitly accept the displayed 's'; it survives below.
    assert_eq!(ml.current_word(), "lis");
    assert_eq!(ml.debug_state(), (true, true, false));
    for code in [20, 24, 17] {
        raw(&mut ml, code);
    }
    assert_eq!(ml.current_word(), "listow");
    assert_eq!(ml.debug_state(), (true, true, false));
    assert!(ml.language_choice_offer().is_none());
    boundary(&mut ml, "listow");
}

#[test]
fn both_unsupported_prefix_keeps_inferred_reading_and_explicit_choice() {
    let mut ml = list_fixture();
    for code in [38, 23, 31, 20, 45] {
        raw(&mut ml, code);
    }
    assert_eq!(ml.current_word(), "listx");
    assert_eq!(ml.debug_state(), (true, true, false));
    let offer = ml
        .language_choice_offer()
        .expect("both unsupported readings");
    assert_eq!(offer.current_language, LangId::En);
    assert_eq!(offer.en, "listx");
    assert_eq!(offer.bg, "листь");
    assert_eq!(ml.language_choice_offer(), Some(offer.clone()));
    assert_eq!(
        ml.accept_language_choice(offer.revision, LangId::Bg),
        Some(vec![Action::HideGhost, Action::CommitText("листь".into())])
    );
    assert!(ml
        .accept_language_choice(offer.revision, LangId::Bg)
        .is_none());
    assert_eq!(ml.current_word(), "");
    assert!(ml.language_choice_offer().is_none());
    assert_eq!(ml.metrics().summary().total_commits, 1);
    assert_eq!(ml.metrics().summary().accepted_predictions, 0);
}

#[test]
fn one_sided_todo_still_commits_english_after_bulgarian_prefix_lock() {
    let mut ml = MasterLoop::new(InputConfig::default());
    ml.load_word_lang("todo", 912, LangId::En);
    ml.load_word_lang("тодор", 57_543, LangId::Bg);
    let context = "Искам да благодаря ";
    ml.set_surrounding_text(Some(context.into()), Some(context.chars().count()));
    ml.set_language_choice_supported(true);
    for code in [20, 24, 32, 24] {
        raw(&mut ml, code);
    }
    assert_eq!(ml.current_word(), "тодо");
    assert_eq!(ml.debug_state(), (true, true, false));
    assert!(ml.language_choice_offer().is_none());
    boundary(&mut ml, "todo");
}
