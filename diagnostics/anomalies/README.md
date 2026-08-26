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
  `offline_probe|structural_trace|full_trace|unit_test|operator_report`, or
  `null`. Enhanced reproduced-or-later bug records also carry exact
  `covered_adjudication_refs` and `covered_source_event_refs`; those two
  fields remain optional for legacy records.
- `red_test` — `{path, name, commit}` (40-hex commit that contains the test),
  or `null`. `red_test_waiver` remains a legacy compatibility field only:
  it is forbidden on every enhanced record, whose fixed-or-later lifecycle
  requires actual covered RED evidence.
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
- `notes` — short free text for legacy records (max 500 chars, single line);
  an enhanced record uses a typed opaque metadata ref or `null`.
- `record_origin` / `baseline_record_sha256` — both keys are mandatory on every
  enhanced record and forbidden, even with a null value, on each retained
  legacy record. `record_origin` is `new` or
  `preexisting_public_baseline`. A new record carries the
  explicitly-present null baseline digest and may not carry a free public-token
  literal. A baseline update carries the nonzero SHA-256 of its complete
  derived legacy projection: remove only the enhanced origin/adjudication/fix
  fields and enhanced evidence-coverage arrays, preserve every legacy identity
  and content field, then serialize as compact ASCII JSON with sorted keys.
  The nine retained unadjudicated records carry neither field.
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
legacy records, and therefore yields 111 records. The action split is also
exact: 20 enhanced updates of the reviewed public baseline + 82 new enhanced
records + 9 unchanged legacy keeps. The two source exclusions live in
top-level `source_exclusions[]`, never as anomaly records. They still
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
  supplemental mappings and exclusions may never use that sentinel. The
  validator additionally checks an explicit
  `normalized_class × classification × disposition × owner_lane` allow
  matrix: grammar/language-quality classes cannot enter mechanical/F4 lanes,
  and mechanical/input-loss classes cannot enter language-quality or guard
  lanes.
- `expected` — per-adjudication `{status, authority, value_ref}`. Status is
  `null_non_unique`, `null_pending_operator_intent`,
  `unique_operator_confirmed`, `unique_authoritative_spelling`, or
  `not_applicable`; authority is `final|null|operator|authoritative|legacy_bridge|not_applicable`.
  Unique expected forms carry a typed value-domain HMAC reference rather than
  a literal token. Null/not-applicable forms carry `null`. This represents a
  redacted unique expected form and split-component fan-in without leaking or
  collapsing distinct expected values. Deprecated `expected_form_status`,
  when present during additive migration, must equal `expected.status`.
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
  `source_record` is mandatory; its ref uses the `source_record` HMAC
  domain, while ruling/receipt/span/other refs use the `metadata` domain.
  Refs contain no path, label, sentence or user text. Every guard mapping —
  including a guard in a mixed bug/guard record — independently needs its own
  adjudication `ruling` or `receipt` ref.
- `source_event_refs` — non-empty canonical
  event-domain IDs. An event may support several split mappings inside one
  dedup record, but it cannot be reconciled to two records and cannot increment
  `occurrence_count` twice.

### Opaque reference construction

The only enhanced public opaque-reference scheme is
`smartkey-g0-hmac-sha256-v1`. Its public representation is:

```text
smartkey-g0-hmac-sha256-v1:<domain>:<key_id>:<64-lowercase-hex-MAC>
```

`domain` is exactly one of `value`, `event`, `metadata`, or `source_record`.
`key_id` is a public random 32-lowercase-hex rotation-epoch identifier; it is
not the key. Every reference in one sealed contract must use that contract's
pinned `key_id`. A MAC suffix cannot be reused across domains or key epochs;
an identical same-domain reference may be shared where several mappings cite
the same source.

The byte contract is pinned as `smartkey-g0-hmac-byte-contract-v1`. The future
private importer uses one cryptographically random key of at least 32 bytes,
stored only in its mode-`0600` private corpus — never in this repository,
debate messages or logs. The complete domain inventory is deliberately small
and contains no floating-point payload:

| domain | payload profile | imported value shape |
|---|---|---|
| `value` | `scalar_utf8` | one Unicode-scalar string, encoded as its exact UTF-8 bytes without normalization |
| `event` | `project_canonical_json_v1` | event identity/evidence envelope |
| `metadata` | `project_canonical_json_v1` | metadata/receipt envelope |
| `source_record` | `project_canonical_json_v1` | source identity/evidence envelope |

The three structured envelopes need only null, booleans, safe integers,
Unicode-scalar strings, arrays and string-key objects. They use
`smartkey-g0-canonical-json-v1`, a project contract — **not** an assertion of
RFC 8785/JCS compatibility:

1. Integers are limited to `-9007199254740991` through
   `9007199254740991` and emitted as minimal base-10 syntax. Raw textual `-0`,
   all decimals/exponents, every float, non-finite number and out-of-range
   integer are rejected.
2. Object keys are Unicode-scalar strings, unique at the raw parser boundary,
   and sorted lexicographically by Unicode code point. Arrays retain order.
   Compact `,` and `:` delimiters are used with no whitespace.
3. Strings receive no Unicode normalization. Quote and backslash become
   `\"` and `\\`; backspace, tab, newline, form feed and carriage return use
   `\b`, `\t`, `\n`, `\f`, `\r`; other U+0000–U+001F controls use lowercase
   `\u00xx`. Slash is not escaped. Every other Unicode scalar is emitted
   literally and the final text is UTF-8 encoded. Lone surrogates are rejected.
4. A raw importer must first call `parse_project_canonical_json`, which sees
   and rejects duplicate keys and textual `-0` before parsing erases that
   information. `canonical_structured_payload_bytes` is a separate typed-value
   encoder: it rejects unsupported typed values but cannot recover duplicate
   keys or numeric spelling already discarded by another parser.

The exact MAC input is the concatenation of:

1. ASCII `smartkey-g0-hmac-sha256-v1`;
2. one NUL byte;
3. the ASCII domain (so domain separation is inside the MAC input, not only
   in the printed prefix);
4. one NUL byte;
5. the payload length as one unsigned 8-byte big-endian integer;
6. the payload bytes.

Plain or unsalted `sha256(raw/user text)` and the legacy
`hmac-sha256-v1:...` form are forbidden because short typed forms are
dictionary-recoverable. Key rotation changes `key_id` and requires complete
independent re-derivation and re-receipting of every reference. The public
validator checks scheme/domain syntax, pinned-key consistency and
cross-domain/key-epoch suffix reuse; it cannot prove secret derivation. The
external private receipt must recompute every reference, domain, serialization
choice and key ID. It must bind the byte-contract version, the exact domain
profile map and vector-set receipt
`50aa96caa624f6b4159d18f553910060a0ffb13de6bb91fc5415459007b6fc4f`.

### Public synthetic interoperability vectors

These vectors contain only deliberately synthetic public fixture data. The
test key is the literal ASCII byte string
`smartkey-g0-public-synthetic-vector-key-v1` (42 bytes), hex
`736d6172746b65792d67302d7075626c69632d73796e7468657469632d766563746f722d6b65792d7631`;
its public fixture `key_id` is `0123456789abcdef0123456789abcdef`.
It is not a production/private key and must never be reused by an importer.
For each row, HMAC-SHA256 is computed over the complete frame described above.

The four typed inputs, written using their accepted raw-JSON spellings except
for the scalar first row, are:

```text
value_non_ascii = café
event_nested = {"active":true,"count":2,"event":"synthetic","parts":["alpha",null,{"ok":false}]}
metadata_escaping = {"control":"\b\t\n\f\r\u0000","label":"café","quote":"\"\\/"}
source_record_integer_edges = {"max":9007199254740991,"min":-9007199254740991,"records":[1,0,-1]}
```

| name / domain / profile | payload hex | complete framed-input hex | HMAC-SHA256 |
|---|---|---|---|
| `value_non_ascii` / `value` / `scalar_utf8` | `636166c3a9` | `736d6172746b65792d67302d686d61632d7368613235362d76310076616c7565000000000000000005636166c3a9` | `134f37d80a6cf037adb96d129c300af20c98ecd25c325fa99bb0c43791a5fda1` |
| `event_nested` / `event` / `project_canonical_json_v1` | `7b22616374697665223a747275652c22636f756e74223a322c226576656e74223a2273796e746865746963222c227061727473223a5b22616c706861222c6e756c6c2c7b226f6b223a66616c73657d5d7d` | `736d6172746b65792d67302d686d61632d7368613235362d7631006576656e740000000000000000517b22616374697665223a747275652c22636f756e74223a322c226576656e74223a2273796e746865746963222c227061727473223a5b22616c706861222c6e756c6c2c7b226f6b223a66616c73657d5d7d` | `15fe13ea61be04ae11ad44f24bdffb2bc15bf3ad4086686a9a816f2c432ea319` |
| `metadata_escaping` / `metadata` / `project_canonical_json_v1` | `7b22636f6e74726f6c223a225c625c745c6e5c665c725c7530303030222c226c6162656c223a22636166c3a9222c2271756f7465223a225c225c5c2f227d` | `736d6172746b65792d67302d686d61632d7368613235362d7631006d6574616461746100000000000000003e7b22636f6e74726f6c223a225c625c745c6e5c665c725c7530303030222c226c6162656c223a22636166c3a9222c2271756f7465223a225c225c5c2f227d` | `3c190f2b02d21c5dec8e31ce21c0bd45abdc0187f157f9329d351d5295f667b2` |
| `source_record_integer_edges` / `source_record` / `project_canonical_json_v1` | `7b226d6178223a393030373139393235343734303939312c226d696e223a2d393030373139393235343734303939312c227265636f726473223a5b312c302c2d315d7d` | `736d6172746b65792d67302d686d61632d7368613235362d763100736f757263655f7265636f72640000000000000000437b226d6178223a393030373139393235343734303939312c226d696e223a2d393030373139393235343734303939312c227265636f726473223a5b312c302c2d315d7d` | `121118ba0fc9558a6870837176a3e5c5fa028522391ab0cc067609a4e3a0630e` |

The vector-set receipt is SHA-256 over the compact, ASCII-escaped,
sorted-key JSON array used in `ibus/test_anomaly_registry.py`; each item binds
`name`, `domain`, `profile`, `key_id`, `key_hex`, `payload_hex`, `frame_hex`
and `mac_sha256`. This public set is an interoperability check, not authority
for private derivation or real imported data.

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

## Complete projection commitments

The semantic tuple digest does not by itself bind record-only fields or the
nine retained unadjudicated records. A sealed contract therefore also requires
two nonzero, recomputed SHA-256 commitments:

- `legacy_projection_sha256` hashes the nine complete legacy record objects,
  sorted by record ID and serialized as compact ASCII JSON with sorted keys.
- `registry_projection_sha256` hashes the complete sealed document —
  all 111 records, both top-level exclusions and contract metadata — after
  removing only the self-referential `registry_projection_sha256` field.
  It consequently also binds the semantic and legacy digests.
- Every baseline-update record carries its own `baseline_record_sha256` over
  the complete derived legacy projection described above.
  `baseline_record_set_sha256` then hashes the sorted exact set of 20
  `{record_id, baseline_record_sha256}` pairs. The sealed
  `baseline_external_receipt` must approve that exact set and count. Therefore
  changing a literal, adding another literal-bearing legacy field or
  transplanting a record digest fails even after ordinary semantic/registry
  digests are locally recomputed. Changing the approved baseline set is a new
  external-authority event, not a local reseal.

Stale digests reject identity, payload, note, evidence, exclusion and legacy
substitutions even when counts stay unchanged. A digest that merely recomputes
over attacker-supplied data is not authority, so the real import must publish
an independently reviewed external receipt for all projection and baseline
digests in the same integration commit. The public validator checks the
receipt envelope and exact committed set; independent tooling owns receipt
authenticity. Synthetic fixture digests/receipt shapes prove validator
mechanics only and must never be reused as production pins.

This schema lane intentionally has no real baseline digest to pin. The real
import integration must promote its independently reviewed 20-record-set
digest and detached receipt SHA to validator/schema constants in that same
commit. Until that happens, a locally rewritten receipt envelope is merely a
claim and must not be treated as authority or an import PASS.

## Lifecycle gates (enforced by `validate.py`)

| status | must carry |
|---|---|
| `open_suspected_smartkey` | nothing beyond the envelope; **the default** |
| `reproduced` | `reproducer` |
| `red_tested` | `reproducer` + `red_test` |
| `fixed` | `reproducer` + `fix_commit` + `red_test` (legacy may use a waiver; enhanced may not) |
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

For an enhanced bug record, advancing to `reproduced` requires `reproducer`
coverage equal to every relevant bug adjudication ref and the corresponding
source-event union. `red_tested` adds the same coverage in `red_test`;
`fixed` adds it in `fix_evidence`; `verified`/`closed` add it in
`verification`. Duplicate refs/events and partial coverage fail. Thus one
reproduced or fixed component cannot advance an uncovered sibling mapping in a
fan-in record.

## Privacy rules (enforced)

Never full text, credentials, employer data, keystreams or live trace copies.
The validator walks every string of every record and rejects: any `@`;
`http` outside `source_refs`; a digit run longer than six outside SHA and
identifier fields (`build_sha`, `commit`, `fix_commit`, `ref`,
`evidence_refs`, `id`); a Latin/Cyrillic letter run longer than 40 characters;
newlines; more than one token per context side; whitespace inside a token.
Unknown provenance is written as `unknown` / `null`, never inferred.

G0 adds a closed, recursive, path/type-aware allowlist for every string-bearing
enhanced field and every dynamic dictionary key. Bare 12/16/40/64-hex strings
are accepted only by explicitly typed SHA/ID fields. Environment labels and
dynamic flag keys; legacy source refs/surfaces; reproducer, test and
verification refs/paths/names/surfaces; summaries, notes, descriptors and
closure reasons must be controlled enums, correctly typed
`smartkey-g0-hmac-sha256-v1:metadata:…` refs or null. There is no global
"known enum" fallback: a value allowed as a status, authority or confidence
cannot be borrowed by notes, summaries or any other field. Literal
`synthetic` is accepted only at the three documented environment-label paths
in validator fixtures. JSON Schema's `additionalProperties: false` closes
structural dictionary keys; the validator separately checks dynamic `flags`
keys.

Free `public_token` literals are forbidden on every `record_origin: new`
record, regardless of whether they look alphabetic, base64-like, hex-like or
otherwise harmless. Literal token sides survive only in an externally
approved `preexisting_public_baseline` projection; its exact per-record and
20-record-set commitments, rather than a shape heuristic, are the safety
boundary. Redacted/metadata-only new records use null/placeholder sides plus
typed metadata/value refs.

The aggregate gate walks values and dynamic keys, assigns every
reconstruction-bearing fragment from both values and dynamic keys to each
linked opaque `source_record`, and
rejects more than eight distinct fragments across linked records
(`E_PRIV_AGGREGATE`). Typed metadata HMAC refs do not consume the
budget. This closes split-record and split-field reconstruction attacks; there
is no production-data exception. Validation errors are non-reflective: they
report a code, a safe JSON path and an expected shape/type (optionally a safe
length/count), never a rejected token, key, dedup value or corpus fragment.

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
and the complete 111-record/9-legacy projections, the 20-update baseline set,
the exact 20/82/9 action split and the full private HMAC receipt. Independently
receipt all real digests and derivations in that integration commit. No mapping
draft, synthetic digest/receipt shape or count summary can substitute for this
integration gate.

The registry is bookkeeping for the RED-only F4 lane: recording a pair here
neither widens nor bypasses F4, and no engine code reads this directory.
