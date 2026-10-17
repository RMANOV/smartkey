//! Platform-independent parts of the core-to-TSF text contract.

use smartkey_core::input::{Action, Key, Modifiers};

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
    if matches!(key, Key::Escape) && mods.contains(Modifiers::SUPER) {
        return true;
    }
    if !enabled {
        return false;
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
