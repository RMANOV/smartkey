//! FFI serialisation protocol — single source of truth for delimiters and
//! encode/decode helpers shared by PyO3, C FFI, and test harnesses.

/// Unit Separator (ASCII 31) — delimits `replace_len` from `text` in
/// [`Action::ReplaceWord`](crate::input::Action::ReplaceWord) payloads
/// sent across the FFI boundary.
pub const ACTION_SEPARATOR: char = '\x1F';

/// Null character — delimits `typed` from `ghost` in
/// [`Action::ShowComposing`](crate::input::Action::ShowComposing) payloads.
pub const COMPOSING_SEPARATOR: char = '\x00';

// ── ReplaceWord ────────────────────────────────────────────────────

/// Encode a `ReplaceWord` action into the FFI wire format:
/// `"{replace_len}\x1F{text}"`.
pub fn encode_replace_payload(replace_len: usize, text: &str) -> String {
    format!("{replace_len}{}{text}", ACTION_SEPARATOR)
}

/// Decode a `ReplaceWord` FFI payload back into `(replace_len, text)`.
///
/// Returns `None` if the separator is missing or `replace_len` is not
/// a valid `usize`.
pub fn decode_replace_payload(payload: &str) -> Option<(usize, &str)> {
    let (n_str, text) = payload.split_once(ACTION_SEPARATOR)?;
    Some((n_str.parse().ok()?, text))
}

// ── ShowComposing ──────────────────────────────────────────────────

/// Encode a `ShowComposing` action into the FFI wire format:
/// `"{typed}\x00{ghost}"`.
pub fn encode_composing_payload(typed: &str, ghost: &str) -> String {
    format!("{typed}{}{ghost}", COMPOSING_SEPARATOR)
}

/// Decode a `ShowComposing` FFI payload back into `(typed, ghost)`.
///
/// Returns `None` if the separator is missing.
pub fn decode_composing_payload(payload: &str) -> Option<(&str, &str)> {
    payload.split_once(COMPOSING_SEPARATOR)
}

// ======================================================================
// Tests
// ======================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ── ReplaceWord round-trips ────────────────────────────────────

    #[test]
    fn round_trip_replace_basic() {
        let payload = encode_replace_payload(3, "hello");
        let (len, text) = decode_replace_payload(&payload).unwrap();
        assert_eq!(len, 3);
        assert_eq!(text, "hello");
    }

    #[test]
    fn round_trip_replace_cyrillic() {
        let payload = encode_replace_payload(1, "зд");
        let (len, text) = decode_replace_payload(&payload).unwrap();
        assert_eq!(len, 1);
        assert_eq!(text, "зд");
    }

    #[test]
    fn round_trip_replace_zero_len() {
        let payload = encode_replace_payload(0, "text");
        let (len, text) = decode_replace_payload(&payload).unwrap();
        assert_eq!(len, 0);
        assert_eq!(text, "text");
    }

    #[test]
    fn round_trip_replace_empty_text() {
        let payload = encode_replace_payload(5, "");
        let (len, text) = decode_replace_payload(&payload).unwrap();
        assert_eq!(len, 5);
        assert_eq!(text, "");
    }

    #[test]
    fn round_trip_replace_text_containing_separator() {
        // split_once splits on FIRST occurrence only.
        let payload = encode_replace_payload(2, "a\x1Fb");
        let (len, text) = decode_replace_payload(&payload).unwrap();
        assert_eq!(len, 2);
        assert_eq!(text, "a\x1Fb");
    }

    #[test]
    fn decode_replace_invalid_no_separator() {
        assert!(decode_replace_payload("no separator").is_none());
    }

    #[test]
    fn decode_replace_invalid_non_numeric() {
        assert!(decode_replace_payload("abc\x1Ftext").is_none());
    }

    // ── ShowComposing round-trips ──────────────────────────────────

    #[test]
    fn round_trip_composing_basic() {
        let payload = encode_composing_payload("hel", "lo world");
        let (typed, ghost) = decode_composing_payload(&payload).unwrap();
        assert_eq!(typed, "hel");
        assert_eq!(ghost, "lo world");
    }

    #[test]
    fn round_trip_composing_cyrillic() {
        let payload = encode_composing_payload("зд", "равей");
        let (typed, ghost) = decode_composing_payload(&payload).unwrap();
        assert_eq!(typed, "зд");
        assert_eq!(ghost, "равей");
    }

    #[test]
    fn round_trip_composing_empty_ghost() {
        let payload = encode_composing_payload("hello", "");
        let (typed, ghost) = decode_composing_payload(&payload).unwrap();
        assert_eq!(typed, "hello");
        assert_eq!(ghost, "");
    }

    #[test]
    fn decode_composing_no_separator() {
        assert!(decode_composing_payload("nosep").is_none());
    }
}
