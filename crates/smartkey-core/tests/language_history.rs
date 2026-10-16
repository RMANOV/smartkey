//! Retracted statistical evidence must not outrank the surviving physical word.
//!
//! Public MasterLoop producers only: these tests do not assign lock state,
//! fabricate a ghost, or load/save a personal profile.

use smartkey_core::input::{Action, InputConfig, Key, KeyEvent, Modifiers};
use smartkey_core::lang_detect::LangId;
use smartkey_core::master_loop::MasterLoop;

const BG_CONTEXT: &str = "Искам да благодаря ";
const EN_CONTEXT: &str = "I want to thank ";

fn fixture(context: &str, ghost_text: bool) -> MasterLoop {
    let mut ml = MasterLoop::new(InputConfig {
        ghost_text,
        ..InputConfig::default()
    });
    // The existing terminal-context fixture separates prefix evidence (with)
    // from the two supported completed readings (wi / ви).
    ml.load_word_lang("with", 7_079_457, LangId::En);
    ml.load_word_lang("wi", 12_302, LangId::En);
    ml.load_word_lang("ви", 2_137_962, LangId::Bg);
    ml.set_surrounding_text(Some(context.to_string()), Some(context.chars().count()));
    ml
}

fn edit_preedit(ml: &mut MasterLoop, key: Key) {
    let actions = ml.handle_key(KeyEvent {
        key,
        modifiers: Modifiers::empty(),
    });
    assert!(
        actions.iter().any(|action| matches!(action,
            Action::ShowComposing { typed, .. } if typed.as_str() == ml.current_word())),
        "actual composing update missing: {actions:?}"
    );
    assert!(
        !actions.iter().any(|action| matches!(
            action,
            Action::CommitText(_) | Action::ForwardKey | Action::ReplaceWord { .. }
        )),
        "an uncommitted edit delivered client text: {actions:?}"
    );
    assert_eq!(ml.metrics().summary().total_commits, 0);
}

fn type_wi(ml: &mut MasterLoop) {
    edit_preedit(ml, Key::RawCode(17));
    edit_preedit(ml, Key::RawCode(23));
    assert_eq!(ml.current_word(), "wi");
    assert_eq!(ml.debug_state(), (true, false, true));
}

fn type_with_then_erase_suffix(ml: &mut MasterLoop) {
    for code in [17, 23, 20, 35] {
        edit_preedit(ml, Key::RawCode(code));
    }
    assert_eq!(ml.current_word(), "with");
    assert_eq!(ml.debug_state(), (true, true, false));
    for _ in 0..2 {
        edit_preedit(ml, Key::RawCode(14));
    }
    assert_eq!(ml.current_word(), "wi");
}

fn commit_space(ml: &mut MasterLoop, expected: &str) {
    let actions = ml.handle_key(KeyEvent {
        key: Key::RawCode(57),
        modifiers: Modifiers::empty(),
    });
    let committed: Vec<&str> = actions
        .iter()
        .filter_map(|action| match action {
            Action::CommitText(word) => Some(word.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(committed, vec![expected], "{actions:?}");
    assert_eq!(
        actions
            .iter()
            .filter(|action| matches!(action, Action::ForwardKey))
            .count(),
        1,
        "one physical Space must forward once: {actions:?}"
    );
    assert!(!actions
        .iter()
        .any(|action| matches!(action, Action::ReplaceWord { .. })));
    assert_eq!(ml.current_word(), "");
    assert_eq!(ml.debug_state(), (false, false, false));
    let metrics = ml.metrics().summary();
    assert_eq!(metrics.total_commits, 1);
    assert_eq!(metrics.accepted_predictions, 0);
}

#[test]
fn language_history_retracted_statistical_prefix_matches_fresh_boundary() {
    let mut fresh = fixture(BG_CONTEXT, false);
    type_wi(&mut fresh);
    commit_space(&mut fresh, "ви");

    let mut edited = fixture(BG_CONTEXT, false);
    type_with_then_erase_suffix(&mut edited);
    commit_space(&mut edited, "ви");
}

#[test]
fn language_history_partial_right_choice_survives_later_suffix_deletion() {
    let mut ml = fixture(BG_CONTEXT, true);
    // Both completed readings are supported, so preserving the actual accepted
    // English reading matters at the boundary even with Bulgarian context.
    ml.load_word_lang("wit", 12_302, LangId::En);
    ml.load_word_lang("вит", 2_137_962, LangId::Bg);
    type_wi(&mut ml);
    assert_eq!(ml.ghost_text(), "th");
    edit_preedit(&mut ml, Key::Right);
    assert_eq!(ml.current_word(), "wit");
    assert_eq!(ml.debug_state(), (true, true, false));

    edit_preedit(&mut ml, Key::RawCode(35));
    assert_eq!(ml.current_word(), "with");
    edit_preedit(&mut ml, Key::RawCode(14));
    assert_eq!(ml.current_word(), "wit");
    assert_eq!(ml.debug_state(), (true, true, false));
    // The accepted 't' survives; only the subsequently typed 'h' was removed.
    commit_space(&mut ml, "wit");
}

#[test]
fn language_history_english_context_keeps_both_histories_literal() {
    let mut fresh = fixture(EN_CONTEXT, false);
    type_wi(&mut fresh);
    commit_space(&mut fresh, "wi");

    let mut edited = fixture(EN_CONTEXT, false);
    type_with_then_erase_suffix(&mut edited);
    commit_space(&mut edited, "wi");
}
