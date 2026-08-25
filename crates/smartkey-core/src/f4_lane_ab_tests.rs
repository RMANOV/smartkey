//! F4 Phase R — lane A/B RED tests (ADVOCATE_CODEX ruling 221af6d0a1c8,
//! minimum RED matrix rows A and B).
//!
//! Row A (`DualBuffer` crossover): an EN display lock on `stat` at char 4
//! must still yield to strong exact Bulgarian full-word evidence before
//! commit (`статия` / `статии`), while `status` / `static` / `station` stay
//! EN.  Row B (provenance): both-zero corpus counts => `UnsupportedBoth`,
//! unlocked, with no inherited corpus confidence; EN/BG prior-only states
//! never lock.
//!
//! Plumbing under test: `crate::dual_buffer::{DualBuffer, ScoreProvenance}`
//! (`provenance()`, `apply_exact_word_evidence()`) and
//! `crate::ensemble::SmartKeyEngine::score_exact_both()`.  This is a
//! crate-level sibling module (declared in `lib.rs`), so import through
//! `crate::…` paths.  Written by the lane A/B agent only.

// RED discipline: every RED test below fails on the current code for a
// BEHAVIOURAL reason (an assertion on baseline behaviour); guards that already
// hold on baseline are labelled `guard_` and must stay green.  No production
// code is modified by this module.

use crate::dual_buffer::{DualBuffer, DualBufferConfig, ScoreProvenance};
use crate::ensemble::SmartKeyEngine;
use crate::lang_detect::LangId;

// Physical evdev keycodes (BG phonetic): Latin reading / Cyrillic reading.
const KEY_S: u16 = 31; // s / с
const KEY_T: u16 = 20; // t / т
const KEY_A: u16 = 30; // a / а
const KEY_I: u16 = 23; // i / и
const KEY_Q: u16 = 16; // q / я
const KEY_U: u16 = 22; // u / у
const KEY_C: u16 = 46; // c / ц
const KEY_O: u16 = 24; // o / о
const KEY_N: u16 = 49; // n / н
const KEY_L: u16 = 38; // l / л
const KEY_E: u16 = 18; // e / е
const KEY_M: u16 = 50; // m / м
const KEY_P: u16 = 25; // p / п
const KEY_J: u16 = 36; // j / й
const KEY_Y: u16 = 21; // y / ъ
const KEY_X: u16 = 45; // x / ь

/// Prefix evidence measured live for physical `stat` / `стат` on revision
/// 7b9901a (ruling 221af6d0a1c8, code-evidence basis §1): confidence 0.9164,
/// EN locks at char 4.
const STAT_EN_PREFIX: f64 = 602_559.0;
const STAT_BG_PREFIX: f64 = 54_954.0;

fn default_buffer() -> DualBuffer {
    DualBuffer::from_config(&DualBufferConfig::default())
}

fn push_all(b: &mut DualBuffer, codes: &[u16]) {
    for &code in codes {
        assert!(
            b.push_keycode(code, false).is_some(),
            "keycode {code} must map to a letter pair"
        );
    }
}

/// Mirror `InputMethodCore::handle_dual_buffer_key` for an *unlocked* buffer:
/// push, then rescore with the given prefix evidence.
fn push_scored(b: &mut DualBuffer, code: u16, en_prefix: f64, bg_prefix: f64) {
    assert!(
        !b.is_locked(),
        "push_scored models the unlocked rescoring path"
    );
    push_all(b, &[code]);
    b.update_scores(en_prefix, bg_prefix);
}

/// Type physical `s t a t` with the live prefix evidence.  Baseline
/// precondition (not a RED claim): the buffer is display-locked EN at char 4
/// with `stat` / `стат` in the two readings.
fn lock_en_on_stat() -> DualBuffer {
    let mut b = default_buffer();
    for code in [KEY_S, KEY_T, KEY_A, KEY_T] {
        push_scored(&mut b, code, STAT_EN_PREFIX, STAT_BG_PREFIX);
    }
    assert_eq!(b.en_text(), "stat");
    assert_eq!(b.bg_text(), "стат");
    assert!(
        b.is_locked(),
        "precondition: baseline locks EN on `stat` at char 4 (min_lock_chars=4)"
    );
    assert_eq!(b.winner_lang(), LangId::En, "precondition: EN display lock");
    assert!(
        b.confidence() > 0.9,
        "precondition: 0.9164 prefix confidence"
    );
    b
}

/// After a display lock the core stops rescoring (`if !was_locked`), so the
/// rest of the word is pushed with NO `update_scores()` call.
fn finish_after_lock(b: &mut DualBuffer, codes: &[u16]) {
    assert!(b.is_locked());
    push_all(b, codes);
}

/// Deterministic controlled corpus for the engine-driven rows: `stat` is a
/// prefix of a dominant English word, while the full words `статия` /
/// `статии` exist only in Bulgarian and `status` / `static` / `station` only
/// in English (`статус` is also Bulgarian, giving one both-supported guard).
fn controlled_engine() -> SmartKeyEngine {
    let mut engine = SmartKeyEngine::new();
    engine.load_word_lang("state", 600_000, LangId::En);
    engine.load_word_lang("status", 40_000, LangId::En);
    engine.load_word_lang("static", 30_000, LangId::En);
    engine.load_word_lang("station", 25_000, LangId::En);
    engine.load_word_lang("статия", 12_000, LangId::Bg);
    engine.load_word_lang("статии", 3_000, LangId::Bg);
    engine.load_word_lang("статус", 9_000, LangId::Bg);
    engine.load_word_lang("стар", 50_000, LangId::Bg);
    engine
}

/// Type `codes` against `engine` exactly the way the core does: rescore with
/// prefix evidence only while unlocked.
fn type_with_engine(engine: &SmartKeyEngine, codes: &[u16]) -> DualBuffer {
    let mut b = default_buffer();
    for &code in codes {
        let was_locked = b.is_locked();
        push_all(&mut b, &[code]);
        if !was_locked {
            let (en, bg) = engine.score_both(b.en_text(), b.bg_text());
            b.update_scores(en, bg);
        }
    }
    b
}

// ======================================================================
// Row A — DualBuffer crossover: EN display lock on `stat` must yield to
// exact Bulgarian full-word evidence before commit (lane B contract of the
// ruling; the DualBuffer-level half of lane A for unlocked short words).
// ======================================================================

/// RED: `стат` locks EN at char 4, the user finishes `статия` (physical
/// `statiq`), and at the delimiter exact evidence is EN 0 / BG strong.  The
/// crossover hook must flip the winner to the Bulgarian reading atomically.
/// Baseline: `apply_exact_word_evidence` is an inert stub (returns `false`,
/// winner stays En, `winner_text()` stays `statiq`).
#[test]
fn a_en_display_lock_on_stat_yields_to_exact_bg_evidence_for_statiq() {
    let mut b = lock_en_on_stat();
    finish_after_lock(&mut b, &[KEY_I, KEY_Q]);
    assert_eq!(b.en_text(), "statiq");
    assert_eq!(b.bg_text(), "статия");
    assert_eq!(b.winner_text(), "statiq", "display before the delimiter");

    let crossed = b.apply_exact_word_evidence(0.0, 5_000.0, Some(LangId::Bg));

    assert!(
        crossed,
        "exact evidence EN 0 / BG 5000 must override the EN display lock; \
         baseline stub reports no crossover and keeps winner {:?} = {:?}",
        b.winner_lang(),
        b.winner_text()
    );
    assert_eq!(b.winner_lang(), LangId::Bg);
    assert_eq!(b.winner_text(), "статия");
    assert_eq!(b.winner_text(), b.bg_text());
    assert!(
        b.confidence() > 0.5,
        "one-sided exact support is real confidence, not the 0.5 no-support floor"
    );
    assert!(
        !b.flip_detected(),
        "pre-commit crossover must not route through the ReplaceWord flip flag"
    );
    // Exactly one decision: re-applying the same evidence changes nothing.
    assert!(
        !b.apply_exact_word_evidence(0.0, 5_000.0, Some(LangId::Bg)),
        "second application must be a no-op"
    );
    assert_eq!(b.winner_lang(), LangId::Bg);
    assert_eq!(b.winner_text(), "статия");
}

/// RED: same crossover for `статии` (physical `statii`) with NO contextual
/// prior — the first word of a document has none, and a reading that is the
/// only exactly supported one may win on its own (ruling, lane A/B).
#[test]
fn a_en_display_lock_on_stat_yields_to_exact_bg_evidence_for_statii_without_prior() {
    let mut b = lock_en_on_stat();
    finish_after_lock(&mut b, &[KEY_I, KEY_I]);
    assert_eq!(b.en_text(), "statii");
    assert_eq!(b.bg_text(), "статии");

    let crossed = b.apply_exact_word_evidence(0.0, 1_200.0, None);

    assert!(
        crossed,
        "exact evidence EN 0 / BG 1200 with no prior must still override the \
         EN display lock; baseline keeps {:?}",
        b.winner_text()
    );
    assert_eq!(b.winner_lang(), LangId::Bg);
    assert_eq!(b.winner_text(), "статии");
    assert!(!b.flip_detected());
}

/// RED (engine-driven): the same crossover with evidence produced by the
/// plumbing itself — `score_both()` drives the display lock, and
/// `score_exact_both()` supplies the delimiter-time exact counts from a
/// controlled in-memory corpus.  Baseline: the display lock is reproduced
/// (`state` 600k vs `статия` 12k at `stat`), the exact evidence is correct
/// (EN 0 / BG 12000), but the hook ignores it.
#[test]
fn a_controlled_corpus_exact_evidence_crosses_over_after_display_lock() {
    let engine = controlled_engine();
    let mut b = type_with_engine(&engine, &[KEY_S, KEY_T, KEY_A, KEY_T, KEY_I, KEY_Q]);
    assert_eq!(b.en_text(), "statiq");
    assert_eq!(b.bg_text(), "статия");
    assert!(
        b.is_locked(),
        "precondition: prefix evidence locks EN at `stat`"
    );
    assert_eq!(b.winner_lang(), LangId::En, "precondition: EN display lock");

    let (en_exact, bg_exact) = engine.score_exact_both(b.en_text(), b.bg_text());
    assert_eq!(
        (en_exact, bg_exact),
        (0.0, 12_000.0),
        "plumbing: exact counts are not masked by the `state` completion"
    );

    let crossed = b.apply_exact_word_evidence(en_exact, bg_exact, Some(LangId::Bg));

    assert!(
        crossed,
        "exact corpus evidence must change the winner; baseline keeps {:?}",
        b.winner_text()
    );
    assert_eq!(b.winner_lang(), LangId::Bg);
    assert_eq!(b.winner_text(), "статия");
    assert!(!b.flip_detected());
}

/// PASS_GUARD: `status` / `static` / `station` typed after the same `stat`
/// lock have exact EN support and no BG support — they stay EN even under a
/// Bulgarian prior, and the hook reports no crossover.
#[test]
fn a_guard_status_static_station_stay_en_under_bg_prior() {
    let cases: [(&[u16], &str, f64); 3] = [
        (&[KEY_U, KEY_S], "status", 40_000.0),
        (&[KEY_I, KEY_C], "static", 30_000.0),
        (&[KEY_I, KEY_O, KEY_N], "station", 25_000.0),
    ];
    for (tail, en_word, en_exact) in cases {
        let mut b = lock_en_on_stat();
        finish_after_lock(&mut b, tail);
        assert_eq!(b.en_text(), en_word);

        let crossed = b.apply_exact_word_evidence(en_exact, 0.0, Some(LangId::Bg));

        assert!(!crossed, "{en_word}: no crossover on one-sided EN evidence");
        assert_eq!(b.winner_lang(), LangId::En, "{en_word} stays EN");
        assert_eq!(b.winner_text(), en_word);
        assert!(b.is_locked(), "{en_word}: the EN lock is untouched");
        assert!(!b.flip_detected());
    }
}

/// PASS_GUARD (engine-driven): the controlled corpus reproduces the guard
/// with evidence from `score_exact_both()`; `status` / `статус` is supported
/// on BOTH sides and, with no prior, the displayed reading must be kept.
#[test]
fn a_guard_controlled_corpus_english_full_words_keep_en_lock() {
    let engine = controlled_engine();
    /// (tail keycodes, EN word, expected exact counts, prior)
    type ExactGuardCase = (&'static [u16], &'static str, (f64, f64), Option<LangId>);
    let cases: [ExactGuardCase; 3] = [
        (&[KEY_U, KEY_S], "status", (40_000.0, 9_000.0), None),
        (&[KEY_I, KEY_C], "static", (30_000.0, 0.0), Some(LangId::Bg)),
        (
            &[KEY_I, KEY_O, KEY_N],
            "station",
            (25_000.0, 0.0),
            Some(LangId::Bg),
        ),
    ];
    for (tail, en_word, expected_exact, prior) in cases {
        let codes: Vec<u16> = [KEY_S, KEY_T, KEY_A, KEY_T]
            .into_iter()
            .chain(tail.iter().copied())
            .collect();
        let mut b = type_with_engine(&engine, &codes);
        assert_eq!(b.en_text(), en_word);
        assert!(b.is_locked() && b.winner_lang() == LangId::En);

        let (en_exact, bg_exact) = engine.score_exact_both(b.en_text(), b.bg_text());
        assert_eq!(
            (en_exact, bg_exact),
            expected_exact,
            "{en_word} exact counts"
        );

        assert!(!b.apply_exact_word_evidence(en_exact, bg_exact, prior));
        assert_eq!(b.winner_lang(), LangId::En, "{en_word} stays EN");
        assert_eq!(b.winner_text(), en_word);
    }
}

/// PASS_GUARD: with no exact support on either side the hook must not flip on
/// the prior alone — the ruling (§4) finds no safe evidence separating an
/// unsupported Bulgarian word from a genuine English OOV, and unresolved
/// ambiguity must never silently flip.
#[test]
fn a_guard_no_exact_support_either_side_keeps_displayed_lock() {
    for prior in [Some(LangId::Bg), Some(LangId::En), None] {
        let mut b = lock_en_on_stat();
        finish_after_lock(&mut b, &[KEY_X]);
        assert_eq!(b.en_text(), "statx");
        assert_eq!(b.bg_text(), "стать");

        assert!(
            !b.apply_exact_word_evidence(0.0, 0.0, prior),
            "prior {prior:?}: zero/zero exact evidence must not decide anything"
        );
        assert_eq!(b.winner_lang(), LangId::En);
        assert_eq!(b.winner_text(), "statx");
        assert!(b.is_locked());
    }
}

/// PASS_GUARD: both readings exactly supported and no prior => ambiguity is
/// unresolved, the displayed (locked) reading is kept.
#[test]
fn a_guard_both_exact_readings_supported_without_prior_keeps_displayed_lock() {
    let mut b = lock_en_on_stat();
    finish_after_lock(&mut b, &[KEY_U, KEY_S]);
    assert_eq!(b.en_text(), "status");
    assert_eq!(b.bg_text(), "статус");

    assert!(!b.apply_exact_word_evidence(40_000.0, 9_000.0, None));
    assert_eq!(b.winner_lang(), LangId::En);
    assert_eq!(b.winner_text(), "status");
}

/// RED: the DualBuffer half of lane A — an UNLOCKED short word (`ли`,
/// physical `li`) whose EN prefix completion dominates display scoring
/// (`like`) but whose exact EN reading has no support, under a Bulgarian
/// prior, must resolve to the Bulgarian reading at the delimiter.  Baseline:
/// stub returns `false`, winner stays En, `winner_text()` stays `li`.
#[test]
fn a_unlocked_short_word_li_resolves_bg_on_one_sided_exact_evidence() {
    let mut b = default_buffer();
    push_scored(&mut b, KEY_L, 2_000_000.0, 900_000.0); // "l"/"л" prefix
    push_scored(&mut b, KEY_I, 1_500_000.0, 400_000.0); // "li"/"ли" prefix (like)
    assert_eq!(b.en_text(), "li");
    assert_eq!(b.bg_text(), "ли");
    assert!(!b.is_locked(), "precondition: 2 chars < min_lock_chars");
    assert_eq!(
        b.winner_lang(),
        LangId::En,
        "precondition: prefix display En"
    );

    let crossed = b.apply_exact_word_evidence(0.0, 250_000.0, Some(LangId::Bg));

    assert!(
        crossed,
        "exact EN 0 / BG 250000 under a BG prior must resolve `ли`; baseline keeps {:?}",
        b.winner_text()
    );
    assert_eq!(b.winner_lang(), LangId::Bg);
    assert_eq!(b.winner_text(), "ли");
    assert!(!b.flip_detected());
}

/// PASS_GUARD: genuine English `no` / `li` / `to` / `is` after English
/// context stay English: with an EN prior and the EN reading exactly
/// supported, the hook reports no crossover regardless of BG support.
#[test]
fn a_guard_english_context_keeps_genuine_english_short_words() {
    let cases: [(&[u16], &str, (f64, f64)); 4] = [
        (&[KEY_N, KEY_O], "no", (900_000.0, 700_000.0)),
        (&[KEY_L, KEY_I], "li", (40.0, 250_000.0)),
        (&[KEY_T, KEY_O], "to", (5_000_000.0, 300_000.0)),
        (&[KEY_I, KEY_S], "is", (3_000_000.0, 0.0)),
    ];
    for (codes, en_word, (en_exact, bg_exact)) in cases {
        let mut b = default_buffer();
        for &code in codes {
            push_scored(&mut b, code, 1_000_000.0, 200_000.0);
        }
        assert_eq!(b.en_text(), en_word);
        assert_eq!(b.winner_lang(), LangId::En);
        assert!(!b.is_locked());

        assert!(
            !b.apply_exact_word_evidence(en_exact, bg_exact, Some(LangId::En)),
            "{en_word}: English context keeps the English reading"
        );
        assert_eq!(b.winner_lang(), LangId::En, "{en_word} stays EN");
        assert_eq!(b.winner_text(), en_word);
    }
}

// ======================================================================
// Row B — provenance: both-zero corpus counts => UnsupportedBoth, unlocked,
// no inherited corpus confidence; prior-only states never lock.
// ======================================================================

/// Physical `темплейтът` (`templejtyt`), the ruling's out-of-corpus example.
/// The zero-support tail is modelled from char 2 on; this pins the
/// zero-support STATE only and makes no claim that the word is resolved
/// (ruling §4, lane C).
const OOV_CODES: [u16; 10] = [
    KEY_T, KEY_E, KEY_M, KEY_P, KEY_L, KEY_E, KEY_J, KEY_T, KEY_Y, KEY_T,
];

/// RED: after a char-1 prior bias, `update_scores(0, 0)` must report
/// `UnsupportedBoth`, stay unlocked with confidence at or below 0.5, and
/// keep refusing to lock as the word grows past `min_lock_chars`.
/// Baseline: `update_scores(0, 0)` collapses to 0.5/0.5 (confidence 0.5)
/// and keeps the previous winner, so the stub reports `Inherited` — the
/// zero-support state is indistinguishable from the initial state.
#[test]
fn b_both_zero_after_char_1_prior_bias_is_unsupported_both_and_unlocked() {
    let mut b = default_buffer();
    // Char 1 carries real single-letter prefix evidence; the BG prior biases
    // the display (F1: winner Bg, confidence 0.65, no lock).
    push_scored(&mut b, OOV_CODES[0], 900_000.0, 400_000.0);
    b.apply_prior_lock_hint(Some(LangId::Bg));
    assert_eq!(
        b.winner_lang(),
        LangId::Bg,
        "precondition: char-1 prior bias"
    );
    assert!(!b.is_locked());

    // Char 2: no corpus support for either reading.
    push_scored(&mut b, OOV_CODES[1], 0.0, 0.0);
    b.apply_prior_lock_hint(Some(LangId::Bg));
    assert_eq!(
        b.provenance(),
        ScoreProvenance::UnsupportedBoth,
        "both-zero evidence must be explicit; baseline stub reports {:?} for \
         the 0.5/0.5 collapse (winner {:?} is merely kept)",
        b.provenance(),
        b.winner_lang()
    );
    assert!(!b.is_locked());
    assert!(b.confidence() <= 0.5, "no inherited corpus confidence");

    // Chars 3..=6 (len >= min_lock_chars): still unsupported, still unlocked,
    // even with the prior hint re-applied every keystroke like the core does.
    for &code in &OOV_CODES[2..6] {
        push_scored(&mut b, code, 0.0, 0.0);
        b.apply_prior_lock_hint(Some(LangId::Bg));
        assert_eq!(
            b.provenance(),
            ScoreProvenance::UnsupportedBoth,
            "len {}: zero support stays explicit",
            b.len()
        );
        assert!(!b.is_locked(), "len {}: zero support never locks", b.len());
        assert!(b.confidence() <= 0.5, "len {}", b.len());
    }
    assert!(b.len() >= DualBufferConfig::default().min_lock_chars);
}

/// RED: a prior-only state (char 1 without corpus support, biased by the
/// Bulgarian prior) reports `PriorOnly`; once zero-support rescoring follows
/// it must report `PriorOnly` or `UnsupportedBoth` — never `Corpus`, never
/// `Inherited` — and never lock.  Baseline: char 1 reads `PriorOnly` (the
/// 0.65 bias), but the first `update_scores(0, 0)` resets confidence to 0.5
/// and the stub falls back to `Inherited`.
#[test]
fn b_bg_prior_only_state_reports_prior_only_then_unsupported_and_never_locks() {
    let mut b = default_buffer();
    push_scored(&mut b, OOV_CODES[0], 0.0, 0.0);
    b.apply_prior_lock_hint(Some(LangId::Bg));
    assert_eq!(b.winner_lang(), LangId::Bg);
    assert!(!b.is_locked());
    assert_eq!(
        b.provenance(),
        ScoreProvenance::PriorOnly,
        "char 1: only the prior has spoken"
    );

    for &code in &OOV_CODES[1..6] {
        push_scored(&mut b, code, 0.0, 0.0);
        b.apply_prior_lock_hint(Some(LangId::Bg));
        assert!(
            matches!(
                b.provenance(),
                ScoreProvenance::PriorOnly | ScoreProvenance::UnsupportedBoth
            ),
            "len {}: prior-only/zero-support state must not be reported as {:?}",
            b.len(),
            b.provenance()
        );
        assert!(
            !b.is_locked(),
            "len {}: prior-only state never locks",
            b.len()
        );
    }
}

/// RED: the symmetric EN prior-only state.  An EN prior on char 1 agrees
/// with the default EN winner, so the F1 bias branch does not fire and no
/// 0.65 confidence is recorded; the stub therefore cannot tell "EN prior,
/// no corpus" from the initial state and reports `Inherited`.  GREEN must
/// report `PriorOnly` (or `UnsupportedBoth` once zeros were scored).
#[test]
fn b_en_prior_only_state_is_not_reported_as_inherited() {
    let mut b = default_buffer();
    push_scored(&mut b, OOV_CODES[0], 0.0, 0.0);
    b.apply_prior_lock_hint(Some(LangId::En));
    assert_eq!(b.winner_lang(), LangId::En);
    assert!(!b.is_locked());

    assert!(
        matches!(
            b.provenance(),
            ScoreProvenance::PriorOnly | ScoreProvenance::UnsupportedBoth
        ),
        "an EN prior with zero corpus support is a prior-only state, \
         baseline reports {:?}",
        b.provenance()
    );
}

/// PASS_GUARD: EN and BG prior-only states never lock, however long the
/// unsupported word grows and however often the hint is re-applied; the
/// prior remains a provisional display bias (`winner_lang()` == prior) with
/// confidence never above the 0.65 bias value.
#[test]
fn b_guard_prior_only_states_never_lock_for_either_prior() {
    for prior in [LangId::En, LangId::Bg] {
        let mut b = default_buffer();
        for &code in &OOV_CODES[..8] {
            push_scored(&mut b, code, 0.0, 0.0);
            b.apply_prior_lock_hint(Some(prior));
            assert!(
                !b.is_locked(),
                "{prior:?} prior, len {}: prior-only state must not lock",
                b.len()
            );
            assert_eq!(b.winner_lang(), prior, "display bias follows the prior");
            assert!(b.confidence() <= 0.65 + 1e-9);
            assert_ne!(
                b.provenance(),
                ScoreProvenance::Corpus,
                "{prior:?} prior, len {}: no corpus ever spoke",
                b.len()
            );
        }
    }
}

/// RED: total < 1 after a corpus-backed prefix is the lane-C defect itself:
/// the winner is silently carried over from the previous prefix.  The state
/// must be explicit `UnsupportedBoth` with no inherited corpus confidence,
/// and neither further zeros nor a matching prior hint may lock it.
/// Baseline: stub reports `Inherited` (0.5/0.5 collapse) — the both-zero
/// case is not distinguishable from "nothing was ever scored".
#[test]
fn b_total_below_one_after_corpus_prefix_is_explicit_unsupported_both() {
    let mut b = default_buffer();
    push_scored(&mut b, KEY_S, 2_000_000.0, 300_000.0);
    push_scored(&mut b, KEY_T, STAT_EN_PREFIX, STAT_BG_PREFIX);
    assert_eq!(b.provenance(), ScoreProvenance::Corpus, "precondition");
    assert_eq!(b.winner_lang(), LangId::En);
    assert!(b.confidence() > 0.9);

    push_scored(&mut b, KEY_X, 0.0, 0.0); // "stx" / "сть": no support either side
    assert_eq!(
        b.provenance(),
        ScoreProvenance::UnsupportedBoth,
        "total < 1 must be explicit zero support; baseline reports {:?} and \
         keeps the carried-over winner {:?}",
        b.provenance(),
        b.winner_lang()
    );
    assert!(b.confidence() <= 0.5, "no inherited corpus confidence");
    assert!(!b.is_locked());

    push_scored(&mut b, KEY_X, 0.0, 0.0); // len 4 == min_lock_chars
    b.apply_prior_lock_hint(Some(LangId::En)); // matches the carried-over winner
    assert_eq!(b.provenance(), ScoreProvenance::UnsupportedBoth);
    assert!(
        !b.is_locked(),
        "a matching prior must not lock a zero-support word"
    );
}

/// RED: scores carried past a display lock are `Inherited`, not `Corpus`.
/// After the EN lock on `stat` the core pushes `и`, `я` WITHOUT rescoring, so
/// the 0.9164 prefix confidence now describes `stat`, not `statiq`/`статия`.
/// A resolver must never read that as corpus confidence for the current
/// buffers.  Baseline: the stub derives provenance from the stale unequal
/// scores and reports `Corpus`.
#[test]
fn b_scores_carried_past_a_display_lock_are_inherited_not_corpus() {
    let mut b = lock_en_on_stat();
    assert_eq!(
        b.provenance(),
        ScoreProvenance::Corpus,
        "precondition at `stat`"
    );

    finish_after_lock(&mut b, &[KEY_I, KEY_Q]);
    assert_eq!(b.bg_text(), "статия");

    assert_eq!(
        b.provenance(),
        ScoreProvenance::Inherited,
        "no evidence was consulted for `statiq`/`статия`; the winner and its \
         confidence are carried over from `stat`, baseline reports {:?}",
        b.provenance()
    );
}

/// PASS_GUARD: provenance semantics that already hold and must survive
/// GREEN — a fresh buffer is the inherited EN default, pushing without any
/// scoring stays inherited, and real corpus counts read as `Corpus`.
#[test]
fn b_guard_fresh_buffer_is_inherited_default_and_corpus_counts_read_corpus() {
    let mut b = default_buffer();
    assert_eq!(b.provenance(), ScoreProvenance::Inherited, "fresh buffer");
    assert_eq!(b.winner_lang(), LangId::En);
    assert!(!b.is_locked());

    push_all(&mut b, &[KEY_S]);
    assert_eq!(b.provenance(), ScoreProvenance::Inherited, "unscored push");

    b.update_scores(2_000_000.0, 300_000.0);
    assert_eq!(b.provenance(), ScoreProvenance::Corpus, "normalised counts");
    assert!(!b.is_locked(), "1 char never locks");

    b.clear();
    assert_eq!(b.provenance(), ScoreProvenance::Inherited, "clear() resets");
}
