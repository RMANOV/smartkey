"""G0-R9 RED contracts for timestamp authority and source-record identity.

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
import tracemalloc
from pathlib import Path

import pytest
from jsonschema import Draft202012Validator


_REPO = Path(__file__).resolve().parent.parent
_ANOMALIES = _REPO / "diagnostics" / "anomalies"
_SCHEMA = _ANOMALIES / "schema.json"
_VALIDATOR = _ANOMALIES / "validate.py"

_AUTHORITY_VERSION = "smartkey-g0-semantic-authority-projection-v2"
_SOURCE_RECORD_PROFILE = "smartkey-g0-source-record-semantic-v1"
_CURRENT_SOURCE_RECORD_PROFILE = (
    "smartkey-g0-source-record-adjudication-scoped-semantic-v2"
)
_HMAC_CONTRACT_VERSION = "smartkey-g0-hmac-byte-contract-v4"
_OLD_HMAC_CONTRACT_VERSION = "smartkey-g0-hmac-byte-contract-v3"
_OLD_SOURCE_RECORD_PROFILE = _SOURCE_RECORD_PROFILE
_OLD_VECTOR_SET_SHA256 = (
    "7a1f3543ff11c1060149b72f9abad4a0bb603a06463f9229edb775078f005ecb"
)
_PRE_R9_VECTOR_SET_SHA256 = (
    "0672508a1525bb5d606a79b30940dfe9ff8dca032532cd29f15d110ed5185d0f"
)
_V3_VECTOR_SET_SHA256 = (
    "2fa017fb46cfc08512ca0f9f6c8b4dafc56b3b28dab0bc8ab2fa9b45ae3c7192"
)
_V4_VECTOR_SET_SHA256 = (
    "8f6644ed05a898bd650f4e42dca4bc278e19db601523ed5442fcc20dd5ab0b3c"
)
_SYNTHETIC_KEY_ID = "0123456789abcdef0123456789abcdef"
_PUBLIC_VECTOR_KEY = b"smartkey-g0-public-synthetic-vector-key-v1"
_SAFE_INTEGER_MAX = 9_007_199_254_740_991
_SOURCE_VECTOR_ENVELOPE = {
    "profile": _SOURCE_RECORD_PROFILE,
    "merged_id": ("c0045ec8791a7f514f4addc5982ecfbdd9e28ab80bdaea91f5e6fbc6dfbf395e"),
    "source_schema_version": "synthetic-source-v1",
    "platform": "synthetic-platform",
    "authorship_confidence": "synthetic-confidence",
    "excluded_segments": [
        {
            "authorship_confidence": "synthetic-alpha",
            "start_char": 0,
            "end_char": 4,
            "reason": "synthetic-alpha",
        },
        {
            "authorship_confidence": "synthetic-alpha",
            "start_char": 0,
            "end_char": 4,
            "reason": "synthetic-zulu",
        },
        {
            "authorship_confidence": "synthetic-beta",
            "start_char": 0,
            "end_char": 4,
            "reason": "synthetic-alpha",
        },
        {
            "authorship_confidence": "synthetic-zulu",
            "start_char": 0,
            "end_char": 4,
            "reason": "synthetic-zulu",
        },
        {
            "authorship_confidence": "synthetic-alpha",
            "start_char": 0,
            "end_char": 6,
            "reason": "synthetic-alpha",
        },
        {
            "authorship_confidence": "synthetic-aaa",
            "start_char": 8,
            "end_char": 12,
            "reason": "synthetic-aaa",
        },
        {
            "authorship_confidence": "\ue000",
            "start_char": 16,
            "end_char": 20,
            "reason": "synthetic-zulu",
        },
        {
            "authorship_confidence": "\U00010000",
            "start_char": 16,
            "end_char": 20,
            "reason": "synthetic-alpha",
        },
    ],
    "raw_sha256": ("56d890f43577f16f03358ea0c94bb7bd7e1e6864b908585e24be54ae2734cd56"),
}
_SOURCE_VECTOR_PAYLOAD_HEX = (
    "7b22617574686f72736869705f636f6e666964656e6365223a2273796e746865"
    "7469632d636f6e666964656e6365222c226578636c756465645f7365676d656e"
    "7473223a5b7b22617574686f72736869705f636f6e666964656e6365223a2273"
    "796e7468657469632d616c706861222c22656e645f63686172223a342c227265"
    "61736f6e223a2273796e7468657469632d616c706861222c2273746172745f63"
    "686172223a307d2c7b22617574686f72736869705f636f6e666964656e636522"
    "3a2273796e7468657469632d616c706861222c22656e645f63686172223a342c"
    "22726561736f6e223a2273796e7468657469632d7a756c75222c227374617274"
    "5f63686172223a307d2c7b22617574686f72736869705f636f6e666964656e63"
    "65223a2273796e7468657469632d62657461222c22656e645f63686172223a34"
    "2c22726561736f6e223a2273796e7468657469632d616c706861222c22737461"
    "72745f63686172223a307d2c7b22617574686f72736869705f636f6e66696465"
    "6e6365223a2273796e7468657469632d7a756c75222c22656e645f6368617222"
    "3a342c22726561736f6e223a2273796e7468657469632d7a756c75222c227374"
    "6172745f63686172223a307d2c7b22617574686f72736869705f636f6e666964"
    "656e6365223a2273796e7468657469632d616c706861222c22656e645f636861"
    "72223a362c22726561736f6e223a2273796e7468657469632d616c706861222c"
    "2273746172745f63686172223a307d2c7b22617574686f72736869705f636f6e"
    "666964656e6365223a2273796e7468657469632d616161222c22656e645f6368"
    "6172223a31322c22726561736f6e223a2273796e7468657469632d616161222c"
    "2273746172745f63686172223a387d2c7b22617574686f72736869705f636f6e"
    "666964656e6365223a22ee8080222c22656e645f63686172223a32302c227265"
    "61736f6e223a2273796e7468657469632d7a756c75222c2273746172745f6368"
    "6172223a31367d2c7b22617574686f72736869705f636f6e666964656e636522"
    "3a22f0908080222c22656e645f63686172223a32302c22726561736f6e223a22"
    "73796e7468657469632d616c706861222c2273746172745f63686172223a3136"
    "7d5d2c226d65726765645f6964223a226330"
    "3034356563383739316137663531346634616464633539383265636662646439"
    "653238616238306264616561393166356536666263366466626633393565222c"
    "22706c6174666f726d223a2273796e7468657469632d706c6174666f726d222c"
    "2270726f66696c65223a22736d6172746b65792d67302d736f757263652d7265"
    "636f72642d73656d616e7469632d7631222c227261775f736861323536223a22"
    "3536643839306634333537376631366630333335386561306339346262376264"
    "3765316536383634623930383538356532346265353461653237333463643536"
    "222c22736f757263655f736368656d615f76657273696f6e223a2273796e7468"
    "657469632d736f757263652d7631227d"
)
_SOURCE_VECTOR_PAYLOAD_BYTES = bytes.fromhex(_SOURCE_VECTOR_PAYLOAD_HEX)
_SOURCE_VECTOR_FRAME_HEX = (
    "736d6172746b65792d67302d686d61632d7368613235362d763100736f757263"
    "655f7265636f7264000000000000000462" + _SOURCE_VECTOR_PAYLOAD_HEX
)
_SOURCE_VECTOR_FRAME_BYTES = bytes.fromhex(_SOURCE_VECTOR_FRAME_HEX)
_SOURCE_VECTOR_MAC_SHA256 = (
    "dec2460826116e6f9e907387f617f9d26371b024202b4f8a064ed3a22b6869e4"
)
_V3_VECTOR_RECEIPTS = [
    {
        "name": "value_non_ascii",
        "domain": "value",
        "profile": "scalar_utf8",
        "key_id": _SYNTHETIC_KEY_ID,
        "key_hex": _PUBLIC_VECTOR_KEY.hex(),
        "payload_hex": "636166c3a9",
        "frame_hex": (
            "736d6172746b65792d67302d686d61632d7368613235362d76310076616c7565"
            "000000000000000005636166c3a9"
        ),
        "mac_sha256": (
            "134f37d80a6cf037adb96d129c300af20c98ecd25c325fa99bb0c43791a5fda1"
        ),
    },
    {
        "name": "event_nested",
        "domain": "event",
        "profile": "project_canonical_json_v1",
        "key_id": _SYNTHETIC_KEY_ID,
        "key_hex": _PUBLIC_VECTOR_KEY.hex(),
        "payload_hex": (
            "7b22616374697665223a747275652c22636f756e74223a322c226576656e7422"
            "3a2273796e746865746963222c227061727473223a5b22616c706861222c6e75"
            "6c6c2c7b226f6b223a66616c73657d5d2c22756e69745f736570617261746f"
            "72223a225c7530303166222c22ee8080223a22626d70222c22f0908080223a22"
            "61737472616c227d"
        ),
        "frame_hex": (
            "736d6172746b65792d67302d686d61632d7368613235362d7631006576656e74"
            "0000000000000000877b22616374697665223a747275652c22636f756e74223a"
            "322c226576656e74223a2273796e746865746963222c227061727473223a5b22"
            "616c706861222c6e756c6c2c7b226f6b223a66616c73657d5d2c22756e6974"
            "5f736570617261746f72223a225c7530303166222c22ee8080223a22626d7022"
            "2c22f0908080223a2261737472616c227d"
        ),
        "mac_sha256": (
            "800c5952070ce32356a99537c95d2eeec515a854a7356379623d5ae4001ae94d"
        ),
    },
    {
        "name": "metadata_escaping",
        "domain": "metadata",
        "profile": "project_canonical_json_v1",
        "key_id": _SYNTHETIC_KEY_ID,
        "key_hex": _PUBLIC_VECTOR_KEY.hex(),
        "payload_hex": (
            "7b22636f6e74726f6c223a225c625c745c6e5c665c725c7530303030222c226c"
            "6162656c223a22636166c3a9222c2271756f7465223a225c225c5c2f227d"
        ),
        "frame_hex": (
            "736d6172746b65792d67302d686d61632d7368613235362d7631006d65746164"
            "61746100000000000000003e7b22636f6e74726f6c223a225c625c745c6e5c66"
            "5c725c7530303030222c226c6162656c223a22636166c3a9222c2271756f7465"
            "223a225c225c5c2f227d"
        ),
        "mac_sha256": (
            "3c190f2b02d21c5dec8e31ce21c0bd45abdc0187f157f9329d351d5295f667b2"
        ),
    },
    {
        "name": "source_record_whole_source_semantic_v1",
        "domain": "source_record",
        "profile": _SOURCE_RECORD_PROFILE,
        "key_id": _SYNTHETIC_KEY_ID,
        "key_hex": _PUBLIC_VECTOR_KEY.hex(),
        "payload_hex": _SOURCE_VECTOR_PAYLOAD_HEX,
        "frame_hex": _SOURCE_VECTOR_FRAME_HEX,
        "mac_sha256": _SOURCE_VECTOR_MAC_SHA256,
    },
]


def _reference_json_string_bytes(value: str) -> bytes:
    assert type(value) is str
    return json.dumps(
        value,
        ensure_ascii=False,
        separators=(",", ":"),
    ).encode("utf-8")


def _reference_canonical_json_bytes(value) -> bytes:
    """Independent stdlib-only oracle; never calls the production encoder."""
    if value is None:
        return b"null"
    if value is True:
        return b"true"
    if value is False:
        return b"false"
    if type(value) is int:
        assert -_SAFE_INTEGER_MAX <= value <= _SAFE_INTEGER_MAX
        return str(value).encode("ascii")
    if type(value) is str:
        return _reference_json_string_bytes(value)
    if type(value) is list:
        return (
            b"["
            + b",".join(_reference_canonical_json_bytes(item) for item in value)
            + b"]"
        )
    if type(value) is dict:
        assert all(type(key) is str for key in value)
        keys = sorted(value, key=lambda key: tuple(ord(char) for char in key))
        members = (
            _reference_json_string_bytes(key)
            + b":"
            + _reference_canonical_json_bytes(value[key])
            for key in keys
        )
        return b"{" + b",".join(members) + b"}"
    raise AssertionError(f"unsupported reference type: {type(value).__name__}")


def _reference_hmac_frame(domain: str, payload: bytes) -> bytes:
    assert domain in {"value", "event", "metadata", "source_record"}
    return (
        b"smartkey-g0-hmac-sha256-v1"
        + b"\0"
        + domain.encode("ascii")
        + b"\0"
        + len(payload).to_bytes(8, "big", signed=False)
        + payload
    )


def _segment_order_key(segment: dict) -> tuple:
    return (
        segment["start_char"],
        segment["end_char"],
        segment["authorship_confidence"].encode("utf-8"),
        segment["reason"].encode("utf-8"),
    )


def _segment_scalar_order_key(segment: dict) -> tuple:
    return (
        segment["start_char"],
        segment["end_char"],
        tuple(ord(char) for char in segment["authorship_confidence"]),
        tuple(ord(char) for char in segment["reason"]),
    )


def _segment_utf16_order_key(segment: dict) -> tuple:
    return (
        segment["start_char"],
        segment["end_char"],
        segment["authorship_confidence"].encode("utf-16-be"),
        segment["reason"].encode("utf-16-be"),
    )


def _segment_reason_first_order_key(segment: dict) -> tuple:
    return (
        segment["start_char"],
        segment["end_char"],
        segment["reason"].encode("utf-8"),
        segment["authorship_confidence"].encode("utf-8"),
    )


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
    segments = _SOURCE_VECTOR_ENVELOPE["excluded_segments"]
    return {
        "merged_id": _SOURCE_VECTOR_ENVELOPE["merged_id"],
        "source_schema_version": _SOURCE_VECTOR_ENVELOPE["source_schema_version"],
        "platform": _SOURCE_VECTOR_ENVELOPE["platform"],
        "authorship_confidence": _SOURCE_VECTOR_ENVELOPE["authorship_confidence"],
        "excluded_segments": [
            copy.deepcopy(segments[index]) for index in (7, 5, 3, 1, 6, 4, 0, 2)
        ],
        "raw_sha256": _SOURCE_VECTOR_ENVELOPE["raw_sha256"],
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
        "event": "synthetic-excluded-event",
        "unit": "synthetic-excluded-unit",
        "location": "synthetic-excluded-location",
        "source_event_refs": ["synthetic-excluded-event-ref"],
        "key_id": "synthetic-excluded-key-id",
        "mac": "synthetic-excluded-mac",
    }


def _envelope_builder():
    builder = getattr(V, "source_record_identity_envelope", None)
    assert callable(builder), (
        "G0-R8 RED: missing public source_record_identity_envelope contract"
    )
    scope_ref = sorted(V.CANONICAL_ORIGINAL_REFS)[0]
    adjudication = {
        "grain": "original_candidate",
        "ref": scope_ref,
        "classification": "synthetic-ignored",
        "disposition": "synthetic-ignored",
        "normalized_class": "synthetic-ignored",
        "expected": {"synthetic": "ignored"},
        "causal_confidence": {"synthetic": "ignored"},
        "owner_lane": "synthetic-ignored",
        "privacy_class": "synthetic-ignored",
        "source_refs": ["synthetic-ignored"],
        "source_event_refs": ["synthetic-ignored"],
    }

    def scoped(source_record):
        return builder(source_record, adjudication)

    return scoped


def _current_source_vector_envelope() -> dict:
    envelope = copy.deepcopy(_SOURCE_VECTOR_ENVELOPE)
    envelope["profile"] = _CURRENT_SOURCE_RECORD_PROFILE
    envelope["scope_ref"] = sorted(V.CANONICAL_ORIGINAL_REFS)[0]
    return envelope


def _assert_stable_hmac_contract_error(call, marker: str | None = None) -> None:
    with pytest.raises(Exception) as caught:
        call()
    error = caught.value
    assert isinstance(error, ValueError)
    assert error.args == ("E_HMAC_CONTRACT",)
    assert error.__cause__ is None
    assert error.__context__ is None
    if marker is not None:
        assert marker not in repr(error.args)
        assert marker not in repr(vars(error))


def _assert_hmac_contract_rejected(envelope, marker: str | None = None) -> None:
    _assert_stable_hmac_contract_error(
        lambda: V.hmac_payload_bytes("source_record", envelope),
        marker,
    )


def _synthetic_hmac_contract() -> dict:
    from ibus import test_anomaly_registry as R10

    return copy.deepcopy(R10._synthetic_r2_doc()["adjudication_contract"])


def _hmac_preflight_errors(contract: dict) -> list[str]:
    from ibus import test_anomaly_registry as R10

    doc = R10._synthetic_r2_doc()
    doc["adjudication_contract"] = contract
    errors: list[str] = []
    V._check_hmac_contract_preflight(
        doc,
        contract,
        V._adjudication_locations(doc),
        errors,
    )
    return errors


# ---------------------------------------------------------------- schema/time


def test_g0_r8_recorded_utc_key_accepts_explicit_json_null():
    assert _validate(_synthetic_record(recorded_utc=None)) == []


def test_g0_r8_last_seen_utc_key_accepts_explicit_json_null():
    assert _validate(_synthetic_record(last_seen_utc=None)) == []


def test_g0_r8_external_draft202012_accepts_required_timestamp_nulls():
    document = _synthetic_doc(_synthetic_record(recorded_utc=None, last_seen_utc=None))
    errors = list(Draft202012Validator(_schema()).iter_errors(document))
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
    assert getattr(V, "SOURCE_RECORD_HMAC_PROFILE", None) == (
        _CURRENT_SOURCE_RECORD_PROFILE
    )
    assert V.HMAC_DOMAIN_PAYLOAD_PROFILES == {
        "value": "scalar_utf8",
        "event": "project_canonical_json_v1",
        "metadata": "project_canonical_json_v1",
        "source_record": _CURRENT_SOURCE_RECORD_PROFILE,
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
        _CURRENT_SOURCE_RECORD_PROFILE
    )


def test_g0_r8_source_record_profile_bump_requires_a_new_vector_set():
    assert V.HMAC_VECTOR_SET_SHA256 != _OLD_VECTOR_SET_SHA256


def test_g0_r8_source_record_public_vector_pins_independent_bytes_and_mac():
    reference_payload = _reference_canonical_json_bytes(_SOURCE_VECTOR_ENVELOPE)
    reference_frame = _reference_hmac_frame("source_record", reference_payload)
    reference_mac = hmac.new(
        _PUBLIC_VECTOR_KEY,
        reference_frame,
        hashlib.sha256,
    ).hexdigest()
    assert reference_payload == _SOURCE_VECTOR_PAYLOAD_BYTES
    assert reference_payload.hex() == _SOURCE_VECTOR_PAYLOAD_HEX
    assert reference_frame == _SOURCE_VECTOR_FRAME_BYTES
    assert reference_frame.hex() == _SOURCE_VECTOR_FRAME_HEX
    assert reference_mac == _SOURCE_VECTOR_MAC_SHA256

    _assert_hmac_contract_rejected(_SOURCE_VECTOR_ENVELOPE)
    envelope = _envelope_builder()(_synthetic_merged_source_record())
    assert envelope == _current_source_vector_envelope()
    assert V.hmac_payload_bytes("source_record", envelope) != reference_payload
    assert V.hmac_frame_bytes("source_record", envelope) != reference_frame


def test_g0_r8_complete_v3_vector_set_is_exact_and_fails_closed():
    reference_vector_set = _reference_canonical_json_bytes(_V3_VECTOR_RECEIPTS)
    stdlib_vector_set = json.dumps(
        _V3_VECTOR_RECEIPTS,
        ensure_ascii=True,
        separators=(",", ":"),
        sort_keys=True,
    ).encode("utf-8")
    assert reference_vector_set == stdlib_vector_set
    assert hashlib.sha256(reference_vector_set).hexdigest() == (_V3_VECTOR_SET_SHA256)
    assert _V3_VECTOR_SET_SHA256 != _PRE_R9_VECTOR_SET_SHA256

    schema = _schema()
    contract_schema = schema["$defs"]["adjudication_contract"]["properties"]
    receipt_schema = schema["$defs"]["hmac_external_receipt"]["properties"]
    assert V.HMAC_VECTOR_SET_SHA256 == _V4_VECTOR_SET_SHA256
    assert V.SEALED_CONTRACT_CONSTS["hmac_vector_set_sha256"] == (_V4_VECTOR_SET_SHA256)
    assert contract_schema["hmac_vector_set_sha256"]["const"] == (_V4_VECTOR_SET_SHA256)
    assert receipt_schema["vector_set_sha256"]["const"] == _V4_VECTOR_SET_SHA256

    contract = _synthetic_hmac_contract()
    assert contract["hmac_vector_set_sha256"] == _V4_VECTOR_SET_SHA256
    assert contract["hmac_external_receipt"]["vector_set_sha256"] == (
        _V4_VECTOR_SET_SHA256
    )
    arbitrary_sha = "a" * 64
    assert arbitrary_sha not in {
        _OLD_VECTOR_SET_SHA256,
        _PRE_R9_VECTOR_SET_SHA256,
        _V3_VECTOR_SET_SHA256,
        _V4_VECTOR_SET_SHA256,
    }
    contract["hmac_vector_set_sha256"] = arbitrary_sha
    contract["hmac_external_receipt"]["vector_set_sha256"] = arbitrary_sha
    errors = _hmac_preflight_errors(contract)
    assert _codes(errors) == {"E_HMAC_CONTRACT"}
    assert arbitrary_sha not in "\n".join(errors)


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
    assert envelope == _current_source_vector_envelope()

    payload = V.hmac_payload_bytes("source_record", envelope)
    frame = V.hmac_frame_bytes("source_record", envelope)
    reference_payload = _reference_canonical_json_bytes(envelope)
    assert payload == reference_payload
    assert frame == _reference_hmac_frame("source_record", reference_payload)

    encoded = reference_payload
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
        b"event",
        b"unit",
        b"location",
        b"source_event_refs",
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
        ("event", "synthetic changed excluded event"),
        ("unit", "synthetic changed excluded unit"),
        ("location", "synthetic changed excluded location"),
        ("source_event_refs", ["synthetic-changed-event-ref"]),
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
    assert hmac.new(_PUBLIC_VECTOR_KEY, first_frame, hashlib.sha256).digest() == (
        hmac.new(_PUBLIC_VECTOR_KEY, second_frame, hashlib.sha256).digest()
    )


@pytest.mark.parametrize(
    ("extra_key", "first_value", "second_value"),
    [
        ("synthetic_extra_scalar", "synthetic-alpha", "synthetic-beta"),
        ("synthetic_extra_integer", 1, 2),
        ("synthetic_extra_boolean", False, True),
        ("synthetic_extra_null", None, "synthetic-present"),
        ("synthetic_extra_array", ["synthetic-alpha"], ["synthetic-beta"]),
        (
            "synthetic_extra_object",
            {"synthetic": "alpha"},
            {"synthetic": "beta"},
        ),
    ],
)
def test_g0_r8_arbitrary_nonwhitelist_source_input_is_nondecisional(
    extra_key,
    first_value,
    second_value,
):
    whitelist = {
        "profile",
        "scope_ref",
        "merged_id",
        "source_schema_version",
        "platform",
        "authorship_confidence",
        "excluded_segments",
        "raw_sha256",
    }
    assert extra_key not in whitelist
    builder = _envelope_builder()
    first = _synthetic_merged_source_record()
    second = copy.deepcopy(first)
    first[extra_key] = first_value
    second[extra_key] = second_value

    first_envelope = builder(first)
    second_envelope = builder(second)
    first_payload = V.hmac_payload_bytes("source_record", first_envelope)
    second_payload = V.hmac_payload_bytes("source_record", second_envelope)
    first_frame = V.hmac_frame_bytes("source_record", first_envelope)
    second_frame = V.hmac_frame_bytes("source_record", second_envelope)
    assert first_envelope == second_envelope == _current_source_vector_envelope()
    assert first_payload == second_payload
    assert first_frame == second_frame
    assert hmac.new(_PUBLIC_VECTOR_KEY, first_frame, hashlib.sha256).digest() == (
        hmac.new(_PUBLIC_VECTOR_KEY, second_frame, hashlib.sha256).digest()
    )


@pytest.mark.parametrize(
    ("field", "replacement"),
    [
        ("event", "synthetic-event-beta"),
        ("unit", "synthetic-unit-beta"),
        ("location", "synthetic-location-beta"),
    ],
)
def test_g0_r8_event_lane_keeps_event_unit_and_location_decisional(
    field,
    replacement,
):
    first = {
        "event": "synthetic-event-alpha",
        "unit": "synthetic-unit-alpha",
        "location": "synthetic-location-alpha",
    }
    second = copy.deepcopy(first)
    second[field] = replacement
    first_payload = V.hmac_payload_bytes("event", first)
    second_payload = V.hmac_payload_bytes("event", second)
    first_frame = V.hmac_frame_bytes("event", first)
    second_frame = V.hmac_frame_bytes("event", second)
    assert first_payload != second_payload
    assert first_frame != second_frame
    assert hmac.new(_PUBLIC_VECTOR_KEY, first_frame, hashlib.sha256).digest() != (
        hmac.new(_PUBLIC_VECTOR_KEY, second_frame, hashlib.sha256).digest()
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
    assert hmac.new(_PUBLIC_VECTOR_KEY, first_frame, hashlib.sha256).digest() != (
        hmac.new(_PUBLIC_VECTOR_KEY, second_frame, hashlib.sha256).digest()
    )


@pytest.mark.parametrize(
    "field",
    [
        "profile",
        "scope_ref",
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
        ("scope_ref", "orig:synthetic-invalid"),
        ("profile", ""),
        ("profile", 1),
        ("merged_id", "f" * 63),
        ("merged_id", "g" * 64),
        ("merged_id", 1),
        ("source_schema_version", ""),
        ("source_schema_version", 1),
        ("platform", ""),
        ("platform", []),
        ("authorship_confidence", ""),
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


def test_g0_r9_builder_rejects_oversize_before_large_transient_allocation():
    maximum_payload_bytes = V.HMAC_RESOURCE_LIMITS["max_canonical_payload_bytes"]
    allocation_ceiling = 8 * maximum_payload_bytes
    shared_8k_text = "s" * 8192
    segments = [
        {
            "authorship_confidence": shared_8k_text,
            "start_char": index * 2,
            "end_char": index * 2 + 1,
            "reason": shared_8k_text,
        }
        for index in range(64)
    ]
    source = _synthetic_merged_source_record()
    source["excluded_segments"] = segments
    builder = _envelope_builder()

    assert type(shared_8k_text) is str
    assert len(shared_8k_text.encode("utf-8")) == 8192
    assert len({id(segment) for segment in segments}) == 64
    assert all(type(segment) is dict for segment in segments)
    assert all(
        segment["authorship_confidence"] is shared_8k_text
        and segment["reason"] is shared_8k_text
        for segment in segments
    )
    assert len(segments) * 2 * 8192 > maximum_payload_bytes

    error = None
    result = None
    tracemalloc.start()
    try:
        result = builder(source)
    except Exception as caught:
        error = caught
    finally:
        _current_bytes, peak_bytes = tracemalloc.get_traced_memory()
        tracemalloc.stop()

    assert result is None
    assert type(error) is ValueError
    assert error.args == ("E_HMAC_CONTRACT",)
    assert error.__cause__ is None
    assert error.__context__ is None
    assert vars(error) == {}
    assert peak_bytes <= allocation_ceiling


def test_g0_r9_input_field_lookup_ignores_extras_and_checks_all_six(monkeypatch):
    required_fields = (
        "merged_id",
        "source_schema_version",
        "platform",
        "authorship_confidence",
        "excluded_segments",
        "raw_sha256",
    )

    class HostileSource(dict):
        def __init__(self):
            super().__init__()
            self.iter_calls = 0
            self.contains_calls = 0

        def __iter__(self):
            self.iter_calls += 1
            raise AssertionError("source iteration override was invoked")

        def __contains__(self, key):
            self.contains_calls += 1
            raise AssertionError("source membership override was invoked")

    class IgnoredExtraTripwire:
        def __contains__(self, key):
            if key not in required_fields:
                raise AssertionError("ignored source metadata was consulted")
            return True

    def source_missing(missing_field=None):
        source = HostileSource()
        dict.__setitem__(source, "synthetic-ignored-before", None)
        for field in required_fields:
            if field != missing_field:
                dict.__setitem__(source, field, None)
        dict.__setitem__(source, "synthetic-ignored-after", None)
        return source

    complete = source_missing()
    assert (
        tuple(field for field in required_fields if dict.__contains__(complete, field))
        == required_fields
    )
    monkeypatch.setattr(
        V,
        "_SOURCE_RECORD_INPUT_FIELD_SET",
        IgnoredExtraTripwire(),
    )

    assert V._source_record_has_input_fields(complete) is True
    incomplete = [source_missing(field) for field in required_fields]
    assert [V._source_record_has_input_fields(source) for source in incomplete] == [
        False,
    ] * 6
    for source in [complete, *incomplete]:
        assert source.iter_calls == 0
        assert source.contains_calls == 0


def test_g0_r9_segment_order_is_position_then_utf8_fields_with_crossovers():
    source = _synthetic_merged_source_record()
    input_segments = source["excluded_segments"]
    expected = _SOURCE_VECTOR_ENVELOPE["excluded_segments"]
    assert sorted(input_segments, key=_segment_order_key) == expected
    assert [
        (
            segment["start_char"],
            segment["end_char"],
            segment["authorship_confidence"],
            segment["reason"],
        )
        for segment in expected
    ] == [
        (0, 4, "synthetic-alpha", "synthetic-alpha"),
        (0, 4, "synthetic-alpha", "synthetic-zulu"),
        (0, 4, "synthetic-beta", "synthetic-alpha"),
        (0, 4, "synthetic-zulu", "synthetic-zulu"),
        (0, 6, "synthetic-alpha", "synthetic-alpha"),
        (8, 12, "synthetic-aaa", "synthetic-aaa"),
        (16, 20, "\ue000", "synthetic-zulu"),
        (16, 20, "\U00010000", "synthetic-alpha"),
    ]

    crossovers = expected[-2:]
    assert sorted(crossovers, key=_segment_order_key) == crossovers
    assert sorted(crossovers, key=_segment_scalar_order_key) == crossovers
    assert sorted(crossovers, key=_segment_utf16_order_key) == list(
        reversed(crossovers)
    )
    assert sorted(crossovers, key=_segment_reason_first_order_key) == list(
        reversed(crossovers)
    )

    label_first = sorted(
        input_segments,
        key=lambda segment: (
            segment["authorship_confidence"].encode("utf-8"),
            segment["reason"].encode("utf-8"),
            segment["start_char"],
            segment["end_char"],
        ),
    )
    start_then_label = sorted(
        input_segments,
        key=lambda segment: (
            segment["start_char"],
            segment["authorship_confidence"].encode("utf-8"),
            segment["end_char"],
            segment["reason"].encode("utf-8"),
        ),
    )
    assert label_first != expected
    assert start_then_label != expected
    assert _envelope_builder()(source)["excluded_segments"] == expected


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


def test_g0_r8_source_record_envelope_rejects_exact_duplicate_segment():
    marker = "synthetic-duplicate-segment-marker"
    envelope = _envelope_builder()(_synthetic_merged_source_record())
    duplicate = copy.deepcopy(envelope["excluded_segments"][0])
    duplicate["reason"] = marker
    envelope["excluded_segments"].extend([duplicate, copy.deepcopy(duplicate)])
    envelope["excluded_segments"].sort(key=_segment_order_key)
    _assert_hmac_contract_rejected(envelope, marker)


def test_g0_r8_overlapping_nonduplicate_segments_remain_valid():
    envelope = _envelope_builder()(_synthetic_merged_source_record())
    segments = envelope["excluded_segments"]
    assert segments[0]["start_char"] == segments[1]["start_char"] == 0
    assert segments[0]["end_char"] == segments[1]["end_char"] == 4
    assert segments[0] != segments[1]
    assert segments[3]["end_char"] == 4 < segments[4]["end_char"] == 6
    assert V.hmac_payload_bytes("source_record", envelope) == (
        _reference_canonical_json_bytes(envelope)
    )


def test_g0_r8_segment_safe_integer_max_and_unlisted_strings_are_valid():
    source = _synthetic_merged_source_record()
    source["source_schema_version"] = "synthetic-unlisted-schema"
    source["platform"] = "synthetic-unlisted-platform"
    source["authorship_confidence"] = "synthetic-unlisted-root-confidence"
    source["excluded_segments"] = [
        {
            "authorship_confidence": "synthetic-unlisted-segment-confidence",
            "start_char": _SAFE_INTEGER_MAX - 1,
            "end_char": _SAFE_INTEGER_MAX,
            "reason": "synthetic-unlisted-reason",
        }
    ]
    envelope = _envelope_builder()(source)
    assert envelope["source_schema_version"] == "synthetic-unlisted-schema"
    assert envelope["platform"] == "synthetic-unlisted-platform"
    assert envelope["authorship_confidence"] == ("synthetic-unlisted-root-confidence")
    assert envelope["excluded_segments"] == source["excluded_segments"]
    assert V.hmac_payload_bytes("source_record", envelope)


@pytest.mark.parametrize(
    ("field", "replacement"),
    [
        ("authorship_confidence", 1),
        ("authorship_confidence", ""),
        ("start_char", -1),
        ("start_char", "0"),
        ("start_char", True),
        ("start_char", 0.0),
        ("start_char", 5),
        ("end_char", 0),
        ("end_char", "4"),
        ("end_char", True),
        ("end_char", 4.0),
        ("end_char", _SAFE_INTEGER_MAX + 1),
        ("reason", ""),
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
