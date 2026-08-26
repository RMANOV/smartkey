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
- `recorded_utc`, `first_seen_utc`, `last_seen_utc` — all three keys are
  required, while each value is a **date, datetime, or `null`** despite the
  `_utc` suffix: `YYYY-MM-DD` when only the day is genuinely known and
  `YYYY-MM-DDTHH:MM[:SS]Z` when the time is. `null` means only that there is
  no authoritative public timestamp; it does not reveal whether a value was
  originally unknown or was withheld by an approved private projection.
  Known pairs alone participate in chronology checks, and timestamps are
  never guessed.
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

### Timestamp-authority boundary

The public schema deliberately contains no authority mask, inferred-value
provenance, timestamp override or reconstructed-file receipt field. A public
`null` therefore cannot be used to infer private provenance. The private R3
producer follows the fixed
`smartkey-g0-semantic-authority-projection-v2` contract: for its exact audited
input, it projects the two approved non-authoritative `recorded_utc` and
`last_seen_utc` values to `null` before public commitments are calculated.
That projection is not selectable by a document or caller, and no public API
accepts a mask, override set or trust flag. Ordinary records may independently
carry `null` because no authoritative timestamp was known.

The SHA-256 of an exact reconstructed file remains a private, receipt-only
fact. It is non-decisional for public projections, absent from the public
schema and source-record HMAC envelope, and cannot be reconstructed from a
public `null`.

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

The byte contract is pinned as `smartkey-g0-hmac-byte-contract-v3`. The future
private importer uses one cryptographically random key of at least 32 bytes,
stored only in its mode-`0600` private corpus — never in this repository,
debate messages or logs. The complete domain inventory is deliberately small
and contains no floating-point payload:

| domain | payload profile | imported value shape |
|---|---|---|
| `value` | `scalar_utf8` | one exact built-in Unicode-scalar `str`, encoded as its exact UTF-8 bytes without normalization |
| `event` | `project_canonical_json_v1` | one exact built-in `dict` event identity/evidence envelope |
| `metadata` | `project_canonical_json_v1` | one exact built-in `dict` metadata/receipt envelope |
| `source_record` | `smartkey-g0-source-record-semantic-v1` | one exact built-in `dict` whole-merged-source semantic envelope |

The generic event and metadata envelopes, plus the closed source-record
envelope defined below, need only null, booleans, safe integers,
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
4. A raw importer must first call `parse_project_canonical_json` with an exact
   built-in `str`. It sees and rejects duplicate keys and textual `-0` before
   parsing erases that information. `canonical_structured_payload_bytes` is a
   separate typed-value encoder: it accepts only exact built-in `None`, `bool`,
   `int`, `str`, `list` and `dict` values with exact built-in `str` keys. It
   cannot recover duplicate keys or numeric spelling already discarded by
   another parser. Subclasses are rejected before any overridden method can be
   invoked.
5. HMAC payloads add a root-shape gate: `value` is an exact built-in `str`,
   while `event` and `metadata` are exact built-in `dict` envelopes. The
   `source_record` domain adds the stricter closed semantic envelope below.
   Arrays and the other supported scalar types remain valid only below a
   structured root.

The public `source_record_identity_envelope(source_record)` builder accepts
one whole merged-source record and no policy arguments. It hardcodes profile
`smartkey-g0-source-record-semantic-v1`, copies only `merged_id`,
`source_schema_version`, `platform`, `authorship_confidence`,
`excluded_segments`, and `raw_sha256` into fresh built-in containers, and
ignores every other input field. In particular, timestamps, text, paths,
session/source hashes, reconstruction/file receipts, adjudication/action data,
event/unit/location candidates and `source_event_refs` cannot affect this
domain. Event, unit and location identity remains independently decisional in
the `event` HMAC lane. `raw_sha256` is the approved merged-source field; it is
never populated from a full reconstructed-file receipt fallback.

The HMAC-facing source envelope is closed: those six copied fields plus the
fixed `profile` are all required and no extras are allowed. `merged_id` and
`raw_sha256` are exactly 64 lowercase hex characters; the four other root
strings are non-empty. `excluded_segments` is an exact built-in list of exact
built-in dictionaries, each with only `authorship_confidence`, `start_char`,
`end_char`, and `reason`. Its strings are non-empty, and its positions are
JSON integers (not booleans) satisfying
`0 <= start_char < end_char <= 9007199254740991`. Builder output is sorted by
the exact ascending tuple `(start_char, end_char, authorship_confidence UTF-8
bytes, reason UTF-8 bytes)`. The HMAC boundary requires that order already;
it never silently repairs a fabricated envelope. Exact duplicate segment
objects are invalid, while overlapping non-identical ranges are allowed.
Every malformed source envelope fails non-reflectively as exactly
`ValueError("E_HMAC_CONTRACT")`.

The resource profile is independently pinned as
`smartkey-g0-hmac-resource-contract-v1`. The schema lane inventories one
scalar value and three per-reference object envelopes; it does not authorize
placing the complete 111-record projection in a single payload. Those objects
carry bounded identifiers, enum-like metadata, safe integer counters and
short arrays/objects. The published 159-reference and 111-record cardinalities
remain well below the container/node ceilings, while byte ceilings leave
substantial room for future envelope fields without permitting unbounded
materialization:

The inventory used for those ceilings is public structure, not private corpus
measurement: a record has 28 declared fields, an adjudication has 12, the
longest declared field name is 27 UTF-8 bytes, a typed HMAC ref is at most 138
bytes, a registry token is at most 40 Unicode scalars (160 UTF-8 bytes at the
worst scalar width), the sealed import has exactly 159 adjudication refs,
and the deepest current record/adjudication shape is four containers. Thus the
member, key/string, array and depth limits provide at least 18x, 37x/409x,
6.4x and 8x headroom respectively. There is no real importer or private-data
measurement in this schema-only lane; integration must prove each reviewed
envelope fits these public limits or revise and independently re-receipt the
contract rather than silently widening it.

| resource | exact public limit |
|---|---:|
| raw JSON UTF-8 bytes | 262,144 |
| canonical payload bytes | 65,536 |
| lexical nesting depth before parsing | 32 |
| typed nesting depth | 32 |
| total nodes, including object keys | 4,096 |
| members in one array | 1,024 |
| members in one object | 512 |
| one string's UTF-8 bytes | 65,528 |
| one key's UTF-8 bytes | 1,024 |
| digits in one raw integer token | 16 |

The raw boundary counts UTF-8 bytes, lexical depth, nodes, members, decoded
string/key bytes (including every escape) and integer digits before
`json.loads` can materialize a container or integer. Before canonical
traversal, the typed boundary captures each exact built-in list or dictionary
into a bounded immutable deep snapshot: lists and dictionary item pairs are
captured at no more than their limit plus one, keys are validated before
sorting, and the live source is never read again. Active identities reject
cycles; a repeated alias reuses its first completed snapshot. A separate
encoder walk still charges every logical serialized occurrence against the
node and depth budgets, and appends to a byte buffer only while the
canonical-payload budget remains. Boundary values are contractual; every
`+1` case fails closed.
Malformed input, duplicate keys, unsupported types/numbers, resource excess,
depth excess, cycles, bad roots and unexpected internals become one of the
fixed non-reflective codes `canonical_syntax`, `canonical_type`,
`canonical_bounds`, `canonical_resource`, `canonical_depth`,
`canonical_cycle`, `canonical_root` or `canonical_internal`. The public
exception is raised from a clean helper after the private parser/encoder has
discarded raw values, typed containers and internal decoder exceptions; it has
no cause or context and never echoes a value or key.

The resource-contract digest is SHA-256 over compact sorted-key ASCII JSON of
its version, exact domain-root map and limit map:
`50c430ef4de54c935e9bbbc4e6929dc7fbd28ba0f549da843526dcaf0f271bab`.

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
choice and key ID. It must bind the byte-contract version, exact domain profile
and root maps, resource-contract version and digest, and vector-set receipt
`0672508a1525bb5d606a79b30940dfe9ff8dca032532cd29f15d110ed5185d0f`.
The v3 change replaces the former generic source-record profile with the
closed whole-source semantic profile. A v2 contract, generic source profile,
or stale/arbitrary vector receipt is rejected; migration requires complete
private re-derivation and re-receipting of every affected reference rather
than locally relabelling an old MAC.

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
event_nested = {"active":true,"count":2,"event":"synthetic","parts":["alpha",null,{"ok":false}],"unit_separator":"\u001f","\ue000":"bmp","\ud800\udc00":"astral"}
metadata_escaping = {"control":"\b\t\n\f\r\u0000","label":"café","quote":"\"\\/"}
source_record_whole_source_semantic_v1 = {
  "profile":"smartkey-g0-source-record-semantic-v1",
  "merged_id":"c0045ec8791a7f514f4addc5982ecfbdd9e28ab80bdaea91f5e6fbc6dfbf395e",
  "source_schema_version":"synthetic-source-v1",
  "platform":"synthetic-platform",
  "authorship_confidence":"synthetic-confidence",
  "excluded_segments":[
    {"authorship_confidence":"synthetic-alpha","start_char":0,"end_char":4,"reason":"synthetic-alpha"},
    {"authorship_confidence":"synthetic-alpha","start_char":0,"end_char":4,"reason":"synthetic-zulu"},
    {"authorship_confidence":"synthetic-beta","start_char":0,"end_char":4,"reason":"synthetic-alpha"},
    {"authorship_confidence":"synthetic-zulu","start_char":0,"end_char":4,"reason":"synthetic-zulu"},
    {"authorship_confidence":"synthetic-alpha","start_char":0,"end_char":6,"reason":"synthetic-alpha"},
    {"authorship_confidence":"synthetic-aaa","start_char":8,"end_char":12,"reason":"synthetic-aaa"}
  ],
  "raw_sha256":"56d890f43577f16f03358ea0c94bb7bd7e1e6864b908585e24be54ae2734cd56"
}
```

| name / domain / profile | payload hex | complete framed-input hex | HMAC-SHA256 |
|---|---|---|---|
| `value_non_ascii` / `value` / `scalar_utf8` | `636166c3a9` | `736d6172746b65792d67302d686d61632d7368613235362d76310076616c7565000000000000000005636166c3a9` | `134f37d80a6cf037adb96d129c300af20c98ecd25c325fa99bb0c43791a5fda1` |
| `event_nested` / `event` / `project_canonical_json_v1` | `7b22616374697665223a747275652c22636f756e74223a322c226576656e74223a2273796e746865746963222c227061727473223a5b22616c706861222c6e756c6c2c7b226f6b223a66616c73657d5d2c22756e69745f736570617261746f72223a225c7530303166222c22ee8080223a22626d70222c22f0908080223a2261737472616c227d` | `736d6172746b65792d67302d686d61632d7368613235362d7631006576656e740000000000000000877b22616374697665223a747275652c22636f756e74223a322c226576656e74223a2273796e746865746963222c227061727473223a5b22616c706861222c6e756c6c2c7b226f6b223a66616c73657d5d2c22756e69745f736570617261746f72223a225c7530303166222c22ee8080223a22626d70222c22f0908080223a2261737472616c227d` | `800c5952070ce32356a99537c95d2eeec515a854a7356379623d5ae4001ae94d` |
| `metadata_escaping` / `metadata` / `project_canonical_json_v1` | `7b22636f6e74726f6c223a225c625c745c6e5c665c725c7530303030222c226c6162656c223a22636166c3a9222c2271756f7465223a225c225c5c2f227d` | `736d6172746b65792d67302d686d61632d7368613235362d7631006d6574616461746100000000000000003e7b22636f6e74726f6c223a225c625c745c6e5c665c725c7530303030222c226c6162656c223a22636166c3a9222c2271756f7465223a225c225c5c2f227d` | `3c190f2b02d21c5dec8e31ce21c0bd45abdc0187f157f9329d351d5295f667b2` |
| `source_record_whole_source_semantic_v1` / `source_record` / `smartkey-g0-source-record-semantic-v1` | `7b22617574686f72736869705f636f6e666964656e6365223a2273796e7468657469632d636f6e666964656e6365222c226578636c756465645f7365676d656e7473223a5b7b22617574686f72736869705f636f6e666964656e6365223a2273796e7468657469632d616c706861222c22656e645f63686172223a342c22726561736f6e223a2273796e7468657469632d616c706861222c2273746172745f63686172223a307d2c7b22617574686f72736869705f636f6e666964656e6365223a2273796e7468657469632d616c706861222c22656e645f63686172223a342c22726561736f6e223a2273796e7468657469632d7a756c75222c2273746172745f63686172223a307d2c7b22617574686f72736869705f636f6e666964656e6365223a2273796e7468657469632d62657461222c22656e645f63686172223a342c22726561736f6e223a2273796e7468657469632d616c706861222c2273746172745f63686172223a307d2c7b22617574686f72736869705f636f6e666964656e6365223a2273796e7468657469632d7a756c75222c22656e645f63686172223a342c22726561736f6e223a2273796e7468657469632d7a756c75222c2273746172745f63686172223a307d2c7b22617574686f72736869705f636f6e666964656e6365223a2273796e7468657469632d616c706861222c22656e645f63686172223a362c22726561736f6e223a2273796e7468657469632d616c706861222c2273746172745f63686172223a307d2c7b22617574686f72736869705f636f6e666964656e6365223a2273796e7468657469632d616161222c22656e645f63686172223a31322c22726561736f6e223a2273796e7468657469632d616161222c2273746172745f63686172223a387d5d2c226d65726765645f6964223a2263303034356563383739316137663531346634616464633539383265636662646439653238616238306264616561393166356536666263366466626633393565222c22706c6174666f726d223a2273796e7468657469632d706c6174666f726d222c2270726f66696c65223a22736d6172746b65792d67302d736f757263652d7265636f72642d73656d616e7469632d7631222c227261775f736861323536223a2235366438393066343335373766313666303333353865613063393462623762643765316536383634623930383538356532346265353461653237333463643536222c22736f757263655f736368656d615f76657273696f6e223a2273796e7468657469632d736f757263652d7631227d` | `736d6172746b65792d67302d686d61632d7368613235362d763100736f757263655f7265636f72640000000000000003b07b22617574686f72736869705f636f6e666964656e6365223a2273796e7468657469632d636f6e666964656e6365222c226578636c756465645f7365676d656e7473223a5b7b22617574686f72736869705f636f6e666964656e6365223a2273796e7468657469632d616c706861222c22656e645f63686172223a342c22726561736f6e223a2273796e7468657469632d616c706861222c2273746172745f63686172223a307d2c7b22617574686f72736869705f636f6e666964656e6365223a2273796e7468657469632d616c706861222c22656e645f63686172223a342c22726561736f6e223a2273796e7468657469632d7a756c75222c2273746172745f63686172223a307d2c7b22617574686f72736869705f636f6e666964656e6365223a2273796e7468657469632d62657461222c22656e645f63686172223a342c22726561736f6e223a2273796e7468657469632d616c706861222c2273746172745f63686172223a307d2c7b22617574686f72736869705f636f6e666964656e6365223a2273796e7468657469632d7a756c75222c22656e645f63686172223a342c22726561736f6e223a2273796e7468657469632d7a756c75222c2273746172745f63686172223a307d2c7b22617574686f72736869705f636f6e666964656e6365223a2273796e7468657469632d616c706861222c22656e645f63686172223a362c22726561736f6e223a2273796e7468657469632d616c706861222c2273746172745f63686172223a307d2c7b22617574686f72736869705f636f6e666964656e6365223a2273796e7468657469632d616161222c22656e645f63686172223a31322c22726561736f6e223a2273796e7468657469632d616161222c2273746172745f63686172223a387d5d2c226d65726765645f6964223a2263303034356563383739316137663531346634616464633539383265636662646439653238616238306264616561393166356536666263366466626633393565222c22706c6174666f726d223a2273796e7468657469632d706c6174666f726d222c2270726f66696c65223a22736d6172746b65792d67302d736f757263652d7265636f72642d73656d616e7469632d7631222c227261775f736861323536223a2235366438393066343335373766313666303333353865613063393462623762643765316536383634623930383538356532346265353461653237333463643536222c22736f757263655f736368656d615f76657273696f6e223a2273796e7468657469632d736f757263652d7631227d` | `76f946e28426dd9264533c26bca65c1470842bf50332c0c1b22a318c9e4b83ce` |

The vector-set receipt is SHA-256 over the compact, ASCII-escaped,
sorted-key JSON array used in `ibus/test_anomaly_registry.py`; each item binds
`name`, `domain`, `profile`, `key_id`, `key_hex`, `payload_hex`, `frame_hex`
and `mac_sha256`. This public set is an interoperability check, not authority
for private derivation or real imported data.

The event vector deliberately combines U+001F, BMP key U+E000 and astral key
U+10000. Code-point ordering places U+E000 before U+10000; JavaScript's default
UTF-16 `.sort()` produces the opposite order and therefore is not compliant.
Tests pin an explicit Node code-point comparator alongside Python output, and
OpenSSL independently recomputes each published HMAC.

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
