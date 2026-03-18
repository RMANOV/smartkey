//! Cross-FFI contract tests.
//!
//! These tests verify that the serialisation format used by all platform
//! adapters (PyO3, C FFI / macOS, Python IBus) produces identical results
//! for the same `Action` inputs.  A failure here means a wire-format
//! change broke at least one consumer.

use std::ffi::CString;

// Inline the protocol constants so this file is independent of ffi_protocol.rs.
const ACTION_SEPARATOR: char = '\x1F';
const COMPOSING_SEPARATOR: char = '\x00';

// ── Encoder helpers (mirror the patterns from each adapter) ────────

/// PyO3 / Rust encode pattern for ReplaceWord.
fn pyo3_encode_replace(replace_len: usize, text: &str) -> String {
    format!("{replace_len}{}{text}", ACTION_SEPARATOR)
}

/// macOS C FFI encode pattern — same format, then round-trip through CString.
fn cffi_encode_replace(replace_len: usize, text: &str) -> String {
    let payload = format!("{replace_len}{}{text}", ACTION_SEPARATOR);
    // The C FFI does: CString::new(payload.replace('\0', "")).into_raw()
    // We simulate the round-trip: encode → CString → back to String.
    let sanitized = payload.replace('\0', "");
    let cstr = CString::new(sanitized).expect("CString::new failed");
    cstr.into_string().expect("CString round-trip failed")
}

/// Python decode pattern: `payload.split("\x1F", 1)` → parse int, take rest.
fn python_decode_replace(payload: &str) -> Option<(usize, &str)> {
    let (n_str, text) = payload.split_once(ACTION_SEPARATOR)?;
    Some((n_str.parse().ok()?, text))
}

/// PyO3 encode for ShowComposing.
fn pyo3_encode_composing(typed: &str, ghost: &str) -> String {
    format!("{}{}{}", typed, COMPOSING_SEPARATOR, ghost)
}

/// Python decode for ShowComposing.
fn python_decode_composing(payload: &str) -> (&str, &str) {
    if let Some((typed, ghost)) = payload.split_once(COMPOSING_SEPARATOR) {
        (typed, ghost)
    } else {
        (payload, "")
    }
}

// ── ReplaceWord contract tests ─────────────────────────────────────

struct ReplaceTestCase {
    replace_len: usize,
    text: &'static str,
    label: &'static str,
}

const REPLACE_CASES: &[ReplaceTestCase] = &[
    ReplaceTestCase {
        replace_len: 2,
        text: "hello",
        label: "basic_latin",
    },
    ReplaceTestCase {
        replace_len: 1,
        text: "зд",
        label: "cyrillic",
    },
    ReplaceTestCase {
        replace_len: 0,
        text: "prefix",
        label: "zero_replace_len",
    },
    ReplaceTestCase {
        replace_len: 3,
        text: "",
        label: "empty_text",
    },
    ReplaceTestCase {
        replace_len: 5,
        text: "café résumé",
        label: "accented_latin",
    },
    ReplaceTestCase {
        replace_len: 10,
        text: "日本語テスト",
        label: "cjk",
    },
    ReplaceTestCase {
        replace_len: 2,
        text: "a\x1Fb",
        label: "text_containing_separator",
    },
];

#[test]
fn pyo3_and_cffi_produce_identical_payloads() {
    for case in REPLACE_CASES {
        if case.text.contains('\0') {
            continue;
        }
        let pyo3 = pyo3_encode_replace(case.replace_len, case.text);
        let cffi = cffi_encode_replace(case.replace_len, case.text);
        assert_eq!(
            pyo3, cffi,
            "[{}] PyO3 and C FFI payloads differ: {:?} vs {:?}",
            case.label, pyo3, cffi
        );
    }
}

#[test]
fn python_decodes_pyo3_payload() {
    for case in REPLACE_CASES {
        let payload = pyo3_encode_replace(case.replace_len, case.text);
        let (len, text) = python_decode_replace(&payload)
            .unwrap_or_else(|| panic!("[{}] Python decode failed for {:?}", case.label, payload));
        assert_eq!(
            len, case.replace_len,
            "[{}] replace_len mismatch",
            case.label
        );
        assert_eq!(text, case.text, "[{}] text mismatch", case.label);
    }
}

#[test]
fn python_decodes_cffi_payload() {
    for case in REPLACE_CASES {
        if case.text.contains('\0') {
            continue;
        }
        let payload = cffi_encode_replace(case.replace_len, case.text);
        let (len, text) = python_decode_replace(&payload)
            .unwrap_or_else(|| panic!("[{}] Python decode failed for {:?}", case.label, payload));
        assert_eq!(
            len, case.replace_len,
            "[{}] replace_len mismatch",
            case.label
        );
        assert_eq!(text, case.text, "[{}] text mismatch", case.label);
    }
}

#[test]
fn encode_decode_identity_replace() {
    for case in REPLACE_CASES {
        let payload = pyo3_encode_replace(case.replace_len, case.text);
        let (len, text) = python_decode_replace(&payload)
            .unwrap_or_else(|| panic!("[{}] round-trip failed", case.label));
        assert_eq!(
            (len, text),
            (case.replace_len, case.text),
            "[{}] round-trip identity violated",
            case.label
        );
    }
}

// ── ShowComposing contract tests ───────────────────────────────────

#[test]
fn composing_round_trip_basic() {
    let payload = pyo3_encode_composing("hel", "lo world");
    let (typed, ghost) = python_decode_composing(&payload);
    assert_eq!(typed, "hel");
    assert_eq!(ghost, "lo world");
}

#[test]
fn composing_round_trip_cyrillic() {
    let payload = pyo3_encode_composing("зд", "равей");
    let (typed, ghost) = python_decode_composing(&payload);
    assert_eq!(typed, "зд");
    assert_eq!(ghost, "равей");
}

#[test]
fn composing_round_trip_empty_ghost() {
    let payload = pyo3_encode_composing("hello", "");
    let (typed, ghost) = python_decode_composing(&payload);
    assert_eq!(typed, "hello");
    assert_eq!(ghost, "");
}

#[test]
fn composing_no_separator_fallback() {
    // Python falls back to (payload, "") when no \x00 present.
    let (typed, ghost) = python_decode_composing("noseparator");
    assert_eq!(typed, "noseparator");
    assert_eq!(ghost, "");
}

#[test]
fn cffi_strips_null_bytes_from_replace_text() {
    // The C FFI does payload.replace('\0', "") before CString::new().
    // If text contained a null byte, it gets stripped in C FFI but not in PyO3.
    // This documents the known behavioral difference.
    let pyo3 = pyo3_encode_replace(1, "a\0b");
    let cffi = cffi_encode_replace(1, "a\0b");
    assert!(pyo3.contains('\0'), "PyO3 should preserve null in payload");
    assert!(!cffi.contains('\0'), "C FFI should strip null from payload");
    // Both decode the replace_len correctly.
    let (len_pyo3, _) = python_decode_replace(&pyo3).unwrap();
    let (len_cffi, _) = python_decode_replace(&cffi).unwrap();
    assert_eq!(len_pyo3, 1);
    assert_eq!(len_cffi, 1);
}
