// Hardware scancode → character mapping for dual-buffer layout-agnostic input.
//
// Maps evdev scancodes (raw hardware codes as delivered by IBus on Wayland)
// to characters for both EN QWERTY and BG Phonetic layouts.  This enables
// the dual-buffer engine to produce two interpretations of every physical
// keypress without knowing which OS layout is active.
//
// NOTE: evdev codes, NOT XKB (XKB = evdev + 8).  The Python IBus adapter
// normalises X11 keycodes (XKB) by subtracting 8 before calling Rust.

/// Keyboard layout identity for scancode resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    En,
    Bg,
}

/// Map an evdev scancode + shift state to a character for the given layout.
///
/// Returns `None` for non-character keys (modifiers, function keys, arrows, etc.).
pub fn scancode_to_char(code: u16, shift: bool, layout: Layout) -> Option<char> {
    match layout {
        Layout::En => en_qwerty(code, shift),
        Layout::Bg => bg_phonetic(code, shift),
    }
}

/// Map a scancode to both EN and BG interpretations simultaneously.
///
/// Returns `None` if the scancode doesn't map to a character on either layout.
pub fn scancode_to_both(code: u16, shift: bool) -> Option<(char, char)> {
    let en = en_qwerty(code, shift)?;
    let bg = bg_phonetic(code, shift)?;
    Some((en, bg))
}

/// Returns `true` if the scancode maps to a letter key (A-Z), where EN and BG
/// layouts produce different characters. Non-letter keys (numbers, punctuation)
/// produce identical output on both layouts and don't need dual interpretation.
pub fn is_alpha_scancode(code: u16) -> bool {
    matches!(
        code,
        16..=25 | 30..=38 | 44..=50 // Q-P, A-L, Z-M rows (evdev)
    )
}

/// Resolve a scancode to a platform-neutral special key, if applicable.
///
/// Used by the RawCode handler to route special keys through the existing
/// `handle_key` logic (Tab, Backspace, Space, etc.) rather than the dual buffer.
pub fn scancode_to_special(code: u16) -> Option<SpecialKey> {
    match code {
        1 => Some(SpecialKey::Escape),
        14 => Some(SpecialKey::Backspace),
        15 => Some(SpecialKey::Tab),
        28 => Some(SpecialKey::Return),
        57 => Some(SpecialKey::Space),
        102 => Some(SpecialKey::Home),
        103 => Some(SpecialKey::Up),
        104 => Some(SpecialKey::PageUp),
        105 => Some(SpecialKey::Left),
        106 => Some(SpecialKey::Right),
        107 => Some(SpecialKey::End),
        108 => Some(SpecialKey::Down),
        109 => Some(SpecialKey::PageDown),
        _ => None,
    }
}

/// Special keys identifiable by scancode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecialKey {
    Escape,
    Tab,
    Backspace,
    Return,
    Space,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
}

// ======================================================================
// US QWERTY layout (evdev scancodes)
// ======================================================================

fn en_qwerty(code: u16, shift: bool) -> Option<char> {
    let (lower, upper) = match code {
        // Number row
        2 => ('1', '!'),
        3 => ('2', '@'),
        4 => ('3', '#'),
        5 => ('4', '$'),
        6 => ('5', '%'),
        7 => ('6', '^'),
        8 => ('7', '&'),
        9 => ('8', '*'),
        10 => ('9', '('),
        11 => ('0', ')'),
        12 => ('-', '_'),
        13 => ('=', '+'),
        // QWERTY row
        16 => ('q', 'Q'),
        17 => ('w', 'W'),
        18 => ('e', 'E'),
        19 => ('r', 'R'),
        20 => ('t', 'T'),
        21 => ('y', 'Y'),
        22 => ('u', 'U'),
        23 => ('i', 'I'),
        24 => ('o', 'O'),
        25 => ('p', 'P'),
        26 => ('[', '{'),
        27 => (']', '}'),
        // Home row
        30 => ('a', 'A'),
        31 => ('s', 'S'),
        32 => ('d', 'D'),
        33 => ('f', 'F'),
        34 => ('g', 'G'),
        35 => ('h', 'H'),
        36 => ('j', 'J'),
        37 => ('k', 'K'),
        38 => ('l', 'L'),
        39 => (';', ':'),
        40 => ('\'', '"'),
        41 => ('`', '~'),
        // Bottom row
        43 => ('\\', '|'),
        44 => ('z', 'Z'),
        45 => ('x', 'X'),
        46 => ('c', 'C'),
        47 => ('v', 'V'),
        48 => ('b', 'B'),
        49 => ('n', 'N'),
        50 => ('m', 'M'),
        51 => (',', '<'),
        52 => ('.', '>'),
        53 => ('/', '?'),
        _ => return None,
    };
    Some(if shift { upper } else { lower })
}

// ======================================================================
// BG Phonetic layout (evdev scancodes)
// ======================================================================
//
// Letter keys use the standard BG Phonetic mapping (consistent with
// `lang_detect::phonetic_map`). Non-letter keys (numbers, punctuation)
// return the same characters as EN QWERTY — those keys don't vary
// between layouts for our dual-buffer purposes.

fn bg_phonetic(code: u16, shift: bool) -> Option<char> {
    let (lower, upper) = match code {
        // Number row — same as EN QWERTY
        2 => ('1', '!'),
        3 => ('2', '@'),
        4 => ('3', '#'),
        5 => ('4', '$'),
        6 => ('5', '%'),
        7 => ('6', '^'),
        8 => ('7', '&'),
        9 => ('8', '*'),
        10 => ('9', '('),
        11 => ('0', ')'),
        12 => ('-', '_'),
        13 => ('=', '+'),
        // QWERTY row → Cyrillic phonetic
        16 => ('я', 'Я'), // q → я
        17 => ('ш', 'Ш'), // w → ш
        18 => ('е', 'Е'), // e → е
        19 => ('р', 'Р'), // r → р
        20 => ('т', 'Т'), // t → т
        21 => ('ъ', 'Ъ'), // y → ъ
        22 => ('у', 'У'), // u → у
        23 => ('и', 'И'), // i → и
        24 => ('о', 'О'), // o → о
        25 => ('п', 'П'), // p → п
        26 => ('[', '{'), // brackets — same as EN
        27 => (']', '}'),
        // Home row → Cyrillic phonetic
        30 => ('а', 'А'), // a → а
        31 => ('с', 'С'), // s → с
        32 => ('д', 'Д'), // d → д
        33 => ('ф', 'Ф'), // f → ф
        34 => ('г', 'Г'), // g → г
        35 => ('х', 'Х'), // h → х
        36 => ('й', 'Й'), // j → й
        37 => ('к', 'К'), // k → к
        38 => ('л', 'Л'), // l → л
        39 => (';', ':'), // punctuation — same as EN
        40 => ('\'', '"'),
        41 => ('`', '~'),
        // Bottom row → Cyrillic phonetic
        43 => ('\\', '|'), // backslash — same as EN
        44 => ('з', 'З'),  // z → з
        45 => ('ь', 'Ь'),  // x → ь
        46 => ('ц', 'Ц'),  // c → ц
        47 => ('в', 'В'),  // v → в
        48 => ('б', 'Б'),  // b → б
        49 => ('н', 'Н'),  // n → н
        50 => ('м', 'М'),  // m → м
        51 => (',', '<'),  // comma — same as EN
        52 => ('.', '>'),
        53 => ('/', '?'),
        _ => return None,
    };
    Some(if shift { upper } else { lower })
}

// ======================================================================
// Tests
// ======================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_en_letters() {
        assert_eq!(scancode_to_char(16, false, Layout::En), Some('q'));
        assert_eq!(scancode_to_char(16, true, Layout::En), Some('Q'));
        assert_eq!(scancode_to_char(30, false, Layout::En), Some('a'));
        assert_eq!(scancode_to_char(44, false, Layout::En), Some('z'));
    }

    #[test]
    fn test_bg_phonetic_letters() {
        assert_eq!(scancode_to_char(16, false, Layout::Bg), Some('я'));
        assert_eq!(scancode_to_char(16, true, Layout::Bg), Some('Я'));
        assert_eq!(scancode_to_char(30, false, Layout::Bg), Some('а'));
        assert_eq!(scancode_to_char(44, false, Layout::Bg), Some('з'));
        assert_eq!(scancode_to_char(35, false, Layout::Bg), Some('х'));
    }

    #[test]
    fn test_numbers_same_on_both() {
        for code in 2..=11 {
            let en = scancode_to_char(code, false, Layout::En);
            let bg = scancode_to_char(code, false, Layout::Bg);
            assert_eq!(
                en, bg,
                "number scancode {code} should be same on both layouts"
            );
        }
    }

    #[test]
    fn test_scancode_to_both() {
        let (en, bg) = scancode_to_both(35, false).unwrap(); // 'h' key
        assert_eq!(en, 'h');
        assert_eq!(bg, 'х');

        let (en, bg) = scancode_to_both(2, false).unwrap(); // '1' key
        assert_eq!(en, '1');
        assert_eq!(bg, '1');
    }

    #[test]
    fn test_is_alpha_scancode() {
        assert!(is_alpha_scancode(16)); // Q
        assert!(is_alpha_scancode(30)); // A
        assert!(is_alpha_scancode(44)); // Z
        assert!(!is_alpha_scancode(2)); // 1
        assert!(!is_alpha_scancode(57)); // Space
        assert!(!is_alpha_scancode(1)); // Escape
    }

    #[test]
    fn test_scancode_to_special() {
        assert_eq!(scancode_to_special(1), Some(SpecialKey::Escape));
        assert_eq!(scancode_to_special(15), Some(SpecialKey::Tab));
        assert_eq!(scancode_to_special(14), Some(SpecialKey::Backspace));
        assert_eq!(scancode_to_special(57), Some(SpecialKey::Space));
        assert_eq!(scancode_to_special(28), Some(SpecialKey::Return));
        assert_eq!(scancode_to_special(102), Some(SpecialKey::Home));
        assert_eq!(scancode_to_special(103), Some(SpecialKey::Up));
        assert_eq!(scancode_to_special(104), Some(SpecialKey::PageUp));
        assert_eq!(scancode_to_special(105), Some(SpecialKey::Left));
        assert_eq!(scancode_to_special(106), Some(SpecialKey::Right));
        assert_eq!(scancode_to_special(107), Some(SpecialKey::End));
        assert_eq!(scancode_to_special(108), Some(SpecialKey::Down));
        assert_eq!(scancode_to_special(109), Some(SpecialKey::PageDown));
        assert_eq!(scancode_to_special(16), None); // Q — not special
    }

    #[test]
    fn test_unknown_scancode_returns_none() {
        assert_eq!(scancode_to_char(200, false, Layout::En), None);
        assert_eq!(scancode_to_both(200, false), None);
    }

    #[test]
    fn test_phonetic_consistency() {
        // Verify BG phonetic mapping matches lang_detect::phonetic_map for all letters.
        let letter_scancodes = [
            (16, 'q', 'я'),
            (17, 'w', 'ш'),
            (18, 'e', 'е'),
            (19, 'r', 'р'),
            (20, 't', 'т'),
            (21, 'y', 'ъ'),
            (22, 'u', 'у'),
            (23, 'i', 'и'),
            (24, 'o', 'о'),
            (25, 'p', 'п'),
            (30, 'a', 'а'),
            (31, 's', 'с'),
            (32, 'd', 'д'),
            (33, 'f', 'ф'),
            (34, 'g', 'г'),
            (35, 'h', 'х'),
            (36, 'j', 'й'),
            (37, 'k', 'к'),
            (38, 'l', 'л'),
            (44, 'z', 'з'),
            (45, 'x', 'ь'),
            (46, 'c', 'ц'),
            (47, 'v', 'в'),
            (48, 'b', 'б'),
            (49, 'n', 'н'),
            (50, 'm', 'м'),
        ];
        for (code, expected_en, expected_bg) in letter_scancodes {
            let en = scancode_to_char(code, false, Layout::En).unwrap();
            let bg = scancode_to_char(code, false, Layout::Bg).unwrap();
            assert_eq!(en, expected_en, "EN mismatch for scancode {code}");
            assert_eq!(bg, expected_bg, "BG mismatch for scancode {code}");
        }
    }

    #[test]
    fn test_shift_variants_bg() {
        assert_eq!(scancode_to_char(30, true, Layout::Bg), Some('А'));
        assert_eq!(scancode_to_char(44, true, Layout::Bg), Some('З'));
        assert_eq!(scancode_to_char(17, true, Layout::Bg), Some('Ш'));
    }
}
