"""G0-R8 RED contracts for timestamp authority and source-record identity.

These fixtures are synthetic and intentionally contain no corpus values, real
record identifiers, private paths, or reconstructed-source material.  The
private generator owns the exact timestamp-override set.  Public code accepts
only the already-projected registry document and must not expose a caller- or
document-declared masking API.
"""

from __future__ import annotations

import copy
import hashlib
import hmac
import importlib.util
import json
from pathlib import Path

import pytest


_REPO = Path(__file__).resolve().parent.parent
_ANOMALIES = _REPO / "diagnostics" / "anomalies"
_SCHEMA = _ANOMALIES / "schema.json"
_VALIDATOR = _ANOMALIES / "validate.py"

_AUTHORITY_VERSION = "smartkey-g0-semantic-authority-projection-v2"
_SOURCE_RECORD_PROFILE = "smartkey-g0-source-record-semantic-v1"
_HMAC_CONTRACT_VERSION = "smartkey-g0-hmac-byte-contract-v3"
_OLD_HMAC_CONTRACT_VERSION = "smartkey-g0-hmac-byte-contract-v2"
_OLD_SOURCE_RECORD_PROFILE = "project_canonical_json_v1"
_OLD_VECTOR_SET_SHA256 = (
    "7a1f3543ff11c1060149b72f9abad4a0bb603a06463f9229edb775078f005ecb"
)
_SYNTHETIC_KEY_ID = "0123456789abcdef0123456789abcdef"
_SYNTHETIC_HMAC_KEY = b"smartkey-g0-r8-synthetic-vector-key"


def _load_validator():
    spec = importlib.util.spec_from_file_location(
        "smartkey_anomaly_timestamp_authority_red", _VALIDATOR
    )
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


V = _load_validator()


def _schema() -> dict:
    return json.loads(_SCHEMA.read_text(encoding="utf-8"))


def _codes(errors: list[str]) -> set[str]:
    return {error.split(":", 1)[0] for error in errors}


def _synthetic_record(
    *,
    label: str = "alpha",
    recorded_utc: str | None = "2026-08-26T12:00Z",
    first_seen_utc: str | None = "2026-08-26T11:00Z",
    last_seen_utc: str | None = "2026-08-26T11:30Z",
    occurrence_count: int = 1,
    repeat: bool | None = None,
) -> dict:
    record = {
        "id": "pending",
        "dedup_key": "pending",
        "category": "unknown",
        "observed": {
            "token": f"syntheticobserved{label}",
            "script": "latin",
            "descriptor": None,
        },
        "expected": {
            "token": f"syntheticexpected{label}",
            "script": "latin",
            "descriptor": None,
        },
        "needs_operator_confirmation": False,
        "minimal_context": {"before": None, "after": None},
        "recorded_utc": recorded_utc,
        "first_seen_utc": first_seen_utc,
        "last_seen_utc": last_seen_utc,
        "repeat": occurrence_count > 1 if repeat is None else repeat,
        "occurrence_count": occurrence_count,
        "environment": {
            "app_surface": "synthetic",
            "os": "synthetic",
            "build_sha": "unknown",
            "build_label": "synthetic",
            "flags": None,
        },
        "source_refs": [
            {
                "kind": "other",
                "ref": f"synthetic-source-{label}",
                "observed_utc": None,
                "build_sha": None,
                "surface": None,
                "note": None,
            }
        ],
        "status": "open_suspected_smartkey",
        "suspected_layer": "unknown",
        "reproducer": None,
        "red_test": None,
        "red_test_waiver": None,
        "fix_commit": None,
        "verification": None,
        "closure_reason": None,
        "privacy_classification": "public_token",
        "notes": None,
    }
    record["dedup_key"] = V.compute_dedup_key(record)
    record["id"] = V.compute_id(record)
    return record


def _synthetic_doc(*records: dict) -> dict:
    return {
        "schema_version": 1,
        "registry": "smartkey-anomaly-registry",
        "gate_ref": "abcdefabcdef",
        "records": [copy.deepcopy(record) for record in records],
    }


def _validate(*records: dict) -> list[str]:
    return V.validate_document(_synthetic_doc(*records), _schema())


def _projected_copy(record: dict) -> dict:
    """Model only the private generator's already-approved public output."""
    projected = copy.deepcopy(record)
    projected["recorded_utc"] = None
    projected["last_seen_utc"] = None
    return projected


def _baseline_set_input(record: dict) -> dict:
    item = copy.deepcopy(record)
    item["record_origin"] = "preexisting_public_baseline"
    item["baseline_record_sha256"] = V.baseline_record_sha256(item)
    return {"records": [item]}


def _private_bytes(record: dict) -> bytes:
    return V.canonical_structured_payload_bytes(record)


def _synthetic_merged_source_record() -> dict:
    """One whole synthetic merged source, including non-decisional metadata."""
    return {
        "merged_id": hashlib.sha256(b"synthetic-merged-source").hexdigest(),
        "source_schema_version": "synthetic-source-v1",
        "platform": "synthetic-platform",
        "authorship_confidence": "synthetic-high",
        "excluded_segments": [
            {
                "authorship_confidence": "synthetic-low",
                "start_char": 8,
                "end_char": 12,
                "reason": "synthetic-beta",
            },
            {
                "authorship_confidence": "synthetic-high",
                "start_char": 0,
                "end_char": 4,
                "reason": "synthetic-beta",
            },
            {
                "authorship_confidence": "synthetic-high",
                "start_char": 0,
                "end_char": 4,
                "reason": "synthetic-alpha",
            },
        ],
        "raw_sha256": hashlib.sha256(b"synthetic-merged-raw").hexdigest(),
        "raw_text": "synthetic excluded source text",
        "recorded_utc": "2026-08-26T12:00Z",
        "first_seen_utc": "2026-08-26T11:00Z",
        "last_seen_utc": "2026-08-26T11:30Z",
        "source_path_hash": hashlib.sha256(b"synthetic-path").hexdigest(),
        "source_session_hash": hashlib.sha256(b"synthetic-session").hexdigest(),
        "source_record_hash": hashlib.sha256(b"synthetic-record").hexdigest(),
        "seed_sha256": hashlib.sha256(b"synthetic-seed").hexdigest(),
        "file_sha256": hashlib.sha256(b"synthetic-file").hexdigest(),
        "reconstruction_receipt_sha256": hashlib.sha256(
            b"synthetic-reconstruction-receipt"
        ).hexdigest(),
        "ordinal": 7,
        "extra": {"synthetic": "excluded"},
        "final_adjudication": "synthetic-excluded-adjudication",
        "action": "synthetic-excluded-action",
        "register": "synthetic-excluded-register",
        "classification": "synthetic-excluded-classification",
        "unit": "synthetic-excluded-unit",
        "key_id": "synthetic-excluded-key-id",
        "mac": "synthetic-excluded-mac",
    }


def _envelope_builder():
    builder = getattr(V, "source_record_identity_envelope", None)
    assert callable(builder), (
        "G0-R8 RED: missing public source_record_identity_envelope contract"
    )
    return builder


def _assert_hmac_contract_rejected(envelope, marker: str | None = None) -> None:
    with pytest.raises(Exception) as caught:
        V.hmac_payload_bytes("source_record", envelope)
    error = caught.value
    assert isinstance(error, ValueError)
    assert error.args == ("E_HMAC_CONTRACT",)
    assert error.__cause__ is None
    assert error.__context__ is None
    if marker is not None:
        assert marker not in repr(error.args)
        assert marker not in repr(vars(error))


def _synthetic_hmac_contract() -> dict:
    receipt_sha256 = hashlib.sha256(b"synthetic-hmac-receipt").hexdigest()
    return {
        "hmac_scheme": V.HMAC_SCHEME,
        "hmac_min_key_bytes": V.HMAC_MIN_KEY_BYTES,
        "hmac_contract_version": V.HMAC_CONTRACT_VERSION,
        "hmac_domains": list(V.HMAC_DOMAINS),
        "hmac_domain_payload_profiles": copy.deepcopy(V.HMAC_DOMAIN_PAYLOAD_PROFILES),
        "hmac_domain_root_types": copy.deepcopy(V.HMAC_DOMAIN_ROOT_TYPES),
        "hmac_resource_contract_version": V.HMAC_RESOURCE_CONTRACT_VERSION,
        "hmac_resource_limits": copy.deepcopy(V.HMAC_RESOURCE_LIMITS),
        "hmac_resource_contract_sha256": V.HMAC_RESOURCE_CONTRACT_SHA256,
        "hmac_scalar_payload_encoding": V.HMAC_SCALAR_PAYLOAD_ENCODING,
        "hmac_structured_payload_encoding": V.HMAC_STRUCTURED_PAYLOAD_ENCODING,
        "hmac_input_frame": V.HMAC_INPUT_FRAME,
        "hmac_vector_set_sha256": V.HMAC_VECTOR_SET_SHA256,
        "hmac_key_id": _SYNTHETIC_KEY_ID,
        "hmac_external_receipt": {
            "state": V.HMAC_RECEIPT_STATE,
            "scheme": V.HMAC_SCHEME,
            "key_id": _SYNTHETIC_KEY_ID,
            "contract_version": V.HMAC_CONTRACT_VERSION,
            "resource_contract_version": V.HMAC_RESOURCE_CONTRACT_VERSION,
            "resource_contract_sha256": V.HMAC_RESOURCE_CONTRACT_SHA256,
            "vector_set_sha256": V.HMAC_VECTOR_SET_SHA256,
            "coverage": V.HMAC_RECEIPT_COVERAGE,
            "receipt_sha256": receipt_sha256,
        },
    }


def _hmac_preflight_errors(contract: dict) -> list[str]:
    errors: list[str] = []
    V._check_hmac_contract_preflight(
        {"records": []},
        contract,
        [("$.synthetic", {})],
        errors,
    )
    return errors


# ---------------------------------------------------------------- schema/time


def test_g0_r8_recorded_utc_key_accepts_explicit_json_null():
    assert _validate(_synthetic_record(recorded_utc=None)) == []


def test_g0_r8_last_seen_utc_key_accepts_explicit_json_null():
    assert _validate(_synthetic_record(last_seen_utc=None)) == []


def test_g0_r8_external_draft202012_accepts_required_timestamp_nulls():
    jsonschema = pytest.importorskip("jsonschema")
    document = _synthetic_doc(_synthetic_record(recorded_utc=None, last_seen_utc=None))
    errors = list(jsonschema.Draft202012Validator(_schema()).iter_errors(document))
    assert errors == []


@pytest.mark.parametrize("field", ["recorded_utc", "last_seen_utc"])
def test_g0_r8_timestamp_keys_remain_required_when_value_may_be_null(field):
    record = _synthetic_record()
    record.pop(field)
    errors = _validate(record)
    assert _codes(errors) == {"E_SCHEMA"}
    assert any(f"missing required key '{field}'" in error for error in errors)


@pytest.mark.parametrize("field", ["recorded_utc", "last_seen_utc"])
def test_g0_r8_textual_unknown_is_not_a_timestamp_null_sentinel(field):
    record = _synthetic_record()
    record[field] = "unknown"
    errors = _validate(record)
    assert _codes(errors) == {"E_SCHEMA"}
    assert "unknown" not in "\n".join(errors)


def test_g0_r8_null_recorded_utc_with_known_ordered_pair_does_not_crash():
    record = _synthetic_record(
        recorded_utc=None,
        first_seen_utc="2026-08-26T10:00Z",
        last_seen_utc="2026-08-26T11:00Z",
    )
    assert _validate(record) == []


def test_g0_r8_null_recorded_utc_still_compares_known_first_last_pair():
    record = _synthetic_record(
        recorded_utc=None,
        first_seen_utc="2026-08-26T11:00Z",
        last_seen_utc="2026-08-26T10:00Z",
    )
    assert _codes(_validate(record)) == {"E_TIME"}


@pytest.mark.parametrize(
    ("first_seen_utc", "last_seen_utc"),
    [
        ("2026-08-26T13:00Z", None),
        (None, "2026-08-26T13:00Z"),
    ],
)
def test_g0_r8_each_known_seen_timestamp_is_compared_to_known_recorded(
    first_seen_utc,
    last_seen_utc,
):
    record = _synthetic_record(
        recorded_utc="2026-08-26T12:00Z",
        first_seen_utc=first_seen_utc,
        last_seen_utc=last_seen_utc,
    )
    assert _codes(_validate(record)) == {"E_TIME"}


def test_g0_r8_fully_known_inverted_first_last_pair_remains_e_time():
    record = _synthetic_record(
        recorded_utc="2026-08-26T12:00Z",
        first_seen_utc="2026-08-26T11:00Z",
        last_seen_utc="2026-08-26T10:00Z",
    )
    assert _codes(_validate(record)) == {"E_TIME"}


# -------------------------------------------------------- public/private seam


@pytest.mark.parametrize(
    ("scope", "field", "value"),
    [
        ("record", "timestamp_authority_mask", {"recorded_utc": "null"}),
        ("record", "inferred_timestamp_provenance", {"kind": "synthetic"}),
        ("record", "reconstructed_file_sha256", "f" * 64),
        ("document", "timestamp_authority_overrides", []),
        ("document", "inferred_value_provenance", {"kind": "synthetic"}),
        ("document", "source_bytes_sha256", "e" * 64),
    ],
)
def test_g0_r8_public_schema_rejects_private_authority_and_receipt_fields(
    scope,
    field,
    value,
):
    marker = "synthetic-nonreflective-marker"
    if isinstance(value, dict):
        value = {**value, "marker": marker}
    document = _synthetic_doc(_synthetic_record())
    target = document["records"][0] if scope == "record" else document
    target[field] = value

    errors = V.validate_document(document, _schema())

    assert _codes(errors) == {"E_SCHEMA"}
    assert marker not in "\n".join(errors)


def test_g0_r8_public_schema_has_no_receipt_or_authority_mask_properties():
    schema = _schema()
    record_fields = set(schema["$defs"]["record"]["properties"])
    document_fields = set(schema["properties"])
    forbidden = {
        "timestamp_authority_mask",
        "timestamp_authority_overrides",
        "inferred_timestamp_provenance",
        "inferred_value_provenance",
        "reconstructed_file_sha256",
        "source_bytes_sha256",
    }
    assert record_fields.isdisjoint(forbidden)
    assert document_fields.isdisjoint(forbidden)


# ---------------------------------------- already-projected public semantics


def test_g0_r8_masked_timestamp_variants_share_every_public_commitment():
    first = _synthetic_record(
        recorded_utc="2026-08-26T12:00Z",
        last_seen_utc="2026-08-26T11:30Z",
    )
    second = _synthetic_record(
        recorded_utc="2026-08-26T12:05Z",
        last_seen_utc="2026-08-26T11:35Z",
    )
    first_private = _private_bytes(first)
    second_private = _private_bytes(second)
    first_receipt = hashlib.sha256(first_private).hexdigest()
    second_receipt = hashlib.sha256(second_private).hexdigest()
    assert first_private != second_private
    assert first_receipt != second_receipt

    first_public = _projected_copy(first)
    second_public = _projected_copy(second)
    first_public_bytes = V.canonical_structured_payload_bytes(first_public)
    second_public_bytes = V.canonical_structured_payload_bytes(second_public)
    assert first_public["recorded_utc"] is None
    assert first_public["last_seen_utc"] is None
    assert first_public_bytes == second_public_bytes
    assert first_receipt.encode("ascii") not in first_public_bytes
    assert second_receipt.encode("ascii") not in second_public_bytes

    assert V.baseline_record_sha256(first_public) == V.baseline_record_sha256(
        second_public
    )
    assert V.baseline_record_set_sha256(
        _baseline_set_input(first_public)
    ) == V.baseline_record_set_sha256(_baseline_set_input(second_public))
    assert V.legacy_projection_sha256(
        _synthetic_doc(first_public)
    ) == V.legacy_projection_sha256(_synthetic_doc(second_public))
    assert V.registry_projection_sha256(
        _synthetic_doc(first_public)
    ) == V.registry_projection_sha256(_synthetic_doc(second_public))
    assert _validate(first_public) == []
    assert _validate(second_public) == []


def test_g0_r8_null_to_known_timestamp_remains_publicly_decisional():
    known = _synthetic_record()
    projected_null = copy.deepcopy(known)
    projected_null["recorded_utc"] = None

    assert known["id"] == projected_null["id"]
    assert known["dedup_key"] == projected_null["dedup_key"]
    assert V.baseline_record_sha256(known) != V.baseline_record_sha256(projected_null)
    assert V.baseline_record_set_sha256(
        _baseline_set_input(known)
    ) != V.baseline_record_set_sha256(_baseline_set_input(projected_null))
    assert V.legacy_projection_sha256(
        _synthetic_doc(known)
    ) != V.legacy_projection_sha256(_synthetic_doc(projected_null))
    assert V.registry_projection_sha256(
        _synthetic_doc(known)
    ) != V.registry_projection_sha256(_synthetic_doc(projected_null))
    assert _validate(known) == []
    assert _validate(projected_null) == []


def test_g0_r8_unmasked_authoritative_identity_change_remains_decisional():
    original = _projected_copy(_synthetic_record())
    changed = copy.deepcopy(original)
    changed["expected"]["token"] = "syntheticexpectedomega"
    changed["dedup_key"] = V.compute_dedup_key(changed)
    changed["id"] = V.compute_id(changed)

    assert changed["id"] != original["id"]
    assert changed["dedup_key"] != original["dedup_key"]
    assert V.canonical_structured_payload_bytes(changed) != (
        V.canonical_structured_payload_bytes(original)
    )
    assert V.baseline_record_sha256(changed) != V.baseline_record_sha256(original)
    assert V.legacy_projection_sha256(
        _synthetic_doc(changed)
    ) != V.legacy_projection_sha256(_synthetic_doc(original))
    assert V.registry_projection_sha256(
        _synthetic_doc(changed)
    ) != V.registry_projection_sha256(_synthetic_doc(original))


def test_g0_r8_timestamp_null_does_not_change_identity_or_dedup():
    known = _synthetic_record()
    projected = _projected_copy(known)
    assert V.compute_id(projected) == known["id"]
    assert V.compute_dedup_key(projected) == known["dedup_key"]


def test_g0_r8_timestamp_null_preserves_valid_repeat_occurrence_pair():
    record = _synthetic_record(
        recorded_utc=None,
        last_seen_utc=None,
        occurrence_count=2,
        repeat=True,
    )
    assert _validate(record) == []


def test_g0_r8_timestamp_null_cannot_hide_repeat_occurrence_mismatch():
    record = _synthetic_record(
        recorded_utc=None,
        last_seen_utc=None,
        occurrence_count=2,
        repeat=False,
    )
    assert _codes(_validate(record)) == {"E_REPEAT"}


def test_g0_r8_timestamp_null_cannot_hide_duplicate_identity():
    first = _projected_copy(_synthetic_record())
    second = copy.deepcopy(first)
    assert _codes(_validate(first, second)) == {"E_DUP"}


# ---------------------------------------------------------- public HMAC seam


def test_g0_r8_authority_and_source_record_profile_versions_are_pinned():
    assert getattr(V, "SEMANTIC_AUTHORITY_PROJECTION_VERSION", None) == (
        _AUTHORITY_VERSION
    )
    assert getattr(V, "SOURCE_RECORD_HMAC_PROFILE", None) == _SOURCE_RECORD_PROFILE
    assert V.HMAC_DOMAIN_PAYLOAD_PROFILES == {
        "value": "scalar_utf8",
        "event": "project_canonical_json_v1",
        "metadata": "project_canonical_json_v1",
        "source_record": _SOURCE_RECORD_PROFILE,
    }


def test_g0_r8_hmac_contract_version_is_bumped_and_pinned_everywhere():
    schema = _schema()
    contract = schema["$defs"]["adjudication_contract"]["properties"]
    receipt = schema["$defs"]["hmac_external_receipt"]["properties"]
    assert V.HMAC_CONTRACT_VERSION == _HMAC_CONTRACT_VERSION
    assert V.SEALED_CONTRACT_CONSTS["hmac_contract_version"] == (_HMAC_CONTRACT_VERSION)
    assert contract["hmac_contract_version"]["const"] == _HMAC_CONTRACT_VERSION
    assert receipt["contract_version"]["const"] == _HMAC_CONTRACT_VERSION
    assert contract["hmac_domain_payload_profiles"]["const"]["source_record"] == (
        _SOURCE_RECORD_PROFILE
    )


def test_g0_r8_source_record_profile_bump_requires_a_new_vector_set():
    assert V.HMAC_VECTOR_SET_SHA256 != _OLD_VECTOR_SET_SHA256


@pytest.mark.parametrize(
    ("field", "replacement"),
    [
        ("hmac_contract_version", _OLD_HMAC_CONTRACT_VERSION),
        ("hmac_domain_payload_profiles", _OLD_SOURCE_RECORD_PROFILE),
    ],
)
def test_g0_r8_old_contract_and_generic_source_profile_fail_closed(
    field,
    replacement,
):
    contract = _synthetic_hmac_contract()
    if field == "hmac_domain_payload_profiles":
        contract[field]["source_record"] = replacement
    else:
        contract[field] = replacement
        contract["hmac_external_receipt"]["contract_version"] = replacement

    errors = _hmac_preflight_errors(contract)

    assert _codes(errors) == {"E_HMAC_CONTRACT"}
    assert replacement not in "\n".join(errors)


def test_g0_r8_wrong_source_profile_error_is_stable_and_nonreflective():
    marker = "synthetic-reflective-profile-marker"
    contract = _synthetic_hmac_contract()
    contract["hmac_domain_payload_profiles"]["source_record"] = marker
    errors = _hmac_preflight_errors(contract)
    assert _codes(errors) == {"E_HMAC_CONTRACT"}
    assert marker not in "\n".join(errors)


def test_g0_r8_source_record_identity_envelope_has_exact_closed_shape():
    source = _synthetic_merged_source_record()
    envelope = _envelope_builder()(source)
    assert envelope == {
        "profile": _SOURCE_RECORD_PROFILE,
        "merged_id": source["merged_id"],
        "source_schema_version": source["source_schema_version"],
        "platform": source["platform"],
        "authorship_confidence": source["authorship_confidence"],
        "excluded_segments": [
            {
                "authorship_confidence": "synthetic-high",
                "start_char": 0,
                "end_char": 4,
                "reason": "synthetic-alpha",
            },
            {
                "authorship_confidence": "synthetic-high",
                "start_char": 0,
                "end_char": 4,
                "reason": "synthetic-beta",
            },
            {
                "authorship_confidence": "synthetic-low",
                "start_char": 8,
                "end_char": 12,
                "reason": "synthetic-beta",
            },
        ],
        "raw_sha256": source["raw_sha256"],
    }

    payload = V.hmac_payload_bytes("source_record", envelope)
    frame = V.hmac_frame_bytes("source_record", envelope)
    prefix = V.HMAC_SCHEME.encode("ascii") + b"\0source_record\0"
    assert payload == V.canonical_structured_payload_bytes(envelope)
    assert frame == prefix + len(payload).to_bytes(8, "big") + payload

    encoded = V.canonical_structured_payload_bytes(envelope)
    for forbidden in (
        b"raw_text",
        b"recorded_utc",
        b"first_seen_utc",
        b"last_seen_utc",
        b"source_path_hash",
        b"source_session_hash",
        b"source_record_hash",
        b"seed_sha256",
        b"file_sha256",
        b"reconstruction_receipt_sha256",
        b"ordinal",
        b"extra",
        b"final_adjudication",
        b"action",
        b"register",
        b"classification",
        b"unit",
        b"key_id",
        b"mac",
    ):
        assert forbidden not in encoded


@pytest.mark.parametrize(
    ("field", "replacement"),
    [
        ("raw_text", "synthetic changed excluded text"),
        ("recorded_utc", "2026-08-26T12:05Z"),
        ("first_seen_utc", "2026-08-26T11:05Z"),
        ("last_seen_utc", "2026-08-26T11:35Z"),
        ("source_path_hash", "a" * 64),
        ("source_session_hash", "b" * 64),
        ("source_record_hash", "c" * 64),
        ("seed_sha256", "d" * 64),
        ("file_sha256", "e" * 64),
        ("reconstruction_receipt_sha256", "f" * 64),
        ("ordinal", 8),
        ("extra", {"synthetic": "changed"}),
        ("final_adjudication", "synthetic changed excluded adjudication"),
        ("action", "synthetic changed excluded action"),
        ("register", "synthetic changed excluded register"),
        ("classification", "synthetic changed excluded classification"),
        ("unit", "synthetic changed excluded unit"),
        ("key_id", "synthetic changed excluded key id"),
        ("mac", "synthetic changed excluded mac"),
    ],
)
def test_g0_r8_excluded_source_metadata_is_nondecisional(field, replacement):
    builder = _envelope_builder()
    first = _synthetic_merged_source_record()
    second = copy.deepcopy(first)
    second[field] = replacement
    assert _private_bytes(first) != _private_bytes(second)

    first_envelope = builder(first)
    second_envelope = builder(second)
    first_payload = V.hmac_payload_bytes("source_record", first_envelope)
    second_payload = V.hmac_payload_bytes("source_record", second_envelope)
    first_frame = V.hmac_frame_bytes("source_record", first_envelope)
    second_frame = V.hmac_frame_bytes("source_record", second_envelope)

    assert first_envelope == second_envelope
    assert first_payload == second_payload
    assert first_frame == second_frame
    assert hmac.new(_SYNTHETIC_HMAC_KEY, first_frame, hashlib.sha256).digest() == (
        hmac.new(_SYNTHETIC_HMAC_KEY, second_frame, hashlib.sha256).digest()
    )


@pytest.mark.parametrize(
    "field",
    [
        "merged_id",
        "source_schema_version",
        "platform",
        "authorship_confidence",
        "excluded_segments",
        "raw_sha256",
    ],
)
def test_g0_r8_each_included_source_field_is_decisional(field):
    builder = _envelope_builder()
    first = _synthetic_merged_source_record()
    second = copy.deepcopy(first)
    replacements = {
        "merged_id": hashlib.sha256(b"synthetic-other-merged-source").hexdigest(),
        "source_schema_version": "synthetic-source-v2",
        "platform": "synthetic-other-platform",
        "authorship_confidence": "synthetic-medium",
        "excluded_segments": [
            {
                "authorship_confidence": "synthetic-low",
                "start_char": 1,
                "end_char": 3,
                "reason": "synthetic-changed",
            }
        ],
        "raw_sha256": hashlib.sha256(b"synthetic-other-raw").hexdigest(),
    }
    second[field] = replacements[field]

    first_envelope = builder(first)
    second_envelope = builder(second)
    first_payload = V.hmac_payload_bytes("source_record", first_envelope)
    second_payload = V.hmac_payload_bytes("source_record", second_envelope)
    first_frame = V.hmac_frame_bytes("source_record", first_envelope)
    second_frame = V.hmac_frame_bytes("source_record", second_envelope)

    assert first_envelope != second_envelope
    assert first_payload != second_payload
    assert first_frame != second_frame
    assert hmac.new(_SYNTHETIC_HMAC_KEY, first_frame, hashlib.sha256).digest() != (
        hmac.new(_SYNTHETIC_HMAC_KEY, second_frame, hashlib.sha256).digest()
    )


@pytest.mark.parametrize(
    "field",
    [
        "profile",
        "merged_id",
        "source_schema_version",
        "platform",
        "authorship_confidence",
        "excluded_segments",
        "raw_sha256",
    ],
)
def test_g0_r8_source_record_envelope_rejects_each_missing_root_key(field):
    envelope = _envelope_builder()(_synthetic_merged_source_record())
    envelope.pop(field)
    _assert_hmac_contract_rejected(envelope)


def test_g0_r8_source_record_envelope_rejects_extra_root_key_nonreflectively():
    marker = "synthetic-extra-root-marker"
    envelope = _envelope_builder()(_synthetic_merged_source_record())
    envelope["unexpected"] = marker
    _assert_hmac_contract_rejected(envelope, marker)


@pytest.mark.parametrize(
    ("field", "replacement"),
    [
        ("profile", _OLD_SOURCE_RECORD_PROFILE),
        ("profile", 1),
        ("merged_id", "f" * 63),
        ("merged_id", "g" * 64),
        ("merged_id", 1),
        ("source_schema_version", 1),
        ("platform", []),
        ("authorship_confidence", None),
        ("excluded_segments", {}),
        ("excluded_segments", [None]),
        ("raw_sha256", "e" * 65),
        ("raw_sha256", "g" * 64),
        ("raw_sha256", 1),
    ],
)
def test_g0_r8_source_record_envelope_rejects_wrong_root_contract(
    field,
    replacement,
):
    envelope = _envelope_builder()(_synthetic_merged_source_record())
    envelope[field] = replacement
    _assert_hmac_contract_rejected(envelope)


def test_g0_r8_source_record_envelope_rejects_noncanonical_segment_order():
    envelope = _envelope_builder()(_synthetic_merged_source_record())
    envelope["excluded_segments"] = list(reversed(envelope["excluded_segments"]))
    _assert_hmac_contract_rejected(envelope)


@pytest.mark.parametrize(
    "field",
    ["authorship_confidence", "start_char", "end_char", "reason"],
)
def test_g0_r8_source_record_envelope_rejects_missing_segment_key(field):
    envelope = _envelope_builder()(_synthetic_merged_source_record())
    envelope["excluded_segments"][0].pop(field)
    _assert_hmac_contract_rejected(envelope)


def test_g0_r8_source_record_envelope_rejects_extra_segment_key_nonreflectively():
    marker = "synthetic-extra-segment-marker"
    envelope = _envelope_builder()(_synthetic_merged_source_record())
    envelope["excluded_segments"][0]["unexpected"] = marker
    _assert_hmac_contract_rejected(envelope, marker)


@pytest.mark.parametrize(
    ("field", "replacement"),
    [
        ("authorship_confidence", 1),
        ("start_char", -1),
        ("start_char", "0"),
        ("start_char", True),
        ("end_char", 0),
        ("end_char", "4"),
        ("end_char", True),
        ("reason", []),
    ],
)
def test_g0_r8_source_record_envelope_rejects_invalid_segment_contract(
    field,
    replacement,
):
    envelope = _envelope_builder()(_synthetic_merged_source_record())
    envelope["excluded_segments"][0][field] = replacement
    _assert_hmac_contract_rejected(envelope)
