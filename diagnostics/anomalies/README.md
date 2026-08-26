# SmartKey anomaly registry

Canonical, Git-tracked ledger of anomalies seen in typed text while SmartKey
was active. It implements the standing **ANOMALY GATE** (ledger `af4d6969b2ad`,
2026-08-25): *every anomaly is an OPEN SmartKey defect until evidence closes
it.* The registry is a quality artifact in the same spirit as `DEBUG.md` and
the `smartkey-debug` trace tool: it records **what went wrong, on which build,
with what evidence** — it never changes engine behaviour and is never read by
the engine.

| File | Role |
|---|---|
| `registry.json` | the records (canonical JSON, indent 2, non-ASCII preserved) |
| `schema.json` | structural contract; single source of truth for every enum |
| `validate.py` | stdlib-only validator; non-zero exit on any violation |
| `../../ibus/test_anomaly_registry.py` | pytest: real registry must validate, synthetic bad records must fail |

Why here and not under `receipts/`, `data/` or a trace directory: those are
gitignored by design (they hold live captures). The registry must be reviewed
in diffs and survive machines, so it lives in a tracked `diagnostics/` tree
beside the code it indicts.

## Record envelope

One record per **(observed, expected)** pair. Fields, in file order:

- `id` — `anom-` + first 12 hex of `sha256(dedup_key)`. Deterministic; the
  suspected layer is deliberately **not** part of it so reclassifying a record
  never changes its identity.
- `dedup_key` — `normalize(observed) | normalize(expected or "null")` where
  normalize = NFC + casefold + collapsed whitespace. When no token was captured
  the side key is `desc:` + normalized descriptor. Must be unique.
- `category` — `script_flip_bg_to_en`, `script_flip_en_to_bg`,
  `inconsistent_acceptance`, `unknown`.
- `observed` / `expected` — `{token, script, descriptor}`. Exactly one
  whitespace-free token (max 40 chars) or `null` with a descriptor. `script`
  is recomputed by the validator (`latin` / `cyrillic` / `mixed` / `other` /
  `unknown`).
- `needs_operator_confirmation` — mandatory `true` when the expected form is
  unknown; such records cannot leave `open_suspected_smartkey` / `reproduced`.
- `minimal_context` — `{before, after}`: at most one token per side, or `null`.
- `recorded_utc`, `first_seen_utc`, `last_seen_utc` — **date OR datetime**
  despite the `_utc` suffix: `YYYY-MM-DD` when only the day is genuinely
  known, `YYYY-MM-DDTHH:MM[:SS]Z` when the time is; unknown is `null`, never
  guessed.
- `repeat` / `occurrence_count` — count of distinct observation events (a live
  sighting, or one probe session; several sightings inside one text count
  each). `repeat` must equal `occurrence_count > 1`. For an enhanced G0 record,
  this count is recomputed from the union of its adjudications'
  `source_event_refs`; repeating an event ID cannot inflate the count.
- `environment` — `app_surface`, `os`, `build_sha` (40 hex or `unknown`),
  `build_label`, `flags` (map of flag name to `on|off|absent|unknown`, or
  `null` when nothing was observable). Environment of the first sighting;
  later builds go into `source_refs[].build_sha`.
- `source_refs` — non-empty list of `{kind, ref, observed_utc, build_sha,
  surface, note}`; `kind` is `ledger|ruling|plan|trace|probe|operator_report|commit|other`.
  Duplicates of an existing pair **add a source ref and bump
  `occurrence_count`** instead of creating a record.
- `status` — exactly one of `open_suspected_smartkey`, `reproduced`,
  `red_tested`, `fixed`, `verified`, `closed`, `not_smartkey`.
- `suspected_layer` — hypothesis, freely revised: `dual_buffer_short_word`,
  `dual_buffer_prefix_lock`, `dual_buffer_unsupported_inheritance`,
  `lang_prior_lock`, `accept_gesture`, `unknown`.
- `reproducer` — `{kind, ref, build_sha, summary}` with `kind` in
  `offline_probe|structural_trace|full_trace|unit_test|operator_report`, or `null`.
- `red_test` — `{path, name, commit}` (40-hex commit that contains the test),
  or `null`; `red_test_waiver` explains a not-applicable test (mutually exclusive).
- `fix_commit` — exact 40-hex commit or `null`.
- `fix_evidence` — enhanced-record coverage envelope for `fix_commit`, or
  `null`: exact `covered_adjudication_refs` plus the corresponding
  `covered_source_event_refs`. Legacy records may omit it.
- `verification` — `{build_sha, evidence_refs, surface, summary}`: post-fix
  evidence **on the reported surface**, or `null`.
- `closure_reason` — required for `closed` and `not_smartkey`.
- `privacy_classification` — `public_token` (dictionary word / common token),
  `redacted` (masked token), `unknown` (no token captured or applicable).
  This is a **data-safety classification only** — it says the isolated token
  is safe to store in a public repository; it is not publication consent and
  grants no right to quote the operator's text.
- `notes` — short free text (max 500 chars, single line).
- `adjudications` — optional only for backwards-compatible migration of the
  pre-G0 records. Once present anywhere, every item maps exactly one canonical
  original-candidate or supplemental-hypothesis ref and the sealed top-level
  `adjudication_contract` becomes mandatory. Multiple adjudications may map to
  one deterministic dedup record; expected authority, expected commitment,
  evidence, privacy and source events stay per-adjudication across that fan-in.

## G0 adjudication envelope

The unchanged 20-record legacy registry may omit the G0 layer. As soon as one
enhanced adjudication or source exclusion exists, `adjudication_contract` is
mandatory and `state` must be `sealed`. It pins ruling `6e0704632fef`, the
metadata-only resealed adjudication receipt SHA-256
`92c6dd612be8095d54dc385044c60e9d33e91d0ed9e9f62f6701c87722edbb72`,
the stale-but-structurally-audited mapping-draft receipt
`7838b51bf55fbe7f7ac2ec7e1f5fbb0285df425242b1cbcfeaccaa3a0a6902a4`,
and the merged security/crosswalk contract version. The mapping draft is an
import-HOLD input, not current registry data.

The sealed accounting is exact: 106 original refs + 53 supplemental refs,
classified as 125 bug candidates + 32 guards + 2 source exclusions. Dedup
projects those mappings into 76 bug records + 26 guard records, retains 9
legacy records, and therefore yields 111 records. The two source exclusions
live in top-level `source_exclusions[]`, never as anomaly records. They still
participate in both 106/53 source-grain and 125/32/2 disposition accounting.

Each `adjudications[]` item contains no prose and has these fields:

- `grain` + `ref` — exact namespace grammar: `orig:<16-lowercase-hex>` for an
  `original_candidate`; `supp:G###:H###` for a
  `supplemental_hypothesis`. Grammar and grain are cross-checked, and a ref may
  appear exactly once across records plus top-level exclusions. All-letter hex
  is valid. The authoritative sorted-ref-set commitment is
  `81168506c76776f060aaf9cd71bb4ed0e2fbde31b286ac30b69823bc8a54a5ee`:
  SHA-256 of the sorted ref array serialized as compact ASCII JSON.
- `classification` — one of `mechanical_red_candidate`,
  `instrumentation_first_candidate`, `language_quality_feature_candidate`,
  `intent_confirmation_needed`, `guard_not_bug_unless_intent_changes`, or
  `source_authorship_exclusion`.
- `disposition` — the derived coarse register: `bug_candidate`, `guard`, or
  `source_exclusion`. The validator rejects a classification/disposition
  mismatch.
- `normalized_class` — strict enum of every ratified supplemental mapping class,
  including `source_harness_framing`. Because the final source has no such axis
  for the 106 originals, they must use the explicit `not_applicable` sentinel;
  supplemental mappings and exclusions may never use that sentinel.
- `expected` — per-adjudication `{status, authority, value_ref}`. Status is
  `null_non_unique`, `null_pending_operator_intent`,
  `unique_operator_confirmed`, `unique_authoritative_spelling`, or
  `not_applicable`; authority is `final|null|operator|authoritative|legacy_bridge|not_applicable`.
  Unique expected forms carry `value:<sha256>` rather than requiring a literal
  token. Null/not-applicable forms carry `null`. This represents a redacted
  unique expected form and split-component fan-in without leaking or collapsing
  distinct expected values. Deprecated `expected_form_status`, when present
  during additive migration, must equal `expected.status`.
- `causal_confidence` — independent `anomaly_or_guard_presence`,
  `expected_form`, `runtime_mechanism`, and `smartkey_attribution` axes. Each is
  `none|low|medium|high|not_applicable`; one axis never stands in for another.
- `owner_lane` — one of the ratified G0-G6/deferred lane identifiers from
  `schema.json`, constrained by an exact classification-to-lane allow matrix.
  Outside-product, guard and language-quality lanes cannot be borrowed by an
  unrelated classification.
- `privacy_class` — `public_token`, `redacted`, or `metadata_only`, coupled to
  both record sides and legacy `privacy_classification`. Redacted means
  placeholder/null on observed and expected; metadata-only means no token on
  either side; a public literal is treated independently on each side.
- `source_refs` — non-empty opaque `{kind, ref}` pairs. At least one
  `source_record` is mandatory; refs contain only opaque identifiers, never a
  path, label, sentence, or user text. Guards and exclusions additionally need
  an adjudication `ruling` or `receipt` ref.
- `source_event_refs` — non-empty canonical `event:<sha256>` IDs. An event may
  support several split mappings inside one dedup record, but it cannot be
  reconciled to two records and cannot increment `occurrence_count` twice.

## Exact semantic commitment

Counts cannot detect count-preserving substitutions. The sealed contract also
stores a nonzero `semantic_commitment_sha256`, recomputed over all 159 mappings
(record adjudications plus top-level exclusions). The serialization algorithm
is `sha256-canonical-json-v1`:

1. Build one object per mapping containing `ref`, `grain`, `classification`,
   `disposition`, `normalized_class`, expected `status`/`authority`/
   `value_commitment`, all four confidence axes, `owner_lane`, `privacy_class`,
   the target record ID (or top-level-exclusion sentinel), sorted `(kind, ref)`
   source bindings and sorted source-event refs.
2. Sort mapping objects by `ref`.
3. Serialize as UTF-8 JSON with ASCII escaping, sorted object keys and compact
   separators (`,` and `:`), then SHA-256 the exact bytes.

There is deliberately no zero or placeholder digest. A synthetic validator
fixture uses the real opaque 159-ref namespace, synthetic-only payloads and
its own honestly recomputed nonzero semantic digest. The real import must
compute and externally receipt its reviewed semantic digest in the same
integration commit; this schema task does not import data or invent expected
authority, value commitments or source events.

## Lifecycle gates (enforced by `validate.py`)

| status | must carry |
|---|---|
| `open_suspected_smartkey` | nothing beyond the envelope; **the default** |
| `reproduced` | `reproducer` |
| `red_tested` | `reproducer` + `red_test` |
| `fixed` | `reproducer` + `fix_commit` + (`red_test` or `red_test_waiver`) |
| `verified` | all of `fixed` + `verification` on the reported surface |
| `closed` | all of `verified` + `closure_reason` |
| `not_smartkey` | legacy/non-guard: `reproducer` + `closure_reason`; enhanced all-guard: per-mapping ruling/receipt + `closure_reason` |

Open/reproduced/red_tested records may not carry `fix_commit` or
`verification`; `verification` always implies `fix_commit`. A record whose
expected form is unconfirmed cannot advance past `reproduced`.

For adjudicated records, lifecycle is also coupled to disposition: a record
with any bug-candidate mapping cannot be `not_smartkey`; an all-guard record
must be `not_smartkey` and cannot carry fix evidence. A guard is justified by
its adjudication/ruling receipt, not by manufacturing a `unit_test` reproducer.
Source exclusions never enter the record lifecycle because they live outside
`records`.

For a bug record, advancing to `red_tested` requires `red_test` coverage equal
to every bug adjudication ref and the corresponding source-event union.
`fixed` adds the same exact coverage in `fix_evidence`; `verified`/`closed` add
it in `verification`. Duplicate refs/events and partial coverage fail. Thus one
fixed component cannot close an uncovered sibling mapping in a fan-in record.

## Privacy rules (enforced)

Never full text, credentials, employer data, keystreams or live trace copies.
The validator walks every string of every record and rejects: any `@`;
`http` outside `source_refs`; a digit run longer than six outside SHA and
identifier fields (`build_sha`, `commit`, `fix_commit`, `ref`,
`evidence_refs`, `id`); a Latin/Cyrillic letter run longer than 40 characters;
newlines; more than one token per context side; whitespace inside a token.
Unknown provenance is written as `unknown` / `null`, never inferred.

G0 adds a closed, recursive allowlist for every string-bearing enhanced field.
Environment labels and dynamic flag keys; legacy source refs/surfaces;
reproducer, test and verification refs/paths/names/surfaces; summaries, notes,
descriptors and closure reasons must be enums, SHAs, canonical opaque IDs,
`metadata:<64-lowercase-hex>` or null. JSON Schema's
`additionalProperties: false` closes structural dictionary keys; the validator
separately checks the dynamic `flags` keys. Clearly synthetic validator values
use the reserved `synthetic` / `synthetic:<sha256>` forms.

The aggregate gate walks values and dynamic keys, assigns every
reconstruction-bearing fragment to each linked opaque `source_record`, and
rejects more than eight distinct fragments across linked records
(`E_PRIV_AGGREGATE`). `metadata:<sha256>` descriptors do not consume the
budget. This closes split-record and split-field reconstruction attacks; there
is no production-data exception.

## Adding or updating a record

1. Look for an existing record with the same `dedup_key` first — if found,
   append a `source_refs` entry, bump `occurrence_count`, set `repeat`,
   update `last_seen_utc`.
2. Otherwise add a record with `status: open_suspected_smartkey`; compute the
   identity with
   `python3 -c 'import json,sys; sys.path.insert(0,"diagnostics/anomalies"); import validate as v; r=json.load(sys.stdin); print(v.compute_dedup_key(r), v.compute_id(r))'`.
3. Re-serialise canonically (`validate.canonical_text`) and run
   `python3 diagnostics/anomalies/validate.py` — it must print `OK`.
4. Advance `status` only with the evidence the table above requires; the
   validator refuses shortcuts.

During the G0 data migration, add the sealed `adjudication_contract`, all 157
record mappings and both top-level exclusions in the same canonical update.
The validator requires exact 106+53 source-grain equality, 125/32/2
disposition accounting, the pinned ref set, a recomputed semantic commitment
and the 111-record dedup projection. No mapping draft or count summary can
substitute for this integration gate.

The registry is bookkeeping for the RED-only F4 lane: recording a pair here
neither widens nor bypasses F4, and no engine code reads this directory.
