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
  each). `repeat` must equal `occurrence_count > 1`.
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
  pre-G0 records. Once present anywhere, every item maps exactly one opaque
  original-candidate or supplemental-hypothesis ID and the top-level
  `adjudication_contract` becomes mandatory. Multiple adjudications may map to
  one deterministic dedup record; this preserves source-grain accounting when
  the registry record count is smaller than the adjudication count.

## G0 adjudication envelope

`adjudication_contract` pins the ruling ref, private-artifact SHA-256 and the
ratified units: 106 original IDs + 53 supplemental IDs, classified as 125 bug
candidates + 32 guards + 2 source exclusions. The validator derives every
count from unique `(grain, ref)` pairs; stored summary numbers never substitute
for ID accounting.

Each `adjudications[]` item contains no prose and has these fields:

- `grain` + `ref` — `original_candidate` or `supplemental_hypothesis`, plus a
  hash/ID-shaped opaque reference. A mapping ID may appear exactly once across
  the whole registry.
- `classification` — one of `mechanical_red_candidate`,
  `instrumentation_first_candidate`, `language_quality_feature_candidate`,
  `intent_confirmation_needed`, `guard_not_bug_unless_intent_changes`, or
  `source_authorship_exclusion`.
- `disposition` — the derived coarse register: `bug_candidate`, `guard`, or
  `source_exclusion`. The validator rejects a classification/disposition
  mismatch.
- `expected_form_status` — `null_non_unique`,
  `null_pending_operator_intent`, `unique_operator_confirmed`,
  `unique_authoritative_spelling`, or `not_applicable`. A unique status requires
  a stored confirmed token; a null status cannot carry an expected value.
- `causal_confidence` — independent `anomaly_or_guard_presence`,
  `expected_form`, `runtime_mechanism`, and `smartkey_attribution` axes. Each is
  `none|low|medium|high|not_applicable`; one axis never stands in for another.
- `owner_lane` — one of the ratified G0-G6/deferred lane identifiers from
  `schema.json`. Source exclusions and language-quality candidates have fixed
  owner lanes.
- `privacy_class` — `public_token`, `redacted`, or `metadata_only`, coupled to
  the record payload and legacy `privacy_classification`.
- `source_refs` — non-empty opaque `{kind, ref}` pairs. At least one
  `source_record` is mandatory; refs contain only a 12/16/64-hex identifier or
  a short hypothesis ID, never a path, label, sentence, or user text.

## Lifecycle gates (enforced by `validate.py`)

| status | must carry |
|---|---|
| `open_suspected_smartkey` | nothing beyond the envelope; **the default** |
| `reproduced` | `reproducer` |
| `red_tested` | `reproducer` + `red_test` |
| `fixed` | `reproducer` + `fix_commit` + (`red_test` or `red_test_waiver`) |
| `verified` | all of `fixed` + `verification` on the reported surface |
| `closed` | all of `verified` + `closure_reason` |
| `not_smartkey` | `reproducer` (diagnostic evidence) + `closure_reason` |

Open/reproduced/red_tested records may not carry `fix_commit` or
`verification`; `verification` always implies `fix_commit`. A record whose
expected form is unconfirmed cannot advance past `reproduced`.

For adjudicated records, lifecycle is also coupled to disposition: a record
with any bug-candidate mapping cannot be `not_smartkey`; a record containing
only guards/source exclusions must be `not_smartkey` and cannot carry fix
evidence. This record-level rule permits dedup to retain several source-grain
mappings without turning a guard into a defect or hiding a real defect behind
a guard.

## Privacy rules (enforced)

Never full text, credentials, employer data, keystreams or live trace copies.
The validator walks every string of every record and rejects: any `@`;
`http` outside `source_refs`; a digit run longer than six outside SHA and
identifier fields (`build_sha`, `commit`, `fix_commit`, `ref`,
`evidence_refs`, `id`); a Latin/Cyrillic letter run longer than 40 characters;
newlines; more than one token per context side; whitespace inside a token.
Unknown provenance is written as `unknown` / `null`, never inferred.

G0 adds a second, aggregate privacy gate for adjudicated records. Context is
always null; free text is null or `metadata:<opaque-id>`; `metadata_only`
cannot carry token payload. The validator groups every payload-bearing string
by opaque `source_record` and rejects more than eight distinct fragments across
all linked records (`E_PRIV_AGGREGATE`). This closes the split-record attack in
which individually short values can be assembled into private text. Synthetic
validator fixtures remain valid when they use isolated synthetic tokens and
distinct hash-shaped source IDs; no production-data exception exists.

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

During the G0 data migration, add `adjudication_contract` and all 159 opaque
mapping IDs in the same canonical update. The validator requires exact 106+53
source-grain equality and exact 125/32/2 disposition accounting. Do not infer
that the resulting `records` length must be 159: deterministic dedup/upsert is
still authoritative.

The registry is bookkeeping for the RED-only F4 lane: recording a pair here
neither widens nor bypasses F4, and no engine code reads this directory.
