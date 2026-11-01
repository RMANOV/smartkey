//! Explicit whole-token language choice must conserve delivery and learning.
//!
//! The fixtures use public corpus/key/context producers, never assigned Core
//! fields, synthetic ghosts, injected actions, or personal-profile files.

use smartkey_core::input::{Action, InputConfig, Key, KeyEvent, Modifiers};
use smartkey_core::keymap;
use smartkey_core::lang_detect::LangId;
use smartkey_core::light_profile::LightProfile;
use smartkey_core::master_loop::{LanguageChoiceOffer, MasterLoop};

const CONTEXT: &str = "Искам да благодаря ";

fn event(key: Key) -> KeyEvent {
    KeyEvent {
        key,
        modifiers: Modifiers::empty(),
    }
}

fn fixture(supported: bool, ghost: bool, space_accept: bool) -> MasterLoop {
    let mut ml = MasterLoop::new(InputConfig {
        ghost_text: ghost,
        space_accept,
        ..InputConfig::default()
    });
    // The long target readings are deliberately absent. These public words
    // produce inferred prefix locks without confirming either completed token.
    for word in ["drafting", "temple", "markdown", "with"] {
        ml.load_word_lang(word, 7_079_457, LangId::En);
    }
    for (en, bg) in [("tn", "тн"), ("top", "топ"), ("wi", "ви")] {
        ml.load_word_lang(en, 12_302, LangId::En);
        ml.load_word_lang(bg, 2_137_962, LangId::Bg);
    }
    ml.set_surrounding_text(Some(CONTEXT.into()), Some(CONTEXT.chars().count()));
    if supported {
        ml.set_language_choice_supported(true);
    }
    ml
}

fn raw_code(ch: char) -> u16 {
    (2..=53)
        .find(|code| keymap::scancode_to_both(*code, false).is_some_and(|pair| pair.0 == ch))
        .expect("fixture character has a real unshifted physical key")
}

fn type_preedit(ml: &mut MasterLoop, text: &str) {
    let committed_before = ml.metrics().summary().total_commits;
    for ch in text.chars() {
        let actions = ml.handle_key(event(Key::RawCode(raw_code(ch))));
        assert!(
            actions.iter().any(|action| matches!(action,
            Action::ShowComposing { typed, .. } if typed == ml.current_word())),
            "missing real composing update for {ch:?}: {actions:?}"
        );
        assert!(
            !actions.iter().any(|action| matches!(
                action,
                Action::CommitText(_) | Action::ForwardKey | Action::ReplaceWord { .. }
            )),
            "typing delivered text before an explicit commit: {actions:?}"
        );
        assert_eq!(ml.metrics().summary().total_commits, committed_before);
    }
}

fn offer(ml: &MasterLoop, en: &str, bg: &str) -> LanguageChoiceOffer {
    let before = ml.current_word().to_string();
    let offer = ml
        .language_choice_offer()
        .expect("whole-token choice is available");
    assert_eq!(offer.en, en);
    assert_eq!(offer.bg, bg);
    assert_eq!(
        ml.current_word(),
        before,
        "lookup mutated the composing token"
    );
    assert_eq!(
        match offer.current_language {
            LangId::En => offer.en.as_str(),
            LangId::Bg => offer.bg.as_str(),
            LangId::Tech => panic!("alphabetic pair cannot claim a Tech reading"),
        },
        before
    );
    assert_eq!(
        ml.language_choice_offer(),
        Some(offer.clone()),
        "read-only lookup changed identity"
    );
    offer
}

fn profile_bits(profile: &LightProfile) -> ([u64; 3], u64, u8, u64) {
    (
        profile.lang_prior.map(f64::to_bits),
        profile.ghost_accept_rate.to_bits(),
        profile.suppress_after_reject,
        profile.confidence_floor.to_bits(),
    )
}

fn assert_only_commit(actions: &[Action], expected: &str, forward: bool) {
    let commits: Vec<_> = actions
        .iter()
        .filter_map(|action| match action {
            Action::CommitText(text) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(commits, [expected], "{actions:?}");
    assert_eq!(
        actions
            .iter()
            .filter(|action| matches!(action, Action::ForwardKey))
            .count(),
        usize::from(forward),
        "{actions:?}"
    );
    assert!(
        !actions.iter().any(|action| matches!(
            action,
            Action::ReplaceWord { .. } | Action::ShowComposing { .. } | Action::ShowGhost(_)
        )),
        "commit changed or re-offered the selected text: {actions:?}"
    );
}

fn pair_is_offered(en: &str, bg: &str, inferred_lock: bool) {
    let mut ml = fixture(true, false, false);
    let before = profile_bits(ml.light_profile());
    type_preedit(&mut ml, en);
    assert_eq!(ml.debug_state().0, true);
    if inferred_lock {
        assert!(
            ml.debug_state().1,
            "fixture must acquire a real inferred lock"
        );
    }
    offer(&ml, en, bg);
    assert_eq!(profile_bits(ml.light_profile()), before);
    assert_eq!(ml.metrics().summary().total_commits, 0);
}

#[test]
fn language_choice_offers_complete_draftyt_without_rewriting() {
    pair_is_offered("draftyt", "драфтът", true);
}

#[test]
fn language_choice_offers_complete_templejtyt_without_rewriting() {
    pair_is_offered("templejtyt", "темплейтът", true);
}

#[test]
fn language_choice_offers_complete_markdaun_without_rewriting() {
    pair_is_offered("markdaun", "маркдаун", true);
}

#[test]
fn language_choice_offers_both_supported_tn_without_rewriting() {
    pair_is_offered("tn", "тн", false);
}

#[test]
fn language_choice_offers_both_supported_top_without_rewriting() {
    pair_is_offered("top", "топ", false);
}

#[test]
fn language_choice_commits_either_selected_reading_and_observes_it_once() {
    for language in [LangId::En, LangId::Bg] {
        let mut ml = fixture(true, false, false);
        type_preedit(&mut ml, "draftyt");
        let offered = offer(&ml, "draftyt", "драфтът");
        let selected = if language == LangId::En {
            &offered.en
        } else {
            &offered.bg
        };
        let mut expected_profile = ml.light_profile().clone();
        expected_profile.observe_lang(language);
        let actions = ml
            .accept_language_choice(offered.revision, language)
            .expect("current explicit choice must commit");
        assert_only_commit(&actions, selected, false);
        assert_eq!(ml.current_word(), "");
        assert_eq!(ml.debug_state(), (false, false, false));
        assert!(ml.language_choice_offer().is_none());
        assert_eq!(ml.metrics().summary().total_commits, 1);
        assert_eq!(ml.metrics().summary().accepted_predictions, 0);
        assert_eq!(
            profile_bits(ml.light_profile()),
            profile_bits(&expected_profile)
        );
        assert!(ml
            .accept_language_choice(offered.revision, language)
            .is_none());
        assert_eq!(
            ml.handle_key(event(Key::RawCode(57)))
                .iter()
                .filter(|action| matches!(action, Action::CommitText(_)))
                .count(),
            0
        );
        assert_eq!(ml.metrics().summary().total_commits, 1);
        assert_eq!(
            profile_bits(ml.light_profile()),
            profile_bits(&expected_profile)
        );
    }
}

#[test]
fn language_choice_space_keeps_current_reading_even_with_space_accept_enabled() {
    for dismissal in [false, true] {
        let mut ml = fixture(true, true, true);
        type_preedit(&mut ml, "wi");
        assert_eq!(
            ml.ghost_text(),
            "th",
            "Space must have an actual completion to reject"
        );
        let offered = offer(&ml, "wi", "ви");
        let current = ml.current_word().to_string();
        if dismissal {
            // The adapter consumes choice-Escape through this API. Sending
            // legacy Core Escape instead would test a different cancel path.
            assert!(ml.dismiss_language_choice(offered.revision));
            assert!(ml.language_choice_offer().is_none());
            assert!(!ml.dismiss_language_choice(offered.revision));
        }
        let actions = ml.handle_key(event(Key::RawCode(57)));
        assert_only_commit(&actions, &current, true);
        assert_eq!(ml.metrics().summary().total_commits, 1);
        assert_eq!(ml.metrics().summary().accepted_predictions, 0);
        assert!(ml.language_choice_offer().is_none());
    }
}

#[test]
fn language_choice_support_loss_does_not_reinterpret_the_offered_token() {
    let mut ml = fixture(true, true, true);
    type_preedit(&mut ml, "wi");
    let offered = offer(&ml, "wi", "ви");
    let current = ml.current_word().to_string();
    ml.set_language_choice_supported(false);
    assert!(ml.language_choice_offer().is_none());
    assert!(ml
        .accept_language_choice(offered.revision, LangId::Bg)
        .is_none());
    assert_only_commit(&ml.handle_key(event(Key::RawCode(57))), &current, true);
    assert_eq!(ml.metrics().summary().accepted_predictions, 0);
}

#[derive(Debug, Clone, Copy)]
enum Invalidation {
    Edit,
    Context,
    EraseAndRetype,
    FocusLost,
    FocusGained,
    Reset,
    Support,
    Corpus,
    Capability,
}

fn invalidate(ml: &mut MasterLoop, reason: Invalidation) {
    match reason {
        Invalidation::Edit => {
            ml.handle_key(event(Key::RawCode(raw_code('x'))));
        }
        Invalidation::Context => ml.set_surrounding_text(Some("I want to thank ".into()), Some(16)),
        Invalidation::EraseAndRetype => {
            for _ in 0.."draftyt".chars().count() {
                ml.handle_key(event(Key::RawCode(14)));
            }
            assert_eq!(ml.current_word(), "");
            type_preedit(ml, "draftyt");
        }
        Invalidation::FocusLost => {
            ml.focus_lost();
        }
        Invalidation::FocusGained => ml.focus_gained(),
        Invalidation::Reset => {
            ml.reset();
        }
        Invalidation::Support => {
            ml.set_language_choice_supported(false);
            ml.set_language_choice_supported(true);
        }
        Invalidation::Corpus => ml.load_word_lang("another", 99, LangId::En),
        Invalidation::Capability => ml.set_external_rewrite_supported(false),
    }
}

#[test]
fn language_choice_stale_revisions_never_commit_or_learn_on_a_later_state() {
    for reason in [
        Invalidation::Edit,
        Invalidation::Context,
        Invalidation::EraseAndRetype,
        Invalidation::FocusLost,
        Invalidation::FocusGained,
        Invalidation::Reset,
        Invalidation::Support,
        Invalidation::Corpus,
        Invalidation::Capability,
    ] {
        let mut ml = fixture(true, false, false);
        type_preedit(&mut ml, "draftyt");
        let old = offer(&ml, "draftyt", "драфтът");
        invalidate(&mut ml, reason);
        let word_after_mutation = ml.current_word().to_string();
        let profile_after_mutation = profile_bits(ml.light_profile());
        let commits_after_mutation = ml.metrics().summary().total_commits;
        assert!(
            ml.accept_language_choice(old.revision, LangId::Bg)
                .is_none(),
            "{reason:?}"
        );
        assert_eq!(ml.current_word(), word_after_mutation, "{reason:?}");
        assert_eq!(
            profile_bits(ml.light_profile()),
            profile_after_mutation,
            "{reason:?}"
        );
        assert_eq!(
            ml.metrics().summary().total_commits,
            commits_after_mutation,
            "{reason:?}"
        );
        if let Some(fresh) = ml.language_choice_offer() {
            assert_ne!(
                fresh.revision, old.revision,
                "{reason:?} reused an offer identity"
            );
        }
    }
}

#[test]
fn language_choice_identical_context_and_getters_preserve_offer_identity() {
    let mut ml = fixture(true, false, false);
    type_preedit(&mut ml, "draftyt");
    let first = offer(&ml, "draftyt", "драфтът");
    for _ in 0..3 {
        ml.set_surrounding_text(Some(CONTEXT.into()), Some(CONTEXT.chars().count()));
        assert_eq!(ml.language_choice_offer(), Some(first.clone()));
    }
}

#[test]
fn language_choice_defaults_off_and_keeps_existing_boundary_resolution() {
    let mut ml = fixture(false, false, false);
    type_preedit(&mut ml, "wi");
    assert_eq!(ml.current_word(), "wi");
    assert!(ml.language_choice_offer().is_none());
    assert_only_commit(&ml.handle_key(event(Key::RawCode(57))), "ви", true);
}

#[test]
fn language_choice_excludes_unowned_mixed_technical_short_and_one_sided_tokens() {
    let mut semantic = fixture(true, false, false);
    for ch in "literal".chars() {
        semantic.handle_key(event(Key::Char(ch)));
    }
    assert!(semantic.language_choice_offer().is_none());

    let mut mixed = fixture(true, false, false);
    type_preedit(&mut mixed, "wi");
    mixed.handle_key(event(Key::Char('a')));
    assert!(
        !mixed.debug_state().0,
        "semantic handoff must have delivered the preedit"
    );
    assert!(mixed.language_choice_offer().is_none());

    let mut technical = fixture(true, false, false);
    for ch in "foo2".chars() {
        technical.handle_key(event(Key::RawCode(raw_code(ch))));
    }
    assert!(technical.language_choice_offer().is_none());

    let mut short = fixture(true, false, false);
    type_preedit(&mut short, "a");
    assert!(short.language_choice_offer().is_none());

    let mut one_sided = fixture(true, false, false);
    type_preedit(&mut one_sided, "with");
    assert!(one_sided.language_choice_offer().is_none());
}

#[test]
fn language_choice_excludes_a_surviving_explicit_partial_right_choice() {
    let mut ml = fixture(true, true, false);
    ml.load_word_lang("wit", 12_302, LangId::En);
    ml.load_word_lang("вит", 2_137_962, LangId::Bg);
    type_preedit(&mut ml, "wi");
    assert_eq!(ml.ghost_text(), "th");
    ml.handle_key(event(Key::Right));
    assert_eq!(ml.current_word(), "wit");
    assert!(ml.debug_state().1);
    assert!(ml.language_choice_offer().is_none());
    assert_only_commit(&ml.handle_key(event(Key::RawCode(57))), "wit", true);
}

#[test]
fn language_choice_unselected_tab_keeps_the_existing_visible_ghost_acceptance() {
    let mut ml = fixture(true, true, false);
    type_preedit(&mut ml, "wi");
    assert_eq!(ml.ghost_text(), "th");
    offer(&ml, "wi", "ви");
    let actions = ml.handle_key(event(Key::Tab));
    assert_only_commit(&actions, "with", false);
    assert_eq!(ml.metrics().summary().total_commits, 1);
    assert_eq!(ml.metrics().summary().accepted_predictions, 1);
}

fn correction_fixture() -> MasterLoop {
    let mut ml = MasterLoop::new(InputConfig {
        ghost_text: false,
        space_accept: true,
        post_commit_autocorrect: true,
        ..InputConfig::default()
    });
    ml.load_word_lang("seed", 1_000, LangId::En);
    ml.set_language_choice_supported(true);
    // A genuine prior commit makes these language-policy checks middle-word
    // cases, independent of the opt-in first-word capitalization feature.
    type_preedit(&mut ml, "seed");
    let seed_actions = ml.handle_key(event(Key::RawCode(57)));
    assert_eq!(
        seed_actions
            .iter()
            .filter(|action| matches!(action,
        Action::CommitText(text) if text == "seed"))
            .count(),
        1
    );
    assert_eq!(ml.metrics().summary().total_commits, 1);
    ml
}

#[test]
fn language_choice_ambiguous_space_does_not_postcorrect_a_supported_literal_reading() {
    for dismissal in [false, true] {
        let mut ml = correction_fixture();
        // Prefix support favors the literal EN reading; exact frequency ratio
        // would trigger the old post-commit language correction to the BG row.
        ml.load_word_lang("tnames", 900_000, LangId::En);
        ml.load_word_lang("tn", 48, LangId::En);
        ml.load_word_lang("тн", 5_128, LangId::Bg);
        type_preedit(&mut ml, "tn");
        assert_eq!(ml.current_word(), "tn");
        let offered = offer(&ml, "tn", "тн");
        if dismissal {
            assert!(ml.dismiss_language_choice(offered.revision));
        }
        let mut expected_profile = ml.light_profile().clone();
        expected_profile.observe_lang(LangId::En);
        assert_only_commit(&ml.handle_key(event(Key::RawCode(57))), "tn", true);
        assert_eq!(ml.metrics().summary().total_commits, 2);
        assert_eq!(ml.metrics().summary().accepted_predictions, 0);
        assert_eq!(
            profile_bits(ml.light_profile()),
            profile_bits(&expected_profile)
        );
    }
}

#[test]
fn language_choice_early_ambiguity_keeps_one_sided_final_correction_available() {
    let mut ml = correction_fixture();
    for (word, frequency) in [
        ("status", 900_000),
        ("static", 600_000),
        ("station", 500_000),
    ] {
        ml.load_word_lang(word, frequency, LangId::En);
    }
    // EN keeps prefix support through "stat" (status/static/station), so
    // inferred-prefix recovery cannot make this boundary test pass early.
    // Only the completed BG reading has exact support.
    ml.load_word_lang("стат", 50_000, LangId::Bg);
    type_preedit(&mut ml, "st");
    let early = offer(&ml, "st", "ст");
    assert!(ml.dismiss_language_choice(early.revision));
    type_preedit(&mut ml, "at");
    assert_eq!(ml.current_word(), "stat");
    assert!(
        ml.debug_state().1,
        "strong EN prefix must really survive locked"
    );
    assert!(
        ml.language_choice_offer().is_none(),
        "one-sided terminal evidence is not ambiguous"
    );
    assert_only_commit(&ml.handle_key(event(Key::RawCode(57))), "стат", true);
    assert_eq!(ml.metrics().summary().total_commits, 2);
    assert_eq!(ml.metrics().summary().accepted_predictions, 0);
}
