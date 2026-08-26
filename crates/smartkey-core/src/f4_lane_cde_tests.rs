//! F4 Phase R — lane C/D/E RED tests (ADVOCATE_CODEX ruling 221af6d0a1c8,
//! minimum RED matrix rows C, D and E).
//!
//! Row C (`InputMethodCore`, controlled in-memory corpus via
//! `load_word_lang`): `stat` locks EN at char 4, the delimiter resolves
//! `статия` BG with exactly one `CommitText`, one forwarded delimiter and no
//! `ReplaceWord`.  Row D (boundary resolver): a Bulgarian phrase context plus
//! physical `no` / `li` resolves to `но` / `ли`, with symmetric
//! English-context guards (`no`, `li`, `to`, `is` stay English).  Row E
//! (`MasterLoop`): surrounding text `smartkey` proves the zero-support prior
//! is EN and stays explicitly unresolved; genuine English OOV after English
//! context must not auto-flip BG.
//!
//! Plumbing under test: `InputMethodCore::resolve_word_boundary()` (stub,
//! not yet on the delimiter path), the `pub(crate)` views
//! `InputMethodCore::{engine, dual_buffer, active_lang_prior}` and
//! `MasterLoop::core()`, plus `DualBuffer::provenance()`.  Crate-level
//! sibling module (declared in `lib.rs`): import through `crate::…` paths.
//! Written by the lane C/D/E agent only.
//!
//! Controlled corpus (deterministic, in memory, shaped like the live one):
//! the EN `stat` family (`state` 5M top completion) outweighs the Bulgarian
//! `стат` family (`статия` 600k) so `stat` locks EN at char 4 exactly as the
//! live engine does; the EN `li`/`no` prefix mass (`like`, `list`, `not`,
//! `now`) outweighs the exact Bulgarian `ли`/`но` so the live ли→li / но→no
//! defect reproduces (prefix completion beats the exact word); `no`/`to`/`is`
//! are moderate exact EN entries (both readings supported for `но`/`no`);
//! `template` is the live-like trigger for the out-of-corpus inheritance of
//! `темплейтът` (EN display lock on `tem`, zero support afterwards).
//!
//! Baseline classification (revision 7b9901a + Phase R plumbing) is recorded
//! on every test: `RED` = fails behaviourally on the baseline, `GUARD` =
//! holds on the baseline and must keep holding after GREEN.

use crate::dual_buffer::ScoreProvenance;
use crate::input::{Action, InputConfig, InputMethodCore, Key, KeyEvent, Modifiers};
use crate::lang_detect::LangId;
use crate::master_loop::{Hints, MasterLoop};

// ── Controlled bilingual corpus ─────────────────────────────────────────────

const EN_WORDS: &[(&str, u32)] = &[
    // `stat` prefix family: top completion 5M → EN locks at char 4.
    ("state", 5_000_000),
    ("status", 3_000_000),
    ("static", 2_500_000),
    ("station", 2_000_000),
    // English phrase context.
    ("the", 9_000_000),
    ("quick", 500_000),
    ("brown", 400_000),
    // `li` prefix mass (no exact `li`): prefix beats exact `ли`.
    ("like", 3_000_000),
    ("list", 2_000_000),
    ("little", 1_500_000),
    // `no` prefix mass plus a moderate exact `no`.
    ("not", 5_000_000),
    ("now", 3_000_000),
    ("no", 1_200_000),
    // Moderate exact short words shared with Bulgarian physical keys.
    ("to", 5_000_000),
    ("is", 4_000_000),
    ("it", 3_500_000),
    // Live-like out-of-corpus inheritance trigger for темплейтът.
    ("template", 1_000_000),
];

const BG_WORDS: &[(&str, u32)] = &[
    // `стат` family kept LOWER than the EN `stat` family.
    ("статия", 600_000),
    ("статии", 200_000),
    ("статус", 100_000),
    // Exact short words the operator reported as flipping to Latin.
    ("ли", 2_000_000),
    ("но", 1_500_000),
    ("тн", 300_000),
    ("топ", 400_000),
    // Bulgarian phrase context.
    ("искаш", 800_000),
    ("това", 2_000_000),
    ("е", 3_000_000),
    ("тест", 700_000),
    // Prefix neighbours so the BG side has completions beyond the exact word.
    ("нов", 400_000),
    ("лице", 300_000),
];

fn bilingual_core() -> InputMethodCore {
    let mut core = InputMethodCore::new(InputConfig::default());
    for &(w, f) in EN_WORDS {
        core.load_word_lang(w, f, LangId::En);
    }
    for &(w, f) in BG_WORDS {
        core.load_word_lang(w, f, LangId::Bg);
    }
    core
}

fn bilingual_loop() -> MasterLoop {
    let mut ml = MasterLoop::new(InputConfig::default());
    for &(w, f) in EN_WORDS {
        ml.load_word_lang(w, f, LangId::En);
    }
    for &(w, f) in BG_WORDS {
        ml.load_word_lang(w, f, LangId::Bg);
    }
    ml
}

// ── Physical keys (evdev scancodes, BG phonetic) ────────────────────────────

const SPACE: u16 = 57;
const STATIQ: [u16; 6] = [31, 20, 30, 20, 23, 16]; // s t a t i q → статия
const STATII: [u16; 6] = [31, 20, 30, 20, 23, 23]; // s t a t i i → статии
const STATUS: [u16; 6] = [31, 20, 30, 20, 22, 31]; // s t a t u s → статус
const STATIC: [u16; 6] = [31, 20, 30, 20, 23, 46]; // s t a t i c → статиц
const STATION: [u16; 7] = [31, 20, 30, 20, 23, 24, 49]; // s t a t i o n → статион
const LI: [u16; 2] = [38, 23]; // l i → ли
const NO: [u16; 2] = [49, 24]; // n o → но
const TO: [u16; 2] = [20, 24]; // t o → то
const IS: [u16; 2] = [23, 31]; // i s → ис
const ISKASH: [u16; 5] = [23, 31, 37, 30, 26]; // i s k a [ → искаш
const TOVA: [u16; 4] = [20, 24, 17, 30]; // t o w a → това
const THE: [u16; 3] = [20, 35, 18]; // t h e → тхе
const QUICK: [u16; 5] = [16, 22, 23, 46, 37]; // q u i c k → яуицк
const BROWN: [u16; 5] = [48, 19, 24, 17, 49]; // b r o w n → бровн
const TEMPLEJTYT: [u16; 10] = [20, 18, 50, 25, 38, 18, 36, 20, 21, 20]; // темплейтът
const BEKGRAUND: [u16; 9] = [48, 18, 37, 34, 19, 30, 22, 49, 32]; // бекграунд
const ZORBLY: [u16; 6] = [44, 24, 19, 48, 38, 21]; // z o r b l y → зорблъ

fn press_raw(code: u16) -> KeyEvent {
    KeyEvent {
        key: Key::RawCode(code),
        modifiers: Modifiers::empty(),
    }
}

fn type_word(core: &mut InputMethodCore, codes: &[u16]) -> String {
    for &c in codes {
        core.handle_key(press_raw(c));
    }
    core.current_word().to_string()
}

fn type_word_loop(ml: &mut MasterLoop, codes: &[u16]) -> String {
    for &c in codes {
        ml.handle_key(press_raw(c));
    }
    ml.current_word().to_string()
}

fn is_cyrillic(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| ('\u{0400}'..='\u{04FF}').contains(&c))
}

fn is_latin(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_ascii_alphabetic())
}

/// The committed texts, the forwarded-delimiter count and the `ReplaceWord`
/// count of one delimiter action batch.
fn delimiter_shape(actions: &[Action]) -> (Vec<String>, usize, usize) {
    let commits = actions
        .iter()
        .filter_map(|a| match a {
            Action::CommitText(t) => Some(t.clone()),
            _ => None,
        })
        .collect();
    let forwards = actions
        .iter()
        .filter(|a| matches!(a, Action::ForwardKey))
        .count();
    let replaces = actions
        .iter()
        .filter(|a| matches!(a, Action::ReplaceWord { .. }))
        .count();
    (commits, forwards, replaces)
}

/// What `MasterLoop::anticipate()` injects for Cyrillic surrounding text,
/// followed by two Bulgarian words typed on physical keys and committed, so
/// both prior sources (surrounding-text hint and typing momentum) say BG.
fn bulgarian_phrase_context(core: &mut InputMethodCore) {
    core.apply_hints(&Hints {
        lang_prior: Some(LangId::Bg),
        context_words: vec!["искаш".to_string(), "това".to_string()],
        ..Default::default()
    });
    for (codes, expected) in [(&ISKASH[..], "искаш"), (&TOVA[..], "това")] {
        let shown = type_word(core, codes);
        assert_eq!(shown, expected, "phrase context word must display Cyrillic");
        let (commits, _, _) = delimiter_shape(&core.handle_key(press_raw(SPACE)));
        assert_eq!(commits, vec![expected.to_string()]);
    }
    assert_eq!(core.active_lang_prior(), Some(LangId::Bg));
}

/// Symmetric English context: Latin surrounding hint plus three committed
/// English words (`the quick brown`).
fn english_phrase_context(core: &mut InputMethodCore) {
    core.apply_hints(&Hints {
        lang_prior: Some(LangId::En),
        context_words: vec!["the".to_string(), "quick".to_string(), "brown".to_string()],
        ..Default::default()
    });
    for (codes, expected) in [
        (&THE[..], "the"),
        (&QUICK[..], "quick"),
        (&BROWN[..], "brown"),
    ] {
        let shown = type_word(core, codes);
        assert_eq!(shown, expected, "phrase context word must display Latin");
        let (commits, _, _) = delimiter_shape(&core.handle_key(press_raw(SPACE)));
        assert_eq!(commits, vec![expected.to_string()]);
    }
    assert_eq!(core.active_lang_prior(), Some(LangId::En));
}

// ════════════════════════════════════════════════════════════════════════════
// Row C — InputMethodCore: stat locks EN at char 4, delimiter resolves статия
// ════════════════════════════════════════════════════════════════════════════

/// RED: `stat` locks EN at char 4 (precondition, holds on baseline), then the
/// Space delimiter must commit the exact Bulgarian word `статия` once — the
/// baseline commits the locked Latin reading `statiq`.
#[test]
fn lane_c_stat_locks_en_at_char_4_then_delimiter_commits_statia_cyrillic() {
    let mut core = bilingual_core();

    // Precondition: the display lock the ruling describes.
    let shown_at_4 = type_word(&mut core, &STATIQ[..4]);
    let db = core
        .dual_buffer()
        .expect("dual buffer active inside the word");
    assert_eq!((db.en_text(), db.bg_text()), ("stat", "стат"));
    assert!(
        db.is_locked(),
        "stat must display-lock at char 4 (min_lock_chars)"
    );
    assert_eq!(db.winner_lang(), LangId::En);
    assert_eq!(shown_at_4, "stat");

    // Finish the word: exact evidence is BG-only at the delimiter.
    let shown = type_word(&mut core, &STATIQ[4..]);
    let db = core.dual_buffer().expect("dual buffer still active");
    assert_eq!((db.en_text(), db.bg_text()), ("statiq", "статия"));
    assert_eq!(
        core.engine().score_exact_both(db.en_text(), db.bg_text()),
        (0.0, 600_000.0),
        "statiq is no English word; статия is a Bulgarian corpus entry"
    );

    let actions = core.handle_key(press_raw(SPACE));
    let (commits, forwards, replaces) = delimiter_shape(&actions);
    assert_eq!(commits.len(), 1, "exactly one CommitText: {actions:?}");
    assert_eq!(forwards, 1, "exactly one forwarded delimiter: {actions:?}");
    assert_eq!(
        replaces, 0,
        "no ReplaceWord on the pre-commit path: {actions:?}"
    );
    assert_eq!(
        commits[0], "статия",
        "delimiter must resolve the exact Bulgarian word (displayed `{shown}`)"
    );
    assert!(
        core.current_word().is_empty(),
        "word state reset after commit"
    );
}

/// RED: same crossover for the plural `статии` (`statii` has no EN support).
#[test]
fn lane_c_delimiter_commits_statii_cyrillic_exactly_once() {
    let mut core = bilingual_core();
    type_word(&mut core, &STATII);
    let db = core.dual_buffer().expect("dual buffer active");
    assert!(
        db.is_locked() && db.winner_lang() == LangId::En,
        "EN display lock"
    );
    assert_eq!(
        core.engine().score_exact_both(db.en_text(), db.bg_text()),
        (0.0, 200_000.0)
    );

    let actions = core.handle_key(press_raw(SPACE));
    let (commits, forwards, replaces) = delimiter_shape(&actions);
    assert_eq!(
        (commits.len(), forwards, replaces),
        (1, 1, 0),
        "{actions:?}"
    );
    assert_eq!(commits[0], "статии");
}

/// RED: the resolver's own view of the same word — with no delimiter typed
/// yet, `resolve_word_boundary()` must name `статия` (`en_text()` or
/// `bg_text()` only); the stub answers `None`.
#[test]
fn lane_c_resolver_names_statia_before_the_delimiter() {
    let mut core = bilingual_core();
    let shown = type_word(&mut core, &STATIQ);
    let db = core.dual_buffer().expect("dual buffer active");
    let en = db.en_text().to_string();
    let bg = db.bg_text().to_string();
    assert_eq!((en.as_str(), bg.as_str()), ("statiq", "статия"));

    let resolved = core.resolve_word_boundary();
    assert!(
        matches!(resolved.as_deref(), Some(t) if t == en || t == bg),
        "resolver must return en_text or bg_text, got {resolved:?}"
    );
    assert_eq!(resolved.as_deref(), Some("статия"), "displayed `{shown}`");
}

/// GUARD: the delimiter batch shape is already right on the baseline —
/// exactly one `CommitText` before exactly one `ForwardKey`, never a
/// `ReplaceWord` — and GREEN must keep it.
#[test]
fn lane_c_guard_delimiter_batch_has_one_commit_one_forward_no_replace() {
    let mut core = bilingual_core();
    type_word(&mut core, &STATIQ);
    let actions = core.handle_key(press_raw(SPACE));
    let (commits, forwards, replaces) = delimiter_shape(&actions);
    assert_eq!(commits.len(), 1, "{actions:?}");
    assert_eq!(forwards, 1, "{actions:?}");
    assert_eq!(replaces, 0, "{actions:?}");
    let commit_idx = actions
        .iter()
        .position(|a| matches!(a, Action::CommitText(_)))
        .unwrap();
    let forward_idx = actions
        .iter()
        .position(|a| matches!(a, Action::ForwardKey))
        .unwrap();
    assert!(
        commit_idx < forward_idx,
        "commit precedes the forwarded delimiter"
    );
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, Action::ShowComposing { .. })),
        "no fresh preedit races the forwarded delimiter: {actions:?}"
    );
}

/// GUARD: genuine English `status` / `static` / `station` typed on the same
/// physical keys stay English and commit exactly once.  `status` is the
/// both-supported case (`статус` is a Bulgarian loanword in the corpus) with
/// no Bulgarian context — the locked English reading must keep winning;
/// `static` / `station` have exact EN support only.
#[test]
fn lane_c_guard_status_static_station_commit_english_once() {
    for (codes, expected, bg_supported) in [
        (&STATUS[..], "status", true),
        (&STATIC[..], "static", false),
        (&STATION[..], "station", false),
    ] {
        let mut core = bilingual_core();
        let shown = type_word(&mut core, codes);
        assert_eq!(shown, expected);
        let db = core.dual_buffer().expect("dual buffer active");
        let (en_exact, bg_exact) = core.engine().score_exact_both(db.en_text(), db.bg_text());
        assert!(en_exact > 0.0, "{expected}: exact EN support");
        assert_eq!(bg_exact > 0.0, bg_supported, "{expected}: BG exact support");
        assert_eq!(
            core.resolve_word_boundary().as_deref().unwrap_or(expected),
            expected
        );

        let actions = core.handle_key(press_raw(SPACE));
        let (commits, forwards, replaces) = delimiter_shape(&actions);
        assert_eq!(commits, vec![expected.to_string()], "{actions:?}");
        assert_eq!((forwards, replaces), (1, 0), "{actions:?}");
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Row D — boundary resolver: Bulgarian phrase context + physical no / li
// ════════════════════════════════════════════════════════════════════════════

/// RED: after a Bulgarian phrase context, physical `l i` displays the Latin
/// prefix winner `li` (the live defect; only `ли` has exact support) and the
/// resolver must answer `Some("ли")`.
#[test]
fn lane_d_bulgarian_context_physical_li_resolves_to_li_cyrillic() {
    let mut core = bilingual_core();
    bulgarian_phrase_context(&mut core);

    let shown = type_word(&mut core, &LI);
    let db = core.dual_buffer().expect("dual buffer active");
    assert_eq!((db.en_text(), db.bg_text()), ("li", "ли"));
    assert_eq!(
        core.engine().score_exact_both("li", "ли"),
        (0.0, 2_000_000.0),
        "only the Bulgarian reading is an exact corpus word"
    );
    assert_eq!(
        shown, "li",
        "baseline display: prefix completion `like` wins"
    );

    assert_eq!(
        core.resolve_word_boundary().as_deref(),
        Some("ли"),
        "resolver must pick the exact Bulgarian reading under Bulgarian context"
    );
}

/// RED: `но` / `no` are BOTH exact corpus words; the explicit Bulgarian
/// phrase/prior margin must decide `но`.
#[test]
fn lane_d_bulgarian_context_physical_no_resolves_to_no_cyrillic() {
    let mut core = bilingual_core();
    bulgarian_phrase_context(&mut core);

    let shown = type_word(&mut core, &NO);
    let db = core.dual_buffer().expect("dual buffer active");
    assert_eq!((db.en_text(), db.bg_text()), ("no", "но"));
    let (en_exact, bg_exact) = core.engine().score_exact_both("no", "но");
    assert!(en_exact > 0.0 && bg_exact > 0.0, "both readings supported");
    assert_eq!(
        shown, "no",
        "baseline display: prefix completion `not` wins"
    );

    assert_eq!(
        core.resolve_word_boundary().as_deref(),
        Some("но"),
        "both supported → the Bulgarian phrase margin must decide"
    );
}

/// GUARD: symmetric English context — physical `no`, `li`, `to`, `is` keep
/// their Latin reading: the resolver answers `None` (keep display) or the
/// English text, never Cyrillic.
#[test]
fn lane_d_guard_english_context_short_words_stay_english() {
    for (codes, expected) in [
        (&NO[..], "no"),
        (&LI[..], "li"),
        (&TO[..], "to"),
        (&IS[..], "is"),
    ] {
        let mut core = bilingual_core();
        english_phrase_context(&mut core);

        let shown = type_word(&mut core, codes);
        assert_eq!(
            shown, expected,
            "English context displays the Latin reading"
        );
        let resolved = core.resolve_word_boundary();
        let keeps_english = match resolved.as_deref() {
            None => true,
            Some(t) => t == expected,
        };
        assert!(
            keeps_english,
            "{expected}: resolver must not flip to Cyrillic, got {resolved:?}"
        );
        assert!(
            is_latin(core.current_word()),
            "the resolver mutates no display"
        );

        let (commits, forwards, replaces) = delimiter_shape(&core.handle_key(press_raw(SPACE)));
        assert_eq!(commits, vec![expected.to_string()]);
        assert_eq!((forwards, replaces), (1, 0));
    }
}

/// GUARD: with NO phrase context at all, `no` / `но` are both supported and
/// nothing supplies a margin — unresolved ambiguity must not silently flip.
#[test]
fn lane_d_guard_no_context_both_supported_does_not_flip() {
    let mut core = bilingual_core();
    let shown = type_word(&mut core, &NO);
    assert_eq!(shown, "no");
    assert_eq!(core.active_lang_prior(), None);
    let resolved = core.resolve_word_boundary();
    assert!(
        matches!(resolved.as_deref(), None | Some("no")),
        "no margin → keep the displayed reading, got {resolved:?}"
    );
}

/// GUARD: the resolver never invents a third string and never touches the
/// display — its answer is `None`, `en_text()` or `bg_text()`.
#[test]
fn lane_d_guard_resolver_answers_only_en_text_or_bg_text() {
    let mut core = bilingual_core();
    bulgarian_phrase_context(&mut core);
    let shown = type_word(&mut core, &LI);
    let (en, bg) = {
        let db = core.dual_buffer().expect("dual buffer active");
        (db.en_text().to_string(), db.bg_text().to_string())
    };
    let resolved = core.resolve_word_boundary();
    let is_physical_reading = match resolved.as_deref() {
        None => true,
        Some(t) => t == en || t == bg,
    };
    assert!(is_physical_reading, "got {resolved:?}");
    assert_eq!(
        core.current_word(),
        shown,
        "the resolver does not mutate the display"
    );
}

/// GUARD (row D precondition at the `MasterLoop` level): Cyrillic
/// surrounding text yields a BG prior for the next word, mirroring the
/// Latin `smartkey` → EN case the plumbing already proves.
#[test]
fn lane_d_guard_cyrillic_surrounding_text_yields_bg_prior() {
    let mut ml = bilingual_loop();
    ml.set_surrounding_text(Some("искаш това ".to_string()), Some(11));
    let shown = type_word_loop(&mut ml, &LI[..1]);
    assert_eq!(ml.core().active_lang_prior(), Some(LangId::Bg));
    assert!(
        is_cyrillic(&shown),
        "char-1 prior displays Cyrillic, got `{shown}`"
    );
    let db = ml.core().dual_buffer().expect("dual buffer active");
    assert!(!db.is_locked(), "char-1 prior is a bias, never a lock");
}

// ════════════════════════════════════════════════════════════════════════════
// Row E — MasterLoop: surrounding "smartkey", zero-support provenance
// ════════════════════════════════════════════════════════════════════════════

/// RED: surrounding `smartkey ` makes the prior EN (proven precondition);
/// `темплейтът` display-locks EN on `tem` (`template`), then every further
/// prefix has zero support in BOTH corpora.  At the end of the word the
/// buffer must report explicit zero-support provenance (`UnsupportedBoth`, or
/// at most the prior-only bias) and be UNLOCKED — never a corpus lock
/// inherited from the `tem` prefix.
#[test]
fn lane_e_smartkey_prior_is_en_and_templejtyt_stays_explicitly_unresolved() {
    let mut ml = bilingual_loop();
    ml.set_surrounding_text(Some("smartkey ".to_string()), Some(9));

    let shown = type_word_loop(&mut ml, &TEMPLEJTYT);
    assert_eq!(
        ml.core().active_lang_prior(),
        Some(LangId::En),
        "zero-support prior is EN"
    );
    let db = ml.core().dual_buffer().expect("dual buffer active");
    assert_eq!((db.en_text(), db.bg_text()), ("templejtyt", "темплейтът"));
    assert_eq!(
        ml.core()
            .engine()
            .score_exact_both(db.en_text(), db.bg_text()),
        (0.0, 0.0),
        "neither reading is a corpus word"
    );
    assert_eq!(
        ml.core().engine().score_both(db.en_text(), db.bg_text()),
        (0.0, 0.0),
        "neither reading has any prefix completion either"
    );

    let provenance = db.provenance();
    assert!(
        matches!(
            provenance,
            ScoreProvenance::UnsupportedBoth | ScoreProvenance::PriorOnly
        ),
        "zero support must be explicit, got {provenance:?} (displayed `{shown}`)"
    );
    assert!(
        !db.is_locked(),
        "an inherited winner is never corpus confidence: no lock without support"
    );
}

/// RED: the pure inheritance case — `бекграунд` has prefix support only on
/// its first physical key (`b` → `brown`); from char 2 on both counts are
/// zero and the baseline silently keeps the inherited EN winner as an
/// unlabelled `Inherited` state.  The ruling requires `UnsupportedBoth`.
#[test]
fn lane_e_zero_support_after_char_1_is_unsupported_both_not_inherited() {
    let mut ml = bilingual_loop();
    ml.set_surrounding_text(Some("smartkey ".to_string()), Some(9));

    type_word_loop(&mut ml, &BEKGRAUND);
    let db = ml.core().dual_buffer().expect("dual buffer active");
    assert_eq!((db.en_text(), db.bg_text()), ("bekgraund", "бекграунд"));
    assert_eq!(
        ml.core().engine().score_both(db.en_text(), db.bg_text()),
        (0.0, 0.0)
    );
    assert!(!db.is_locked(), "baseline already leaves this unlocked");
    assert_eq!(
        db.provenance(),
        ScoreProvenance::UnsupportedBoth,
        "both counts zero must be reported explicitly, not as an inherited winner"
    );
}

/// GUARD: genuine English OOV after English context stays Latin — the EN
/// prior may be provisional display bias, and the word commits once as typed.
#[test]
fn lane_e_guard_genuine_english_oov_after_english_context_stays_latin() {
    let mut ml = bilingual_loop();
    ml.set_surrounding_text(Some("the quick brown ".to_string()), Some(16));

    let shown = type_word_loop(&mut ml, &ZORBLY);
    assert_eq!(ml.core().active_lang_prior(), Some(LangId::En));
    assert_eq!(shown, "zorbly", "genuine English OOV must not auto-flip BG");
    let db = ml.core().dual_buffer().expect("dual buffer active");
    assert_eq!(
        ml.core()
            .engine()
            .score_exact_both(db.en_text(), db.bg_text()),
        (0.0, 0.0)
    );
    assert!(!db.is_locked(), "zero support never locks");

    let actions = ml.handle_key(press_raw(SPACE));
    let (commits, forwards, replaces) = delimiter_shape(&actions);
    assert_eq!(commits, vec!["zorbly".to_string()], "{actions:?}");
    assert_eq!((forwards, replaces), (1, 0), "{actions:?}");
}

/// GUARD: the same zero-support word after `smartkey ` also commits as the
/// displayed Latin reading exactly once — this lane hardens provenance only
/// and makes no claim that `темплейтът` is fixed.
#[test]
fn lane_e_guard_templejtyt_commits_displayed_reading_once() {
    let mut ml = bilingual_loop();
    ml.set_surrounding_text(Some("smartkey ".to_string()), Some(9));
    let shown = type_word_loop(&mut ml, &TEMPLEJTYT);
    assert!(
        is_latin(&shown) || is_cyrillic(&shown),
        "one whole script: `{shown}`"
    );

    let actions = ml.handle_key(press_raw(SPACE));
    let (commits, forwards, replaces) = delimiter_shape(&actions);
    assert_eq!(commits.len(), 1, "{actions:?}");
    assert!(
        commits[0] == "templejtyt" || commits[0] == "темплейтът",
        "commit is one of the two physical readings: {actions:?}"
    );
    assert_eq!((forwards, replaces), (1, 0), "{actions:?}");
}
