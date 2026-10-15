//! Physical special keys must reach the orchestrator with the same identity
//! as semantic keys, while releases and Ctrl/Alt shortcuts remain ineligible
//! for adaptation. All data is synthetic and no profile is loaded or saved.

use smartkey_core::context_sampler::ContextSampler;
use smartkey_core::input::{Action, InputConfig, InputMethodCore, Key, KeyEvent, Modifiers};
use smartkey_core::master_loop::{MasterLoop, Phase};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

fn key(key: Key) -> KeyEvent {
    modified(key, Modifiers::empty())
}

fn modified(key: Key, modifiers: Modifiers) -> KeyEvent {
    KeyEvent { key, modifiers }
}

fn config() -> InputConfig {
    InputConfig {
        ghost_text_separation_margin: 0.0,
        use_ppm: false,
        use_reranker: false,
        ..InputConfig::default()
    }
}

fn master_loop() -> MasterLoop {
    let mut master = MasterLoop::new(config());
    master.load_word("здравей", 1_000_000);
    master
}

fn compose(master: &mut MasterLoop) {
    // These letter codes must stay physical until the dual buffer sees them.
    for code in [44, 32, 19, 30] {
        master.handle_key(key(Key::RawCode(code)));
    }
    assert_eq!(master.current_word(), "здра");
    assert_eq!(master.ghost_text(), "вей");
}

fn delete_twice_suppresses(backspace: Key) {
    let mut master = master_loop();
    compose(&mut master);
    master.handle_key(key(backspace.clone()));
    master.handle_key(key(Key::RawCode(30)));
    assert_eq!(master.current_word(), "здра");
    master.handle_key(key(backspace));
    assert!(master.is_ghost_suppressed("здра", "здравей"));
}

#[test]
fn semantic_backspace_twice_suppresses() {
    delete_twice_suppresses(Key::Backspace);
}

#[test]
fn physical_backspace_twice_suppresses() {
    delete_twice_suppresses(Key::RawCode(14));
}

fn tab_accept_updates_profile(tab: Key) {
    let mut master = master_loop();
    compose(&mut master);
    let before = master.light_profile().ghost_accept_rate;
    let actions = master.handle_key(key(tab));
    assert!(actions.contains(&Action::CommitText("здравей".into())));
    assert!(master.light_profile().ghost_accept_rate > before);
}

#[test]
fn semantic_tab_accept_updates_profile() {
    tab_accept_updates_profile(Key::Tab);
}

#[test]
fn physical_tab_accept_updates_profile() {
    tab_accept_updates_profile(Key::RawCode(15));
}

fn navigation_resets_phase(left: Key) {
    let mut master = master_loop();
    compose(&mut master);
    let actions = master.handle_key(key(left));
    assert!(actions.contains(&Action::CommitText("здра".into())));
    assert_eq!(master.phase(), Phase::Anticipating);
}

#[test]
fn semantic_navigation_resets_phase() {
    navigation_resets_phase(Key::Left);
}

#[test]
fn physical_navigation_resets_phase() {
    navigation_resets_phase(Key::RawCode(105));
}

#[test]
fn semantic_kill_switch_disables_and_reenables() {
    let mut core = InputMethodCore::new(config());
    core.handle_key(modified(Key::Escape, Modifiers::SUPER));
    assert!(!core.is_enabled());
    core.handle_key(modified(Key::Escape, Modifiers::SUPER));
    assert!(core.is_enabled());
}

#[test]
fn physical_kill_switch_disables() {
    let mut core = InputMethodCore::new(config());
    core.handle_key(modified(Key::RawCode(1), Modifiers::SUPER));
    assert!(!core.is_enabled());
}

#[test]
fn physical_kill_switch_reenables_disabled_core() {
    let mut core = InputMethodCore::new(config());
    core.handle_key(modified(Key::Escape, Modifiers::SUPER));
    assert!(!core.is_enabled());
    core.handle_key(modified(Key::RawCode(1), Modifiers::SUPER));
    assert!(core.is_enabled());
}

#[test]
fn semantic_kill_release_does_not_reverse_press() {
    let mut core = InputMethodCore::new(config());
    core.handle_key(modified(Key::Escape, Modifiers::SUPER));
    assert!(!core.is_enabled());
    assert_eq!(
        core.handle_key(modified(Key::Escape, Modifiers::SUPER | Modifiers::RELEASE)),
        vec![Action::ForwardKey]
    );
    assert!(!core.is_enabled());
}

#[test]
fn semantic_backspace_release_does_not_reject() {
    let mut master = master_loop();
    compose(&mut master);
    master.handle_key(modified(Key::Backspace, Modifiers::RELEASE));
    assert_eq!(master.rejection_prefix_count(), 0);
    assert_eq!(master.current_word(), "здра");
}

#[test]
fn ctrl_letter_shortcut_preserves_composition() {
    let mut master = master_loop();
    compose(&mut master);
    assert_eq!(
        master.handle_key(modified(Key::RawCode(46), Modifiers::CTRL)),
        vec![Action::ForwardKey]
    );
    assert_eq!(master.current_word(), "здра");
}

#[test]
fn physical_kill_press_release_toggles_once_in_core_and_master() {
    let mut core = InputMethodCore::new(config());
    let mut master = master_loop();
    for expected in [false, true] {
        core.handle_key(modified(Key::RawCode(1), Modifiers::SUPER));
        master.handle_key(modified(Key::RawCode(1), Modifiers::SUPER));
        assert_eq!(core.is_enabled(), expected);
        assert_eq!(master.is_enabled(), expected);
        let release = modified(Key::RawCode(1), Modifiers::SUPER | Modifiers::RELEASE);
        assert_eq!(core.handle_key(release.clone()), vec![Action::ForwardKey]);
        assert_eq!(master.handle_key(release), vec![Action::ForwardKey]);
        assert_eq!(core.is_enabled(), expected);
        assert_eq!(master.is_enabled(), expected);
    }
}

#[test]
fn release_ctrl_and_alt_specials_preserve_composition_and_feedback() {
    for modifiers in [Modifiers::RELEASE, Modifiers::CTRL, Modifiers::ALT] {
        for special in [
            Key::Backspace,
            Key::RawCode(14),
            Key::Tab,
            Key::RawCode(15),
            Key::Left,
            Key::RawCode(105),
            Key::Escape,
            Key::RawCode(1),
        ] {
            let mut master = master_loop();
            compose(&mut master);
            let accept_rate = master.light_profile().ghost_accept_rate;
            assert_eq!(
                master.handle_key(modified(special.clone(), modifiers)),
                vec![Action::ForwardKey],
                "{special:?} with {modifiers:?}"
            );
            assert_eq!(master.current_word(), "здра");
            assert_eq!(master.ghost_text(), "вей");
            assert_eq!(master.phase(), Phase::Tracking);
            assert_eq!(master.rejection_prefix_count(), 0);
            assert_eq!(master.light_profile().ghost_accept_rate, accept_rate);
        }
    }
}

#[test]
fn release_ctrl_and_alt_events_do_not_start_anticipation() {
    for modifiers in [Modifiers::RELEASE, Modifiers::CTRL, Modifiers::ALT] {
        let mut master = master_loop();
        assert_eq!(
            master.handle_key(modified(Key::RawCode(14), modifiers)),
            vec![Action::ForwardKey]
        );
        assert_eq!(master.phase(), Phase::Anticipating);
    }
}

#[test]
fn physical_terminal_accept_then_backspace_records_one_rejection() {
    for tab_accept in [true, false] {
        let mut master = master_loop();
        compose(&mut master);
        let before = master.light_profile().ghost_accept_rate;
        let accept = if tab_accept {
            master.handle_key(key(Key::RawCode(15)))
        } else {
            for expected_prefix in ["здрав", "здраве"] {
                master.handle_key(key(Key::RawCode(106)));
                assert_eq!(master.current_word(), expected_prefix);
                assert_eq!(master.light_profile().ghost_accept_rate, before);
            }
            master.handle_key(key(Key::RawCode(106)))
        };
        assert!(accept.contains(&Action::CommitText("здравей".into())));
        assert!(master.light_profile().ghost_accept_rate > before);
        assert_eq!(master.rejection_prefix_count(), 0);
        // The matching release must not discard the accepted-then-deleted
        // attribution before the next actual press.
        master.handle_key(modified(
            Key::RawCode(if tab_accept { 15 } else { 106 }),
            Modifiers::RELEASE,
        ));
        master.handle_key(key(Key::RawCode(14)));
        assert_eq!(master.rejection_prefix_count(), 1);
    }
}

struct CountingSampler(Arc<AtomicUsize>);

impl ContextSampler for CountingSampler {
    fn get_surrounding_text(&self, _max_chars: usize) -> Option<String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        None
    }
}

#[test]
fn physical_navigation_resamples_context_before_the_next_word() {
    for code in [102, 103, 104, 105, 107, 108, 109] {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut master =
            MasterLoop::with_context_sampler(config(), Box::new(CountingSampler(calls.clone())));
        master.load_word("здравей", 1_000_000);
        compose(&mut master);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        master.handle_key(key(Key::RawCode(code)));
        assert_eq!(master.phase(), Phase::Anticipating, "scancode {code}");
        master.handle_key(key(Key::RawCode(44)));
        assert_eq!(calls.load(Ordering::SeqCst), 2, "scancode {code}");
    }
}

#[test]
fn semantic_and_physical_space_keep_display_attestation_and_modifier_rules() {
    for space in [Key::Space, Key::RawCode(57)] {
        for modifiers in [
            Modifiers::empty(),
            Modifiers::CAPS_LOCK,
            Modifiers::SHIFT,
            Modifiers::SUPER,
            Modifiers::CTRL,
            Modifiers::ALT,
            Modifiers::RELEASE,
        ] {
            let mut master = MasterLoop::new(InputConfig {
                space_accept: true,
                ..config()
            });
            master.load_word("здравей", 1_000_000);
            compose(&mut master);
            let before = master.light_profile().ghost_accept_rate;
            let actions = master.handle_key(modified(space.clone(), modifiers));
            if modifiers.intersects(Modifiers::CTRL | Modifiers::ALT | Modifiers::RELEASE) {
                assert_eq!(actions, vec![Action::ForwardKey]);
                assert_eq!(master.current_word(), "здра");
                assert_eq!(master.light_profile().ghost_accept_rate, before);
            } else if modifiers.intersects(Modifiers::SHIFT | Modifiers::SUPER) {
                assert!(actions.contains(&Action::CommitText("здра".into())));
                assert!(actions.contains(&Action::ForwardKey));
                assert!(!actions.contains(&Action::CommitText("здравей ".into())));
                assert_eq!(master.light_profile().ghost_accept_rate, before);
            } else {
                assert!(actions.contains(&Action::CommitText("здравей ".into())));
                assert!(!actions.contains(&Action::ForwardKey));
                assert!(master.light_profile().ghost_accept_rate > before);
            }
        }
    }
}

#[test]
fn shortcut_press_invalidates_immediate_accept_then_delete_attribution() {
    for tab in [Key::Tab, Key::RawCode(15)] {
        for backspace in [Key::Backspace, Key::RawCode(14)] {
            for modifiers in [Modifiers::CTRL, Modifiers::ALT] {
                let mut master = master_loop();
                compose(&mut master);
                let actions = master.handle_key(key(tab.clone()));
                assert!(actions.contains(&Action::CommitText("здравей".into())));
                assert_eq!(master.rejection_prefix_count(), 0);
                // Ctrl+V/Alt+V can change client text outside the composition.
                // A later deletion is no longer attributable to the accept.
                assert_eq!(
                    master.handle_key(modified(Key::RawCode(47), modifiers)),
                    vec![Action::ForwardKey]
                );
                master.handle_key(key(backspace.clone()));
                assert_eq!(master.rejection_prefix_count(), 0, "{tab:?}, {modifiers:?}");
            }
        }
    }
}

#[test]
fn shortcut_press_invalidates_stale_live_ghost_rejection_attribution() {
    for backspace in [Key::Backspace, Key::RawCode(14)] {
        for modifiers in [Modifiers::CTRL, Modifiers::ALT] {
            let mut master = master_loop();
            compose(&mut master);
            master.handle_key(modified(Key::RawCode(47), modifiers));
            master.handle_key(key(backspace.clone()));
            assert_eq!(master.current_word(), "здр");
            assert_eq!(master.rejection_prefix_count(), 0);
        }
    }
}

#[test]
fn shortcut_key_release_preserves_immediate_accept_then_delete_attribution() {
    for backspace in [Key::Backspace, Key::RawCode(14)] {
        for modifiers in [
            Modifiers::RELEASE,
            Modifiers::RELEASE | Modifiers::CTRL,
            Modifiers::RELEASE | Modifiers::ALT,
        ] {
            let mut master = master_loop();
            compose(&mut master);
            master.handle_key(key(Key::RawCode(15)));
            assert_eq!(
                master.handle_key(modified(Key::RawCode(47), modifiers)),
                vec![Action::ForwardKey]
            );
            master.handle_key(key(backspace.clone()));
            assert_eq!(master.rejection_prefix_count(), 1);
        }
    }
}

#[test]
fn space_after_shortcut_requires_a_fresh_display_attestation() {
    for refresh_display in [false, true] {
        let mut master = MasterLoop::new(InputConfig {
            space_accept: true,
            ..config()
        });
        master.load_word("здравей", 1_000_000);
        compose(&mut master);
        assert_eq!(
            master.handle_key(modified(Key::RawCode(46), Modifiers::CTRL)),
            vec![Action::ForwardKey]
        );
        assert_eq!(master.current_word(), "здра");
        assert_eq!(master.ghost_text(), "вей");
        if refresh_display {
            // A real edit and its fresh displayed offer can attest Space again.
            master.handle_key(key(Key::RawCode(14)));
            master.handle_key(key(Key::RawCode(30)));
            assert_eq!(master.current_word(), "здра");
            assert_eq!(master.ghost_text(), "вей");
        }
        let actions = master.handle_key(key(Key::RawCode(57)));
        if refresh_display {
            assert!(actions.contains(&Action::CommitText("здравей ".into())));
            assert!(!actions.contains(&Action::ForwardKey));
        } else {
            assert!(actions.contains(&Action::CommitText("здра".into())));
            assert!(actions.contains(&Action::ForwardKey));
            assert!(!actions.contains(&Action::CommitText("здравей ".into())));
        }
    }
}
