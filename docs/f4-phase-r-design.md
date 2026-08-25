# F4 Phase R — pre-commit resolver design (RED phase)

Scope: design plumbing only, branch `agent/f4-phase-r` (base `7b9901a`), per
ADVOCATE_CODEX ruling `221af6d0a1c8` (2026-08-24): one F4 umbrella = shared
evidence/pre-commit plumbing + three separately testable decision contracts.
Baseline behaviour is unchanged; every stub is marked `F4 Phase R stub —
baseline behaviour, GREEN pending ADVOCATE PASS`.

## Why a pre-commit resolver

While a dual-buffer word is typed, both readings (`en_text`, `bg_text`) are
entirely in preedit.  Display scoring uses *prefix* evidence
(`SmartKeyEngine::score_both` = top completion frequency per corpus) and a
display lock at `min_lock_chars`.  Prefix evidence is stateless and can be
wrong for the *whole word* (`stat` → EN 602k vs BG 55k, locks EN; yet `statiq`
is no English word and `статия` is Bulgarian).  Nothing has been committed, so
the delimiter is the one safe point to decide once, on exact evidence — no
`ReplaceWord`, no post-commit autocorrect.

## Resolver contract (`InputMethodCore::resolve_word_boundary`)

1. Called exactly once per delimiter from the `Key::Space | Key::Return`
   composing branch (and the non-alphabetic `Key::Char` composing guard) of
   `handle_key`, before `current_word` is cloned and committed.
2. Output is `Some(en_text)` or `Some(bg_text)` or `None` (keep displayed
   text) — never a third string, never a partial replacement.
3. The caller then emits exactly one `CommitText` and forwards the delimiter
   exactly once; `ReplaceWord` is never emitted on this path.
4. Final text and detected/learned language (`lang_detector` momentum,
   regime, commit metrics) are updated atomically from the same decision.
5. The display lock is hysteresis for the preedit only; it is not final truth.
6. Unresolved ambiguity never silently flips: absent a rule that fires, the
   displayed interpretation commits unchanged.

## Evidence inputs

- Exact-word counts: `SmartKeyEngine::score_exact_both(en, bg)` (case-folded
  like `score_both`; backed by `NgramTrie::exact_frequency`, which is
  immune to a longer, more frequent completion masking the exact word).
- Prefix counts already in the buffer (`en_score`, `bg_score`, `confidence`).
- Provenance: `DualBuffer::provenance()` — `Corpus | PriorOnly |
  UnsupportedBoth | Inherited`.
- Prior: `InputMethodCore::active_lang_prior()` (surrounding text / light
  profile via `MasterLoop::anticipate`) and `LanguageDetector::momentum_lang`.
- Phrase context: `context_words()` / previous committed words, for the
  explicit margin lane A needs when both readings are supported.

## Decision contracts

- Lane A — delimiter-time exact short word (`ли/li`, `но/no`, `тн/tn`,
  `топ/top`): compare exact EN vs exact BG counts, not the strongest prefix
  completion.  Only one exact reading supported → it may win.  Both
  supported → require an explicit phrase/prior margin; otherwise keep the
  displayed reading.  Guards: genuine English `no`, `li`, `to`, `is` after
  English context stay English.
- Lane B — wrong early lock / full-word crossover (`статия`, `статии`):
  evidence keeps accumulating after a display lock; at the delimiter
  `DualBuffer::apply_exact_word_evidence(en_exact, bg_exact, prior)` may
  change the winner when the locked reading has no exact support and the
  other does, updating winner/confidence/locked language atomically.  Guards:
  `status`, `static`, `station` remain EN; Cyrillic is committed exactly once.
- Lane C — zero-support provenance (invariant hardening only): both counts
  zero → `UnsupportedBoth`, unlocked; an inherited winner is never reported
  as corpus confidence; a context prior is provisional display bias only.
  Guard: genuine English OOV after English context must not auto-flip BG.
  No claim that `темплейтът`, `бекграунд` or `фичъри` are fixed.

## Lane files (one author per file)

`crates/smartkey-core/src/f4_lane_ab_tests.rs` (rows A, B),
`crates/smartkey-core/src/f4_lane_cde_tests.rs` (rows C, D, E),
`ibus/test_native_f4_lane_f.py` (row F), `ibus/test_f4_lane_g_fixture.py`
(row G: partitioned 45-word diagnostic, never a single pass/fail gate).

## GREEN proposal — boundary decision table (design only, ruling 49b3eb66f3ef)

Inputs at the delimiter (nothing committed yet): `E = (en_exact, bg_exact)`
from `score_exact_both`, `P = provenance()`, `prior` = active surrounding
prior or momentum (`Bg` / `En` / `None`), `phrase` = script of the previous
committed word(s) (`Bg` / `En` / `None`), `D` = displayed winner.

Context is first collapsed into ONE value so that a prior/phrase conflict is
decided before any directional rule can fire:

`ctx = agree(prior, phrase)` = `Bg` if both are `Bg` or one is `Bg` and the
other `None`; `En` symmetrically; `None` if both are `None`; **`Conflict`** if
they disagree.

| # | en_exact | bg_exact | ctx | D | decision |
|---|---------|---------|-----|---|----------|
| 1 | 0 | 0 | any | any | keep `D` — never fabricate a flip (lane C; also covers `PriorOnly`/`Inherited` with `E=(0,0)`, i.e. genuine English OOV after English context) |
| 2 | 0 | >0 | any | En | commit `bg_text` — one-sided exact (lane B `статия` after prefix-lock; lane A `ли`) |
| 3 | >0 | 0 | any | Bg | commit `en_text` — symmetric one-sided |
| 4 | >0 | >0 | `None` or `Conflict` | any | keep `D` — both supported without agreed context; unresolved ambiguity never silently flips |
| 5 | >0 | >0 | `Bg` and bg_exact ≥ margin·en_exact | En | commit `bg_text` (`но` after a Bulgarian phrase) |
| 6 | >0 | >0 | `En` and en_exact ≥ margin·bg_exact | Bg | commit `en_text` |
| 7 | otherwise | | | | keep `D` |

Rows are evaluated top-down and the first match wins; rows 5/6 are the only
directional rows and are reachable only with an agreed context, because row
4 has already consumed `None`/`Conflict`. `margin` is a single explicit
constant (proposal ≥ 1.5), the only tunable; one-sided rows 2/3 need none.

Invariants (each row above must hold under these; RED lanes already pin them):

1. `(0,0)` exact evidence never changes the committed script (row 1).
2. Both-supported with no agreed context preserves the displayed reading
   (row 4 — covers both "no context" and "prior/phrase conflict").
3. Genuine English OOV after English context commits Latin (row 1, lane E
   guard `zorbly`); `status`/`static`/`station` commit English (rows 3/4).
4. Delimiter emission is exactly one `CommitText` followed by exactly one
   forwarded delimiter; zero `ReplaceWord`; the decision and the learned
   language (`lang_detector`, regime, metrics) update atomically from the
   same row.
5. The display lock is hysteresis only: row 2 may override an EN display
   lock at the delimiter (lane B), never mid-word and never via
   post-commit autocorrect.
6. Rows 2/5 must not promote out-of-corpus Bulgarian (`темплейтът`): with
   `bg_exact = 0` they cannot fire (row 1) — no OOV claim.

API surface: every Phase R symbol (`ScoreProvenance`, `provenance()`,
`apply_exact_word_evidence`, `NgramTrie::exact_frequency`,
`score_exact_both`, `resolve_word_boundary`) is used only inside
`smartkey-core` (lane tests) and by no adapter crate (`smartkey-py` imports
only `ffi_protocol`, `input::{Action, InputConfig, Key, KeyEvent,
Modifiers}` and `MasterLoop`). They can be narrowed to `pub(crate)` (and the
read-only views are already `#[cfg(test)]`) before GREEN wiring without any
external breakage; recommended for the GREEN patch so that the only
production-visible change is the delimiter path itself.

## Deferred (out of Phase R)

F4-b per-corpus normalisation; typo/hyphen correction; new BPE/PPM or
morphological OOV models; Unicode normalisation; post-commit autocorrect;
Space-accept Unicode eligibility; raw Tab/Right accounting; Windows.
Production config, live engine, deploy, merge and release stay on HOLD.
