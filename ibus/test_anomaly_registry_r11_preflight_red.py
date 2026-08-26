"""R11a RED contracts for sealed tuple, scope, and import authority.

Every fixture in this module is synthetic or reuses the already-public,
deidentified contract fixtures.  No private candidate or corpus data is read.
"""

from __future__ import annotations

import copy
import hashlib
import hmac
import inspect
import json
import tracemalloc
from pathlib import Path

import pytest
from jsonschema import Draft202012Validator

from ibus import test_anomaly_registry as R10
from ibus import test_anomaly_registry_timestamp_authority_red as R9


V = R10.V

_SOURCE_PROFILE = "smartkey-g0-source-record-adjudication-scoped-semantic-v2"
_HMAC_CONTRACT_VERSION = "smartkey-g0-hmac-byte-contract-v4"
_HMAC_RECEIPT_COVERAGE = (
    "all-refs-domain-serialization-resource-key-id-adjudication-scope-"
    "uniqueness-whole-source-private-recomputation-v3"
)
_VECTOR_SET_SHA256 = "8f6644ed05a898bd650f4e42dca4bc278e19db601523ed5442fcc20dd5ab0b3c"
_FRAGMENT_POLICY = "smartkey-g0-source-ref-scoped-cap-v1"
_PRESENCE_POLICY = "smartkey-g0-original-retained-exactly-once-presence-v1"
_PRESENCE_RULING_REF = "4e0198426714"
_CROSSWALK_PROFILE = "smartkey-g0-crosswalk-target-binding-v2"
_PLACEMENT_PROFILE = "smartkey-g0-source-scope-placement-v1"
_PRESEAL_AUDIT_CONTRACT = "g0-r11a-preseal-crosswalk"
_IMPORT_HOLD = "E_R3_IMPORT_HOLD"
_SOURCE_SCOPE_ERROR = "E_ADJ_SOURCE_SCOPE"
_SOURCE_PLACEMENT_ERROR = "E_SOURCE_SCOPE_PLACEMENT"
_PRESENCE_ERROR = "E_ADJ_PRESENCE"
_CROSSWALK_ERROR = "E_CROSSWALK_CONTRACT"
_MATRIX_SHA256 = "4c32653fa93b8dbe2ca48b5ee42cb4d733aec6ebc374c2ca712f558203237a04"
_ORIGINAL_REF_SET_SHA256 = (
    "6a2d167b00ece704a369772c76b231821513b0da5602ab960c16ac9bed78a3f2"
)
_CROSSWALK_SHA256 = "dff41e032dab140ed77b3d0339787e354a5772354f60093b0c6b8153bbe384d6"
_PLACEMENT_SHA256 = "c4f2a3a642a15c0a62ce9bb5904adfdec43721e88af65be1577f68d7e122f119"
_PRESENCE_RECEIPT_SHA256 = (
    "6e9d2931631a284fb25e0f60e354eb42e42f026a5032d5f58db3a1a7e07b96ee"
)
_ORIGINAL_SCOPE = R10._CANONICAL_ORIGINAL_REFS[0]
_SUPPLEMENTAL_SCOPE = R10._canonical_supplemental_refs()[0]
_VECTOR_ORIGINAL_SCOPE = "orig:0123456789abcdef"
_VECTOR_SUPPLEMENTAL_SCOPE = "supp:G000:H000"
_PUBLIC_VECTOR_KEY = R9._PUBLIC_VECTOR_KEY

_APPROVED_TUPLES = {
    "same_script_character_substitution": frozenset(
        {
            (
                "instrumentation_first_candidate",
                "bug_candidate",
                "same_script_loss",
            ),
            (
                "instrumentation_first_candidate",
                "bug_candidate",
                "adapter_event_integrity",
            ),
        }
    ),
    "duplicated_character": frozenset(
        {
            (
                "instrumentation_first_candidate",
                "bug_candidate",
                "same_script_loss",
            ),
            (
                "instrumentation_first_candidate",
                "bug_candidate",
                "adapter_event_integrity",
            ),
        }
    ),
    "brand_or_native_word_layout_ambiguity": frozenset(
        {
            (
                "guard_not_bug_unless_intent_changes",
                "guard",
                "intended_english_quoted_guard",
            ),
            (
                "intent_confirmation_needed",
                "bug_candidate",
                "future_language_quality_layer",
            ),
        }
    ),
    "brand_or_intentional_transliteration_ambiguity": frozenset(
        {
            (
                "guard_not_bug_unless_intent_changes",
                "guard",
                "intended_english_quoted_guard",
            ),
            (
                "intent_confirmation_needed",
                "bug_candidate",
                "future_language_quality_layer",
            ),
        }
    ),
    "intentional_transliteration_guard": frozenset(
        {
            (
                "guard_not_bug_unless_intent_changes",
                "guard",
                "intended_english_quoted_guard",
            ),
            (
                "guard_not_bug_unless_intent_changes",
                "guard",
                "future_language_quality_layer",
            ),
        }
    ),
}

_R11_MATRIX_ROWS = tuple(
    sorted(
        (normalized_class, classification, disposition, owner_lane)
        for normalized_class, tuples in {
            **{
                key: frozenset(value)
                for key, value in V.NORMALIZED_CLASS_ALLOWED_TUPLES.items()
            },
            **_APPROVED_TUPLES,
        }.items()
        for classification, disposition, owner_lane in tuples
    )
)
_R11_ADDED_ROWS = frozenset(
    {
        (
            "same_script_character_substitution",
            "instrumentation_first_candidate",
            "bug_candidate",
            "adapter_event_integrity",
        ),
        (
            "duplicated_character",
            "instrumentation_first_candidate",
            "bug_candidate",
            "adapter_event_integrity",
        ),
        (
            "brand_or_native_word_layout_ambiguity",
            "intent_confirmation_needed",
            "bug_candidate",
            "future_language_quality_layer",
        ),
        (
            "brand_or_intentional_transliteration_ambiguity",
            "intent_confirmation_needed",
            "bug_candidate",
            "future_language_quality_layer",
        ),
        (
            "intentional_transliteration_guard",
            "guard_not_bug_unless_intent_changes",
            "guard",
            "future_language_quality_layer",
        ),
    }
)
_R10_MATRIX_ROWS = tuple(row for row in _R11_MATRIX_ROWS if row not in _R11_ADDED_ROWS)


def _codes(errors: list[str]) -> set[str]:
    return {error.split(":", 1)[0] for error in errors}


def _synthetic_presence_receipt_sha256(
    domain: str, epoch: str, *, variant: str | None = None
) -> str:
    """Return a test-only opaque receipt under one explicit fixture grammar."""
    label = f"r11:presence-receipt:{domain}:{epoch}"
    if variant is not None:
        label += f":{variant}"
    return hashlib.sha256(f"synthetic:{label}".encode("utf-8")).hexdigest()


def _required_constant(name: str, expected):
    actual = getattr(V, name, None)
    assert actual == expected, f"R11a RED: missing or stale {name}"
    return actual


def _scoped_builder():
    builder = getattr(V, "source_record_identity_envelope", None)
    assert callable(builder), "R11a RED: missing scoped source-record builder"
    signature = inspect.signature(builder)
    parameters = tuple(signature.parameters)
    assert parameters == (
        "source_record",
        "adjudication",
    ), "R11a RED: source-record builder must derive scope from adjudication"
    assert signature.parameters["adjudication"].default is inspect.Parameter.empty, (
        "R11a RED: adjudication authority cannot have a default"
    )
    return builder


def _reference_json_string(value: str) -> bytes:
    assert type(value) is str
    return json.dumps(
        value,
        ensure_ascii=False,
        separators=(",", ":"),
    ).encode("utf-8")


def _reference_canonical_bytes(value) -> bytes:
    """Test-local stdlib oracle; deliberately independent of production."""
    if value is None:
        return b"null"
    if value is True:
        return b"true"
    if value is False:
        return b"false"
    if type(value) is int:
        assert -9_007_199_254_740_991 <= value <= 9_007_199_254_740_991
        return str(value).encode("ascii")
    if type(value) is str:
        return _reference_json_string(value)
    if type(value) is list:
        return b"[" + b",".join(_reference_canonical_bytes(v) for v in value) + b"]"
    if type(value) is dict:
        assert all(type(key) is str for key in value)
        keys = sorted(value, key=lambda key: tuple(ord(char) for char in key))
        return (
            b"{"
            + b",".join(
                _reference_json_string(key)
                + b":"
                + _reference_canonical_bytes(value[key])
                for key in keys
            )
            + b"}"
        )
    raise AssertionError(f"unsupported reference value: {type(value).__name__}")


def _reference_frame(payload: bytes) -> bytes:
    return (
        b"smartkey-g0-hmac-sha256-v1"
        + b"\0source_record\0"
        + len(payload).to_bytes(8, "big", signed=False)
        + payload
    )


def _source_input() -> dict:
    source = R9._synthetic_merged_source_record()
    source["scope_ref"] = _SUPPLEMENTAL_SCOPE
    source["scope_salt"] = "synthetic-ignored-salt"
    return source


def _authoritative_adjudication(scope_ref: str) -> dict:
    """Return one complete public synthetic contract mapping, never a bare ref."""
    doc = R10._synthetic_r2_doc()
    items = [
        item for record in doc["records"] for item in record.get("adjudications", ())
    ]
    items.extend(doc["source_exclusions"])
    return copy.deepcopy(next(item for item in items if item["ref"] == scope_ref))


def _reference_scoped_envelope(scope_ref: str) -> dict:
    base = copy.deepcopy(R9._SOURCE_VECTOR_ENVELOPE)
    base["profile"] = _SOURCE_PROFILE
    base["scope_ref"] = scope_ref
    return base


def _reference_vector(name: str, scope_ref: str) -> dict:
    envelope = _reference_scoped_envelope(scope_ref)
    payload = _reference_canonical_bytes(envelope)
    frame = _reference_frame(payload)
    return {
        "name": name,
        "domain": "source_record",
        "profile": _SOURCE_PROFILE,
        "key_id": R9._SYNTHETIC_KEY_ID,
        "key_hex": _PUBLIC_VECTOR_KEY.hex(),
        "payload_hex": payload.hex(),
        "frame_hex": frame.hex(),
        "mac_sha256": hmac.new(_PUBLIC_VECTOR_KEY, frame, hashlib.sha256).hexdigest(),
    }


_SCOPED_VECTOR_PINS = {
    _VECTOR_ORIGINAL_SCOPE: {
        "payload_len": 1178,
        "payload_sha256": "c0b1b1b5df60f7f3a9f2a3b5164560153b5c5312b810bedc90ad639632f6feb0",
        "frame_len": 1227,
        "frame_sha256": "85272754b5a81698924859c7f8d67fb5a05b75dd4926e5668980dfe05142f66c",
        "mac_sha256": "449e7353f1489ea1a2f265fd8f1ab8aff8fe68d0ef6068c7c2a767b5a52fba11",
    },
    _VECTOR_SUPPLEMENTAL_SCOPE: {
        "payload_len": 1171,
        "payload_sha256": "3effba1df5a71aa4e2709b71025931ed39d0d55b9a39d450faf140af26f16b5a",
        "frame_len": 1220,
        "frame_sha256": "a0d02fb33b35fe1946597d9dac2d3ceaddf4a9dd4117538a7c9c5b8b089d179a",
        "mac_sha256": "68993f816d23bb8e8c5a0c88b1d715421b87b496835faa3c1dc6a7ab3fef620b",
    },
}


def _assert_hmac_contract_error(call, marker: str | None = None) -> None:
    with pytest.raises(ValueError) as caught:
        call()
    error = caught.value
    assert type(error) is ValueError
    assert error.args == ("E_HMAC_CONTRACT",)
    assert vars(error) == {}
    assert error.__cause__ is None
    assert error.__context__ is None
    if marker is not None:
        assert not R10._r6_value_contains_marker(error.args, marker)
        assert not R10._r6_value_contains_marker(vars(error), marker)
        assert not R10._r6_value_contains_marker(error.__cause__, marker)
        assert not R10._r6_value_contains_marker(error.__context__, marker)
        production_path = Path(V.__file__).resolve()
        production_frames = 0
        traceback_cursor = error.__traceback__
        while traceback_cursor is not None:
            frame = traceback_cursor.tb_frame
            if Path(frame.f_code.co_filename).resolve() == production_path:
                production_frames += 1
                assert not R10._r6_value_contains_marker(frame.f_locals, marker)
            traceback_cursor = traceback_cursor.tb_next
        assert production_frames > 0


def _assert_presence_contract_error(call) -> None:
    with pytest.raises(ValueError) as caught:
        call()
    assert caught.value.args == (_PRESENCE_ERROR,)
    assert caught.value.__cause__ is None
    assert caught.value.__context__ is None


def _crosswalk_builder():
    builder = getattr(V, "crosswalk_target_binding_projection", None)
    assert callable(builder), "R11a RED: missing closed target-binding crosswalk"
    return builder


def _crosswalk_digest():
    digest = getattr(V, "crosswalk_target_binding_sha256", None)
    assert callable(digest), "R11a RED: missing target-binding crosswalk digest"
    return digest


def _placement_builder():
    builder = getattr(V, "source_scope_placement_projection", None)
    assert callable(builder), "R11a RED: missing scoped-placement projection"
    return builder


def _placement_digest():
    digest = getattr(V, "source_scope_placement_sha256", None)
    assert callable(digest), "R11a RED: missing scoped-placement digest"
    return digest


def _reference_crosswalk_projection(doc: dict) -> dict:
    groups = []
    for record in doc["records"]:
        refs = sorted(item["ref"] for item in record.get("adjudications", ()))
        if refs:
            groups.append(
                {
                    "target_kind": "record",
                    "target_id": record["id"],
                    "mapping_refs": refs,
                }
            )
    for item in doc["source_exclusions"]:
        groups.append(
            {
                "target_kind": "source_exclusion",
                "target_id": f"source-exclusion:{item['ref']}",
                "mapping_refs": [item["ref"]],
            }
        )
    groups.sort(
        key=lambda group: (
            group["target_kind"],
            group["target_id"],
            tuple(group["mapping_refs"]),
        )
    )
    return {"profile": _CROSSWALK_PROFILE, "groups": groups}


def _reference_crosswalk_sha256(doc: dict) -> str:
    return hashlib.sha256(
        _reference_canonical_bytes(_reference_crosswalk_projection(doc))
    ).hexdigest()


def _reference_placement_projection(doc: dict) -> dict:
    placements = []
    items = [
        item for record in doc["records"] for item in record.get("adjudications", ())
    ]
    items.extend(doc["source_exclusions"])
    for item in items:
        for source in item["source_refs"]:
            if source["kind"] == "source_record":
                placements.append(
                    {
                        "adjudication_ref": item["ref"],
                        "source_record_ref": source["ref"],
                    }
                )
    placements.sort(
        key=lambda placement: (
            placement["adjudication_ref"],
            placement["source_record_ref"],
        )
    )
    return {"profile": _PLACEMENT_PROFILE, "placements": placements}


def _reference_placement_sha256(doc: dict) -> str:
    return hashlib.sha256(
        _reference_canonical_bytes(_reference_placement_projection(doc))
    ).hexdigest()


def _reference_semantic_payload(doc: dict) -> list[dict]:
    items = [
        (item, record["id"])
        for record in doc["records"]
        for item in record.get("adjudications", ())
    ]
    items.extend(
        (item, "top_level_source_exclusion") for item in doc["source_exclusions"]
    )
    projected = []
    for item, target_id in sorted(items, key=lambda value: value[0]["ref"]):
        confidence = copy.deepcopy(item["causal_confidence"])
        if item["grain"] == "original_candidate":
            confidence["anomaly_or_guard_presence"] = "high"
        projected.append(
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
                "causal_confidence": confidence,
                "owner_lane": item["owner_lane"],
                "privacy_class": item["privacy_class"],
                "source_binding": {
                    "target_record_id": target_id,
                    "source_refs": sorted(
                        f"{source['kind']}:{source['ref']}"
                        for source in item["source_refs"]
                    ),
                    "source_event_refs": sorted(item["source_event_refs"]),
                },
            }
        )
    return projected


def _reference_semantic_sha256(doc: dict) -> str:
    return hashlib.sha256(
        _reference_canonical_bytes(_reference_semantic_payload(doc))
    ).hexdigest()


def _reference_original_ref_set_sha256(doc: dict) -> str:
    refs = sorted(
        item["ref"]
        for record in doc["records"]
        for item in record.get("adjudications", ())
        if item["grain"] == "original_candidate"
    )
    refs.extend(
        item["ref"]
        for item in doc["source_exclusions"]
        if item["grain"] == "original_candidate"
    )
    refs.sort()
    return hashlib.sha256(_reference_canonical_bytes(refs)).hexdigest()


def _strip_original_presence(doc: dict) -> None:
    for record in doc["records"]:
        for item in record.get("adjudications", ()):
            if item["grain"] == "original_candidate":
                item["causal_confidence"].pop("anomaly_or_guard_presence", None)
    for item in doc["source_exclusions"]:
        if item["grain"] == "original_candidate":
            item["causal_confidence"].pop("anomaly_or_guard_presence", None)


def _r11_doc() -> dict:
    doc = R10._synthetic_r2_doc()
    _strip_original_presence(doc)
    contract = doc["adjudication_contract"]
    contract.update(
        {
            "audit_contract": _PRESEAL_AUDIT_CONTRACT,
            "crosswalk_projection_profile": _CROSSWALK_PROFILE,
            "source_scope_placement_profile": _PLACEMENT_PROFILE,
            "normalized_tuple_matrix_sha256": _MATRIX_SHA256,
            "reconstruction_fragment_policy_version": _FRAGMENT_POLICY,
            "original_presence_derivation_version": _PRESENCE_POLICY,
            "original_presence_derivation_ruling_ref": _PRESENCE_RULING_REF,
            "original_ref_set_sha256": _ORIGINAL_REF_SET_SHA256,
            "original_presence_external_receipt": {
                "state": "externally_verified",
                "policy_version": _PRESENCE_POLICY,
                "ruling_ref": _PRESENCE_RULING_REF,
                "original_count": 106,
                "derived_presence": "high",
                "original_ref_set_sha256": _ORIGINAL_REF_SET_SHA256,
                "crosswalk_projection_profile": _CROSSWALK_PROFILE,
                "target_binding_crosswalk_sha256": "0" * 64,
                "audit_contract": _PRESEAL_AUDIT_CONTRACT,
                "receipt_sha256": _PRESENCE_RECEIPT_SHA256,
            },
            "hmac_contract_version": _HMAC_CONTRACT_VERSION,
            "hmac_domain_payload_profiles": {
                **contract["hmac_domain_payload_profiles"],
                "source_record": _SOURCE_PROFILE,
            },
            "hmac_vector_set_sha256": _VECTOR_SET_SHA256,
        }
    )
    receipt = contract["hmac_external_receipt"]
    receipt.update(
        {
            "contract_version": _HMAC_CONTRACT_VERSION,
            "vector_set_sha256": _VECTOR_SET_SHA256,
            "coverage": _HMAC_RECEIPT_COVERAGE,
            "source_scope_policy_version": _FRAGMENT_POLICY,
            "crosswalk_projection_profile": _CROSSWALK_PROFILE,
            "source_scope_placement_profile": _PLACEMENT_PROFILE,
            "audit_contract": _PRESEAL_AUDIT_CONTRACT,
            "registry_schema_version": 1,
        }
    )
    contract["mapping_crosswalk_sha256"] = _reference_crosswalk_sha256(doc)
    contract["source_scope_placement_sha256"] = _reference_placement_sha256(doc)
    contract["semantic_commitment_sha256"] = _reference_semantic_sha256(doc)
    placements = _reference_placement_projection(doc)["placements"]
    receipt["source_scope_binding_count"] = len(placements)
    receipt["mapping_crosswalk_sha256"] = contract["mapping_crosswalk_sha256"]
    receipt["source_scope_placement_sha256"] = contract["source_scope_placement_sha256"]
    receipt["semantic_commitment_sha256"] = contract["semantic_commitment_sha256"]
    contract["original_presence_external_receipt"][
        "target_binding_crosswalk_sha256"
    ] = contract["mapping_crosswalk_sha256"]
    contract["legacy_projection_sha256"] = R10._r3_legacy_projection_digest(doc)
    contract["registry_projection_sha256"] = R10._r3_registry_projection_digest(doc)
    return doc


def _hmac_receipt_probe_doc() -> dict:
    """Isolate receipt validation from the independently RED R11a pins."""
    doc = _r11_doc()
    contract = doc["adjudication_contract"]
    contract.update(
        {
            "hmac_scheme": V.HMAC_SCHEME,
            "hmac_min_key_bytes": V.HMAC_MIN_KEY_BYTES,
            "hmac_contract_version": V.HMAC_CONTRACT_VERSION,
            "hmac_domains": list(V.HMAC_DOMAINS),
            "hmac_domain_payload_profiles": copy.deepcopy(
                V.HMAC_DOMAIN_PAYLOAD_PROFILES
            ),
            "hmac_domain_root_types": copy.deepcopy(V.HMAC_DOMAIN_ROOT_TYPES),
            "hmac_resource_contract_version": V.HMAC_RESOURCE_CONTRACT_VERSION,
            "hmac_resource_limits": copy.deepcopy(V.HMAC_RESOURCE_LIMITS),
            "hmac_resource_contract_sha256": V.HMAC_RESOURCE_CONTRACT_SHA256,
            "hmac_scalar_payload_encoding": V.HMAC_SCALAR_PAYLOAD_ENCODING,
            "hmac_structured_payload_encoding": V.HMAC_STRUCTURED_PAYLOAD_ENCODING,
            "hmac_input_frame": V.HMAC_INPUT_FRAME,
            "hmac_vector_set_sha256": V.HMAC_VECTOR_SET_SHA256,
        }
    )
    receipt = contract["hmac_external_receipt"]
    receipt.update(
        {
            "state": V.HMAC_RECEIPT_STATE,
            "scheme": V.HMAC_SCHEME,
            "key_id": contract["hmac_key_id"],
            "contract_version": V.HMAC_CONTRACT_VERSION,
            "resource_contract_version": V.HMAC_RESOURCE_CONTRACT_VERSION,
            "resource_contract_sha256": V.HMAC_RESOURCE_CONTRACT_SHA256,
            "vector_set_sha256": V.HMAC_VECTOR_SET_SHA256,
            "coverage": V.HMAC_RECEIPT_COVERAGE,
        }
    )
    return doc


def _isolated_hmac_preflight_codes(doc: dict) -> set[str]:
    errors: list[str] = []
    V._check_hmac_contract_preflight(
        doc,
        doc["adjudication_contract"],
        V._adjudication_locations(doc),
        errors,
    )
    return _codes(errors)


# ------------------------------------------------------------------ B1


@pytest.mark.parametrize(
    ("normalized_class", "classification", "disposition", "owner_lane"),
    [
        (
            "same_script_character_substitution",
            "instrumentation_first_candidate",
            "bug_candidate",
            "adapter_event_integrity",
        ),
        (
            "duplicated_character",
            "instrumentation_first_candidate",
            "bug_candidate",
            "adapter_event_integrity",
        ),
        (
            "brand_or_native_word_layout_ambiguity",
            "intent_confirmation_needed",
            "bug_candidate",
            "future_language_quality_layer",
        ),
        (
            "brand_or_intentional_transliteration_ambiguity",
            "intent_confirmation_needed",
            "bug_candidate",
            "future_language_quality_layer",
        ),
        (
            "intentional_transliteration_guard",
            "guard_not_bug_unless_intent_changes",
            "guard",
            "future_language_quality_layer",
        ),
    ],
)
def test_r11_b1_accepts_only_each_ratified_missing_tuple(
    normalized_class, classification, disposition, owner_lane
):
    assert V.normalized_tuple_is_allowed(
        normalized_class,
        "supplemental_hypothesis",
        classification,
        disposition,
        owner_lane,
    )


def test_r11_b1_target_class_inventories_are_exact_closed_sets():
    for normalized_class, expected in _APPROVED_TUPLES.items():
        assert V.NORMALIZED_CLASS_ALLOWED_TUPLES[normalized_class] == expected


def _validator_matrix_rows() -> tuple[tuple[str, str, str, str], ...]:
    return tuple(
        sorted(
            (normalized_class, classification, disposition, owner_lane)
            for normalized_class, tuples in V.NORMALIZED_CLASS_ALLOWED_TUPLES.items()
            for classification, disposition, owner_lane in tuples
        )
    )


def test_r11_b1_full_matrix_is_exact_51_row_plus_five_minus_zero_delta():
    assert len(_R10_MATRIX_ROWS) == 46
    assert len(_R11_MATRIX_ROWS) == 51
    assert frozenset(_R11_MATRIX_ROWS) - frozenset(_R10_MATRIX_ROWS) == (
        _R11_ADDED_ROWS
    )
    assert frozenset(_R10_MATRIX_ROWS) - frozenset(_R11_MATRIX_ROWS) == frozenset()
    encoded = json.dumps(
        _R11_MATRIX_ROWS,
        ensure_ascii=True,
        separators=(",", ":"),
    ).encode("utf-8")
    assert hashlib.sha256(encoded).hexdigest() == _MATRIX_SHA256
    assert _validator_matrix_rows() == _R11_MATRIX_ROWS


def test_r11_b1_full_matrix_digest_is_a_fixed_validator_pin():
    _required_constant("NORMALIZED_TUPLE_MATRIX_SHA256", _MATRIX_SHA256)


def test_r11_b1_exhaustive_grain_by_matrix_truth_table_has_no_wildcards():
    rows = frozenset(_R11_MATRIX_ROWS)
    for normalized_class in V.NORMALIZED_CLASSES:
        for grain in V.ADJUDICATION_GRAINS:
            for classification in V.ADJUDICATION_CLASSIFICATIONS:
                for disposition in V.ADJUDICATION_DISPOSITIONS:
                    for owner_lane in V.OWNER_LANES:
                        row = (
                            normalized_class,
                            classification,
                            disposition,
                            owner_lane,
                        )
                        expected = row in rows and (
                            (
                                normalized_class == "not_applicable"
                                and grain == "original_candidate"
                            )
                            or (
                                normalized_class != "not_applicable"
                                and grain == "supplemental_hypothesis"
                            )
                        )
                        assert (
                            V.normalized_tuple_is_allowed(
                                normalized_class,
                                grain,
                                classification,
                                disposition,
                                owner_lane,
                            )
                            is expected
                        )


@pytest.mark.parametrize(
    ("normalized_class", "classification", "disposition", "owner_lane"),
    [
        (
            "same_script_character_substitution",
            "instrumentation_first_candidate",
            "bug_candidate",
            "output_neutral_provenance",
        ),
        (
            "duplicated_character",
            "mechanical_red_candidate",
            "bug_candidate",
            "adapter_event_integrity",
        ),
        (
            "brand_or_native_word_layout_ambiguity",
            "intent_confirmation_needed",
            "bug_candidate",
            "future_language_quality_or_intent",
        ),
        (
            "brand_or_intentional_transliteration_ambiguity",
            "language_quality_feature_candidate",
            "bug_candidate",
            "future_language_quality_layer",
        ),
        (
            "intentional_transliteration_guard",
            "guard_not_bug_unless_intent_changes",
            "guard",
            "future_language_quality_or_intent",
        ),
    ],
)
def test_r11_b1_adjacent_unratified_tuple_broadening_remains_rejected(
    normalized_class, classification, disposition, owner_lane
):
    assert not V.normalized_tuple_is_allowed(
        normalized_class,
        "supplemental_hypothesis",
        classification,
        disposition,
        owner_lane,
    )


# ------------------------------------------------------------------ B2


@pytest.mark.parametrize(
    ("name", "expected"),
    [
        ("SOURCE_RECORD_HMAC_PROFILE", _SOURCE_PROFILE),
        ("HMAC_CONTRACT_VERSION", _HMAC_CONTRACT_VERSION),
        ("HMAC_RECEIPT_COVERAGE", _HMAC_RECEIPT_COVERAGE),
        ("HMAC_VECTOR_SET_SHA256", _VECTOR_SET_SHA256),
        ("RECONSTRUCTION_FRAGMENT_POLICY_VERSION", _FRAGMENT_POLICY),
        ("CROSSWALK_PROJECTION_PROFILE", _CROSSWALK_PROFILE),
        ("SOURCE_SCOPE_PLACEMENT_PROFILE", _PLACEMENT_PROFILE),
        ("HMAC_SOURCE_SCOPE_BINDING_COUNT", 159),
    ],
)
def test_r11_b2_scope_contract_versions_are_closed_and_pinned(name, expected):
    _required_constant(name, expected)


@pytest.mark.parametrize("scope_ref", [_ORIGINAL_SCOPE, _SUPPLEMENTAL_SCOPE])
def test_r11_b2_builder_derives_exact_scope_from_containing_adjudication(scope_ref):
    envelope = _scoped_builder()(
        _source_input(), _authoritative_adjudication(scope_ref)
    )
    assert envelope == _reference_scoped_envelope(scope_ref)
    assert set(envelope) == {
        "profile",
        "scope_ref",
        "merged_id",
        "source_schema_version",
        "platform",
        "authorship_confidence",
        "excluded_segments",
        "raw_sha256",
    }


def test_r11_b2_builder_requires_explicit_adjudication_argument():
    builder = getattr(V, "source_record_identity_envelope", None)
    assert callable(builder), "R11a RED: missing scoped source-record builder"
    with pytest.raises(TypeError):
        builder(_source_input())


def test_r11_b2_builder_signature_has_required_adjudication_without_default():
    builder = _scoped_builder()
    parameter = inspect.signature(builder).parameters["adjudication"]
    assert parameter.default is inspect.Parameter.empty


def test_r11_b2_builder_does_not_mutate_inputs_or_return_mutable_aliases():
    builder = _scoped_builder()
    source = _source_input()
    adjudication = _authoritative_adjudication(_ORIGINAL_SCOPE)
    source_before = copy.deepcopy(source)
    adjudication_before = copy.deepcopy(adjudication)

    envelope = builder(source, adjudication)

    assert source == source_before
    assert adjudication == adjudication_before
    assert envelope is not source
    assert envelope["excluded_segments"] is not source["excluded_segments"]
    assert all(
        projected is not supplied
        for projected, supplied in zip(
            envelope["excluded_segments"],
            source["excluded_segments"],
            strict=True,
        )
    )
    envelope["excluded_segments"][0]["reason"] = "synthetic-mutated-result"
    envelope["scope_ref"] = _SUPPLEMENTAL_SCOPE
    assert source == source_before
    assert adjudication == adjudication_before


def test_r11_b2_adjudication_ignored_values_are_not_scanned_or_deep_copied():
    marker = "synthetic-adjudication-ignored-tripwire"

    class IgnoredTripwire:
        touched = False

        def _trip(self, *_args, **_kwargs):
            self.touched = True
            raise AssertionError(marker)

        __copy__ = _trip
        __deepcopy__ = _trip
        __iter__ = _trip
        __len__ = _trip
        __repr__ = _trip

    ignored = IgnoredTripwire()
    adjudication = _authoritative_adjudication(_ORIGINAL_SCOPE)
    for field in (
        "classification",
        "disposition",
        "normalized_class",
        "expected",
        "causal_confidence",
        "owner_lane",
        "privacy_class",
        "source_refs",
        "source_event_refs",
    ):
        adjudication[field] = ignored

    envelope = _scoped_builder()(_source_input(), adjudication)

    assert envelope == _reference_scoped_envelope(_ORIGINAL_SCOPE)
    assert ignored.touched is False


def test_r11_b2_repeated_builder_ignores_preallocated_large_shared_adjudication_data():
    allocation_ceiling = 8 * V.HMAC_RESOURCE_LIMITS["max_canonical_payload_bytes"]
    shared_ignored = bytearray(b"s" * (2 * allocation_ceiling))
    adjudication = _authoritative_adjudication(_ORIGINAL_SCOPE)
    for field in (
        "expected",
        "causal_confidence",
        "source_refs",
        "source_event_refs",
    ):
        adjudication[field] = shared_ignored
    builder = _scoped_builder()
    source = _source_input()

    result = None
    tracemalloc.start()
    tracemalloc.reset_peak()
    try:
        for _iteration in range(64):
            result = builder(source, adjudication)
        _current_bytes, peak_bytes = tracemalloc.get_traced_memory()
    finally:
        tracemalloc.stop()

    assert result == _reference_scoped_envelope(_ORIGINAL_SCOPE)
    assert all(
        adjudication[field] is shared_ignored
        for field in (
            "expected",
            "causal_confidence",
            "source_refs",
            "source_event_refs",
        )
    )
    assert peak_bytes <= allocation_ceiling


def test_r11_b2_scoped_builder_keeps_private_material_warning_explicit():
    wording = inspect.getdoc(V.source_record_identity_envelope) or ""
    assert "PRIVATE pre-HMAC" in wording
    assert "must not be persisted" in wording
    assert "only the typed HMAC reference is public" in wording


def test_r11_b2_source_event_metadata_ordinal_and_caller_scope_cannot_fold_into_scope():
    builder = _scoped_builder()
    baseline = builder(_source_input(), _authoritative_adjudication(_ORIGINAL_SCOPE))
    for field, replacement in (
        ("event", "synthetic-mutated-event"),
        ("unit", "synthetic-mutated-unit"),
        ("location", "synthetic-mutated-location"),
        ("source_event_refs", ["synthetic-mutated-event-ref"]),
        ("ordinal", 88),
        ("scope_ref", _SUPPLEMENTAL_SCOPE),
        ("scope_salt", "synthetic-mutated-salt"),
    ):
        source = _source_input()
        source[field] = replacement
        assert builder(source, _authoritative_adjudication(_ORIGINAL_SCOPE)) == baseline


def test_r11_b2_same_source_two_scope_vectors_pin_payload_frame_mac_and_manifest():
    vectors = []
    for name, scope_ref in (
        (
            "source_record_adjudication_scoped_semantic_v2_original",
            _VECTOR_ORIGINAL_SCOPE,
        ),
        (
            "source_record_adjudication_scoped_semantic_v2_supplemental",
            _VECTOR_SUPPLEMENTAL_SCOPE,
        ),
    ):
        expected = _reference_vector(name, scope_ref)
        pin = _SCOPED_VECTOR_PINS[scope_ref]
        payload = bytes.fromhex(expected["payload_hex"])
        frame = bytes.fromhex(expected["frame_hex"])
        assert len(payload) == pin["payload_len"]
        assert hashlib.sha256(payload).hexdigest() == pin["payload_sha256"]
        assert len(frame) == pin["frame_len"]
        assert hashlib.sha256(frame).hexdigest() == pin["frame_sha256"]
        assert expected["mac_sha256"] == pin["mac_sha256"]

        envelope = _reference_scoped_envelope(scope_ref)
        assert V.hmac_payload_bytes("source_record", envelope) == payload
        assert V.hmac_frame_bytes("source_record", envelope) == frame
        vectors.append(expected)

    assert vectors[0]["payload_hex"] != vectors[1]["payload_hex"]
    assert vectors[0]["mac_sha256"] != vectors[1]["mac_sha256"]
    manifest = [*copy.deepcopy(R9._V3_VECTOR_RECEIPTS[:3]), *vectors]
    encoded = json.dumps(
        manifest, ensure_ascii=True, separators=(",", ":"), sort_keys=True
    ).encode("utf-8")
    assert hashlib.sha256(encoded).hexdigest() == _VECTOR_SET_SHA256


@pytest.mark.parametrize(
    "mutation",
    ["missing_scope", "wrong_scope_type", "malformed_scope", "old_profile"],
)
def test_r11_b2_closed_source_envelope_rejects_scope_or_profile_downgrade(mutation):
    envelope = _reference_scoped_envelope(_ORIGINAL_SCOPE)
    assert V.hmac_payload_bytes("source_record", envelope) == (
        _reference_canonical_bytes(envelope)
    )
    if mutation == "missing_scope":
        envelope.pop("scope_ref")
    elif mutation == "wrong_scope_type":
        envelope["scope_ref"] = ["synthetic"]
    elif mutation == "malformed_scope":
        envelope["scope_ref"] = "orig:synthetic-invalid"
    else:
        envelope["profile"] = "smartkey-g0-source-record-semantic-v1"
    _assert_hmac_contract_error(lambda: V.hmac_payload_bytes("source_record", envelope))


def test_r11_b2_builder_rejects_canonical_but_unratified_scope():
    unratified = "orig:0000000000000000"
    assert unratified not in R10._CANONICAL_ORIGINAL_REFS
    _assert_hmac_contract_error(
        lambda: _scoped_builder()(
            _source_input(),
            {
                **_authoritative_adjudication(_ORIGINAL_SCOPE),
                "ref": unratified,
            },
        )
    )


@pytest.mark.parametrize(
    "mutation",
    [
        "missing_grain",
        "missing_ref",
        "partial_mapping",
        "none_adjudication",
        "bare_ref",
        "wrong_grain_type",
        "wrong_ref_type",
        "grain_subclass",
        "ref_subclass",
        "malformed_ref",
        "wrong_grain",
        "wrong_domain",
        "unratified_ref",
        "oversized_ref",
    ],
)
def test_r11_b2_authoritative_builder_rejects_non_authoritative_scope_inputs(
    mutation,
):
    mapping = _authoritative_adjudication(_ORIGINAL_SCOPE)
    if mutation == "missing_grain":
        mapping.pop("grain")
    elif mutation == "missing_ref":
        mapping.pop("ref")
    elif mutation == "partial_mapping":
        mapping = {"grain": mapping["grain"], "ref": mapping["ref"]}
    elif mutation == "none_adjudication":
        mapping = None
    elif mutation == "bare_ref":
        mapping = _ORIGINAL_SCOPE
    elif mutation == "wrong_grain_type":
        mapping["grain"] = ["original_candidate"]
    elif mutation == "wrong_ref_type":
        mapping["ref"] = [_ORIGINAL_SCOPE]
    elif mutation == "grain_subclass":

        class GrainAlias(str):
            pass

        mapping["grain"] = GrainAlias("original_candidate")
    elif mutation == "ref_subclass":

        class RefAlias(str):
            pass

        mapping["ref"] = RefAlias(_ORIGINAL_SCOPE)
    elif mutation == "malformed_ref":
        mapping["ref"] = "orig:synthetic-invalid"
    elif mutation == "wrong_grain":
        mapping["grain"] = "supplemental_hypothesis"
    elif mutation == "wrong_domain":
        mapping["ref"] = R10._synthetic_hmac_ref("event", "scope-domain")
    elif mutation == "unratified_ref":
        mapping["ref"] = "orig:0000000000000000"
        assert mapping["ref"] not in R10._CANONICAL_ORIGINAL_REFS
    else:
        mapping["ref"] = "orig:" + "a" * 70_000
    _assert_hmac_contract_error(lambda: _scoped_builder()(_source_input(), mapping))


def test_r11_b2_authoritative_builder_rejects_mapping_and_source_alias_subclasses():
    marker = "synthetic-alias-marker"

    class MappingAlias(dict):
        def __init__(self, value):
            dict.__init__(self, value)
            dict.__setitem__(self, "synthetic_marker", marker)
            self.touched = False

        def _trip(self, *_args, **_kwargs):
            self.touched = True
            raise AssertionError(marker)

        __contains__ = _trip
        __deepcopy__ = _trip
        __getitem__ = _trip
        __iter__ = _trip
        __len__ = _trip
        copy = _trip
        get = _trip
        items = _trip
        keys = _trip
        values = _trip

    builder = _scoped_builder()
    adjudication_alias = MappingAlias(_authoritative_adjudication(_ORIGINAL_SCOPE))
    _assert_hmac_contract_error(
        lambda: builder(_source_input(), adjudication_alias),
        marker,
    )
    assert adjudication_alias.touched is False

    source_alias = MappingAlias(_source_input())
    _assert_hmac_contract_error(
        lambda: builder(
            source_alias,
            _authoritative_adjudication(_ORIGINAL_SCOPE),
        ),
        marker,
    )
    assert source_alias.touched is False


@pytest.mark.parametrize("field", ["grain", "ref"])
def test_r11_b2_authoritative_builder_rejects_hostile_string_aliases(field):
    marker = f"synthetic-hostile-{field}-marker"

    class HostileStr(str):
        def __new__(cls, value):
            instance = str.__new__(cls, value)
            instance.marker = marker
            instance.touched = False
            return instance

        def _trip(self, *_args, **_kwargs):
            self.touched = True
            raise AssertionError(marker)

        __eq__ = _trip
        __hash__ = _trip
        __len__ = _trip
        __repr__ = _trip
        __str__ = _trip
        encode = _trip
        startswith = _trip

    adjudication = _authoritative_adjudication(_ORIGINAL_SCOPE)
    value = "original_candidate" if field == "grain" else _ORIGINAL_SCOPE
    alias = HostileStr(value)
    adjudication[field] = alias

    _assert_hmac_contract_error(
        lambda: _scoped_builder()(_source_input(), adjudication),
        marker,
    )
    assert alias.touched is False


def test_r11_b2_direct_nonsecret_vector_encoder_accepts_fixed_synthetic_scopes_only():
    for scope_ref in (_VECTOR_ORIGINAL_SCOPE, _VECTOR_SUPPLEMENTAL_SCOPE):
        assert scope_ref not in R10._CANONICAL_ADJUDICATION_REFS
        envelope = _reference_scoped_envelope(scope_ref)
        assert V.hmac_payload_bytes("source_record", envelope) == (
            _reference_canonical_bytes(envelope)
        )


@pytest.mark.parametrize("same_record", [True, False])
def test_r11_b2_public_source_record_ref_is_globally_one_adjudication_only(
    same_record,
):
    doc = _r11_doc()
    first = doc["records"][0]["adjudications"][0]["source_refs"][0]["ref"]
    target = (
        doc["records"][0]["adjudications"][1]
        if same_record
        else doc["records"][1]["adjudications"][0]
    )
    target["source_refs"][0]["ref"] = first
    assert _SOURCE_SCOPE_ERROR in _codes(V.validate_document(doc, R10._schema()))


def test_r11_b2_public_fragment_cap_remains_exactly_eight_without_origin_exemption():
    assert V.MAX_RECONSTRUCTION_FRAGMENTS_PER_SOURCE == 8
    doc = R10._synthetic_r2_doc()
    record = doc["records"][0]
    observed = V.normalize(record["observed"]["token"])
    expected = V.normalize(record["expected"]["token"])
    fragments = V._payload_fragments(record)
    assert {observed, expected} <= fragments
    record["record_origin"] = "new"
    assert {observed, expected} <= V._payload_fragments(record)


def _set_total_fragment_count(record: dict, total: int) -> None:
    labels = (
        "fragmentalpha",
        "fragmentbeta",
        "fragmentgamma",
        "fragmentdelta",
        "fragmentepsilon",
        "fragmentzeta",
        "fragmenteta",
        "fragmenttheta",
        "fragmentiota",
    )
    base = len(V._payload_fragments(record))
    assert base <= total <= base + len(labels)
    record["environment"]["flags"] = {label: "off" for label in labels[: total - base]}
    assert len(V._payload_fragments(record)) == total


@pytest.mark.parametrize("record_index", [0, 20])
@pytest.mark.parametrize("fragment_count", [8, 9])
def test_r11_b2_end_to_end_cap_is_eight_for_baseline_and_new_records(
    record_index, fragment_count
):
    # The cap is deliberately unchanged from R10.  Use the schema-valid R10
    # fixture so this guard cannot pass through an R11 schema short-circuit.
    doc = R10._synthetic_r2_doc()
    _set_total_fragment_count(doc["records"][record_index], fragment_count)
    codes = _codes(V.validate_document(doc, R10._schema()))
    assert ("E_PRIV_AGGREGATE" in codes) is (fragment_count == 9)


def test_r11_b2_dynamic_non_hmac_fragments_consume_budget():
    doc = R10._synthetic_r2_doc()
    record = doc["records"][20]
    _set_total_fragment_count(record, 9)
    assert all(
        not key.startswith(V.HMAC_SCHEME) for key in record["environment"]["flags"]
    )
    assert "E_PRIV_AGGREGATE" in _codes(V.validate_document(doc, R10._schema()))


def test_r11_b2_two_unique_scoped_refs_are_independently_bounded():
    doc = R10._synthetic_r2_doc()
    _set_total_fragment_count(doc["records"][20], 8)
    _set_total_fragment_count(doc["records"][21], 8)
    refs = {
        doc["records"][index]["adjudications"][0]["source_refs"][0]["ref"]
        for index in (20, 21)
    }
    assert len(refs) == 2
    assert "E_PRIV_AGGREGATE" not in _codes(V.validate_document(doc, R10._schema()))


def test_r11_b2_external_receipt_binds_all_scoped_placements_and_crosswalk():
    doc = _r11_doc()
    contract = doc["adjudication_contract"]
    receipt = contract["hmac_external_receipt"]
    placement = _reference_placement_projection(doc)
    assert receipt["source_scope_binding_count"] == len(placement["placements"]) == 159
    assert receipt["source_scope_policy_version"] == _FRAGMENT_POLICY
    assert receipt["mapping_crosswalk_sha256"] == contract["mapping_crosswalk_sha256"]
    assert receipt["crosswalk_projection_profile"] == _CROSSWALK_PROFILE
    assert receipt["source_scope_placement_profile"] == _PLACEMENT_PROFILE
    assert (
        receipt["source_scope_placement_sha256"]
        == contract["source_scope_placement_sha256"]
    )
    assert (
        receipt["semantic_commitment_sha256"] == contract["semantic_commitment_sha256"]
    )
    assert receipt["audit_contract"] == _PRESEAL_AUDIT_CONTRACT
    assert receipt["registry_schema_version"] == 1


def test_r11_b2_scoped_placement_projection_is_closed_sorted_and_pinned():
    doc = _r11_doc()
    projection = _placement_builder()(doc)
    assert set(projection) == {"profile", "placements"}
    assert projection["profile"] == _PLACEMENT_PROFILE
    assert projection == _reference_placement_projection(doc)
    assert len(projection["placements"]) == 159
    assert projection["placements"] == sorted(
        projection["placements"],
        key=lambda item: (item["adjudication_ref"], item["source_record_ref"]),
    )
    assert all(
        set(item) == {"adjudication_ref", "source_record_ref"}
        for item in projection["placements"]
    )
    assert _placement_digest()(doc) == _reference_placement_sha256(doc)
    assert _placement_digest()(doc) == _PLACEMENT_SHA256


@pytest.mark.parametrize("mutation", ["zero", "two"])
def test_r11_b2_each_mapping_requires_exactly_one_source_record_placement(mutation):
    doc = _r11_doc()
    sources = doc["records"][0]["adjudications"][0]["source_refs"]
    if mutation == "zero":
        sources[:] = [source for source in sources if source["kind"] != "source_record"]
    else:
        sources.append(
            {
                "kind": "source_record",
                "ref": R10._synthetic_hmac_ref("source_record", "second-placement"),
            }
        )
    with pytest.raises(ValueError) as caught:
        _placement_builder()(doc)
    assert caught.value.args == (_SOURCE_PLACEMENT_ERROR,)
    assert caught.value.__cause__ is None
    assert caught.value.__context__ is None


@pytest.mark.parametrize(
    "reuse",
    ["record_record", "record_exclusion", "exclusion_exclusion"],
)
def test_r11_b2_source_record_ref_reuse_fails_across_every_target_kind(reuse):
    doc = _r11_doc()
    record_a = doc["records"][0]["adjudications"][0]
    record_b = doc["records"][1]["adjudications"][0]
    exclusion_a, exclusion_b = doc["source_exclusions"]

    def source_ref(item: dict) -> dict:
        return next(
            source
            for source in item["source_refs"]
            if source["kind"] == "source_record"
        )

    if reuse == "record_record":
        source_ref(record_b)["ref"] = source_ref(record_a)["ref"]
    elif reuse == "record_exclusion":
        source_ref(exclusion_a)["ref"] = source_ref(record_a)["ref"]
    else:
        source_ref(exclusion_b)["ref"] = source_ref(exclusion_a)["ref"]
    assert _SOURCE_SCOPE_ERROR in _codes(V.validate_document(doc, R10._schema()))
    with pytest.raises(ValueError, match=f"^{_SOURCE_PLACEMENT_ERROR}$"):
        _placement_builder()(doc)


def test_r11_b2_source_ref_substitution_changes_placement_and_invalidates_receipt():
    doc = _r11_doc()
    baseline = _placement_digest()(doc)
    source = doc["records"][0]["adjudications"][0]["source_refs"][0]
    source["ref"] = R10._synthetic_hmac_ref("source_record", "placement-substitute")
    assert _placement_digest()(doc) != baseline
    codes = _codes(V.validate_document(doc, R10._schema()))
    assert _SOURCE_PLACEMENT_ERROR in codes
    assert "E_HMAC_CONTRACT" in codes


def test_r11_b2_allowed_shared_event_and_metadata_refs_do_not_define_source_scope():
    doc = _r11_doc()
    first, second = doc["records"][0]["adjudications"][:2]
    baseline = _placement_builder()(doc)
    second["source_event_refs"] = copy.deepcopy(first["source_event_refs"])
    shared_metadata = R10._synthetic_hmac_ref("metadata", "shared-evidence")
    first["source_refs"].append({"kind": "other", "ref": shared_metadata})
    second["source_refs"].append({"kind": "other", "ref": shared_metadata})
    assert _placement_builder()(doc) == baseline
    assert _SOURCE_SCOPE_ERROR not in _codes(V.validate_document(doc, R10._schema()))


_HMAC_RECEIPT_BOUND_FIELDS = (
    "contract_version",
    "vector_set_sha256",
    "coverage",
    "key_id",
    "source_scope_policy_version",
    "crosswalk_projection_profile",
    "source_scope_placement_profile",
    "audit_contract",
    "registry_schema_version",
    "source_scope_binding_count",
    "mapping_crosswalk_sha256",
    "source_scope_placement_sha256",
    "semantic_commitment_sha256",
)

_HMAC_RECEIPT_DIGEST_FIELDS = frozenset(
    {
        "vector_set_sha256",
        "mapping_crosswalk_sha256",
        "source_scope_placement_sha256",
        "semantic_commitment_sha256",
    }
)


@pytest.mark.parametrize("field", _HMAC_RECEIPT_BOUND_FIELDS)
@pytest.mark.parametrize("mutation", ["missing", "null", "wrong_type", "wrong_value"])
def test_r11_b2_external_receipt_fails_closed_for_every_bound_field(field, mutation):
    doc = _hmac_receipt_probe_doc()
    assert "E_HMAC_CONTRACT" not in _isolated_hmac_preflight_codes(doc)
    receipt = doc["adjudication_contract"]["hmac_external_receipt"]
    if mutation == "missing":
        receipt.pop(field)
    elif mutation == "null":
        receipt[field] = None
    elif mutation == "wrong_type":
        receipt[field] = []
    elif field in {"registry_schema_version", "source_scope_binding_count"}:
        receipt[field] += 1
    elif field in _HMAC_RECEIPT_DIGEST_FIELDS:
        receipt[field] = "a" * 64
    else:
        receipt[field] = "synthetic-wrong"
    assert "E_HMAC_CONTRACT" in _isolated_hmac_preflight_codes(doc)


def test_r11_b2_external_receipt_rejects_caller_scope_override():
    doc = _hmac_receipt_probe_doc()
    assert "E_HMAC_CONTRACT" not in _isolated_hmac_preflight_codes(doc)
    doc["adjudication_contract"]["hmac_external_receipt"]["scope_override"] = (
        _ORIGINAL_SCOPE
    )
    assert "E_HMAC_CONTRACT" in _isolated_hmac_preflight_codes(doc)


def test_r11_b2_external_receipt_rejects_stale_source_profile():
    doc = _hmac_receipt_probe_doc()
    assert "E_HMAC_CONTRACT" not in _isolated_hmac_preflight_codes(doc)
    contract = doc["adjudication_contract"]
    contract["hmac_domain_payload_profiles"]["source_record"] = (
        "smartkey-g0-source-record-semantic-v1"
    )
    assert "E_HMAC_CONTRACT" in _isolated_hmac_preflight_codes(doc)


# ------------------------------------------------------------------ B3 and pre-seal crosswalk


@pytest.mark.parametrize(
    ("name", "expected"),
    [
        ("ORIGINAL_PRESENCE_DERIVATION_VERSION", _PRESENCE_POLICY),
        ("ORIGINAL_PRESENCE_DERIVATION_RULING_REF", _PRESENCE_RULING_REF),
    ],
)
def test_r11_b3_presence_policy_constants_are_closed_and_pinned(name, expected):
    _required_constant(name, expected)


def test_r11_b3_private_presence_receipt_fixture_is_independently_pinned():
    doc = _r11_doc()
    contract = doc["adjudication_contract"]
    assert contract["original_presence_derivation_version"] == _PRESENCE_POLICY
    assert contract["original_presence_derivation_ruling_ref"] == (_PRESENCE_RULING_REF)
    original_refs = sorted(
        item["ref"]
        for record in doc["records"]
        for item in record.get("adjudications", ())
        if item["grain"] == "original_candidate"
    )
    original_refs.extend(
        item["ref"]
        for item in doc["source_exclusions"]
        if item["grain"] == "original_candidate"
    )
    original_refs.sort()
    assert len(original_refs) == 106
    assert (
        hashlib.sha256(_reference_canonical_bytes(original_refs)).hexdigest()
        == _ORIGINAL_REF_SET_SHA256
    )
    assert contract["original_ref_set_sha256"] == _ORIGINAL_REF_SET_SHA256
    assert contract["original_presence_external_receipt"] == {
        "state": "externally_verified",
        "policy_version": _PRESENCE_POLICY,
        "ruling_ref": _PRESENCE_RULING_REF,
        "original_count": 106,
        "derived_presence": "high",
        "original_ref_set_sha256": _ORIGINAL_REF_SET_SHA256,
        "crosswalk_projection_profile": _CROSSWALK_PROFILE,
        "target_binding_crosswalk_sha256": contract["mapping_crosswalk_sha256"],
        "audit_contract": _PRESEAL_AUDIT_CONTRACT,
        "receipt_sha256": _PRESENCE_RECEIPT_SHA256,
    }


def test_r11_b3_schema_no_longer_requires_caller_presence_on_original_mapping():
    doc = _r11_doc()
    required = R10._schema()["$defs"]["adjudication"]["properties"][
        "causal_confidence"
    ]["required"]
    assert "anomaly_or_guard_presence" not in required
    errors = list(Draft202012Validator(R10._schema()).iter_errors(doc))
    assert errors == []


def test_r11_b3_original_presence_is_derived_high_in_semantic_commitment():
    doc = _r11_doc()
    original_ref = doc["records"][0]["adjudications"][0]["ref"]
    item = next(
        value
        for value in V.semantic_commitment_payload(doc)
        if value["ref"] == original_ref
    )
    assert item["causal_confidence"]["anomaly_or_guard_presence"] == "high"


@pytest.mark.parametrize("value", V.CAUSAL_CONFIDENCES)
def test_r11_b3_caller_cannot_inject_any_original_presence_value(value):
    doc = _r11_doc()
    original = doc["records"][0]["adjudications"][0]
    original["causal_confidence"]["anomaly_or_guard_presence"] = value
    _assert_presence_contract_error(lambda: V.semantic_commitment_payload(doc))
    assert _PRESENCE_ERROR in _codes(V.validate_document(doc, R10._schema()))


def test_r11_b3_supplemental_presence_remains_explicit_and_required():
    doc = _r11_doc()
    supplemental = next(
        item
        for record in doc["records"]
        for item in record.get("adjudications", ())
        if item["grain"] == "supplemental_hypothesis"
    )
    supplemental["causal_confidence"].pop("anomaly_or_guard_presence")
    _assert_presence_contract_error(lambda: V.semantic_commitment_payload(doc))
    assert _PRESENCE_ERROR in _codes(V.validate_document(doc, R10._schema()))


def test_r11_b3_supplemental_explicit_presence_is_preserved_verbatim():
    doc = _r11_doc()
    supplemental = next(
        item
        for record in doc["records"]
        for item in record.get("adjudications", ())
        if item["grain"] == "supplemental_hypothesis"
    )
    supplemental["causal_confidence"]["anomaly_or_guard_presence"] = "medium"
    projected = next(
        item
        for item in V.semantic_commitment_payload(doc)
        if item["ref"] == supplemental["ref"]
    )
    assert projected["causal_confidence"]["anomaly_or_guard_presence"] == "medium"


def test_r11_b3_projection_does_not_mutate_input_or_raise_other_confidence_axes():
    doc = _r11_doc()
    before = copy.deepcopy(doc)
    original = doc["records"][0]["adjudications"][0]
    projected = next(
        item
        for item in V.semantic_commitment_payload(doc)
        if item["ref"] == original["ref"]
    )
    assert doc == before
    assert projected["causal_confidence"]["anomaly_or_guard_presence"] == "high"
    for axis in ("expected_form", "runtime_mechanism", "smartkey_attribution"):
        assert (
            projected["causal_confidence"][axis] == original["causal_confidence"][axis]
        )


_PRESENCE_RECEIPT_BOUND_FIELDS = (
    "state",
    "policy_version",
    "ruling_ref",
    "original_count",
    "derived_presence",
    "original_ref_set_sha256",
    "crosswalk_projection_profile",
    "target_binding_crosswalk_sha256",
    "audit_contract",
)


@pytest.mark.parametrize("field", _PRESENCE_RECEIPT_BOUND_FIELDS)
@pytest.mark.parametrize("mutation", ["missing", "null", "wrong_type", "wrong_value"])
def test_r11_b3_presence_receipt_fails_closed_for_every_authority_field(
    field, mutation
):
    doc = _r11_doc()
    receipt = doc["adjudication_contract"]["original_presence_external_receipt"]
    if mutation == "missing":
        receipt.pop(field)
    elif mutation == "null":
        receipt[field] = None
    elif mutation == "wrong_type":
        receipt[field] = []
    elif field == "original_count":
        receipt[field] += 1
    elif field == "derived_presence":
        receipt[field] = "medium"
    else:
        receipt[field] = "a" * 64
    _assert_presence_contract_error(lambda: V.semantic_commitment_payload(doc))
    assert _PRESENCE_ERROR in _codes(V.validate_document(doc, R10._schema()))


def test_r11_b3_presence_receipt_is_closed_against_caller_override():
    doc = _r11_doc()
    receipt = doc["adjudication_contract"]["original_presence_external_receipt"]
    receipt["presence_override"] = "high"
    _assert_presence_contract_error(lambda: V.semantic_commitment_payload(doc))
    assert _PRESENCE_ERROR in _codes(V.validate_document(doc, R10._schema()))


@pytest.mark.parametrize(
    ("mutation", "replacement"),
    [
        ("missing", None),
        ("null", None),
        ("wrong_type", []),
        ("zero", "0" * 64),
        ("malformed_nonzero", "g" * 64),
        (
            "wrong_domain",
            _synthetic_presence_receipt_sha256("other-domain", "epoch-v1"),
        ),
        (
            "wrong_epoch",
            _synthetic_presence_receipt_sha256("original-presence", "epoch-v2"),
        ),
        (
            "valid_digest_substitution",
            _synthetic_presence_receipt_sha256(
                "original-presence", "epoch-v1", variant="substituted"
            ),
        ),
    ],
)
def test_r11_b3_presence_receipt_sha_is_exact_and_cannot_be_substituted(
    mutation, replacement
):
    doc = _r11_doc()
    receipt = doc["adjudication_contract"]["original_presence_external_receipt"]
    assert receipt["receipt_sha256"] == _PRESENCE_RECEIPT_SHA256
    assert _PRESENCE_RECEIPT_SHA256 == _synthetic_presence_receipt_sha256(
        "original-presence", "epoch-v1"
    )
    assert V.DIGEST_NONZERO_RE.fullmatch(_PRESENCE_RECEIPT_SHA256) is not None
    if mutation == "missing":
        receipt.pop("receipt_sha256")
    else:
        receipt["receipt_sha256"] = replacement
    _assert_presence_contract_error(lambda: V.semantic_commitment_payload(doc))
    assert _PRESENCE_ERROR in _codes(V.validate_document(doc, R10._schema()))


@pytest.mark.parametrize("mutation", ["missing", "duplicate"])
def test_r11_b3_original_presence_fails_closed_without_exactly_once_proof(mutation):
    doc = _r11_doc()
    if mutation == "missing":
        doc["records"][0]["adjudications"].pop(0)
    else:
        original = copy.deepcopy(doc["records"][0]["adjudications"][0])
        doc["records"][1]["adjudications"].append(original)
    _assert_presence_contract_error(lambda: V.semantic_commitment_payload(doc))
    assert _PRESENCE_ERROR in _codes(V.validate_document(doc, R10._schema()))


def test_r11_b3_same_original_ref_set_relocated_to_another_target_fails_receipt():
    doc = _r11_doc()
    baseline_ref_set = _reference_original_ref_set_sha256(doc)
    baseline_crosswalk = _reference_crosswalk_sha256(doc)
    moved = doc["records"][0]["adjudications"].pop(0)
    assert moved["grain"] == "original_candidate"
    doc["records"][1]["adjudications"].append(moved)
    assert _reference_original_ref_set_sha256(doc) == baseline_ref_set
    assert _reference_crosswalk_sha256(doc) != baseline_crosswalk
    _assert_presence_contract_error(lambda: V.semantic_commitment_payload(doc))
    assert _PRESENCE_ERROR in _codes(V.validate_document(doc, R10._schema()))


def test_r11_b3_target_binding_receipt_mismatch_fails_with_exact_ref_set():
    doc = _r11_doc()
    assert _reference_original_ref_set_sha256(doc) == _ORIGINAL_REF_SET_SHA256
    receipt = doc["adjudication_contract"]["original_presence_external_receipt"]
    receipt["target_binding_crosswalk_sha256"] = "a" * 64
    _assert_presence_contract_error(lambda: V.semantic_commitment_payload(doc))
    assert _PRESENCE_ERROR in _codes(V.validate_document(doc, R10._schema()))


def test_r11_crosswalk_projection_is_closed_target_binding_and_independently_pinned():
    doc = _r11_doc()
    projection = _crosswalk_builder()(doc)
    assert set(projection) == {"profile", "groups"}
    assert projection["profile"] == _CROSSWALK_PROFILE
    assert projection == _reference_crosswalk_projection(doc)
    assert _crosswalk_digest()(doc) == _reference_crosswalk_sha256(doc)
    assert _crosswalk_digest()(doc) == _CROSSWALK_SHA256
    assert len(projection["groups"]) == 104
    assert sum(len(group["mapping_refs"]) for group in projection["groups"]) == 159
    assert projection["groups"] == sorted(
        projection["groups"],
        key=lambda group: (
            group["target_kind"],
            group["target_id"],
            tuple(group["mapping_refs"]),
        ),
    )
    record_ids = {record["id"] for record in doc["records"]}
    for group in projection["groups"]:
        assert set(group) == {"target_kind", "target_id", "mapping_refs"}
        assert group["target_kind"] in {"record", "source_exclusion"}
        # target_id is discriminated by target_kind: a public record ID for a
        # record group, or the stable public mapping identity for an exclusion.
        if group["target_kind"] == "record":
            assert group["target_id"] in record_ids
        else:
            assert len(group["mapping_refs"]) == 1
            assert group["target_id"] == (
                f"source-exclusion:{group['mapping_refs'][0]}"
            )
        assert group["mapping_refs"] == sorted(group["mapping_refs"])
    encoded = json.dumps(projection, sort_keys=True)
    for forbidden in (
        "target_record_id",
        "source_refs",
        "source_event_refs",
        "semantic_commitment_sha256",
        "semantic_commitment",
        "registry_projection",
        "hmac-sha256",
        "raw_sha256",
        "mapping_crosswalk_sha256",
    ):
        assert forbidden not in encoded


def test_r11_crosswalk_projection_ignores_self_pin_order_and_non_grouping_fields():
    doc = _r11_doc()
    builder = _crosswalk_builder()
    baseline = builder(doc)
    mutated = copy.deepcopy(doc)
    mutated["adjudication_contract"]["mapping_crosswalk_sha256"] = "a" * 64
    mutated["adjudication_contract"]["semantic_commitment_sha256"] = "b" * 64
    mutated["adjudication_contract"]["registry_projection_sha256"] = "c" * 64
    mutated["records"].reverse()
    for record in mutated["records"]:
        record.get("adjudications", []).reverse()
    item = next(
        item
        for record in mutated["records"]
        for item in record.get("adjudications", ())
    )
    item["source_refs"] = [
        {
            "kind": "source_record",
            "ref": R10._synthetic_hmac_ref("source_record", "crosswalk-decoy"),
        }
    ]
    item["source_event_refs"] = [R10._synthetic_hmac_ref("event", "crosswalk-decoy")]
    assert builder(mutated) == baseline
    assert _crosswalk_digest()(mutated) == _crosswalk_digest()(doc)


@pytest.mark.parametrize("relocation", ["individual", "whole_group"])
def test_r11_crosswalk_projection_changes_when_mapping_target_changes(relocation):
    doc = _r11_doc()
    baseline = _crosswalk_digest()(doc)
    if relocation == "individual":
        moved = doc["records"][0]["adjudications"].pop()
        doc["records"][1]["adjudications"].append(moved)
    else:
        moved = doc["records"][0]["adjudications"]
        doc["records"][0]["adjudications"] = []
        doc["records"][1]["adjudications"].extend(moved)
    assert _crosswalk_digest()(doc) != baseline


def test_r11_crosswalk_projection_binds_safe_target_record_identity():
    doc = _r11_doc()
    baseline = _crosswalk_digest()(doc)
    first = doc["records"][0]
    second = doc["records"][1]
    first["id"], second["id"] = second["id"], first["id"]
    assert _crosswalk_digest()(doc) != baseline


def test_r11_crosswalk_caller_target_id_injection_is_ignored_and_rejected():
    doc = _r11_doc()
    baseline = _crosswalk_builder()(doc)
    doc["records"][0]["adjudications"][0]["target_id"] = "anom-000000000000"
    assert _crosswalk_builder()(doc) == baseline
    assert "E_SCHEMA" in _codes(V.validate_document(doc, R10._schema()))


@pytest.mark.parametrize("mutation", ["missing", "duplicate", "unratified"])
def test_r11_crosswalk_projection_fails_closed_on_invalid_ref_inventory(mutation):
    doc = _r11_doc()
    if mutation == "missing":
        doc["records"][0]["adjudications"].pop(0)
    elif mutation == "duplicate":
        doc["records"][1]["adjudications"].append(
            copy.deepcopy(doc["records"][0]["adjudications"][0])
        )
    else:
        doc["records"][0]["adjudications"][0]["ref"] = "orig:0000000000000000"
    with pytest.raises(ValueError) as caught:
        _crosswalk_builder()(doc)
    assert caught.value.args == (_CROSSWALK_ERROR,)
    assert caught.value.__cause__ is None
    assert caught.value.__context__ is None


def test_r11_crosswalk_digest_cannot_be_arbitrary_or_locally_resealed():
    doc = _r11_doc()
    doc["adjudication_contract"]["mapping_crosswalk_sha256"] = "a" * 64
    doc["adjudication_contract"]["registry_projection_sha256"] = (
        V.registry_projection_sha256(doc)
    )
    codes = _codes(V.validate_document(doc, R10._schema()))
    assert "E_ADJ_CROSSWALK" in codes
    assert _IMPORT_HOLD in codes


def test_r11_receipt_binds_semantic_crosswalk_and_placement_commitments():
    mutations = (
        "semantic_commitment_sha256",
        "mapping_crosswalk_sha256",
        "source_scope_placement_sha256",
    )
    for field in mutations:
        doc = _hmac_receipt_probe_doc()
        assert "E_HMAC_CONTRACT" not in _isolated_hmac_preflight_codes(doc)
        doc["adjudication_contract"][field] = "a" * 64
        assert "E_HMAC_CONTRACT" in _isolated_hmac_preflight_codes(doc)


def test_r11_enhanced_candidate_is_unconditionally_held_after_other_checks_pass():
    errors = V.validate_document(_r11_doc(), R10._schema())
    assert len(errors) == 1
    assert errors[0].startswith(f"{_IMPORT_HOLD}:")


def test_r11a_preseal_staging_cannot_be_mistaken_for_final_r3_authority():
    doc = _r11_doc()
    contract = doc["adjudication_contract"]
    # Ruling 7af114092d26: R11a binds the preparatory audit and a
    # self-derived synthetic algorithm digest, but HOLD remains unconditional.
    # Only R11b may hard-pin the privately synthesized final digest and remove it.
    assert contract["audit_contract"] == _PRESEAL_AUDIT_CONTRACT
    assert contract["mapping_crosswalk_sha256"] == _reference_crosswalk_sha256(doc)
    assert contract["mapping_crosswalk_sha256"] == _CROSSWALK_SHA256
    assert _IMPORT_HOLD in _codes(V.validate_document(doc, R10._schema()))


@pytest.mark.parametrize(
    "mutation",
    [
        "missing_contract",
        "null_contract",
        "wrong_contract_type",
        "empty_contract",
        "missing_hmac_receipt",
        "malformed_hmac_receipt",
        "stale_r2_pins",
        "future_pin",
        "invalid_count",
        "invalid_ref",
    ],
)
def test_r11_import_hold_survives_malformed_or_untrusted_enhanced_contract(mutation):
    doc = _r11_doc()
    contract = doc["adjudication_contract"]
    if mutation == "missing_contract":
        doc.pop("adjudication_contract")
    elif mutation == "null_contract":
        doc["adjudication_contract"] = None
    elif mutation == "wrong_contract_type":
        doc["adjudication_contract"] = []
    elif mutation == "empty_contract":
        doc["adjudication_contract"] = {}
    elif mutation == "missing_hmac_receipt":
        contract.pop("hmac_external_receipt")
    elif mutation == "malformed_hmac_receipt":
        contract["hmac_external_receipt"] = []
    elif mutation == "stale_r2_pins":
        contract["audit_contract"] = "g0-r2-security-crosswalk"
        contract["hmac_contract_version"] = "smartkey-g0-hmac-byte-contract-v3"
    elif mutation == "future_pin":
        contract["audit_contract"] = "g0-r99-unratified"
    elif mutation == "invalid_count":
        contract["original_count"] += 1
    else:
        assert mutation == "invalid_ref"
        doc["records"][0]["adjudications"][0]["ref"] = "orig:0000000000000000"
    assert _IMPORT_HOLD in _codes(V.validate_document(doc, R10._schema()))


def test_r11_import_hold_survives_real_attacker_relocation_and_local_reseal():
    doc = _r11_doc()
    contract = doc["adjudication_contract"]
    baseline_commitments = {
        field: contract[field]
        for field in (
            "semantic_commitment_sha256",
            "mapping_crosswalk_sha256",
            "source_scope_placement_sha256",
            "registry_projection_sha256",
        )
    }
    moved = doc["records"][0]["adjudications"].pop(0)
    doc["records"][1]["adjudications"].append(moved)

    source = next(
        item for item in moved["source_refs"] if item["kind"] == "source_record"
    )
    original_source_ref = source["ref"]
    used_source_refs = {
        item["ref"]
        for record in doc["records"]
        for adjudication in record.get("adjudications", ())
        for item in adjudication["source_refs"]
        if item["kind"] == "source_record"
    }
    used_source_refs.update(
        item["ref"]
        for adjudication in doc["source_exclusions"]
        for item in adjudication["source_refs"]
        if item["kind"] == "source_record"
    )
    substituted_ref = R10._synthetic_hmac_ref(
        "source_record", "r11-attacker-unique-substitution"
    )
    assert substituted_ref not in used_source_refs
    source["ref"] = substituted_ref
    assert source["ref"] != original_source_ref

    contract["semantic_commitment_sha256"] = _reference_semantic_sha256(doc)
    contract["mapping_crosswalk_sha256"] = _reference_crosswalk_sha256(doc)
    contract["source_scope_placement_sha256"] = _reference_placement_sha256(doc)
    hmac_receipt = contract["hmac_external_receipt"]
    hmac_receipt["semantic_commitment_sha256"] = contract["semantic_commitment_sha256"]
    hmac_receipt["mapping_crosswalk_sha256"] = contract["mapping_crosswalk_sha256"]
    hmac_receipt["source_scope_placement_sha256"] = contract[
        "source_scope_placement_sha256"
    ]
    contract["original_presence_external_receipt"][
        "target_binding_crosswalk_sha256"
    ] = contract["mapping_crosswalk_sha256"]
    contract["legacy_projection_sha256"] = R10._r3_legacy_projection_digest(doc)
    contract["registry_projection_sha256"] = R10._r3_registry_projection_digest(doc)

    assert contract["semantic_commitment_sha256"] == _reference_semantic_sha256(doc)
    assert contract["mapping_crosswalk_sha256"] == _reference_crosswalk_sha256(doc)
    assert contract["source_scope_placement_sha256"] == (
        _reference_placement_sha256(doc)
    )
    assert contract["registry_projection_sha256"] == (
        R10._r3_registry_projection_digest(doc)
    )
    assert all(
        contract[field] != baseline_commitments[field] for field in baseline_commitments
    )
    assert _IMPORT_HOLD in _codes(V.validate_document(doc, R10._schema()))


@pytest.mark.parametrize("enhanced_kind", ["record", "source_exclusion"])
def test_r11_import_hold_applies_to_each_enhanced_container_without_contract(
    enhanced_kind,
):
    doc = R10._doc()
    enhanced = _r11_doc()
    if enhanced_kind == "record":
        doc["records"][0]["adjudications"] = copy.deepcopy(
            enhanced["records"][0]["adjudications"]
        )
    else:
        doc["source_exclusions"] = [copy.deepcopy(enhanced["source_exclusions"][0])]
    assert "adjudication_contract" not in doc
    assert _IMPORT_HOLD in _codes(V.validate_document(doc, R10._schema()))


def test_r11_import_hold_has_no_document_or_contract_bypass():
    for key, value in (
        ("r3_import_hold", False),
        ("allow_import", True),
        ("preseal_override", "synthetic"),
    ):
        doc = _r11_doc()
        doc["adjudication_contract"][key] = value
        codes = _codes(V.validate_document(doc, R10._schema()))
        assert "E_SCHEMA" in codes
        assert _IMPORT_HOLD in codes

    for key, value in (
        ("r3_import_hold", False),
        ("allow_import", True),
        ("preseal_override", "synthetic"),
    ):
        doc = _r11_doc()
        doc[key] = value
        codes = _codes(V.validate_document(doc, R10._schema()))
        assert "E_SCHEMA" in codes
        assert _IMPORT_HOLD in codes


def test_r11_legacy_public_registry_remains_green_without_import_hold():
    errors = V.validate_document(R10._doc(), R10._schema())
    assert errors == []
