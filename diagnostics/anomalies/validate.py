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
4. optional G0 adjudication mapping: exact ID/count accounting, enum coupling,
   expected-form provenance, causal confidence and lifecycle rules;
5. lifecycle gates: what evidence each ``status`` must carry;
6. privacy walk over every string in every record: no ``@``, no ``http``
   outside ``source_refs``, no digit run longer than six characters outside
   SHA/reference fields, no letter run longer than 40 characters, no
   newlines, one whitespace-free token per side of the context; enhanced
   records also have an aggregate source-linked reconstruction budget;
7. canonical on-disk format (indent 2, non-ASCII preserved, trailing newline).

The registry never changes SmartKey behaviour: this file only reads JSON.
"""

from __future__ import annotations

import argparse
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
MAX_RECONSTRUCTION_FRAGMENTS_PER_SOURCE = 8

SHA_RE = re.compile(r"^[0-9a-f]{40}$")
DIGIT_RUN_RE = re.compile(r"[0-9]{7,}")
LETTER_RUN_RE = re.compile(r"[A-Za-zЀ-ӿ]{41,}")
WORD_FRAGMENT_RE = re.compile(r"[A-Za-zЀ-ӿ]{2,40}")
OPAQUE_ID_PATTERN = r"(?:[0-9a-f]{12}|[0-9a-f]{16}|[0-9a-f]{64}|H[0-9]{3})"
OPAQUE_ID_RE = re.compile(rf"^{OPAQUE_ID_PATTERN}$")
METADATA_TEXT_RE = re.compile(rf"^metadata:{OPAQUE_ID_PATTERN}$")
CYRILLIC_RE = re.compile(r"[Ѐ-ӿ]")
LATIN_RE = re.compile(r"[A-Za-z]")
# Fields whose values are commit SHAs (40 hex or "unknown") by contract.
SHA_KEYS = frozenset({"build_sha", "commit", "fix_commit"})
# Fields whose values are ledger/ruling/test identifiers or derived identity;
# their source fields are independently privacy-checked.
IDENTIFIER_KEYS = frozenset({"id", "dedup_key", "ref", "evidence_refs"})


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
    return _side_key(record.get("observed") or {}) + "|" + _side_key(record.get("expected") or {})


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
            errors.append(f"E_ENUM: {path}: {value!r} not in {node['enum']}")
            return
        if "type" in node:
            types = node["type"] if isinstance(node["type"], list) else [node["type"]]
            if not any(_TYPE_CHECKS[t](value) for t in types):
                errors.append(f"E_SCHEMA: {path}: expected type {types}, got {type(value).__name__}")
                return
        if value is None:
            return
        if isinstance(value, str):
            if "minLength" in node and len(value) < node["minLength"]:
                errors.append(f"E_SCHEMA: {path}: shorter than {node['minLength']}")
            if "maxLength" in node and len(value) > node["maxLength"]:
                errors.append(f"E_SCHEMA: {path}: longer than {node['maxLength']}")
            if "pattern" in node and not re.search(node["pattern"], value):
                errors.append(f"E_SCHEMA: {path}: {value!r} does not match {node['pattern']}")
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
                    errors.append(f"E_SCHEMA: {path}: unknown key {key!r}")
                elif isinstance(extra, dict):
                    self.check(extra, item, f"{path}.{key}", errors)


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
        errors.append(f"E_DEDUP_KEY: {path}: stored {rec.get('dedup_key')!r} != computed {expected_key!r}")
    expected_id = compute_id(rec)
    if rec.get("id") != expected_id:
        errors.append(f"E_ID: {path}: stored {rec.get('id')!r} != computed {expected_id!r}")


def _check_sides(rec: dict, path: str, errors: list[str]) -> None:
    observed = rec["observed"]
    expected = rec["expected"]
    for name, side in (("observed", observed), ("expected", expected)):
        token = side["token"]
        if isinstance(token, str) and token.split() != [token]:
            errors.append(f"E_TOKEN_WS: {path}.{name}.token: must be one whitespace-free token")
        actual = script_class(token)
        if side["script"] != actual:
            errors.append(f"E_SCRIPT: {path}.{name}.script: declared {side['script']!r}, token is {actual!r}")
    if observed["token"] is None:
        if not (isinstance(observed["descriptor"], str) and observed["descriptor"].strip()):
            errors.append(f"E_OBSERVED: {path}.observed: descriptor required when token is null")
        if rec["privacy_classification"] != "unknown":
            errors.append(f"E_OBSERVED: {path}.privacy_classification: must be 'unknown' when no token was captured")
    elif rec["privacy_classification"] == "unknown":
        errors.append(f"E_OBSERVED: {path}.privacy_classification: 'unknown' but observed.token is present")
    adjudications = rec.get("adjudications") or []
    expected_not_applicable = bool(adjudications) and all(
        adj["expected_form_status"] == "not_applicable" for adj in adjudications
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
        errors.append(f"E_EXPECTED: {path}: status {rec['status']!r} needs a confirmed expected form")
    if rec["repeat"] != (rec["occurrence_count"] > 1):
        errors.append(f"E_REPEAT: {path}: repeat must equal occurrence_count > 1")
    first, last, recorded = rec["first_seen_utc"], rec["last_seen_utc"], rec["recorded_utc"]
    if first and last and _pad_utc(first) > _pad_utc(last):
        errors.append(f"E_TIME: {path}: first_seen_utc after last_seen_utc")
    for name, stamp in (("first_seen_utc", first), ("last_seen_utc", last)):
        if stamp and _pad_utc(stamp) > _pad_utc(recorded):
            errors.append(f"E_TIME: {path}: {name} after recorded_utc")


def _check_gates(rec: dict, path: str, errors: list[str]) -> None:
    status = rec["status"]
    has = {k: rec.get(k) is not None for k in ("reproducer", "red_test", "red_test_waiver", "fix_commit", "verification")}
    has_closure = isinstance(rec.get("closure_reason"), str) and bool(rec["closure_reason"].strip())

    def need(cond: bool, what: str) -> None:
        if not cond:
            errors.append(f"E_GATE: {path}: status {status!r} requires {what}")

    if status in ("reproduced", "red_tested", "fixed", "verified", "closed", "not_smartkey"):
        need(has["reproducer"], "a reproducer (probe/trace/test evidence)")
    if status == "red_tested":
        need(has["red_test"], "a linked RED test")
    if status in ("fixed", "verified", "closed"):
        need(has["fix_commit"], "an exact fix_commit")
        need(has["red_test"] or has["red_test_waiver"], "a linked regression test or an explicit red_test_waiver")
    if status in ("verified", "closed"):
        need(has["verification"], "post-fix verification on the reported surface")
    if status in ("closed", "not_smartkey"):
        need(has_closure, "a closure_reason")
    if status in ("open_suspected_smartkey", "reproduced", "red_tested"):
        if has["fix_commit"] or has["verification"]:
            errors.append(f"E_STATE: {path}: status {status!r} cannot carry fix_commit/verification")
    if has["verification"] and not has["fix_commit"]:
        errors.append(f"E_STATE: {path}: verification without fix_commit")
    if has["red_test"] and has["red_test_waiver"]:
        errors.append(f"E_STATE: {path}: red_test and red_test_waiver are mutually exclusive")


def _walk_strings(value, path: tuple, out: list[tuple[tuple, str]]) -> None:
    if isinstance(value, str):
        out.append((path, value))
    elif isinstance(value, dict):
        for k, v in value.items():
            _walk_strings(v, path + (k,), out)
    elif isinstance(value, list):
        for i, v in enumerate(value):
            _walk_strings(v, path + (i,), out)


def _check_privacy(rec: dict, path: str, errors: list[str]) -> None:
    strings: list[tuple[tuple, str]] = []
    _walk_strings(rec, (), strings)
    for keys, text in strings:
        where = path + "." + ".".join(str(k) for k in keys)
        in_source_refs = bool(keys) and keys[0] == "source_refs"
        last_key = next((k for k in reversed(keys) if isinstance(k, str)), None)
        is_sha_field = last_key in SHA_KEYS
        is_opaque = (
            OPAQUE_ID_RE.fullmatch(text) is not None
            or METADATA_TEXT_RE.fullmatch(text) is not None
        )
        is_identifier = is_sha_field or last_key in IDENTIFIER_KEYS or is_opaque
        if "@" in text:
            errors.append(f"E_PRIV_AT: {where}: '@' is never allowed (addresses/handles)")
        if "http" in text.lower() and not in_source_refs:
            errors.append(f"E_PRIV_URL: {where}: URLs only inside source_refs")
        if "\n" in text or "\r" in text:
            errors.append(f"E_PRIV_MULTILINE: {where}: multi-line text looks like a transcript")
        if LETTER_RUN_RE.search(text) and not is_opaque and last_key != "dedup_key":
            errors.append(f"E_PRIV_LONGRUN: {where}: letter run longer than 40 characters")
        if not is_identifier and DIGIT_RUN_RE.search(text):
            errors.append(f"E_PRIV_DIGITS: {where}: digit run longer than 6 outside SHA/reference fields")
        if is_sha_field and text != "unknown" and not SHA_RE.match(text):
            errors.append(f"E_SHA: {where}: SHA field must be 40 hex or 'unknown'")
    for side in ("before", "after"):
        token = rec["minimal_context"][side]
        if isinstance(token, str) and token.split() != [token]:
            errors.append(f"E_PRIV_CONTEXT: {path}.minimal_context.{side}: at most one whitespace-free token per side")


def _payload_fragments(rec: dict) -> set[str]:
    """Return reconstruction-bearing atoms, never their source order."""
    fragments: set[str] = set()
    for side in ("observed", "expected"):
        for field in ("token", "descriptor"):
            value = rec[side][field]
            if isinstance(value, str) and METADATA_TEXT_RE.fullmatch(value) is None:
                fragments.add(normalize(value))
    for side in ("before", "after"):
        value = rec["minimal_context"][side]
        if isinstance(value, str):
            fragments.add(normalize(value))

    free_text = [rec.get("notes"), rec.get("closure_reason")]
    reproducer = rec.get("reproducer")
    verification = rec.get("verification")
    if reproducer:
        free_text.append(reproducer.get("summary"))
    if verification:
        free_text.append(verification.get("summary"))
    free_text.extend(source.get("note") for source in rec.get("source_refs", ()))
    for value in free_text:
        if isinstance(value, str) and METADATA_TEXT_RE.fullmatch(value) is None:
            fragments.update(
                normalize(word) for word in WORD_FRAGMENT_RE.findall(value)
            )
    return {fragment for fragment in fragments if fragment}


def _check_adjudication_privacy(rec: dict, path: str, errors: list[str]) -> None:
    """Enhanced records carry only isolated values plus opaque metadata."""
    if any(rec["minimal_context"].values()):
        errors.append(
            f"E_ADJ_PRIVACY: {path}.minimal_context: enhanced records require null context"
        )

    templated = [
        ("notes", rec.get("notes")),
        ("closure_reason", rec.get("closure_reason")),
        ("observed.descriptor", rec["observed"].get("descriptor")),
        ("expected.descriptor", rec["expected"].get("descriptor")),
    ]
    if rec.get("reproducer"):
        templated.append(("reproducer.summary", rec["reproducer"].get("summary")))
    if rec.get("verification"):
        templated.append(("verification.summary", rec["verification"].get("summary")))
    for index, source in enumerate(rec.get("source_refs", ())):
        templated.append((f"source_refs[{index}].note", source.get("note")))
    for field, value in templated:
        if isinstance(value, str) and not METADATA_TEXT_RE.fullmatch(value):
            errors.append(
                f"E_ADJ_PRIVACY: {path}.{field}: enhanced free text must be metadata:<opaque-id>"
            )


def _check_adjudication_record(
    rec: dict,
    path: str,
    errors: list[str],
    seen_adjudications: dict[tuple[str, str], str],
    grain_refs: dict[str, set[tuple[str, str]]],
    disposition_refs: dict[str, set[tuple[str, str]]],
    source_fragments: dict[str, set[str]],
) -> None:
    adjudications = rec.get("adjudications")
    if not adjudications:
        return

    _check_adjudication_privacy(rec, path, errors)
    dispositions: set[str] = set()
    privacy_classes: set[str] = set()
    null_statuses = {"null_non_unique", "null_pending_operator_intent"}
    unique_statuses = {"unique_operator_confirmed", "unique_authoritative_spelling"}

    for index, adjudication in enumerate(adjudications):
        adj_path = f"{path}.adjudications[{index}]"
        grain = adjudication["grain"]
        ref = adjudication["ref"]
        mapping_key = (grain, ref)
        prior = seen_adjudications.get(mapping_key)
        if prior is not None:
            errors.append(
                f"E_ADJ_DUP: {adj_path}.ref: mapping already present at {prior}"
            )
        else:
            seen_adjudications[mapping_key] = adj_path
        grain_refs[grain].add(mapping_key)

        classification = adjudication["classification"]
        disposition = adjudication["disposition"]
        dispositions.add(disposition)
        disposition_refs[disposition].add(mapping_key)
        if CLASSIFICATION_DISPOSITION[classification] != disposition:
            errors.append(
                f"E_ADJ_STATE: {adj_path}: classification {classification!r} "
                f"requires disposition {CLASSIFICATION_DISPOSITION[classification]!r}"
            )

        expected_status = adjudication["expected_form_status"]
        confidence = adjudication["causal_confidence"]
        if confidence["anomaly_or_guard_presence"] not in ("medium", "high"):
            errors.append(
                f"E_ADJ_CONFIDENCE: {adj_path}.causal_confidence.anomaly_or_guard_presence: "
                "a retained mapping requires medium or high confidence"
            )
        if expected_status in unique_statuses:
            if (
                not isinstance(rec["expected"]["token"], str)
                or rec["needs_operator_confirmation"]
            ):
                errors.append(
                    f"E_ADJ_EXPECTED: {adj_path}: unique expected status requires a stored, confirmed token"
                )
            if confidence["expected_form"] != "high":
                errors.append(
                    f"E_ADJ_CONFIDENCE: {adj_path}.causal_confidence.expected_form: "
                    "a unique expected form requires high confidence"
                )
        elif expected_status in null_statuses:
            if (
                rec["expected"]["token"] is not None
                or rec["expected"]["descriptor"] is not None
            ):
                errors.append(
                    f"E_ADJ_EXPECTED: {adj_path}: null expected status cannot carry an expected value"
                )
            if not rec["needs_operator_confirmation"]:
                errors.append(
                    f"E_ADJ_EXPECTED: {adj_path}: null expected status requires operator confirmation"
                )
            if confidence["expected_form"] not in ("none", "low", "medium"):
                errors.append(
                    f"E_ADJ_CONFIDENCE: {adj_path}.causal_confidence.expected_form: "
                    "a non-unique expected form cannot be high or not_applicable"
                )
            if (
                expected_status == "null_pending_operator_intent"
                and classification != "intent_confirmation_needed"
            ):
                errors.append(
                    f"E_ADJ_STATE: {adj_path}: pending operator intent requires intent_confirmation_needed"
                )
        else:
            if (
                rec["expected"]["token"] is not None
                or rec["expected"]["descriptor"] is not None
            ):
                errors.append(
                    f"E_ADJ_EXPECTED: {adj_path}: not_applicable cannot carry an expected value"
                )
            if confidence["expected_form"] != "not_applicable":
                errors.append(
                    f"E_ADJ_CONFIDENCE: {adj_path}.causal_confidence.expected_form: "
                    "not_applicable status requires not_applicable confidence"
                )
            if disposition == "bug_candidate":
                errors.append(
                    f"E_ADJ_STATE: {adj_path}: a bug candidate needs an expected-form adjudication"
                )

        if disposition == "bug_candidate":
            for axis in ("runtime_mechanism", "smartkey_attribution"):
                if confidence[axis] == "not_applicable":
                    errors.append(
                        f"E_ADJ_CONFIDENCE: {adj_path}.causal_confidence.{axis}: "
                        "bug candidates require an explicit confidence level"
                    )
        else:
            for axis in ("runtime_mechanism", "smartkey_attribution"):
                if confidence[axis] != "not_applicable":
                    errors.append(
                        f"E_ADJ_CONFIDENCE: {adj_path}.causal_confidence.{axis}: "
                        "guards and exclusions require not_applicable"
                    )

        if (
            classification == "source_authorship_exclusion"
            and adjudication["owner_lane"] != "outside_product_source_authorship"
        ):
            errors.append(
                f"E_ADJ_STATE: {adj_path}.owner_lane: source exclusion has a fixed owner lane"
            )
        if (
            classification == "language_quality_feature_candidate"
            and adjudication["owner_lane"] != "future_language_quality_layer"
        ):
            errors.append(
                f"E_ADJ_STATE: {adj_path}.owner_lane: language quality has a fixed owner lane"
            )

        privacy_class = adjudication["privacy_class"]
        privacy_classes.add(privacy_class)
        if privacy_class == "public_token":
            if (
                rec["privacy_classification"] != "public_token"
                or rec["observed"]["token"] is None
            ):
                errors.append(
                    f"E_ADJ_PRIVACY: {adj_path}.privacy_class: public_token requires an isolated public token"
                )
        elif privacy_class == "metadata_only":
            has_payload = any(
                rec[side]["token"] is not None for side in ("observed", "expected")
            )
            if has_payload or rec["privacy_classification"] != "unknown":
                errors.append(
                    f"E_ADJ_PRIVACY: {adj_path}.privacy_class: metadata_only cannot carry token payload"
                )
        else:
            if rec["privacy_classification"] != "redacted" or rec["observed"][
                "token"
            ] not in {
                "redacted",
                "[redacted]",
            }:
                errors.append(
                    f"E_ADJ_PRIVACY: {adj_path}.privacy_class: redacted requires the canonical placeholder"
                )

        source_keys: set[tuple[str, str]] = set()
        source_records: set[str] = set()
        for source in adjudication["source_refs"]:
            source_key = (source["kind"], source["ref"])
            if source_key in source_keys:
                errors.append(
                    f"E_ADJ_DUP: {adj_path}.source_refs: duplicate opaque source ref"
                )
            source_keys.add(source_key)
            if source["kind"] == "source_record":
                source_records.add(source["ref"])
        if not source_records:
            errors.append(
                f"E_ADJ_STATE: {adj_path}.source_refs: source_record ref is required"
            )
        if privacy_class != "metadata_only":
            fragments = _payload_fragments(rec)
            for source_ref in source_records:
                source_fragments.setdefault(source_ref, set()).update(fragments)

    if len(privacy_classes) > 1:
        errors.append(
            f"E_ADJ_PRIVACY: {path}: one dedup record cannot mix privacy classes"
        )
    has_bug = "bug_candidate" in dispositions
    if has_bug and rec["status"] == "not_smartkey":
        errors.append(
            f"E_ADJ_STATE: {path}: a bug-candidate record cannot be not_smartkey"
        )
    if not has_bug and rec["status"] != "not_smartkey":
        errors.append(
            f"E_ADJ_STATE: {path}: an all-guard/exclusion record must be not_smartkey"
        )
    if not has_bug and any(
        rec.get(field) is not None
        for field in ("red_test", "red_test_waiver", "fix_commit", "verification")
    ):
        errors.append(
            f"E_ADJ_STATE: {path}: guards/exclusions cannot carry fix lifecycle evidence"
        )


def _check_adjudication_contract(
    doc: dict,
    errors: list[str],
    seen_adjudications: dict[tuple[str, str], str],
    grain_refs: dict[str, set[tuple[str, str]]],
    disposition_refs: dict[str, set[tuple[str, str]]],
    source_fragments: dict[str, set[str]],
) -> None:
    contract = doc.get("adjudication_contract")
    if seen_adjudications and contract is None:
        errors.append("E_ADJ_COUNT: $: adjudications require adjudication_contract")
    if contract is not None and not seen_adjudications:
        errors.append(
            "E_ADJ_COUNT: $: adjudication_contract has no mapped adjudication IDs"
        )
    if contract is not None:
        for name, expected in ADJUDICATION_COUNT_CONTRACT.items():
            actual = (
                len(grain_refs[name])
                if name in grain_refs
                else len(disposition_refs[name])
            )
            if actual != expected:
                errors.append(
                    f"E_ADJ_COUNT: $: {name} has {actual} unique ID(s), expected {expected}"
                )
    for fragments in source_fragments.values():
        if len(fragments) > MAX_RECONSTRUCTION_FRAGMENTS_PER_SOURCE:
            errors.append(
                "E_PRIV_AGGREGATE: $.records: one opaque source reconstructs "
                f"{len(fragments)} distinct payload fragments; maximum is "
                f"{MAX_RECONSTRUCTION_FRAGMENTS_PER_SOURCE}"
            )


def _check_schema_drift(schema: dict, errors: list[str]) -> None:
    props = schema.get("$defs", {}).get("record", {}).get("properties", {})
    if tuple(props.get("status", {}).get("enum", ())) != STATUSES:
        errors.append("E_SCHEMA_DRIFT: schema status enum differs from validate.STATUSES")
    if tuple(props.get("privacy_classification", {}).get("enum", ())) != PRIVACY_CLASSES:
        errors.append("E_SCHEMA_DRIFT: schema privacy enum differs from validate.PRIVACY_CLASSES")
    definitions = schema.get("$defs", {})
    enum_contracts = (
        ("adjudication_classification", ADJUDICATION_CLASSIFICATIONS),
        ("adjudication_disposition", ADJUDICATION_DISPOSITIONS),
        ("expected_form_status", EXPECTED_FORM_STATUSES),
        ("causal_confidence", CAUSAL_CONFIDENCES),
        ("owner_lane", OWNER_LANES),
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


def validate_document(doc, schema: dict) -> list[str]:
    """Return every violation for an in-memory registry document."""
    errors: list[str] = []
    _check_schema_drift(schema, errors)
    SchemaChecker(schema).check(schema, doc, "$", errors)
    if errors:
        return errors  # semantic checks assume the structural contract holds
    seen_keys: dict[str, str] = {}
    seen_ids: dict[str, str] = {}
    seen_adjudications: dict[tuple[str, str], str] = {}
    grain_refs = {grain: set() for grain in ADJUDICATION_GRAINS}
    disposition_refs = {disposition: set() for disposition in ADJUDICATION_DISPOSITIONS}
    source_fragments: dict[str, set[str]] = {}
    for i, rec in enumerate(doc["records"]):
        path = f"$.records[{i}]"
        _check_identity(rec, path, errors)
        key, rid = rec["dedup_key"], rec["id"]
        if key in seen_keys:
            errors.append(f"E_DUP: {path}: dedup_key {key!r} already used by {seen_keys[key]}")
        seen_keys.setdefault(key, path)
        if rid in seen_ids:
            errors.append(f"E_DUP: {path}: id {rid!r} already used by {seen_ids[rid]}")
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
        errors.append("E_FORMAT: registry is not in canonical form (indent 2, ensure_ascii=False, trailing newline)")
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
    except (OSError, ValueError) as exc:
        print(f"E_READ: {exc}", file=sys.stderr)
        return 2
    for err in errors:
        print(err)
    count = len(doc.get("records", [])) if isinstance(doc, dict) else 0
    if errors:
        print(f"FAIL: {len(errors)} violation(s) in {count} record(s)")
        return 1
    print(f"OK: {count} record(s); by status: {json.dumps(status_counts(doc), sort_keys=True)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
