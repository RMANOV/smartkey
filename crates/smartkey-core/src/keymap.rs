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

/// Find the physical EN/BG pair whose character on `layout` is `ch`.
///
/// This reverse lookup deliberately reuses the hardware tables above.  The
/// legacy text-transliteration map is not equivalent to xkeyboard-config's
/// traditional phonetic layout for keys such as в/ш/щ/ч/ю.
pub(crate) fn physical_pair_for_char(layout: Layout, ch: char) -> Option<(char, char)> {
    for code in 2..=53 {
        for shift in [false, true] {
            let Some(pair) = scancode_to_both(code, shift) else {
                continue;
            };
            let candidate = match layout {
                Layout::En => pair.0,
                Layout::Bg => pair.1,
            };
            if candidate == ch {
                return Some(pair);
            }
        }
    }
    None
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
// BG traditional phonetic layout (evdev scancodes)
// ======================================================================
//
// Letter keys follow xkeyboard-config's `bg(phonetic)` variant. This is a
// physical keyboard map, intentionally separate from the legacy text
// transliteration helper in `lang_detect`.

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
        17 => ('в', 'В'), // w → в
        18 => ('е', 'Е'), // e → е
        19 => ('р', 'Р'), // r → р
        20 => ('т', 'Т'), // t → т
        21 => ('ъ', 'Ъ'), // y → ъ
        22 => ('у', 'У'), // u → у
        23 => ('и', 'И'), // i → и
        24 => ('о', 'О'), // o → о
        25 => ('п', 'П'), // p → п
        26 => ('ш', 'Ш'), // [ → ш
        27 => ('щ', 'Щ'), // ] → щ
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
        41 => ('ч', 'Ч'), // ` → ч
        // Bottom row → Cyrillic phonetic
        43 => ('ю', 'Ю'), // backslash → ю
        44 => ('з', 'З'), // z → з
        45 => ('ь', 'Ь'), // x → ь
        46 => ('ц', 'Ц'), // c → ц
        47 => ('ж', 'Ж'), // v → ж
        48 => ('б', 'Б'), // b → б
        49 => ('н', 'Н'), // n → н
        50 => ('м', 'М'), // m → м
        51 => (',', '<'), // comma — same as EN
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
        assert_eq!(scancode_to_char(17, false, Layout::Bg), Some('в'));
        assert_eq!(scancode_to_char(47, false, Layout::Bg), Some('ж'));
        assert_eq!(scancode_to_char(30, false, Layout::Bg), Some('а'));
        assert_eq!(scancode_to_char(44, false, Layout::Bg), Some('з'));
        assert_eq!(scancode_to_char(35, false, Layout::Bg), Some('х'));
    }

    #[test]
    fn test_bg_traditional_phonetic_non_alpha_letters() {
        assert_eq!(scancode_to_char(26, false, Layout::Bg), Some('ш'));
        assert_eq!(scancode_to_char(27, false, Layout::Bg), Some('щ'));
        assert_eq!(scancode_to_char(41, false, Layout::Bg), Some('ч'));
        assert_eq!(scancode_to_char(43, false, Layout::Bg), Some('ю'));
    }

    #[test]
    fn test_failed_live_smoke_words_map_exactly() {
        let map = |codes: &[u16]| -> String {
            codes
                .iter()
                .map(|code| scancode_to_char(*code, false, Layout::Bg).unwrap())
                .collect()
        };

        assert_eq!(map(&[17, 18, 41, 18]), "вече");
        assert_eq!(map(&[25, 19, 24, 48, 17, 30, 50, 18]), "пробваме");
        assert_eq!(
            map(&[37, 38, 30, 17, 23, 30, 20, 22, 19, 30, 20, 30]),
            "клавиатурата"
        );
    }

    #[test]
    fn test_browser_phrase_maps_to_single_cyrillic_words() {
        let map = |codes: &[u16]| -> String {
            codes
                .iter()
                .map(|code| scancode_to_char(*code, false, Layout::Bg).unwrap())
                .collect()
        };

        assert_eq!(map(&[32, 30]), "да");
        assert_eq!(map(&[34, 38, 18, 32, 30]), "гледа");
        assert_eq!(map(&[32, 49, 23]), "дни");
        assert_eq!(map(&[49, 30, 25, 19, 18, 32]), "напред");
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
        assert_eq!(scancode_to_both(2, true), Some(('!', '!')));
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
    fn test_physical_pair_reverse_lookup_uses_traditional_phonetic_layout() {
        assert_eq!(physical_pair_for_char(Layout::Bg, 'в'), Some(('w', 'в')));
        assert_eq!(physical_pair_for_char(Layout::Bg, 'ш'), Some(('[', 'ш')));
        assert_eq!(physical_pair_for_char(Layout::Bg, 'щ'), Some((']', 'щ')));
        assert_eq!(physical_pair_for_char(Layout::Bg, 'ч'), Some(('`', 'ч')));
        assert_eq!(physical_pair_for_char(Layout::Bg, 'ю'), Some(('\\', 'ю')));
        assert_eq!(physical_pair_for_char(Layout::Bg, 'В'), Some(('W', 'В')));
        assert_eq!(physical_pair_for_char(Layout::En, 'v'), Some(('v', 'ж')));
    }

    #[test]
    fn test_physical_pair_reverse_lookup_rejects_unmapped_character() {
        assert_eq!(physical_pair_for_char(Layout::Bg, 'ѝ'), None);
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
    fn test_xkb_traditional_phonetic_mapping() {
        // Match xkeyboard-config's `bg(phonetic)` physical letter positions.
        let letter_scancodes = [
            (16, 'q', 'я'),
            (17, 'w', 'в'),
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
            (47, 'v', 'ж'),
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
        assert_eq!(scancode_to_char(17, true, Layout::Bg), Some('В'));
        assert_eq!(scancode_to_char(41, true, Layout::Bg), Some('Ч'));
    }
}
