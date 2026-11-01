//! Platform-independent parts of the core-to-TSF text contract.

use smartkey_core::input::{Action, Key, KeyEvent, Modifiers};
use smartkey_core::MasterLoop;

pub fn handle_key(core: &mut MasterLoop, event: KeyEvent) -> Vec<Action> {
    if bypasses_core(&event.key, event.modifiers) {
        return vec![Action::ForwardKey];
    }
    if core.is_enabled() && is_editing_shortcut(&event.key, event.modifiers) {
        // Deliver only the typed prefix before the application edits or moves
        // it. A shortcut is not acceptance of a prediction or a learning event.
        let mut actions = core.handoff_preedit_for_delivery();
        actions.extend(core.reset());
        actions.push(Action::ForwardKey);
        return actions;
    }
    core.handle_key(event)
}

fn bypasses_core(key: &Key, modifiers: Modifiers) -> bool {
    modifiers.contains(Modifiers::RELEASE)
        // Windows virtual keys, not the X11 modifier keysyms used by the core.
        || matches!(key, Key::Other(0 | 0x10..=0x12 | 0x14 | 0x5b | 0x5c | 0x90 | 0x91 | 0xa0..=0xa5))
        // ToUnicodeEx can resolve AltGr as a printable character with Ctrl+Alt.
        // Leave that character to the layout instead of treating it as a chord.
        || (modifiers.contains(Modifiers::CTRL | Modifiers::ALT)
            && matches!(key, Key::Char(_)))
}

fn is_editing_shortcut(key: &Key, modifiers: Modifiers) -> bool {
    modifiers.intersects(Modifiers::CTRL | Modifiers::ALT | Modifiers::SUPER)
        && !(matches!(key, Key::Escape) && modifiers.contains(Modifiers::SUPER))
}

/// TSF offsets count UTF-16 units; ReplaceWord counts Unicode scalar values.
/// Validate only the requested suffix: a bounded lookback can start mid-pair.
pub fn suffix_units(text: &[u16], scalars: usize) -> Option<usize> {
    let mut start = text.len();
    for _ in 0..scalars {
        start = start.checked_sub(1)?;
        match text[start] {
            0xDC00..=0xDFFF => {
                start = start.checked_sub(1)?;
                if !(0xD800..=0xDBFF).contains(&text[start]) {
                    return None;
                }
            }
            0xD800..=0xDBFF => return None,
            _ => {}
        }
    }
    Some(text.len() - start)
}

pub fn consumes_key(actions: &[Action]) -> bool {
    !actions.is_empty()
        && !actions
            .iter()
            .any(|action| matches!(action, Action::ForwardKey))
}

pub fn claims_key(key: &Key, mods: Modifiers, enabled: bool, pending: bool) -> bool {
    if bypasses_core(key, mods) {
        return false;
    }
    if matches!(key, Key::Escape) && mods.contains(Modifiers::SUPER) {
        return true;
    }
    if !enabled {
        return false;
    }
    if pending && is_editing_shortcut(key, mods) {
        // Ctrl+letter often resolves to Other(VK_*) because its Unicode result
        // is a control character. It still needs OnKeyDown to retire preedit.
        return true;
    }
    match key {
        Key::Tab
        | Key::Right
        | Key::Escape
        | Key::Left
        | Key::Up
        | Key::Down
        | Key::Home
        | Key::End
        | Key::PageUp
        | Key::PageDown => pending,
        Key::Space | Key::Return | Key::Backspace | Key::Char(_) | Key::RawCode(_) => true,
        Key::Other(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smartkey_core::input::InputConfig;
    use smartkey_core::lang_detect::LangId;

    fn prefix(raw: bool) -> MasterLoop {
        let mut core = MasterLoop::new(InputConfig {
            ghost_text_separation_margin: 0.0,
            use_ppm: false,
            use_reranker: false,
            ..InputConfig::default()
        });
        core.load_word_lang("здравей", 1_000_000, LangId::Bg);
        let keys: Vec<Key> = if raw {
            [44, 32, 19, 30].into_iter().map(Key::RawCode).collect()
        } else {
            "здра".chars().map(Key::Char).collect()
        };
        for key in keys {
            handle_key(
                &mut core,
                KeyEvent {
                    key,
                    modifiers: Modifiers::empty(),
                },
            );
        }
        assert_eq!(core.current_word(), "здра");
        assert_eq!(core.ghost_text(), "вей");
        core
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

    #[test]
    fn shortcut_test_callback_claims_control_character_virtual_keys_with_pending_text() {
        for (key, modifiers) in [
            (Key::Other(0x43), Modifiers::CTRL), // ToUnicodeEx filters Ctrl+C's control character.
            (Key::Other(0x56), Modifiers::CTRL),
            (Key::Other(0x53), Modifiers::CTRL | Modifiers::SHIFT),
            (Key::Other(0x44), Modifiers::SUPER),
        ] {
            assert!(
                claims_key(&key, modifiers, true, true),
                "{key:?} {modifiers:?}"
            );
            assert!(!claims_key(&key, modifiers, true, false));
            assert!(!claims_key(&key, modifiers, false, true));
        }
        assert!(!claims_key(
            &Key::Char('€'),
            Modifiers::CTRL | Modifiers::ALT,
            true,
            true
        ));
    }

    #[test]
    fn editing_shortcuts_deliver_only_typed_text_then_retire_without_learning() {
        for raw in [false, true] {
            for (key, modifiers) in [
                (Key::Other(0x43), Modifiers::CTRL),
                (Key::Other(0x56), Modifiers::CTRL),
                (Key::Char('c'), Modifiers::CTRL),
                (Key::Space, Modifiers::CTRL),
                (Key::Tab, Modifiers::ALT),
                (Key::Other(0x44), Modifiers::SUPER),
            ] {
                let mut core = prefix(raw);
                let metrics = core.metrics().summary();
                let prior = core.light_profile().lang_prior;
                let event = KeyEvent { key, modifiers };
                let actions = handle_key(&mut core, event.clone());
                assert_eq!(
                    commits(&actions),
                    if raw { "здра" } else { "" },
                    "{event:?}: {actions:?}"
                );
                assert!(
                    !consumes_key(&actions),
                    "the original chord belongs to the application"
                );
                assert!(!actions.iter().any(|action| matches!(
                    action,
                    Action::ShowGhost(_)
                        | Action::ShowComposing { .. }
                        | Action::ReplaceWord { .. }
                )));
                assert!(
                    core.current_word().is_empty(),
                    "shortcut must retire the old token"
                );
                assert!(!core.has_ghost());
                assert!(!core.native_completion_owned());
                assert_eq!(
                    core.metrics().summary().total_commits,
                    metrics.total_commits
                );
                assert_eq!(
                    core.metrics().summary().accepted_predictions,
                    metrics.accepted_predictions
                );
                assert_eq!(core.light_profile().lang_prior, prior);
                assert!(
                    commits(&handle_key(&mut core, event)).is_empty(),
                    "repeated shortcut must not duplicate text"
                );
                let next = handle_key(
                    &mut core,
                    KeyEvent {
                        key: Key::RawCode(44),
                        modifiers: Modifiers::empty(),
                    },
                );
                assert!(commits(&next).is_empty());
                assert!(next
                    .iter()
                    .any(|action| matches!(action, Action::ShowComposing { .. })));
            }
        }
    }

    #[test]
    fn modifier_taps_releases_and_altgr_do_not_finalize_pending_text() {
        for (key, modifiers) in [
            (Key::Other(0x10), Modifiers::SHIFT),
            (Key::Other(0x11), Modifiers::CTRL),
            (Key::Other(0x12), Modifiers::ALT),
            (Key::Other(0x5b), Modifiers::SUPER),
            (Key::Other(0xa3), Modifiers::CTRL),
            (Key::Other(0xa5), Modifiers::CTRL | Modifiers::ALT),
            (Key::Other(0x43), Modifiers::CTRL | Modifiers::RELEASE),
            (Key::Char('€'), Modifiers::CTRL | Modifiers::ALT),
        ] {
            let mut core = prefix(true);
            assert!(
                !claims_key(&key, modifiers, true, true),
                "{key:?} {modifiers:?}"
            );
            let actions = handle_key(&mut core, KeyEvent { key, modifiers });
            assert_eq!(actions, [Action::ForwardKey]);
            assert_eq!(core.current_word(), "здра");
            assert_eq!(core.ghost_text(), "вей");
        }
    }

    #[test]
    fn kill_switch_discards_pending_text_and_disabled_shortcuts_stay_forwarded() {
        let mut core = prefix(true);
        let actions = handle_key(
            &mut core,
            KeyEvent {
                key: Key::Escape,
                modifiers: Modifiers::SUPER,
            },
        );
        assert_eq!(actions, [Action::HideGhost]);
        assert!(!core.is_enabled());
        assert!(core.current_word().is_empty());
        assert_eq!(
            handle_key(
                &mut core,
                KeyEvent {
                    key: Key::Other(0x43),
                    modifiers: Modifiers::CTRL
                }
            ),
            [Action::ForwardKey]
        );
    }

    #[test]
    fn replaces_scalar_suffix_without_splitting_surrogates() {
        let text: Vec<u16> = "old здра😀в".encode_utf16().collect();
        assert_eq!(suffix_units(&text, 2), Some(3));
        assert_eq!(suffix_units(&text, 6), Some(7));
        assert_eq!(suffix_units(&text, 0), Some(0));
        assert_eq!(suffix_units(&text, 11), None);
    }

    #[test]
    fn bounded_prefix_may_be_split_but_replaced_suffix_must_be_valid() {
        assert_eq!(suffix_units(&[0xDC00, 0x0061], 1), Some(1));
        assert_eq!(suffix_units(&[0xDC00, 0x0061], 2), None);
        assert_eq!(suffix_units(&[0x0061, 0xD800], 1), None);
        assert_eq!(suffix_units(&[0xD800, 0xDC00], 1), Some(2));
    }

    #[test]
    fn post_commit_correction_does_not_swallow_delimiter() {
        let actions = [
            Action::HideGhost,
            Action::ForwardKey,
            Action::ReplaceWord {
                replace_len: 4,
                text: "Word".into(),
            },
        ];
        assert!(!consumes_key(&actions));
        assert!(consumes_key(&[Action::CommitText("a".into())]));
        assert!(!consumes_key(&[]));
    }

    #[test]
    fn reversible_switch_and_navigation_reach_core_without_prediction() {
        for enabled in [true, false] {
            assert!(claims_key(&Key::Escape, Modifiers::SUPER, enabled, false));
        }
        for key in [Key::Tab, Key::Right, Key::Left, Key::Home, Key::Escape] {
            assert!(claims_key(&key, Modifiers::empty(), true, true));
            assert!(!claims_key(&key, Modifiers::empty(), true, false));
        }
        assert!(!claims_key(
            &Key::Char('a'),
            Modifiers::empty(),
            false,
            true
        ));
    }
}
