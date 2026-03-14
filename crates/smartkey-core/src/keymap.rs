// Hardware scancode → character mapping for dual-buffer layout-agnostic input.
//
// Maps evdev scancodes (as delivered by IBus/XKB) to characters for both
// EN QWERTY and BG Phonetic layouts. This enables the dual-buffer engine
// to produce two interpretations of every physical keypress without
// knowing which OS layout is active.

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
        24..=33 | 38..=46 | 52..=58 // Q-P, A-L, Z-M rows
    )
}

/// Resolve a scancode to a platform-neutral special key, if applicable.
///
/// Used by the RawCode handler to route special keys through the existing
/// `handle_key` logic (Tab, Backspace, Space, etc.) rather than the dual buffer.
pub fn scancode_to_special(code: u16) -> Option<SpecialKey> {
    match code {
        9 => Some(SpecialKey::Escape),
        15 => Some(SpecialKey::Tab),
        22 => Some(SpecialKey::Backspace),
        36 => Some(SpecialKey::Return),
        65 => Some(SpecialKey::Space),
        114 => Some(SpecialKey::Right),
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
    Right,
}

// ======================================================================
// US QWERTY layout (evdev scancodes)
// ======================================================================

fn en_qwerty(code: u16, shift: bool) -> Option<char> {
    let (lower, upper) = match code {
        // Number row
        10 => ('1', '!'),
        11 => ('2', '@'),
        12 => ('3', '#'),
        13 => ('4', '$'),
        14 => ('5', '%'),
        15 => ('6', '^'),
        16 => ('7', '&'),
        17 => ('8', '*'),
        18 => ('9', '('),
        19 => ('0', ')'),
        20 => ('-', '_'),
        21 => ('=', '+'),
        // QWERTY row
        24 => ('q', 'Q'),
        25 => ('w', 'W'),
        26 => ('e', 'E'),
        27 => ('r', 'R'),
        28 => ('t', 'T'),
        29 => ('y', 'Y'),
        30 => ('u', 'U'),
        31 => ('i', 'I'),
        32 => ('o', 'O'),
        33 => ('p', 'P'),
        34 => ('[', '{'),
        35 => (']', '}'),
        // Home row
        38 => ('a', 'A'),
        39 => ('s', 'S'),
        40 => ('d', 'D'),
        41 => ('f', 'F'),
        42 => ('g', 'G'),
        43 => ('h', 'H'),
        44 => ('j', 'J'),
        45 => ('k', 'K'),
        46 => ('l', 'L'),
        47 => (';', ':'),
        48 => ('\'', '"'),
        49 => ('`', '~'),
        // Bottom row
        51 => ('\\', '|'),
        52 => ('z', 'Z'),
        53 => ('x', 'X'),
        54 => ('c', 'C'),
        55 => ('v', 'V'),
        56 => ('b', 'B'),
        57 => ('n', 'N'),
        58 => ('m', 'M'),
        59 => (',', '<'),
        60 => ('.', '>'),
        61 => ('/', '?'),
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
        10 => ('1', '!'),
        11 => ('2', '@'),
        12 => ('3', '#'),
        13 => ('4', '$'),
        14 => ('5', '%'),
        15 => ('6', '^'),
        16 => ('7', '&'),
        17 => ('8', '*'),
        18 => ('9', '('),
        19 => ('0', ')'),
        20 => ('-', '_'),
        21 => ('=', '+'),
        // QWERTY row → Cyrillic phonetic
        24 => ('я', 'Я'), // q → я
        25 => ('ш', 'Ш'), // w → ш
        26 => ('е', 'Е'), // e → е
        27 => ('р', 'Р'), // r → р
        28 => ('т', 'Т'), // t → т
        29 => ('ъ', 'Ъ'), // y → ъ
        30 => ('у', 'У'), // u → у
        31 => ('и', 'И'), // i → и
        32 => ('о', 'О'), // o → о
        33 => ('п', 'П'), // p → п
        34 => ('[', '{'), // brackets — same as EN
        35 => (']', '}'),
        // Home row → Cyrillic phonetic
        38 => ('а', 'А'), // a → а
        39 => ('с', 'С'), // s → с
        40 => ('д', 'Д'), // d → д
        41 => ('ф', 'Ф'), // f → ф
        42 => ('г', 'Г'), // g → г
        43 => ('х', 'Х'), // h → х
        44 => ('й', 'Й'), // j → й
        45 => ('к', 'К'), // k → к
        46 => ('л', 'Л'), // l → л
        47 => (';', ':'), // punctuation — same as EN
        48 => ('\'', '"'),
        49 => ('`', '~'),
        // Bottom row → Cyrillic phonetic
        51 => ('\\', '|'), // backslash — same as EN
        52 => ('з', 'З'),  // z → з
        53 => ('ь', 'Ь'),  // x → ь
        54 => ('ц', 'Ц'),  // c → ц
        55 => ('в', 'В'),  // v → в
        56 => ('б', 'Б'),  // b → б
        57 => ('н', 'Н'),  // n → н
        58 => ('м', 'М'),  // m → м
        59 => (',', '<'),  // comma — same as EN
        60 => ('.', '>'),
        61 => ('/', '?'),
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
        assert_eq!(scancode_to_char(24, false, Layout::En), Some('q'));
        assert_eq!(scancode_to_char(24, true, Layout::En), Some('Q'));
        assert_eq!(scancode_to_char(38, false, Layout::En), Some('a'));
        assert_eq!(scancode_to_char(52, false, Layout::En), Some('z'));
    }

    #[test]
    fn test_bg_phonetic_letters() {
        assert_eq!(scancode_to_char(24, false, Layout::Bg), Some('я'));
        assert_eq!(scancode_to_char(24, true, Layout::Bg), Some('Я'));
        assert_eq!(scancode_to_char(38, false, Layout::Bg), Some('а'));
        assert_eq!(scancode_to_char(52, false, Layout::Bg), Some('з'));
        assert_eq!(scancode_to_char(43, false, Layout::Bg), Some('х'));
    }

    #[test]
    fn test_numbers_same_on_both() {
        for code in 10..=19 {
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
        let (en, bg) = scancode_to_both(43, false).unwrap(); // 'h' key
        assert_eq!(en, 'h');
        assert_eq!(bg, 'х');

        let (en, bg) = scancode_to_both(10, false).unwrap(); // '1' key
        assert_eq!(en, '1');
        assert_eq!(bg, '1');
    }

    #[test]
    fn test_is_alpha_scancode() {
        assert!(is_alpha_scancode(24)); // Q
        assert!(is_alpha_scancode(38)); // A
        assert!(is_alpha_scancode(52)); // Z
        assert!(!is_alpha_scancode(10)); // 1
        assert!(!is_alpha_scancode(65)); // Space
        assert!(!is_alpha_scancode(9)); // Escape
    }

    #[test]
    fn test_scancode_to_special() {
        assert_eq!(scancode_to_special(9), Some(SpecialKey::Escape));
        assert_eq!(scancode_to_special(15), Some(SpecialKey::Tab));
        assert_eq!(scancode_to_special(22), Some(SpecialKey::Backspace));
        assert_eq!(scancode_to_special(65), Some(SpecialKey::Space));
        assert_eq!(scancode_to_special(36), Some(SpecialKey::Return));
        assert_eq!(scancode_to_special(114), Some(SpecialKey::Right));
        assert_eq!(scancode_to_special(24), None); // Q — not special
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
            (24, 'q', 'я'),
            (25, 'w', 'ш'),
            (26, 'e', 'е'),
            (27, 'r', 'р'),
            (28, 't', 'т'),
            (29, 'y', 'ъ'),
            (30, 'u', 'у'),
            (31, 'i', 'и'),
            (32, 'o', 'о'),
            (33, 'p', 'п'),
            (38, 'a', 'а'),
            (39, 's', 'с'),
            (40, 'd', 'д'),
            (41, 'f', 'ф'),
            (42, 'g', 'г'),
            (43, 'h', 'х'),
            (44, 'j', 'й'),
            (45, 'k', 'к'),
            (46, 'l', 'л'),
            (52, 'z', 'з'),
            (53, 'x', 'ь'),
            (54, 'c', 'ц'),
            (55, 'v', 'в'),
            (56, 'b', 'б'),
            (57, 'n', 'н'),
            (58, 'm', 'м'),
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
        assert_eq!(scancode_to_char(38, true, Layout::Bg), Some('А'));
        assert_eq!(scancode_to_char(52, true, Layout::Bg), Some('З'));
        assert_eq!(scancode_to_char(25, true, Layout::Bg), Some('Ш'));
    }
}
