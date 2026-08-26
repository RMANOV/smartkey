#!/usr/bin/env python3
"""Validate the SmartKey anomaly registry (stdlib only, no side effects).

    python3 diagnostics/anomalies/validate.py [REGISTRY_JSON] [--schema SCHEMA_JSON]

Exit codes: 0 = valid, 1 = one or more violations (each printed as
``CODE: <path>: <message>``), 2 = the registry or schema could not be read.

Checks, in order:

1. structural contract from ``schema.json`` (types, enums, required keys,
   no unknown keys, patterns, bounds) via a small JSON-Schema subset;
2. deterministic identity: ``dedup_key`` and ``id`` are recomputed from the
   observed/expected sides and must match; keys and ids are unique;
3. side consistency: script class of each token, token/descriptor/privacy
   coupling, ``needs_operator_confirmation`` coupling, ``repeat`` vs
   ``occurrence_count``, timestamp ordering;
4. optional sealed G0 adjudication mapping: exact ID/count accounting, typed
   HMAC-ref domains, enum/normalized-class coupling, expected-form provenance,
   causal confidence and complete semantic/registry/legacy commitments;
5. lifecycle gates: what evidence each ``status`` must carry;
6. privacy walk over every string in every record: no ``@``, no ``http``
   outside ``source_refs``, no digit run longer than six characters outside
   SHA/reference fields, no letter run longer than 40 characters, no
   newlines, one whitespace-free token per side of the context; enhanced
   records also have a path/type-aware closed allowlist, non-reflective
   diagnostics and an aggregate source-linked reconstruction budget;
7. canonical on-disk format (indent 2, non-ASCII preserved, trailing newline).

The registry never changes SmartKey behaviour: this file only reads JSON.
"""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
import re
import sys
import unicodedata
from pathlib import Path

HERE = Path(__file__).resolve().parent
DEFAULT_REGISTRY = HERE / "registry.json"
DEFAULT_SCHEMA = HERE / "schema.json"

STATUSES = (
    "open_suspected_smartkey",
    "reproduced",
    "red_tested",
    "fixed",
    "verified",
    "closed",
    "not_smartkey",
)
PRIVACY_CLASSES = ("public_token", "redacted", "unknown")
UNCONFIRMED_STATUSES = ("open_suspected_smartkey", "reproduced")
ADJUDICATION_CLASSIFICATIONS = (
    "guard_not_bug_unless_intent_changes",
    "instrumentation_first_candidate",
    "intent_confirmation_needed",
    "language_quality_feature_candidate",
    "mechanical_red_candidate",
    "source_authorship_exclusion",
)
ADJUDICATION_DISPOSITIONS = ("bug_candidate", "guard", "source_exclusion")
EXPECTED_FORM_STATUSES = (
    "null_non_unique",
    "null_pending_operator_intent",
    "unique_operator_confirmed",
    "unique_authoritative_spelling",
    "not_applicable",
)
EXPECTED_AUTHORITIES = (
    "final",
    "null",
    "operator",
    "authoritative",
    "legacy_bridge",
    "not_applicable",
)
CAUSAL_CONFIDENCES = ("none", "low", "medium", "high", "not_applicable")
OWNER_LANES = (
    "adapter_event_integrity",
    "composite_boundary_and_provenance",
    "f4_core",
    "f4_core_both_supported_contextual",
    "f4_core_early_lock_full_word",
    "f4_core_one_sided_exact",
    "future_language_quality_layer",
    "future_language_quality_or_intent",
    "intended_english_quoted_guard",
    "output_neutral_provenance",
    "outside_product_source_authorship",
    "punctuation_boundary",
    "same_script_loss",
)
NORMALIZED_CLASSES = (
    "not_applicable",
    "boundary_extra_hyphen",
    "boundary_extra_space",
    "boundary_space_before_terminal_punctuation",
    "brand_or_intentional_transliteration_ambiguity",
    "brand_or_native_word_layout_ambiguity",
    "duplicated_character",
    "f4_early_lock_full_word_crossover",
    "grammar_adjective_or_ellipsis",
    "grammar_definite_article",
    "grammar_missing_comma",
    "grammar_verb_or_mood",
    "intentional_transliteration_guard",
    "loanword_vowel_orthography",
    "orthographic_missing_hyphen",
    "orthographic_word_boundary_space",
    "phrase_level_omission",
    "same_script_character_omission",
    "same_script_character_substitution",
    "semantic_word_form_ambiguity",
    "source_harness_framing",
    "style_punctuation_guard",
    "style_sensitive_extra_comma",
)
ADJUDICATION_PRIVACY_CLASSES = ("public_token", "redacted", "metadata_only")
ADJUDICATION_GRAINS = ("original_candidate", "supplemental_hypothesis")
ADJUDICATION_SOURCE_KINDS = (
    "source_record",
    "source_span",
    "ruling",
    "receipt",
    "other",
)
ADJUDICATION_COUNT_CONTRACT = {
    "original_candidate": 106,
    "supplemental_hypothesis": 53,
    "bug_candidate": 125,
    "guard": 32,
    "source_exclusion": 2,
}
CLASSIFICATION_DISPOSITION = {
    "guard_not_bug_unless_intent_changes": "guard",
    "instrumentation_first_candidate": "bug_candidate",
    "intent_confirmation_needed": "bug_candidate",
    "language_quality_feature_candidate": "bug_candidate",
    "mechanical_red_candidate": "bug_candidate",
    "source_authorship_exclusion": "source_exclusion",
}
CLASSIFICATION_OWNER_LANES = {
    "guard_not_bug_unless_intent_changes": frozenset(
        {"future_language_quality_layer", "intended_english_quoted_guard"}
    ),
    "instrumentation_first_candidate": frozenset(
        {
            "adapter_event_integrity",
            "composite_boundary_and_provenance",
            "output_neutral_provenance",
            "punctuation_boundary",
            "same_script_loss",
        }
    ),
    "intent_confirmation_needed": frozenset(
        {"future_language_quality_layer", "future_language_quality_or_intent"}
    ),
    "language_quality_feature_candidate": frozenset({"future_language_quality_layer"}),
    "mechanical_red_candidate": frozenset(
        {
            "f4_core",
            "f4_core_both_supported_contextual",
            "f4_core_early_lock_full_word",
            "f4_core_one_sided_exact",
            "punctuation_boundary",
        }
    ),
    "source_authorship_exclusion": frozenset({"outside_product_source_authorship"}),
}


def _tuples(
    classification: str, lanes: frozenset[str]
) -> frozenset[tuple[str, str, str]]:
    disposition = CLASSIFICATION_DISPOSITION[classification]
    return frozenset((classification, disposition, lane) for lane in lanes)


_ORIGINAL_NOT_APPLICABLE_TUPLES = frozenset(
    item
    for classification, lanes in CLASSIFICATION_OWNER_LANES.items()
    for item in _tuples(classification, lanes)
)
_BOUNDARY_TUPLES = frozenset(
    {
        (
            "instrumentation_first_candidate",
            "bug_candidate",
            "punctuation_boundary",
        ),
        (
            "mechanical_red_candidate",
            "bug_candidate",
            "punctuation_boundary",
        ),
    }
)
_LANGUAGE_QUALITY_TUPLES = frozenset(
    {
        (
            "language_quality_feature_candidate",
            "bug_candidate",
            "future_language_quality_layer",
        )
    }
)
_INTENT_OR_LANGUAGE_TUPLES = frozenset(
    {
        (
            "intent_confirmation_needed",
            "bug_candidate",
            "future_language_quality_layer",
        ),
        (
            "intent_confirmation_needed",
            "bug_candidate",
            "future_language_quality_or_intent",
        ),
        *_LANGUAGE_QUALITY_TUPLES,
    }
)
_INTENT_GUARD_TUPLES = frozenset(
    {
        (
            "guard_not_bug_unless_intent_changes",
            "guard",
            "intended_english_quoted_guard",
        )
    }
)
NORMALIZED_CLASS_ALLOWED_TUPLES = {
    "not_applicable": _ORIGINAL_NOT_APPLICABLE_TUPLES,
    "boundary_extra_hyphen": _BOUNDARY_TUPLES,
    "boundary_extra_space": _BOUNDARY_TUPLES,
    "boundary_space_before_terminal_punctuation": _BOUNDARY_TUPLES,
    "brand_or_intentional_transliteration_ambiguity": _INTENT_GUARD_TUPLES,
    "brand_or_native_word_layout_ambiguity": _INTENT_GUARD_TUPLES,
    "duplicated_character": frozenset(
        {
            (
                "instrumentation_first_candidate",
                "bug_candidate",
                "same_script_loss",
            )
        }
    ),
    "f4_early_lock_full_word_crossover": _tuples(
        "mechanical_red_candidate",
        frozenset(
            {
                "f4_core",
                "f4_core_both_supported_contextual",
                "f4_core_early_lock_full_word",
                "f4_core_one_sided_exact",
            }
        ),
    ),
    "grammar_adjective_or_ellipsis": _LANGUAGE_QUALITY_TUPLES,
    "grammar_definite_article": _LANGUAGE_QUALITY_TUPLES,
    "grammar_missing_comma": _LANGUAGE_QUALITY_TUPLES,
    "grammar_verb_or_mood": _LANGUAGE_QUALITY_TUPLES,
    "intentional_transliteration_guard": _INTENT_GUARD_TUPLES,
    "loanword_vowel_orthography": _LANGUAGE_QUALITY_TUPLES,
    "orthographic_missing_hyphen": _LANGUAGE_QUALITY_TUPLES,
    "orthographic_word_boundary_space": _LANGUAGE_QUALITY_TUPLES,
    "phrase_level_omission": _LANGUAGE_QUALITY_TUPLES,
    "same_script_character_omission": frozenset(
        {
            (
                "instrumentation_first_candidate",
                "bug_candidate",
                "same_script_loss",
            )
        }
    ),
    "same_script_character_substitution": frozenset(
        {
            (
                "instrumentation_first_candidate",
                "bug_candidate",
                "same_script_loss",
            )
        }
    ),
    "semantic_word_form_ambiguity": _INTENT_OR_LANGUAGE_TUPLES,
    "source_harness_framing": frozenset(
        {
            (
                "source_authorship_exclusion",
                "source_exclusion",
                "outside_product_source_authorship",
            )
        }
    ),
    "style_punctuation_guard": frozenset(
        {
            (
                "guard_not_bug_unless_intent_changes",
                "guard",
                "future_language_quality_layer",
            )
        }
    ),
    "style_sensitive_extra_comma": _LANGUAGE_QUALITY_TUPLES,
}
EXPECTED_AUTHORITY_BY_STATUS = {
    "null_non_unique": frozenset({"final", "null"}),
    "null_pending_operator_intent": frozenset({"final", "null"}),
    "unique_operator_confirmed": frozenset({"operator", "final", "legacy_bridge"}),
    "unique_authoritative_spelling": frozenset(
        {"authoritative", "final", "legacy_bridge"}
    ),
    "not_applicable": frozenset({"not_applicable"}),
}
MAX_RECONSTRUCTION_FRAGMENTS_PER_SOURCE = 8
PROJECTED_RECORD_COUNT = 111
RECORD_SHAPE_CONTRACT = {
    "bug_record_count": 76,
    "guard_record_count": 26,
    "legacy_record_count": 9,
    "baseline_update_record_count": 20,
    "new_record_count": 82,
}
CANONICAL_REF_SET_SHA256 = (
    "81168506c76776f060aaf9cd71bb4ed0e2fbde31b286ac30b69823bc8a54a5ee"
)
SEMANTIC_COMMITMENT_ALGORITHM = "sha256-canonical-json-v1"
HMAC_SCHEME = "smartkey-g0-hmac-sha256-v1"
HMAC_DOMAINS = ("value", "event", "metadata", "source_record")
HMAC_MIN_KEY_BYTES = 32
HMAC_SCALAR_PAYLOAD_ENCODING = "exact-utf8-scalar-no-normalization-v1"
HMAC_STRUCTURED_PAYLOAD_ENCODING = (
    "rfc8259-canonical-json-utf8-sorted-keys-compact-no-unicode-normalization-v1"
)
HMAC_INPUT_FRAME = "ascii-scheme-nul-domain-nul-u64be-length-payload-v1"
HMAC_RECEIPT_COVERAGE = "all-refs-domain-serialization-key-id-private-recomputation-v1"
HMAC_RECEIPT_STATE = "externally_verified"
BASELINE_RECEIPT_STATE = "externally_verified"
BASELINE_COMMITMENT_ALGORITHM = "sha256-canonical-json-v1"
RECORD_ORIGINS = ("new", "preexisting_public_baseline")
SEALED_CONTRACT_CONSTS = {
    "state": "sealed",
    "ruling_ref": "6e0704632fef",
    "artifact_sha256": "92c6dd612be8095d54dc385044c60e9d33e91d0ed9e9f62f6701c87722edbb72",
    "mapping_crosswalk_sha256": "7838b51bf55fbe7f7ac2ec7e1f5fbb0285df425242b1cbcfeaccaa3a0a6902a4",
    "audit_contract": "g0-r2-security-crosswalk",
    "original_count": 106,
    "supplemental_count": 53,
    "bug_candidate_count": 125,
    "guard_count": 32,
    "source_exclusion_count": 2,
    "projected_record_count": PROJECTED_RECORD_COUNT,
    **RECORD_SHAPE_CONTRACT,
    "canonical_ref_set_sha256": CANONICAL_REF_SET_SHA256,
    "semantic_commitment_algorithm": SEMANTIC_COMMITMENT_ALGORITHM,
    "baseline_commitment_algorithm": BASELINE_COMMITMENT_ALGORITHM,
    "hmac_scheme": HMAC_SCHEME,
    "hmac_min_key_bytes": HMAC_MIN_KEY_BYTES,
    "hmac_domains": list(HMAC_DOMAINS),
    "hmac_scalar_payload_encoding": HMAC_SCALAR_PAYLOAD_ENCODING,
    "hmac_structured_payload_encoding": HMAC_STRUCTURED_PAYLOAD_ENCODING,
    "hmac_input_frame": HMAC_INPUT_FRAME,
}

SHA_RE = re.compile(r"^[0-9a-f]{40}$")
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
DIGEST_NONZERO_RE = re.compile(r"^(?!0{64}$)[0-9a-f]{64}$")
DIGIT_RUN_RE = re.compile(r"[0-9]{7,}")
LETTER_RUN_RE = re.compile(r"[A-Za-zЀ-ӿ]{41,}")
WORD_FRAGMENT_RE = re.compile(r"[A-Za-zЀ-ӿ]{2,40}")
OPAQUE_ID_PATTERN = r"(?:[0-9a-f]{12}|[0-9a-f]{16}|[0-9a-f]{64}|H[0-9]{3})"
OPAQUE_ID_RE = re.compile(rf"^{OPAQUE_ID_PATTERN}$")
HMAC_KEY_ID_PATTERN = r"[0-9a-f]{32}"
HMAC_REF_PATTERN = (
    rf"{HMAC_SCHEME}:(?:{'|'.join(HMAC_DOMAINS)}):"
    rf"{HMAC_KEY_ID_PATTERN}:[0-9a-f]{{64}}"
)
HMAC_REF_RE = re.compile(rf"^{HMAC_REF_PATTERN}$")
HMAC_REF_PARSE_RE = re.compile(
    rf"^{HMAC_SCHEME}:(?P<domain>{'|'.join(HMAC_DOMAINS)}):"
    rf"(?P<key_id>{HMAC_KEY_ID_PATTERN}):(?P<mac>[0-9a-f]{{64}})$"
)
LEGACY_HMAC_REF_RE = re.compile(
    r"^hmac-sha256-v1:(?:value|event|metadata|source_record):[0-9a-f]{64}$"
)
METADATA_TEXT_RE = re.compile(
    rf"^{HMAC_SCHEME}:metadata:{HMAC_KEY_ID_PATTERN}:[0-9a-f]{{64}}$"
)
CANONICAL_ORIGINAL_REF_RE = re.compile(r"^orig:[0-9a-f]{16}$")
CANONICAL_SUPPLEMENTAL_REF_RE = re.compile(r"^supp:G[0-9]{3}:H[0-9]{3}$")
SOURCE_EVENT_REF_RE = re.compile(
    rf"^{HMAC_SCHEME}:event:{HMAC_KEY_ID_PATTERN}:[0-9a-f]{{64}}$"
)
EXPECTED_VALUE_REF_RE = re.compile(
    rf"^{HMAC_SCHEME}:value:{HMAC_KEY_ID_PATTERN}:[0-9a-f]{{64}}$"
)
UTC_RE = re.compile(r"^[0-9]{4}-[0-9]{2}-[0-9]{2}(T[0-9]{2}:[0-9]{2}(:[0-9]{2})?Z)?$")
CYRILLIC_RE = re.compile(r"[Ѐ-ӿ]")
LATIN_RE = re.compile(r"[A-Za-z]")
# Fields whose values are commit SHAs (40 hex or "unknown") by contract.
SHA_KEYS = frozenset({"build_sha", "commit", "fix_commit"})
# Fields whose values are ledger/ruling/test identifiers or derived identity;
# their source fields are independently privacy-checked.
IDENTIFIER_KEYS = frozenset(
    {
        "id",
        "dedup_key",
        "ref",
        "evidence_refs",
        "value_ref",
        "source_event_refs",
        "covered_adjudication_refs",
        "covered_source_event_refs",
        "baseline_record_sha256",
    }
)
RECORD_CATEGORIES = (
    "script_flip_bg_to_en",
    "script_flip_en_to_bg",
    "inconsistent_acceptance",
    "unknown",
)
SCRIPTS = ("latin", "cyrillic", "mixed", "other", "unknown")
SUSPECTED_LAYERS = (
    "dual_buffer_short_word",
    "dual_buffer_prefix_lock",
    "dual_buffer_unsupported_inheritance",
    "lang_prior_lock",
    "accept_gesture",
    "unknown",
)
LEGACY_SOURCE_KINDS = (
    "ledger",
    "ruling",
    "plan",
    "trace",
    "probe",
    "operator_report",
    "commit",
    "other",
)
REPRODUCER_KINDS = (
    "offline_probe",
    "structural_trace",
    "full_trace",
    "unit_test",
    "operator_report",
)
FLAG_VALUES = ("on", "off", "absent", "unknown")
ENHANCED_ENUM_PATHS = {
    ("category",): frozenset(RECORD_CATEGORIES),
    ("observed", "script"): frozenset(SCRIPTS),
    ("expected", "script"): frozenset(SCRIPTS),
    ("status",): frozenset(STATUSES),
    ("suspected_layer",): frozenset(SUSPECTED_LAYERS),
    ("privacy_classification",): frozenset(PRIVACY_CLASSES),
    ("record_origin",): frozenset(RECORD_ORIGINS),
    ("source_refs", "[]", "kind"): frozenset(LEGACY_SOURCE_KINDS),
    ("reproducer", "kind"): frozenset(REPRODUCER_KINDS),
    ("environment", "flags", "*"): frozenset(FLAG_VALUES),
    ("adjudications", "[]", "grain"): frozenset(ADJUDICATION_GRAINS),
    ("adjudications", "[]", "classification"): frozenset(ADJUDICATION_CLASSIFICATIONS),
    ("adjudications", "[]", "disposition"): frozenset(ADJUDICATION_DISPOSITIONS),
    ("adjudications", "[]", "normalized_class"): frozenset(NORMALIZED_CLASSES),
    ("adjudications", "[]", "expected_form_status"): frozenset(EXPECTED_FORM_STATUSES),
    ("adjudications", "[]", "expected", "status"): frozenset(EXPECTED_FORM_STATUSES),
    ("adjudications", "[]", "expected", "authority"): frozenset(EXPECTED_AUTHORITIES),
    (
        "adjudications",
        "[]",
        "causal_confidence",
        "anomaly_or_guard_presence",
    ): frozenset(CAUSAL_CONFIDENCES),
    (
        "adjudications",
        "[]",
        "causal_confidence",
        "expected_form",
    ): frozenset(CAUSAL_CONFIDENCES),
    (
        "adjudications",
        "[]",
        "causal_confidence",
        "runtime_mechanism",
    ): frozenset(CAUSAL_CONFIDENCES),
    (
        "adjudications",
        "[]",
        "causal_confidence",
        "smartkey_attribution",
    ): frozenset(CAUSAL_CONFIDENCES),
    ("adjudications", "[]", "owner_lane"): frozenset(OWNER_LANES),
    ("adjudications", "[]", "privacy_class"): frozenset(ADJUDICATION_PRIVACY_CLASSES),
    ("adjudications", "[]", "source_refs", "[]", "kind"): frozenset(
        ADJUDICATION_SOURCE_KINDS
    ),
}


# --------------------------------------------------------------------------
# identity
# --------------------------------------------------------------------------
def normalize(text: str) -> str:
    """NFC + casefold + collapsed whitespace: the dedup normal form."""
    return " ".join(unicodedata.normalize("NFC", text).casefold().split())


def _side_key(side: dict) -> str:
    token = side.get("token")
    if isinstance(token, str):
        return normalize(token)
    descriptor = side.get("descriptor")
    if isinstance(descriptor, str) and descriptor.strip():
        return "desc:" + normalize(descriptor)
    return "null"


def compute_dedup_key(record: dict) -> str:
    """``<normalized observed>|<normalized expected or null>``."""
    return (
        _side_key(record.get("observed") or {})
        + "|"
        + _side_key(record.get("expected") or {})
    )


def compute_id(record: dict) -> str:
    """``anom-`` + first 12 hex of sha256(dedup_key).

    The layer is deliberately *not* part of the id: ``suspected_layer`` is a
    hypothesis that gets revised, and an id must survive reclassification.
    """
    digest = hashlib.sha256(compute_dedup_key(record).encode("utf-8")).hexdigest()
    return "anom-" + digest[:12]


def script_class(token: str | None) -> str:
    if token is None:
        return "unknown"
    has_cyr = bool(CYRILLIC_RE.search(token))
    has_lat = bool(LATIN_RE.search(token))
    if has_cyr and has_lat:
        return "mixed"
    if has_cyr:
        return "cyrillic"
    if has_lat:
        return "latin"
    return "other"


def canonical_ref_matches_grain(grain: str, ref: str) -> bool:
    """Return whether an adjudication ref is canonical for its source grain."""
    if grain == "original_candidate":
        return CANONICAL_ORIGINAL_REF_RE.fullmatch(ref) is not None
    if grain == "supplemental_hypothesis":
        return CANONICAL_SUPPLEMENTAL_REF_RE.fullmatch(ref) is not None
    return False


def typed_hmac_ref_matches(value: str, domain: str | None = None) -> bool:
    """Validate only the public typed-ref shape, never its private derivation."""
    match = HMAC_REF_PARSE_RE.fullmatch(value)
    if match is None:
        return False
    return domain is None or match.group("domain") == domain


def normalized_tuple_is_allowed(
    normalized_class: str,
    grain: str,
    classification: str,
    disposition: str,
    owner_lane: str,
) -> bool:
    """Return whether the independent normalized axis fits its routing tuple."""
    if normalized_class == "not_applicable":
        if grain != "original_candidate":
            return False
    elif grain != "supplemental_hypothesis":
        return False
    return (
        classification,
        disposition,
        owner_lane,
    ) in NORMALIZED_CLASS_ALLOWED_TUPLES.get(normalized_class, ())


# --------------------------------------------------------------------------
# JSON-Schema subset
# --------------------------------------------------------------------------
_TYPE_CHECKS = {
    "object": lambda v: isinstance(v, dict),
    "array": lambda v: isinstance(v, list),
    "string": lambda v: isinstance(v, str),
    "integer": lambda v: isinstance(v, int) and not isinstance(v, bool),
    "number": lambda v: isinstance(v, (int, float)) and not isinstance(v, bool),
    "boolean": lambda v: isinstance(v, bool),
    "null": lambda v: v is None,
}


class SchemaChecker:
    """Just enough of draft 2020-12 for ``schema.json``: $ref (local), type,
    const, enum, required, properties, additionalProperties, items, pattern,
    minLength, maxLength, minimum, minItems, maxItems."""

    def __init__(self, schema: dict) -> None:
        self.root = schema

    def _resolve(self, node: dict) -> dict:
        while "$ref" in node:
            ref = node["$ref"]
            if not ref.startswith("#/"):
                raise ValueError(f"unsupported $ref {ref!r}")
            target = self.root
            for part in ref[2:].split("/"):
                target = target[part]
            node = {**target, **{k: v for k, v in node.items() if k != "$ref"}}
        return node

    def check(self, node: dict, value, path: str, errors: list[str]) -> None:
        node = self._resolve(node)
        if "const" in node and value != node["const"]:
            errors.append(f"E_SCHEMA: {path}: expected constant {node['const']!r}")
            return
        if "enum" in node and value not in node["enum"]:
            errors.append(f"E_ENUM: {path}: expected a declared enum member")
            return
        if "type" in node:
            types = node["type"] if isinstance(node["type"], list) else [node["type"]]
            if not any(_TYPE_CHECKS[t](value) for t in types):
                errors.append(
                    f"E_SCHEMA: {path}: expected type {types}, got {type(value).__name__}"
                )
                return
        if value is None:
            return
        if isinstance(value, str):
            if "minLength" in node and len(value) < node["minLength"]:
                errors.append(f"E_SCHEMA: {path}: shorter than {node['minLength']}")
            if "maxLength" in node and len(value) > node["maxLength"]:
                errors.append(f"E_SCHEMA: {path}: longer than {node['maxLength']}")
            if "pattern" in node and not re.search(node["pattern"], value):
                errors.append(f"E_SCHEMA: {path}: expected the declared string shape")
        elif isinstance(value, bool):
            pass
        elif isinstance(value, (int, float)):
            if "minimum" in node and value < node["minimum"]:
                errors.append(f"E_SCHEMA: {path}: below minimum {node['minimum']}")
        elif isinstance(value, list):
            if "minItems" in node and len(value) < node["minItems"]:
                errors.append(f"E_SCHEMA: {path}: fewer than {node['minItems']} items")
            if "maxItems" in node and len(value) > node["maxItems"]:
                errors.append(f"E_SCHEMA: {path}: more than {node['maxItems']} items")
            if "items" in node:
                for i, item in enumerate(value):
                    self.check(node["items"], item, f"{path}[{i}]", errors)
        elif isinstance(value, dict):
            for key in node.get("required", ()):
                if key not in value:
                    errors.append(f"E_SCHEMA: {path}: missing required key {key!r}")
            props = node.get("properties", {})
            extra = node.get("additionalProperties", True)
            for key, item in value.items():
                if key in props:
                    self.check(props[key], item, f"{path}.{key}", errors)
                elif extra is False:
                    errors.append(f"E_SCHEMA: {path}: unknown key is not allowed")
                elif isinstance(extra, dict):
                    self.check(extra, item, f"{path}.*", errors)


# --------------------------------------------------------------------------
# semantic checks
# --------------------------------------------------------------------------
def _pad_utc(stamp: str) -> str:
    """Bring date-only / minute-precision stamps to full precision for ordering."""
    if len(stamp) == 10:
        return stamp + "T00:00:00Z"
    if len(stamp) == 17:
        return stamp[:-1] + ":00Z"
    return stamp


def _check_identity(rec: dict, path: str, errors: list[str]) -> None:
    expected_key = compute_dedup_key(rec)
    if rec.get("dedup_key") != expected_key:
        errors.append(
            f"E_DEDUP_KEY: {path}.dedup_key: value differs from canonical recomputation"
        )
    expected_id = compute_id(rec)
    if rec.get("id") != expected_id:
        errors.append(f"E_ID: {path}.id: value differs from canonical recomputation")


def _check_sides(rec: dict, path: str, errors: list[str]) -> None:
    observed = rec["observed"]
    expected = rec["expected"]
    adjudications = rec.get("adjudications") or []
    for name, side in (("observed", observed), ("expected", expected)):
        token = side["token"]
        if isinstance(token, str) and token.split() != [token]:
            errors.append(
                f"E_TOKEN_WS: {path}.{name}.token: must be one whitespace-free token"
            )
        actual = script_class(token)
        if side["script"] != actual:
            errors.append(
                f"E_SCRIPT: {path}.{name}.script: value differs from token script class"
            )
    if observed["token"] is None:
        if not (
            isinstance(observed["descriptor"], str) and observed["descriptor"].strip()
        ):
            errors.append(
                f"E_OBSERVED: {path}.observed: descriptor required when token is null"
            )
        allowed_null_privacy = {"unknown", "redacted"} if adjudications else {"unknown"}
        if rec["privacy_classification"] not in allowed_null_privacy:
            errors.append(
                f"E_OBSERVED: {path}.privacy_classification: null observed token "
                "requires an allowed descriptor-based privacy class"
            )
    elif rec["privacy_classification"] == "unknown":
        errors.append(
            f"E_OBSERVED: {path}.privacy_classification: 'unknown' but observed.token is present"
        )
    expected_not_applicable = bool(adjudications) and all(
        (adj.get("expected") or {}).get("status", adj.get("expected_form_status"))
        == "not_applicable"
        for adj in adjudications
    )
    if (
        expected["token"] is None
        and not expected["descriptor"]
        and not rec["needs_operator_confirmation"]
        and not expected_not_applicable
    ):
        errors.append(
            f"E_EXPECTED: {path}: expected form is null, needs_operator_confirmation must be true"
        )
    if rec["needs_operator_confirmation"] and rec["status"] not in UNCONFIRMED_STATUSES:
        errors.append(
            f"E_EXPECTED: {path}.needs_operator_confirmation: state requires a confirmed expected form"
        )
    if rec["repeat"] != (rec["occurrence_count"] > 1):
        errors.append(f"E_REPEAT: {path}: repeat must equal occurrence_count > 1")
    first, last, recorded = (
        rec["first_seen_utc"],
        rec["last_seen_utc"],
        rec["recorded_utc"],
    )
    if first and last and _pad_utc(first) > _pad_utc(last):
        errors.append(f"E_TIME: {path}: first_seen_utc after last_seen_utc")
    for name, stamp in (("first_seen_utc", first), ("last_seen_utc", last)):
        if stamp and _pad_utc(stamp) > _pad_utc(recorded):
            errors.append(f"E_TIME: {path}: {name} after recorded_utc")


def _check_gates(rec: dict, path: str, errors: list[str]) -> None:
    status = rec["status"]
    has = {
        k: rec.get(k) is not None
        for k in (
            "reproducer",
            "red_test",
            "red_test_waiver",
            "fix_commit",
            "verification",
        )
    }
    has_closure = isinstance(rec.get("closure_reason"), str) and bool(
        rec["closure_reason"].strip()
    )
    adjudications = rec.get("adjudications") or []
    all_guard = bool(adjudications) and all(
        adjudication.get("disposition") == "guard" for adjudication in adjudications
    )
    guard_receipts_complete = all_guard and all(
        any(
            source.get("kind") in {"ruling", "receipt"}
            for source in adjudication.get("source_refs", ())
        )
        for adjudication in adjudications
    )

    def need(cond: bool, what: str) -> None:
        if not cond:
            errors.append(f"E_GATE: {path}: status {status!r} requires {what}")

    if status in ("reproduced", "red_tested", "fixed", "verified", "closed"):
        need(has["reproducer"], "a reproducer (probe/trace/test evidence)")
    if status == "not_smartkey":
        if all_guard:
            need(
                guard_receipts_complete,
                "an adjudication ruling/receipt for every guard mapping",
            )
        else:
            need(has["reproducer"], "a reproducer (probe/trace/test evidence)")
    if status == "red_tested":
        need(has["red_test"], "a linked RED test")
    if status in ("fixed", "verified", "closed"):
        need(has["fix_commit"], "an exact fix_commit")
        need(
            has["red_test"] or has["red_test_waiver"],
            "a linked regression test or an explicit red_test_waiver",
        )
    if status in ("verified", "closed"):
        need(has["verification"], "post-fix verification on the reported surface")
    if status in ("closed", "not_smartkey"):
        need(has_closure, "a closure_reason")
    if status in ("open_suspected_smartkey", "reproduced", "red_tested"):
        if has["fix_commit"] or has["verification"]:
            errors.append(
                f"E_STATE: {path}: status {status!r} cannot carry fix_commit/verification"
            )
    if has["verification"] and not has["fix_commit"]:
        errors.append(f"E_STATE: {path}: verification without fix_commit")
    if has["red_test"] and has["red_test_waiver"]:
        errors.append(
            f"E_STATE: {path}: red_test and red_test_waiver are mutually exclusive"
        )
    if all_guard and (rec.get("reproducer") or {}).get("kind") == "unit_test":
        errors.append(
            f"E_ADJ_GUARD_EVIDENCE: {path}.reproducer: guard adjudication "
            "requires ruling/receipt evidence, not a fabricated unit-test reproducer"
        )


def _walk_strings(value, path: tuple, out: list[tuple[tuple, str]]) -> None:
    if isinstance(value, str):
        out.append((path, value))
    elif isinstance(value, dict):
        for k, v in value.items():
            _walk_strings(v, path + (k,), out)
    elif isinstance(value, list):
        for i, v in enumerate(value):
            _walk_strings(v, path + (i,), out)


def _display_path(base: str, keys: tuple) -> str:
    """Render a JSON path without reflecting dynamic dictionary keys."""
    parts = [base]
    dynamic = False
    for key in keys:
        if isinstance(key, int):
            parts[-1] += f"[{key}]"
            continue
        if dynamic:
            parts.append("*")
            dynamic = False
            continue
        parts.append(key)
        if key == "flags":
            dynamic = True
    return ".".join(parts)


def _check_privacy(rec: dict, path: str, errors: list[str]) -> None:
    strings: list[tuple[tuple, str]] = []
    _walk_strings(rec, (), strings)
    for keys, text in strings:
        where = _display_path(path, keys)
        in_source_refs = bool(keys) and keys[0] == "source_refs"
        last_key = next((k for k in reversed(keys) if isinstance(k, str)), None)
        is_sha_field = last_key in SHA_KEYS
        is_opaque = (
            OPAQUE_ID_RE.fullmatch(text) is not None
            or HMAC_REF_RE.fullmatch(text) is not None
            or METADATA_TEXT_RE.fullmatch(text) is not None
            or CANONICAL_ORIGINAL_REF_RE.fullmatch(text) is not None
            or CANONICAL_SUPPLEMENTAL_REF_RE.fullmatch(text) is not None
            or SOURCE_EVENT_REF_RE.fullmatch(text) is not None
            or EXPECTED_VALUE_REF_RE.fullmatch(text) is not None
        )
        is_identifier = is_sha_field or last_key in IDENTIFIER_KEYS or is_opaque
        if "@" in text:
            errors.append(
                f"E_PRIV_AT: {where}: '@' is never allowed (addresses/handles)"
            )
        if "http" in text.lower() and not in_source_refs:
            errors.append(f"E_PRIV_URL: {where}: URLs only inside source_refs")
        if "\n" in text or "\r" in text:
            errors.append(
                f"E_PRIV_MULTILINE: {where}: multi-line text looks like a transcript"
            )
        if LETTER_RUN_RE.search(text) and not is_opaque and last_key != "dedup_key":
            errors.append(
                f"E_PRIV_LONGRUN: {where}: letter run longer than 40 characters"
            )
        if not is_identifier and DIGIT_RUN_RE.search(text):
            errors.append(
                f"E_PRIV_DIGITS: {where}: digit run longer than 6 outside SHA/reference fields"
            )
        if is_sha_field and text != "unknown" and not SHA_RE.match(text):
            errors.append(f"E_SHA: {where}: SHA field must be 40 hex or 'unknown'")
    for side in ("before", "after"):
        token = rec["minimal_context"][side]
        if isinstance(token, str) and token.split() != [token]:
            errors.append(
                f"E_PRIV_CONTEXT: {path}.minimal_context.{side}: at most one whitespace-free token per side"
            )


def _shape_path(keys: tuple) -> tuple[str, ...]:
    """Return a stable field path with array indexes and flag keys erased."""
    shaped: list[str] = []
    for key in keys:
        if isinstance(key, int):
            shaped.append("[]")
        elif len(shaped) == 2 and shaped[:2] == ["environment", "flags"]:
            shaped.append("*")
        else:
            shaped.append(key)
    return tuple(shaped)


def _canonical_adjudication_ref(text: str) -> bool:
    return (
        CANONICAL_ORIGINAL_REF_RE.fullmatch(text) is not None
        or CANONICAL_SUPPLEMENTAL_REF_RE.fullmatch(text) is not None
    )


def _is_enhanced_string_allowed(keys: tuple, text: str) -> bool:
    """Closed, path-specific allowlist for one enhanced-record string value.

    A value valid in one enum field is never implicitly valid in another.
    Literal ``synthetic`` is limited to three documented fixture environment
    labels; real enhanced imports use metadata-domain HMAC references there.
    """
    shaped = _shape_path(keys)
    allowed_enum = ENHANCED_ENUM_PATHS.get(shaped)
    if allowed_enum is not None:
        return text in allowed_enum

    if shaped in {
        ("observed", "token"),
        ("expected", "token"),
        ("minimal_context", "before"),
        ("minimal_context", "after"),
        ("dedup_key",),
    }:
        return True  # origin/side/privacy/identity checks own these payloads
    if shaped == ("id",):
        return re.fullmatch(r"anom-[0-9a-f]{12}", text) is not None
    if shaped in {
        ("recorded_utc",),
        ("first_seen_utc",),
        ("last_seen_utc",),
        ("source_refs", "[]", "observed_utc"),
    }:
        return UTC_RE.fullmatch(text) is not None
    if shaped in {
        ("environment", "build_sha"),
        ("source_refs", "[]", "build_sha"),
        ("reproducer", "build_sha"),
    }:
        return text == "unknown" or SHA_RE.fullmatch(text) is not None
    if shaped in {
        ("red_test", "commit"),
        ("fix_commit",),
        ("verification", "build_sha"),
    }:
        return SHA_RE.fullmatch(text) is not None
    if shaped == ("baseline_record_sha256",):
        return DIGEST_NONZERO_RE.fullmatch(text) is not None

    if shaped in {
        ("environment", "app_surface"),
        ("environment", "os"),
        ("environment", "build_label"),
    }:
        return text == "synthetic" or METADATA_TEXT_RE.fullmatch(text) is not None

    if shaped in {
        ("observed", "descriptor"),
        ("expected", "descriptor"),
        ("source_refs", "[]", "ref"),
        ("source_refs", "[]", "surface"),
        ("source_refs", "[]", "note"),
        ("reproducer", "ref"),
        ("reproducer", "summary"),
        ("red_test", "path"),
        ("red_test", "name"),
        ("red_test_waiver",),
        ("verification", "evidence_refs", "[]"),
        ("verification", "surface"),
        ("verification", "summary"),
        ("closure_reason",),
        ("notes",),
    }:
        return METADATA_TEXT_RE.fullmatch(text) is not None

    if shaped in {
        ("reproducer", "covered_adjudication_refs", "[]"),
        ("red_test", "covered_adjudication_refs", "[]"),
        ("fix_evidence", "covered_adjudication_refs", "[]"),
        ("verification", "covered_adjudication_refs", "[]"),
        ("adjudications", "[]", "ref"),
    }:
        return _canonical_adjudication_ref(text)
    if shaped in {
        ("reproducer", "covered_source_event_refs", "[]"),
        ("red_test", "covered_source_event_refs", "[]"),
        ("fix_evidence", "covered_source_event_refs", "[]"),
        ("verification", "covered_source_event_refs", "[]"),
        ("adjudications", "[]", "source_event_refs", "[]"),
    }:
        return SOURCE_EVENT_REF_RE.fullmatch(text) is not None
    if shaped == ("adjudications", "[]", "expected", "value_ref"):
        return EXPECTED_VALUE_REF_RE.fullmatch(text) is not None
    if shaped == ("adjudications", "[]", "source_refs", "[]", "ref"):
        return HMAC_REF_RE.fullmatch(text) is not None
    return False


def _payload_fragments(rec: dict) -> set[str]:
    """Return unordered reconstruction atoms, including dynamic keys."""
    fragments: set[str] = set()
    for side in ("observed", "expected"):
        token = rec[side].get("token")
        if isinstance(token, str) and token not in {"redacted", "[redacted]"}:
            fragments.add(normalize(token))

    strings: list[tuple[tuple, str]] = []
    _walk_strings(rec, (), strings)
    for keys, text in strings:
        last_key = next((key for key in reversed(keys) if isinstance(key, str)), None)
        if last_key in {
            "token",
            "id",
            "dedup_key",
            "recorded_utc",
            "first_seen_utc",
            "last_seen_utc",
        }:
            continue
        if not _is_enhanced_string_allowed(keys, text):
            fragments.update(normalize(word) for word in WORD_FRAGMENT_RE.findall(text))

    flags = rec.get("environment", {}).get("flags") or {}
    for key in flags:
        if METADATA_TEXT_RE.fullmatch(key) is None:
            fragments.update(normalize(word) for word in WORD_FRAGMENT_RE.findall(key))
    return {fragment for fragment in fragments if fragment}


def _check_adjudication_privacy(rec: dict, path: str, errors: list[str]) -> None:
    """Apply the recursive closed allowlist and side-complete privacy rules."""
    if any(rec["minimal_context"].values()):
        errors.append(
            f"E_ADJ_PRIVACY: {path}.minimal_context: enhanced records require null context"
        )

    strings: list[tuple[tuple, str]] = []
    _walk_strings(rec, (), strings)
    skipped_fields = {
        "token",
        "id",
        "dedup_key",
        "recorded_utc",
        "first_seen_utc",
        "last_seen_utc",
    }
    for keys, text in strings:
        last_key = next((key for key in reversed(keys) if isinstance(key, str)), None)
        if last_key in skipped_fields:
            continue
        if not _is_enhanced_string_allowed(keys, text):
            where = _display_path(path, keys)
            errors.append(
                f"E_ADJ_PRIVACY: {where}: enhanced string is outside the closed allowlist"
            )

    flags = rec.get("environment", {}).get("flags") or {}
    for key in flags:
        if METADATA_TEXT_RE.fullmatch(key) is None:
            errors.append(
                f"E_ADJ_PRIVACY: {path}.environment.flags: dynamic key is outside "
                "the closed allowlist"
            )

    privacy_classes = {
        adjudication["privacy_class"] for adjudication in rec["adjudications"]
    }
    if len(privacy_classes) != 1:
        errors.append(
            f"E_ADJ_PRIVACY: {path}: one dedup record cannot mix privacy classes"
        )
        return
    privacy_class = next(iter(privacy_classes))
    tokens = [rec[side]["token"] for side in ("observed", "expected")]
    descriptors = [rec[side]["descriptor"] for side in ("observed", "expected")]
    if privacy_class == "public_token":
        if rec.get("record_origin") != "preexisting_public_baseline":
            errors.append(
                f"E_ADJ_ORIGIN: {path}.record_origin: free public literals are "
                "forbidden for new enhanced records"
            )
        if (
            rec["privacy_classification"] != "public_token"
            or rec["observed"]["token"] is None
        ):
            errors.append(
                f"E_ADJ_PRIVACY: {path}: public_token requires an independently "
                "classified isolated observed token"
            )
        for side, token, descriptor in zip(
            ("observed", "expected"), tokens, descriptors
        ):
            if isinstance(token, str):
                if "/" in token or "\\" in token:
                    errors.append(
                        f"E_ADJ_PRIVACY: {path}.{side}.token: public token cannot "
                        "encode a filesystem path"
                    )
            if isinstance(descriptor, str) and not METADATA_TEXT_RE.fullmatch(
                descriptor
            ):
                errors.append(
                    f"E_ADJ_PRIVACY: {path}.{side}.descriptor: public representation "
                    "descriptor must be a typed metadata HMAC ref"
                )
    elif privacy_class == "redacted":
        if rec["privacy_classification"] != "redacted" or any(
            token not in {None, "redacted", "[redacted]"} for token in tokens
        ):
            errors.append(
                f"E_ADJ_PRIVACY: {path}: redacted requires placeholder/null on both sides"
            )
        for side, descriptor in zip(("observed", "expected"), descriptors):
            if isinstance(descriptor, str) and not METADATA_TEXT_RE.fullmatch(
                descriptor
            ):
                errors.append(
                    f"E_ADJ_PRIVACY: {path}.{side}.descriptor: redacted descriptor "
                    "must be a typed metadata HMAC ref"
                )
    else:
        if rec["privacy_classification"] != "unknown" or any(
            token is not None for token in tokens
        ):
            errors.append(
                f"E_ADJ_PRIVACY: {path}: metadata_only cannot carry token payload"
            )
        for side, descriptor in zip(("observed", "expected"), descriptors):
            if isinstance(descriptor, str) and not METADATA_TEXT_RE.fullmatch(
                descriptor
            ):
                errors.append(
                    f"E_ADJ_PRIVACY: {path}.{side}.descriptor: metadata-only "
                    "descriptor must be a typed metadata HMAC ref"
                )


def _check_expected_envelope(adjudication: dict, path: str, errors: list[str]) -> None:
    expected = adjudication["expected"]
    status = expected["status"]
    authority = expected["authority"]
    value_ref = expected["value_ref"]
    classification = adjudication["classification"]
    disposition = adjudication["disposition"]
    confidence = adjudication["causal_confidence"]

    bridge_status = adjudication.get("expected_form_status")
    if bridge_status is not None and bridge_status != status:
        errors.append(
            f"E_ADJ_EXPECTED: {path}: deprecated expected_form_status disagrees "
            "with expected.status"
        )
    if authority not in EXPECTED_AUTHORITY_BY_STATUS[status]:
        errors.append(
            f"E_ADJ_EXPECTED: {path}.expected.authority: value is outside the "
            "status-specific authority set"
        )

    unique_statuses = {
        "unique_operator_confirmed",
        "unique_authoritative_spelling",
    }
    null_statuses = {"null_non_unique", "null_pending_operator_intent"}
    if status in unique_statuses:
        if not isinstance(value_ref, str) or not EXPECTED_VALUE_REF_RE.fullmatch(
            value_ref
        ):
            errors.append(
                f"E_ADJ_EXPECTED: {path}.expected.value_ref: unique expected form "
                "requires an opaque value commitment"
            )
        if confidence["expected_form"] != "high":
            errors.append(
                f"E_ADJ_CONFIDENCE: {path}.causal_confidence.expected_form: "
                "unique expected form requires high confidence"
            )
    elif status in null_statuses:
        if value_ref is not None:
            errors.append(
                f"E_ADJ_EXPECTED: {path}.expected.value_ref: null status cannot "
                "carry a value commitment"
            )
        if confidence["expected_form"] not in {"none", "low", "medium"}:
            errors.append(
                f"E_ADJ_CONFIDENCE: {path}.causal_confidence.expected_form: "
                "null expected form cannot be high/not_applicable"
            )
        if (
            status == "null_pending_operator_intent"
            and classification != "intent_confirmation_needed"
        ):
            errors.append(
                f"E_ADJ_STATE: {path}: pending operator intent requires "
                "intent_confirmation_needed"
            )
    else:
        if value_ref is not None:
            errors.append(
                f"E_ADJ_EXPECTED: {path}.expected.value_ref: not_applicable "
                "cannot carry a value commitment"
            )
        if confidence["expected_form"] != "not_applicable":
            errors.append(
                f"E_ADJ_CONFIDENCE: {path}.causal_confidence.expected_form: "
                "not_applicable status requires not_applicable confidence"
            )
        if disposition == "bug_candidate":
            errors.append(
                f"E_ADJ_STATE: {path}: bug candidate needs expected-form adjudication"
            )


def _check_adjudication_item(
    adjudication: dict,
    path: str,
    owner_path: str,
    errors: list[str],
    seen_adjudications: dict[str, str],
    grain_refs: dict[str, set[str]],
    disposition_refs: dict[str, set[str]],
    event_owners: dict[str, str],
) -> tuple[set[str], set[str]]:
    grain = adjudication["grain"]
    ref = adjudication["ref"]
    if not canonical_ref_matches_grain(grain, ref):
        errors.append(f"E_ADJ_REF: {path}.ref: canonical ref does not match its grain")
    prior = seen_adjudications.get(ref)
    if prior is not None:
        errors.append(f"E_ADJ_DUP: {path}.ref: mapping already present at {prior}")
    else:
        seen_adjudications[ref] = path
    grain_refs[grain].add(ref)

    classification = adjudication["classification"]
    disposition = adjudication["disposition"]
    disposition_refs[disposition].add(ref)
    if CLASSIFICATION_DISPOSITION[classification] != disposition:
        errors.append(
            f"E_ADJ_STATE: {path}: classification and disposition are inconsistent"
        )
    owner_lane = adjudication["owner_lane"]
    if owner_lane not in CLASSIFICATION_OWNER_LANES[classification]:
        errors.append(
            f"E_ADJ_OWNER: {path}.owner_lane: value is outside the exclusive "
            "classification lane matrix"
        )

    normalized_class = adjudication["normalized_class"]
    if grain == "original_candidate" and normalized_class != "not_applicable":
        errors.append(
            f"E_ADJ_NORMALIZED: {path}.normalized_class: original mappings "
            "require the explicit not_applicable sentinel"
        )
    if grain == "supplemental_hypothesis" and normalized_class == "not_applicable":
        errors.append(
            f"E_ADJ_NORMALIZED: {path}.normalized_class: supplemental mappings "
            "require their ratified normalized class"
        )
    if not normalized_tuple_is_allowed(
        normalized_class,
        grain,
        classification,
        disposition,
        owner_lane,
    ):
        errors.append(
            f"E_ADJ_NORMALIZED: {path}.normalized_class: value is inconsistent "
            "with grain/classification/disposition/owner lane"
        )

    confidence = adjudication["causal_confidence"]
    if confidence["anomaly_or_guard_presence"] not in {"medium", "high"}:
        errors.append(
            f"E_ADJ_CONFIDENCE: {path}.causal_confidence.anomaly_or_guard_presence: "
            "retained mapping requires medium/high confidence"
        )
    if disposition == "bug_candidate":
        for axis in ("runtime_mechanism", "smartkey_attribution"):
            if confidence[axis] == "not_applicable":
                errors.append(
                    f"E_ADJ_CONFIDENCE: {path}.causal_confidence.{axis}: "
                    "bug candidate requires an explicit confidence level"
                )
    else:
        for axis in ("runtime_mechanism", "smartkey_attribution"):
            if confidence[axis] != "not_applicable":
                errors.append(
                    f"E_ADJ_CONFIDENCE: {path}.causal_confidence.{axis}: "
                    "guard/exclusion requires not_applicable"
                )
    _check_expected_envelope(adjudication, path, errors)

    source_keys: set[tuple[str, str]] = set()
    source_records: set[str] = set()
    for source in adjudication["source_refs"]:
        source_key = (source["kind"], source["ref"])
        if source_key in source_keys:
            errors.append(f"E_ADJ_DUP: {path}.source_refs: duplicate source ref")
        source_keys.add(source_key)
        expected_domain = (
            "source_record" if source["kind"] == "source_record" else "metadata"
        )
        if not typed_hmac_ref_matches(source["ref"], expected_domain):
            errors.append(
                f"E_ADJ_REF_PRIVACY: {path}.source_refs: ref must use the "
                "kind-specific typed HMAC domain"
            )
        if source["kind"] == "source_record":
            source_records.add(source["ref"])
    if not source_records:
        errors.append(f"E_ADJ_STATE: {path}.source_refs: source_record ref is required")
    if disposition == "guard" and not any(
        source["kind"] in {"ruling", "receipt"}
        for source in adjudication["source_refs"]
    ):
        errors.append(
            f"E_ADJ_GUARD_EVIDENCE: {path}.source_refs: guard mapping requires "
            "its own ruling/receipt evidence"
        )

    source_events = adjudication["source_event_refs"]
    event_set = set(source_events)
    if len(event_set) != len(source_events):
        errors.append(
            f"E_ADJ_EVENT: {path}.source_event_refs: duplicate event cannot "
            "increase occurrence count"
        )
    for event in event_set:
        if not SOURCE_EVENT_REF_RE.fullmatch(event):
            errors.append(
                f"E_ADJ_EVENT: {path}.source_event_refs: non-canonical event ID"
            )
        prior_owner = event_owners.setdefault(event, owner_path)
        if prior_owner != owner_path:
            errors.append(
                f"E_ADJ_EVENT: {path}.source_event_refs: event already reconciled "
                "to another record"
            )
    return source_records, event_set


def _check_evidence_coverage(
    rec: dict,
    path: str,
    adjudications: list[dict],
    errors: list[str],
) -> None:
    bug_adjudications = [
        item for item in adjudications if item["disposition"] == "bug_candidate"
    ]
    bug_refs = {item["ref"] for item in bug_adjudications}
    event_by_ref = {
        item["ref"]: set(item["source_event_refs"]) for item in bug_adjudications
    }

    def check(evidence: dict | None, field: str) -> None:
        if not isinstance(evidence, dict):
            errors.append(
                f"E_ADJ_COVERAGE: {path}.{field}: evidence coverage is required"
            )
            return
        covered_refs = evidence.get("covered_adjudication_refs")
        covered_events = evidence.get("covered_source_event_refs")
        if not isinstance(covered_refs, list) or not isinstance(covered_events, list):
            errors.append(
                f"E_ADJ_COVERAGE: {path}.{field}: covered adjudication and "
                "source-event refs are required"
            )
            return
        ref_set = set(covered_refs)
        event_set = set(covered_events)
        if len(ref_set) != len(covered_refs) or ref_set != bug_refs:
            errors.append(
                f"E_ADJ_COVERAGE: {path}.{field}: coverage must equal every bug ref"
            )
        required_events = {
            event for ref in ref_set & bug_refs for event in event_by_ref[ref]
        }
        if len(event_set) != len(covered_events) or event_set != required_events:
            errors.append(
                f"E_ADJ_COVERAGE: {path}.{field}: source-event coverage is partial"
            )

    status = rec["status"]
    if status in {"reproduced", "red_tested", "fixed", "verified", "closed"}:
        check(rec.get("reproducer"), "reproducer")
    if status in {"red_tested", "fixed", "verified", "closed"}:
        check(rec.get("red_test"), "red_test")
    if status in {"fixed", "verified", "closed"}:
        check(rec.get("fix_evidence"), "fix_evidence")
    if status in {"verified", "closed"}:
        check(rec.get("verification"), "verification")


def _check_adjudication_record(
    rec: dict,
    path: str,
    errors: list[str],
    seen_adjudications: dict[str, str],
    grain_refs: dict[str, set[str]],
    disposition_refs: dict[str, set[str]],
    source_fragments: dict[str, set[str]],
    event_owners: dict[str, str],
) -> None:
    adjudications = rec.get("adjudications")
    if not adjudications:
        if (
            rec.get("record_origin") is not None
            or rec.get("baseline_record_sha256") is not None
        ):
            errors.append(
                f"E_BASELINE_AUTHORITY: {path}: retained legacy records cannot "
                "claim enhanced origin authority"
            )
        return

    origin = rec.get("record_origin")
    baseline_digest = rec.get("baseline_record_sha256")
    if origin == "preexisting_public_baseline":
        if (
            not isinstance(baseline_digest, str)
            or not DIGEST_NONZERO_RE.fullmatch(baseline_digest)
            or baseline_digest != baseline_record_sha256(rec)
        ):
            errors.append(
                f"E_BASELINE_AUTHORITY: {path}.baseline_record_sha256: value "
                "does not match the complete derived legacy projection"
            )
    elif origin == "new":
        if baseline_digest is not None:
            errors.append(
                f"E_BASELINE_AUTHORITY: {path}.baseline_record_sha256: new "
                "records cannot inherit a baseline receipt"
            )
    else:
        errors.append(
            f"E_ADJ_ORIGIN: {path}.record_origin: enhanced record requires a "
            "declared new or preexisting-public-baseline origin"
        )

    _check_adjudication_privacy(rec, path, errors)
    if rec.get("red_test_waiver") is not None:
        errors.append(
            f"E_ADJ_WAIVER: {path}.red_test_waiver: enhanced records require "
            "actual covered RED evidence; waivers are forbidden"
        )
    dispositions: set[str] = set()
    record_events: set[str] = set()
    fragments = _payload_fragments(rec)
    for index, adjudication in enumerate(adjudications):
        adj_path = f"{path}.adjudications[{index}]"
        source_records, events = _check_adjudication_item(
            adjudication,
            adj_path,
            path,
            errors,
            seen_adjudications,
            grain_refs,
            disposition_refs,
            event_owners,
        )
        dispositions.add(adjudication["disposition"])
        record_events.update(events)
        for source_ref in source_records:
            source_fragments.setdefault(source_ref, set()).update(fragments)

    if rec["occurrence_count"] != len(record_events):
        errors.append(
            f"E_ADJ_EVENT: {path}.occurrence_count: {rec['occurrence_count']} does "
            f"not equal {len(record_events)} distinct source event(s)"
        )
    if "source_exclusion" in dispositions:
        errors.append(
            f"E_ADJ_STATE: {path}: source exclusions belong only in top-level "
            "source_exclusions"
        )
    has_bug = "bug_candidate" in dispositions
    if has_bug and rec["status"] == "not_smartkey":
        errors.append(
            f"E_ADJ_STATE: {path}: bug-candidate record cannot be not_smartkey"
        )
    if not has_bug and rec["status"] != "not_smartkey":
        errors.append(f"E_ADJ_STATE: {path}: all-guard record must be not_smartkey")
    if not has_bug and any(
        rec.get(field) is not None
        for field in (
            "red_test",
            "red_test_waiver",
            "fix_commit",
            "fix_evidence",
            "verification",
        )
    ):
        errors.append(
            f"E_ADJ_STATE: {path}: guards cannot carry fix lifecycle evidence"
        )
    if has_bug:
        _check_evidence_coverage(rec, path, adjudications, errors)


def semantic_commitment_payload(doc: dict) -> list[dict]:
    """Canonical, non-sensitive tuple set committed by the sealed contract."""
    adjudications = [
        (adjudication, rec["id"])
        for rec in doc.get("records", ())
        for adjudication in rec.get("adjudications", ())
    ]
    adjudications.extend(
        (adjudication, "top_level_source_exclusion")
        for adjudication in doc.get("source_exclusions", ())
    )
    return [
        {
            "ref": item["ref"],
            "grain": item["grain"],
            "classification": item["classification"],
            "disposition": item["disposition"],
            "normalized_class": item["normalized_class"],
            "expected": {
                "status": item["expected"]["status"],
                "authority": item["expected"]["authority"],
                "value_commitment": item["expected"]["value_ref"],
            },
            "causal_confidence": item["causal_confidence"],
            "owner_lane": item["owner_lane"],
            "privacy_class": item["privacy_class"],
            "source_binding": {
                "target_record_id": target_record_id,
                "source_refs": sorted(
                    f"{source['kind']}:{source['ref']}"
                    for source in item["source_refs"]
                ),
                "source_event_refs": sorted(item["source_event_refs"]),
            },
        }
        for item, target_record_id in sorted(
            adjudications, key=lambda value: value[0]["ref"]
        )
    ]


def semantic_commitment_sha256(doc: dict) -> str:
    payload = json.dumps(
        semantic_commitment_payload(doc),
        ensure_ascii=True,
        separators=(",", ":"),
        sort_keys=True,
    )
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()


def _canonical_sha256(value) -> str:
    payload = json.dumps(
        value,
        ensure_ascii=True,
        separators=(",", ":"),
        sort_keys=True,
    )
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()


_ENHANCED_RECORD_FIELDS = frozenset(
    {"adjudications", "fix_evidence", "record_origin", "baseline_record_sha256"}
)
_ENHANCED_EVIDENCE_FIELDS = frozenset(
    {"covered_adjudication_refs", "covered_source_event_refs"}
)


def baseline_record_payload(record: dict) -> dict:
    """Derive the legacy-shaped content reviewed by the baseline authority.

    Only fields introduced by the enhanced adjudication projection are
    removed. Existing record identity and every legacy content field remain
    committed, including public literals on the two token sides.
    """
    payload = copy.deepcopy(record)
    for field in _ENHANCED_RECORD_FIELDS:
        payload.pop(field, None)
    for evidence_field in ("reproducer", "red_test", "verification"):
        evidence = payload.get(evidence_field)
        if isinstance(evidence, dict):
            for field in _ENHANCED_EVIDENCE_FIELDS:
                evidence.pop(field, None)
    return payload


def baseline_record_sha256(record: dict) -> str:
    return _canonical_sha256(baseline_record_payload(record))


def baseline_record_set_payload(doc: dict) -> list[dict]:
    """Canonical externally reviewed identity/digest set for baseline updates."""
    return sorted(
        (
            {
                "record_id": record.get("id"),
                "baseline_record_sha256": record.get("baseline_record_sha256"),
            }
            for record in doc.get("records", ())
            if record.get("record_origin") == "preexisting_public_baseline"
        ),
        key=lambda item: str(item["record_id"]),
    )


def baseline_record_set_sha256(doc: dict) -> str:
    return _canonical_sha256(baseline_record_set_payload(doc))


def legacy_projection_payload(doc: dict) -> list[dict]:
    """Canonical complete identity/content of unadjudicated retained records."""
    records = [
        copy.deepcopy(record)
        for record in doc.get("records", ())
        if not record.get("adjudications")
    ]
    return sorted(records, key=lambda record: record.get("id", ""))


def legacy_projection_sha256(doc: dict) -> str:
    return _canonical_sha256(legacy_projection_payload(doc))


def registry_projection_payload(doc: dict) -> dict:
    """Complete sealed document excluding only its self-referential digest."""
    payload = copy.deepcopy(doc)
    contract = payload.get("adjudication_contract")
    if isinstance(contract, dict):
        contract.pop("registry_projection_sha256", None)
    return payload


def registry_projection_sha256(doc: dict) -> str:
    return _canonical_sha256(registry_projection_payload(doc))


def canonical_ref_set_sha256(refs: set[str]) -> str:
    payload = json.dumps(sorted(refs), ensure_ascii=True, separators=(",", ":"))
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()


def _check_adjudication_contract(
    doc: dict,
    errors: list[str],
    seen_adjudications: dict[str, str],
    grain_refs: dict[str, set[str]],
    disposition_refs: dict[str, set[str]],
    source_fragments: dict[str, set[str]],
) -> None:
    contract = doc.get("adjudication_contract")
    if seen_adjudications and contract is None:
        errors.append("E_ADJ_CONTRACT: $: enhanced mappings require a sealed contract")
    if contract is not None and not seen_adjudications:
        errors.append("E_ADJ_CONTRACT: $: sealed contract has no enhanced mappings")
    if contract is not None:
        for name, expected in SEALED_CONTRACT_CONSTS.items():
            if contract.get(name) != expected:
                errors.append(
                    f"E_ADJ_CONTRACT: $.adjudication_contract.{name}: value is "
                    "not the validator-pinned audit contract"
                )
        if len(doc["records"]) != PROJECTED_RECORD_COUNT:
            errors.append(
                f"E_ADJ_COUNT: $.records: projected registry has {len(doc['records'])} "
                f"record(s), expected {PROJECTED_RECORD_COUNT}"
            )
        record_shape = {name: 0 for name in RECORD_SHAPE_CONTRACT}
        for rec in doc["records"]:
            adjudications = rec.get("adjudications") or []
            if not adjudications:
                record_shape["legacy_record_count"] += 1
            elif any(item["disposition"] == "bug_candidate" for item in adjudications):
                record_shape["bug_record_count"] += 1
            else:
                record_shape["guard_record_count"] += 1
            if rec.get("record_origin") == "preexisting_public_baseline":
                record_shape["baseline_update_record_count"] += 1
            elif rec.get("record_origin") == "new":
                record_shape["new_record_count"] += 1
        for name, expected in RECORD_SHAPE_CONTRACT.items():
            if record_shape[name] != expected:
                errors.append(
                    f"E_ADJ_COUNT: $.records: {name} is {record_shape[name]}, "
                    f"expected {expected}"
                )
        for name, expected in ADJUDICATION_COUNT_CONTRACT.items():
            actual = (
                len(grain_refs[name])
                if name in grain_refs
                else len(disposition_refs[name])
            )
            if actual != expected:
                errors.append(
                    f"E_ADJ_COUNT: $: {name} has {actual} unique ref(s), "
                    f"expected {expected}"
                )
        refs = set(seen_adjudications)
        actual_ref_digest = canonical_ref_set_sha256(refs)
        if actual_ref_digest != CANONICAL_REF_SET_SHA256:
            errors.append(
                "E_ADJ_REF_SET: $: canonical adjudication namespace differs "
                "from the ratified 159-ref set"
            )
        if contract.get("canonical_ref_set_sha256") != actual_ref_digest:
            errors.append(
                "E_ADJ_REF_SET: $.adjudication_contract: stored ref-set digest "
                "does not match mapped refs"
            )
        stored_semantic = contract.get("semantic_commitment_sha256")
        if not isinstance(stored_semantic, str) or not DIGEST_NONZERO_RE.fullmatch(
            stored_semantic
        ):
            errors.append(
                "E_ADJ_COMMITMENT: $.adjudication_contract: semantic digest must "
                "be a nonzero lowercase sha256"
            )
        else:
            actual_semantic = semantic_commitment_sha256(doc)
            if stored_semantic != actual_semantic:
                errors.append(
                    "E_ADJ_COMMITMENT: $.adjudication_contract: semantic tuple "
                    "commitment does not match mapped adjudications"
                )
        stored_legacy = contract.get("legacy_projection_sha256")
        if not isinstance(stored_legacy, str) or not DIGEST_NONZERO_RE.fullmatch(
            stored_legacy
        ):
            errors.append(
                "E_ADJ_PROJECTION: $.adjudication_contract.legacy_projection_sha256: "
                "expected a nonzero lowercase sha256"
            )
        elif stored_legacy != legacy_projection_sha256(doc):
            errors.append(
                "E_ADJ_PROJECTION: $.adjudication_contract.legacy_projection_sha256: "
                "digest does not match the complete legacy-record projection"
            )
        stored_projection = contract.get("registry_projection_sha256")
        if not isinstance(stored_projection, str) or not DIGEST_NONZERO_RE.fullmatch(
            stored_projection
        ):
            errors.append(
                "E_ADJ_PROJECTION: $.adjudication_contract.registry_projection_sha256: "
                "expected a nonzero lowercase sha256"
            )
        elif stored_projection != registry_projection_sha256(doc):
            errors.append(
                "E_ADJ_PROJECTION: $.adjudication_contract.registry_projection_sha256: "
                "digest does not match the complete sealed projection"
            )
        stored_baseline_set = contract.get("baseline_record_set_sha256")
        actual_baseline_set = baseline_record_set_sha256(doc)
        if not isinstance(stored_baseline_set, str) or not DIGEST_NONZERO_RE.fullmatch(
            stored_baseline_set
        ):
            errors.append(
                "E_BASELINE_AUTHORITY: "
                "$.adjudication_contract.baseline_record_set_sha256: expected "
                "a nonzero externally approved digest"
            )
        elif stored_baseline_set != actual_baseline_set:
            errors.append(
                "E_BASELINE_AUTHORITY: "
                "$.adjudication_contract.baseline_record_set_sha256: digest "
                "does not match the exact baseline record identity/content set"
            )
        baseline_receipt = contract.get("baseline_external_receipt")
        if not isinstance(baseline_receipt, dict) or not (
            baseline_receipt.get("state") == BASELINE_RECEIPT_STATE
            and baseline_receipt.get("record_count")
            == RECORD_SHAPE_CONTRACT["baseline_update_record_count"]
            and baseline_receipt.get("approved_record_set_sha256")
            == stored_baseline_set
            and isinstance(baseline_receipt.get("receipt_sha256"), str)
            and DIGEST_NONZERO_RE.fullmatch(baseline_receipt["receipt_sha256"])
            is not None
        ):
            errors.append(
                "E_BASELINE_AUTHORITY: "
                "$.adjudication_contract.baseline_external_receipt: external "
                "approval does not bind the exact 20-record baseline set"
            )
    for fragments in source_fragments.values():
        if len(fragments) > MAX_RECONSTRUCTION_FRAGMENTS_PER_SOURCE:
            errors.append(
                "E_PRIV_AGGREGATE: $.records: one opaque source reconstructs "
                f"{len(fragments)} distinct payload fragments; maximum is "
                f"{MAX_RECONSTRUCTION_FRAGMENTS_PER_SOURCE}"
            )


def _adjudication_locations(doc: dict) -> list[tuple[str, dict]]:
    locations: list[tuple[str, dict]] = []
    records = doc.get("records")
    if not isinstance(records, list):
        records = []
    for index, rec in enumerate(records):
        if not isinstance(rec, dict):
            continue
        adjudications = rec.get("adjudications")
        if not isinstance(adjudications, list):
            continue
        for adj_index, adjudication in enumerate(adjudications):
            if isinstance(adjudication, dict):
                locations.append(
                    (f"$.records[{index}].adjudications[{adj_index}]", adjudication)
                )
    source_exclusions = doc.get("source_exclusions")
    if not isinstance(source_exclusions, list):
        source_exclusions = []
    for index, adjudication in enumerate(source_exclusions):
        if isinstance(adjudication, dict):
            locations.append((f"$.source_exclusions[{index}]", adjudication))
    return locations


def _check_hmac_contract_preflight(
    doc: dict,
    contract: dict | None,
    locations: list[tuple[str, dict]],
    errors: list[str],
) -> None:
    """Fail closed on the public HMAC envelope before schema short-circuiting.

    This validator proves only syntax, domain/key-epoch consistency and
    cross-domain suffix separation. The private external receipt owns actual
    HMAC recomputation because the secret key never enters this repository.
    """
    if not locations:
        return
    if not isinstance(contract, dict):
        errors.append(
            "E_HMAC_CONTRACT: $.adjudication_contract: enhanced mappings require "
            "the sealed HMAC construction declaration"
        )
        return

    expected_fields = {
        "hmac_scheme": HMAC_SCHEME,
        "hmac_min_key_bytes": HMAC_MIN_KEY_BYTES,
        "hmac_domains": list(HMAC_DOMAINS),
        "hmac_scalar_payload_encoding": HMAC_SCALAR_PAYLOAD_ENCODING,
        "hmac_structured_payload_encoding": HMAC_STRUCTURED_PAYLOAD_ENCODING,
        "hmac_input_frame": HMAC_INPUT_FRAME,
    }
    for field, expected in expected_fields.items():
        if contract.get(field) != expected:
            errors.append(
                f"E_HMAC_CONTRACT: $.adjudication_contract.{field}: value does "
                "not match the pinned construction"
            )

    key_id = contract.get("hmac_key_id")
    if not isinstance(key_id, str) or re.fullmatch(HMAC_KEY_ID_PATTERN, key_id) is None:
        errors.append(
            "E_HMAC_CONTRACT: $.adjudication_contract.hmac_key_id: expected a "
            "32-hex public rotation identifier"
        )
        key_id = None
    receipt = contract.get("hmac_external_receipt")
    if not isinstance(receipt, dict):
        errors.append(
            "E_HMAC_CONTRACT: $.adjudication_contract.hmac_external_receipt: "
            "external private recomputation declaration is required"
        )
    else:
        receipt_ok = (
            receipt.get("state") == HMAC_RECEIPT_STATE
            and receipt.get("scheme") == HMAC_SCHEME
            and receipt.get("key_id") == key_id
            and receipt.get("coverage") == HMAC_RECEIPT_COVERAGE
            and isinstance(receipt.get("receipt_sha256"), str)
            and DIGEST_NONZERO_RE.fullmatch(receipt["receipt_sha256"]) is not None
        )
        if not receipt_ok:
            errors.append(
                "E_HMAC_CONTRACT: $.adjudication_contract.hmac_external_receipt: "
                "declaration does not cover the pinned scheme, domain, "
                "serialization and key epoch"
            )

    strings: list[tuple[tuple, str]] = []
    _walk_strings(doc, (), strings)
    for index, record in enumerate(doc.get("records", ())):
        flags = (record.get("environment") or {}).get("flags") or {}
        if isinstance(flags, dict):
            strings.extend(
                (
                    ("records", index, "environment", "flags", "*"),
                    key,
                )
                for key in flags
                if isinstance(key, str)
            )
    suffix_uses: dict[str, set[tuple[str, str]]] = {}
    for keys, text in strings:
        where = _display_path("$", keys)
        if LEGACY_HMAC_REF_RE.fullmatch(text):
            errors.append(
                f"E_HMAC_LEGACY: {where}: legacy unsalted/unspecified-key "
                "opaque reference is forbidden"
            )
            continue
        match = HMAC_REF_PARSE_RE.fullmatch(text)
        if match is None:
            continue
        ref_key_id = match.group("key_id")
        domain = match.group("domain")
        mac = match.group("mac")
        if key_id is not None and ref_key_id != key_id:
            errors.append(
                f"E_HMAC_KEY_ID: {where}: opaque reference uses a different "
                "rotation identifier than the sealed contract"
            )
        suffix_uses.setdefault(mac, set()).add((domain, ref_key_id))
    if any(len(uses) > 1 for uses in suffix_uses.values()):
        errors.append(
            "E_HMAC_SUFFIX: $: one MAC suffix is reused across domains or key epochs"
        )


def _check_enhanced_preflight(doc, errors: list[str]) -> None:
    """Emit fail-closed domain codes even when structural validation fails."""
    if not isinstance(doc, dict):
        return
    locations = _adjudication_locations(doc)
    contract = doc.get("adjudication_contract")
    _check_hmac_contract_preflight(doc, contract, locations, errors)
    if locations and (
        not isinstance(contract, dict) or contract.get("state") != "sealed"
    ):
        errors.append(
            "E_ADJ_CONTRACT: $: enhanced mappings require state='sealed' contract"
        )
    if doc.get("source_exclusions") and not locations:
        errors.append(
            "E_ADJ_CONTRACT: $.source_exclusions: exclusions require enhanced mappings"
        )
    if isinstance(contract, dict):
        digest = contract.get("semantic_commitment_sha256")
        if not isinstance(digest, str) or not DIGEST_NONZERO_RE.fullmatch(digest):
            errors.append(
                "E_ADJ_COMMITMENT: $.adjudication_contract: semantic digest must "
                "be a nonzero lowercase sha256"
            )
        for field in (
            "legacy_projection_sha256",
            "registry_projection_sha256",
        ):
            projection_digest = contract.get(field)
            if not isinstance(
                projection_digest, str
            ) or not DIGEST_NONZERO_RE.fullmatch(projection_digest):
                errors.append(
                    f"E_ADJ_PROJECTION: $.adjudication_contract.{field}: "
                    "expected a nonzero lowercase sha256"
                )
        baseline_digest = contract.get("baseline_record_set_sha256")
        if not isinstance(baseline_digest, str) or not DIGEST_NONZERO_RE.fullmatch(
            baseline_digest
        ):
            errors.append(
                "E_BASELINE_AUTHORITY: "
                "$.adjudication_contract.baseline_record_set_sha256: expected "
                "a nonzero externally approved digest"
            )
        baseline_receipt = contract.get("baseline_external_receipt")
        if not isinstance(baseline_receipt, dict):
            errors.append(
                "E_BASELINE_AUTHORITY: "
                "$.adjudication_contract.baseline_external_receipt: external "
                "baseline approval declaration is required"
            )
        exclusions = doc.get("source_exclusions")
        if not isinstance(exclusions, list) or len(exclusions) != 2:
            errors.append(
                "E_ADJ_COUNT: $.source_exclusions: sealed contract requires "
                "exactly two top-level exclusions"
            )
    for path, adjudication in locations:
        grain = adjudication.get("grain")
        ref = adjudication.get("ref")
        if (
            not isinstance(grain, str)
            or not isinstance(ref, str)
            or not canonical_ref_matches_grain(grain, ref)
        ):
            errors.append(
                f"E_ADJ_REF: {path}.ref: ref must use the canonical grain namespace"
            )
        normalized_class = adjudication.get("normalized_class")
        if normalized_class is None:
            errors.append(f"E_ADJ_NORMALIZED: {path}: normalized_class is mandatory")
        elif grain == "original_candidate" and normalized_class != "not_applicable":
            errors.append(
                f"E_ADJ_NORMALIZED: {path}: original mapping requires not_applicable"
            )
        elif (
            grain == "supplemental_hypothesis" and normalized_class == "not_applicable"
        ):
            errors.append(
                f"E_ADJ_NORMALIZED: {path}: supplemental mapping cannot use not_applicable"
            )
        source_events = adjudication.get("source_event_refs")
        if isinstance(source_events, list):
            strings_only = all(isinstance(event, str) for event in source_events)
            if (
                not strings_only
                or len(source_events) != len(set(source_events))
                or any(
                    SOURCE_EVENT_REF_RE.fullmatch(event) is None
                    for event in source_events
                )
            ):
                errors.append(
                    f"E_ADJ_EVENT: {path}.source_event_refs: event IDs must be "
                    "unique canonical typed event-HMAC refs"
                )


def _check_schema_drift(schema: dict, errors: list[str]) -> None:
    props = schema.get("$defs", {}).get("record", {}).get("properties", {})
    if tuple(props.get("status", {}).get("enum", ())) != STATUSES:
        errors.append(
            "E_SCHEMA_DRIFT: schema status enum differs from validate.STATUSES"
        )
    if (
        tuple(props.get("privacy_classification", {}).get("enum", ()))
        != PRIVACY_CLASSES
    ):
        errors.append(
            "E_SCHEMA_DRIFT: schema privacy enum differs from validate.PRIVACY_CLASSES"
        )
    definitions = schema.get("$defs", {})
    enum_contracts = (
        ("adjudication_classification", ADJUDICATION_CLASSIFICATIONS),
        ("adjudication_disposition", ADJUDICATION_DISPOSITIONS),
        ("expected_form_status", EXPECTED_FORM_STATUSES),
        ("expected_authority", EXPECTED_AUTHORITIES),
        ("causal_confidence", CAUSAL_CONFIDENCES),
        ("owner_lane", OWNER_LANES),
        ("normalized_class", NORMALIZED_CLASSES),
        ("adjudication_privacy_class", ADJUDICATION_PRIVACY_CLASSES),
    )
    for definition, expected in enum_contracts:
        if tuple(definitions.get(definition, {}).get("enum", ())) != expected:
            errors.append(
                f"E_SCHEMA_DRIFT: schema {definition} enum differs from validator contract"
            )
    adjudication = definitions.get("adjudication", {}).get("properties", {})
    if tuple(adjudication.get("grain", {}).get("enum", ())) != ADJUDICATION_GRAINS:
        errors.append(
            "E_SCHEMA_DRIFT: schema adjudication grain enum differs from validator contract"
        )
    source_ref = definitions.get("adjudication_source_ref", {}).get("properties", {})
    if tuple(source_ref.get("kind", {}).get("enum", ())) != ADJUDICATION_SOURCE_KINDS:
        errors.append(
            "E_SCHEMA_DRIFT: schema adjudication source kind differs from validator contract"
        )
    pattern_contracts = (
        ("hmac_key_id", rf"^{HMAC_KEY_ID_PATTERN}$"),
        ("typed_hmac_ref", rf"^{HMAC_REF_PATTERN}$"),
        (
            "source_event_ref",
            rf"^{HMAC_SCHEME}:event:{HMAC_KEY_ID_PATTERN}:[0-9a-f]{{64}}$",
        ),
        (
            "expected_value_ref_or_null",
            rf"^{HMAC_SCHEME}:value:{HMAC_KEY_ID_PATTERN}:[0-9a-f]{{64}}$",
        ),
    )
    for definition, expected_pattern in pattern_contracts:
        if definitions.get(definition, {}).get("pattern") != expected_pattern:
            errors.append(
                f"E_SCHEMA_DRIFT: schema {definition} pattern differs from "
                "the typed-HMAC validator contract"
            )
    contract_props = definitions.get("adjudication_contract", {}).get("properties", {})
    for name, expected in SEALED_CONTRACT_CONSTS.items():
        if contract_props.get(name, {}).get("const") != expected:
            errors.append(
                f"E_SCHEMA_DRIFT: schema adjudication contract pin {name!r} "
                "differs from validator contract"
            )
    contract_required = set(
        definitions.get("adjudication_contract", {}).get("required", ())
    )
    for field in (
        "semantic_commitment_sha256",
        "legacy_projection_sha256",
        "registry_projection_sha256",
        "baseline_record_set_sha256",
        "baseline_external_receipt",
        "hmac_key_id",
        "hmac_external_receipt",
    ):
        if field not in contract_required or field not in contract_props:
            errors.append(
                f"E_SCHEMA_DRIFT: schema adjudication contract omits required "
                f"digest field {field}"
            )
    for field in ("record_origin", "baseline_record_sha256"):
        if field not in props:
            errors.append(
                f"E_SCHEMA_DRIFT: schema enhanced record omits origin field {field}"
            )
    if tuple(props.get("record_origin", {}).get("enum", ())) != RECORD_ORIGINS:
        errors.append(
            "E_SCHEMA_DRIFT: schema record_origin enum differs from validator contract"
        )
    receipt_contracts = {
        "hmac_external_receipt": {
            "state": HMAC_RECEIPT_STATE,
            "scheme": HMAC_SCHEME,
            "coverage": HMAC_RECEIPT_COVERAGE,
        },
        "baseline_external_receipt": {
            "state": BASELINE_RECEIPT_STATE,
            "record_count": RECORD_SHAPE_CONTRACT["baseline_update_record_count"],
        },
    }
    for definition, constants in receipt_contracts.items():
        receipt_schema = definitions.get(definition, {})
        receipt_props = receipt_schema.get("properties", {})
        if receipt_schema.get("additionalProperties") is not False:
            errors.append(
                f"E_SCHEMA_DRIFT: schema {definition} must close unknown fields"
            )
        for field, expected in constants.items():
            if receipt_props.get(field, {}).get("const") != expected:
                errors.append(
                    f"E_SCHEMA_DRIFT: schema {definition} pin {field!r} differs "
                    "from validator contract"
                )


def validate_document(doc, schema: dict) -> list[str]:
    """Return every violation for an in-memory registry document."""
    errors: list[str] = []
    _check_schema_drift(schema, errors)
    _check_enhanced_preflight(doc, errors)
    SchemaChecker(schema).check(schema, doc, "$", errors)
    if any(error.startswith(("E_SCHEMA", "E_ENUM")) for error in errors):
        return errors  # semantic checks assume the structural contract holds
    seen_keys: dict[str, str] = {}
    seen_ids: dict[str, str] = {}
    seen_adjudications: dict[str, str] = {}
    grain_refs = {grain: set() for grain in ADJUDICATION_GRAINS}
    disposition_refs = {disposition: set() for disposition in ADJUDICATION_DISPOSITIONS}
    source_fragments: dict[str, set[str]] = {}
    event_owners: dict[str, str] = {}
    for i, rec in enumerate(doc["records"]):
        path = f"$.records[{i}]"
        _check_identity(rec, path, errors)
        key, rid = rec["dedup_key"], rec["id"]
        if key in seen_keys:
            errors.append(
                f"E_DUP: {path}.dedup_key: value is already used by another record"
            )
        seen_keys.setdefault(key, path)
        if rid in seen_ids:
            errors.append(f"E_DUP: {path}.id: value is already used by another record")
        seen_ids.setdefault(rid, path)
        _check_sides(rec, path, errors)
        _check_gates(rec, path, errors)
        _check_privacy(rec, path, errors)
        _check_adjudication_record(
            rec,
            path,
            errors,
            seen_adjudications,
            grain_refs,
            disposition_refs,
            source_fragments,
            event_owners,
        )
    for index, adjudication in enumerate(doc.get("source_exclusions", ())):
        path = f"$.source_exclusions[{index}]"
        _source_records, _events = _check_adjudication_item(
            adjudication,
            path,
            path,
            errors,
            seen_adjudications,
            grain_refs,
            disposition_refs,
            event_owners,
        )
        if (
            adjudication["grain"] != "supplemental_hypothesis"
            or adjudication["classification"] != "source_authorship_exclusion"
            or adjudication["disposition"] != "source_exclusion"
            or adjudication["normalized_class"] != "source_harness_framing"
            or adjudication["owner_lane"] != "outside_product_source_authorship"
            or adjudication["privacy_class"] != "metadata_only"
        ):
            errors.append(
                f"E_ADJ_STATE: {path}: top-level exclusion must use the exact "
                "source-harness exclusion envelope"
            )
        if not any(
            source["kind"] in {"ruling", "receipt"}
            for source in adjudication["source_refs"]
        ):
            errors.append(
                f"E_ADJ_GUARD_EVIDENCE: {path}.source_refs: exclusion requires "
                "a ruling/receipt"
            )
    _check_adjudication_contract(
        doc,
        errors,
        seen_adjudications,
        grain_refs,
        disposition_refs,
        source_fragments,
    )
    return errors


def canonical_text(doc) -> str:
    return json.dumps(doc, indent=2, ensure_ascii=False) + "\n"


def validate_file(registry: Path, schema_path: Path = DEFAULT_SCHEMA) -> list[str]:
    text = Path(registry).read_text(encoding="utf-8")
    doc = json.loads(text)
    schema = json.loads(Path(schema_path).read_text(encoding="utf-8"))
    errors = validate_document(doc, schema)
    if text != canonical_text(doc):
        errors.append(
            "E_FORMAT: registry is not in canonical form (indent 2, ensure_ascii=False, trailing newline)"
        )
    return errors


def status_counts(doc) -> dict[str, int]:
    counts = {s: 0 for s in STATUSES}
    for rec in doc.get("records", []):
        counts[rec.get("status", "?")] = counts.get(rec.get("status", "?"), 0) + 1
    return {k: v for k, v in counts.items() if v}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("registry", nargs="?", default=str(DEFAULT_REGISTRY))
    parser.add_argument("--schema", default=str(DEFAULT_SCHEMA))
    args = parser.parse_args(argv)
    try:
        errors = validate_file(Path(args.registry), Path(args.schema))
        doc = json.loads(Path(args.registry).read_text(encoding="utf-8"))
    except (OSError, ValueError):
        print("E_READ: input could not be read or parsed", file=sys.stderr)
        return 2
    for err in errors:
        print(err)
    count = len(doc.get("records", [])) if isinstance(doc, dict) else 0
    if errors:
        print(f"FAIL: {len(errors)} violation(s) in {count} record(s)")
        return 1
    print(
        f"OK: {count} record(s); by status: {json.dumps(status_counts(doc), sort_keys=True)}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
