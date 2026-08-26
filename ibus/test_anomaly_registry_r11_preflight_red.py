"""R11a RED contracts for sealed tuple, scope, and import authority.

Every fixture in this module is synthetic or reuses the already-public,
deidentified contract fixtures.  No registry data, private source material,
path, timestamp receipt, or key is read.
"""

from __future__ import annotations

import copy
import hashlib
import hmac
import inspect
import json

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
_CROSSWALK_PROFILE = "smartkey-g0-crosswalk-grouping-v1"
_PRESEAL_AUDIT_CONTRACT = "g0-r11a-preseal-crosswalk"
_IMPORT_HOLD = "E_R3_IMPORT_HOLD"
_SOURCE_SCOPE_ERROR = "E_ADJ_SOURCE_SCOPE"
_PRESENCE_ERROR = "E_ADJ_PRESENCE"
_CROSSWALK_ERROR = "E_CROSSWALK_CONTRACT"
_SOURCE_SCOPE_BINDING_COUNT = 159
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


def _codes(errors: list[str]) -> set[str]:
    return {error.split(":", 1)[0] for error in errors}


def _required_constant(name: str, expected):
    actual = getattr(V, name, None)
    assert actual == expected, f"R11a RED: missing or stale {name}"
    return actual


def _scoped_builder():
    builder = getattr(V, "source_record_identity_envelope", None)
    assert callable(builder), "R11a RED: missing scoped source-record builder"
    parameters = tuple(inspect.signature(builder).parameters)
    assert parameters == (
        "source_record",
        "adjudication",
    ), "R11a RED: source-record builder must derive scope from adjudication"
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


def _adjudication(scope_ref: str) -> dict:
    return {
        "ref": scope_ref,
        "scope_ref": _SUPPLEMENTAL_SCOPE
        if scope_ref != _SUPPLEMENTAL_SCOPE
        else _ORIGINAL_SCOPE,
        "source_event_refs": [R10._synthetic_hmac_ref("event", "scope-decoy")],
        "source_refs": [
            {
                "kind": "receipt",
                "ref": R10._synthetic_hmac_ref("metadata", "scope-decoy"),
            }
        ],
    }


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


def _assert_hmac_contract_error(call) -> None:
    with pytest.raises(ValueError) as caught:
        call()
    assert caught.value.args == ("E_HMAC_CONTRACT",)
    assert caught.value.__cause__ is None
    assert caught.value.__context__ is None


def _crosswalk_builder():
    builder = getattr(V, "crosswalk_grouping_projection", None)
    assert callable(builder), "R11a RED: missing closed crosswalk projection"
    return builder


def _crosswalk_digest():
    digest = getattr(V, "crosswalk_grouping_sha256", None)
    assert callable(digest), "R11a RED: missing crosswalk projection digest"
    return digest


def _strip_original_presence(doc: dict) -> None:
    for record in doc["records"]:
        for item in record.get("adjudications", ()):
            if item["grain"] == "original_candidate":
                item["causal_confidence"].pop("anomaly_or_guard_presence", None)
    for item in doc["source_exclusions"]:
        if item["grain"] == "original_candidate":
            item["causal_confidence"].pop("anomaly_or_guard_presence", None)


def _r11_doc() -> dict:
    _required_constant("SOURCE_RECORD_HMAC_PROFILE", _SOURCE_PROFILE)
    _required_constant("HMAC_CONTRACT_VERSION", _HMAC_CONTRACT_VERSION)
    _required_constant("HMAC_RECEIPT_COVERAGE", _HMAC_RECEIPT_COVERAGE)
    _required_constant("HMAC_VECTOR_SET_SHA256", _VECTOR_SET_SHA256)
    _required_constant("RECONSTRUCTION_FRAGMENT_POLICY_VERSION", _FRAGMENT_POLICY)
    _required_constant("ORIGINAL_PRESENCE_DERIVATION_VERSION", _PRESENCE_POLICY)
    _required_constant("CROSSWALK_PROJECTION_PROFILE", _CROSSWALK_PROFILE)

    doc = R10._synthetic_r2_doc()
    _strip_original_presence(doc)
    contract = doc["adjudication_contract"]
    contract.update(
        {
            "audit_contract": _PRESEAL_AUDIT_CONTRACT,
            "crosswalk_projection_profile": _CROSSWALK_PROFILE,
            "reconstruction_fragment_policy_version": _FRAGMENT_POLICY,
            "original_presence_derivation_version": _PRESENCE_POLICY,
            "original_presence_derivation_ruling_ref": _PRESENCE_RULING_REF,
            "original_presence_external_receipt": {
                "state": "externally_verified",
                "policy_version": _PRESENCE_POLICY,
                "ruling_ref": _PRESENCE_RULING_REF,
                "original_count": 106,
                "derived_presence": "high",
                "receipt_sha256": R10._synthetic_opaque_ref("r11:presence-receipt"),
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
            "source_scope_binding_count": _SOURCE_SCOPE_BINDING_COUNT,
            "source_scope_policy_version": _FRAGMENT_POLICY,
            "crosswalk_projection_profile": _CROSSWALK_PROFILE,
            "audit_contract": _PRESEAL_AUDIT_CONTRACT,
            "registry_schema_version": 1,
        }
    )
    contract["mapping_crosswalk_sha256"] = _crosswalk_digest()(doc)
    receipt["mapping_crosswalk_sha256"] = contract["mapping_crosswalk_sha256"]
    contract["semantic_commitment_sha256"] = V.semantic_commitment_sha256(doc)
    contract["legacy_projection_sha256"] = V.legacy_projection_sha256(doc)
    contract["registry_projection_sha256"] = V.registry_projection_sha256(doc)
    return doc


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


def test_r11_b2_scope_contract_versions_are_closed_and_pinned():
    _required_constant("SOURCE_RECORD_HMAC_PROFILE", _SOURCE_PROFILE)
    _required_constant("HMAC_CONTRACT_VERSION", _HMAC_CONTRACT_VERSION)
    _required_constant("HMAC_RECEIPT_COVERAGE", _HMAC_RECEIPT_COVERAGE)
    _required_constant("HMAC_VECTOR_SET_SHA256", _VECTOR_SET_SHA256)
    _required_constant("RECONSTRUCTION_FRAGMENT_POLICY_VERSION", _FRAGMENT_POLICY)
    _required_constant("HMAC_SOURCE_SCOPE_BINDING_COUNT", 159)


@pytest.mark.parametrize("scope_ref", [_ORIGINAL_SCOPE, _SUPPLEMENTAL_SCOPE])
def test_r11_b2_builder_derives_exact_scope_from_containing_adjudication(scope_ref):
    envelope = _scoped_builder()(_source_input(), _adjudication(scope_ref))
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


def test_r11_b2_source_event_metadata_ordinal_and_caller_scope_cannot_fold_into_scope():
    builder = _scoped_builder()
    baseline = builder(_source_input(), _adjudication(_ORIGINAL_SCOPE))
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
        assert builder(source, _adjudication(_ORIGINAL_SCOPE)) == baseline


def test_r11_b2_same_source_two_scope_vectors_pin_payload_frame_mac_and_manifest():
    _required_constant("HMAC_VECTOR_SET_SHA256", _VECTOR_SET_SHA256)
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
    _required_constant("SOURCE_RECORD_HMAC_PROFILE", _SOURCE_PROFILE)
    envelope = _reference_scoped_envelope(_ORIGINAL_SCOPE)
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
        lambda: _scoped_builder()(_source_input(), _adjudication(unratified))
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


def test_r11_b2_external_receipt_binds_all_scoped_placements_and_crosswalk():
    doc = _r11_doc()
    contract = doc["adjudication_contract"]
    receipt = contract["hmac_external_receipt"]
    assert receipt["source_scope_binding_count"] == 159
    assert receipt["source_scope_policy_version"] == _FRAGMENT_POLICY
    assert receipt["mapping_crosswalk_sha256"] == contract["mapping_crosswalk_sha256"]
    assert receipt["crosswalk_projection_profile"] == _CROSSWALK_PROFILE
    assert receipt["audit_contract"] == _PRESEAL_AUDIT_CONTRACT
    assert receipt["registry_schema_version"] == 1


# ------------------------------------------------------------------ B3 and pre-seal crosswalk


def test_r11_b3_presence_policy_and_private_receipt_are_closed_and_pinned():
    doc = _r11_doc()
    contract = doc["adjudication_contract"]
    assert V.ORIGINAL_PRESENCE_DERIVATION_VERSION == _PRESENCE_POLICY
    assert V.ORIGINAL_PRESENCE_DERIVATION_RULING_REF == _PRESENCE_RULING_REF
    assert contract["original_presence_derivation_version"] == _PRESENCE_POLICY
    assert contract["original_presence_derivation_ruling_ref"] == (_PRESENCE_RULING_REF)
    assert contract["original_presence_external_receipt"] == {
        "state": "externally_verified",
        "policy_version": _PRESENCE_POLICY,
        "ruling_ref": _PRESENCE_RULING_REF,
        "original_count": 106,
        "derived_presence": "high",
        "receipt_sha256": R10._synthetic_opaque_ref("r11:presence-receipt"),
    }


def test_r11_b3_schema_no_longer_requires_caller_presence_on_original_mapping():
    doc = _r11_doc()
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


def test_r11_b3_caller_cannot_inject_original_presence_even_when_value_is_high():
    doc = _r11_doc()
    original = doc["records"][0]["adjudications"][0]
    original["causal_confidence"]["anomaly_or_guard_presence"] = "high"
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
    assert _PRESENCE_ERROR in _codes(V.validate_document(doc, R10._schema()))


@pytest.mark.parametrize("mutation", ["missing", "duplicate"])
def test_r11_b3_original_presence_fails_closed_without_exactly_once_proof(mutation):
    doc = _r11_doc()
    if mutation == "missing":
        doc["records"][0]["adjudications"].pop(0)
    else:
        original = copy.deepcopy(doc["records"][0]["adjudications"][0])
        doc["records"][1]["adjudications"].append(original)
    assert _PRESENCE_ERROR in _codes(V.validate_document(doc, R10._schema()))
    with pytest.raises(ValueError, match=f"^{_PRESENCE_ERROR}$"):
        V.semantic_commitment_payload(doc)


def test_r11_crosswalk_projection_is_closed_grouping_only_and_canonically_sorted():
    doc = _r11_doc()
    projection = _crosswalk_builder()(doc)
    assert set(projection) == {"profile", "groups"}
    assert projection["profile"] == _CROSSWALK_PROFILE
    assert len(projection["groups"]) == 104
    assert sum(len(group["mapping_refs"]) for group in projection["groups"]) == 159
    assert projection["groups"] == sorted(
        projection["groups"],
        key=lambda group: (group["target_kind"], tuple(group["mapping_refs"])),
    )
    for group in projection["groups"]:
        assert set(group) == {"target_kind", "mapping_refs"}
        assert group["target_kind"] in {"record", "source_exclusion"}
        assert group["mapping_refs"] == sorted(group["mapping_refs"])
    encoded = json.dumps(projection, sort_keys=True)
    for forbidden in (
        "target_record_id",
        "source_refs",
        "source_event_refs",
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


def test_r11_crosswalk_projection_changes_only_when_target_grouping_changes():
    doc = _r11_doc()
    baseline = _crosswalk_digest()(doc)
    moved = doc["records"][0]["adjudications"].pop()
    doc["records"][1]["adjudications"].append(moved)
    assert _crosswalk_digest()(doc) != baseline


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


def test_r11_enhanced_candidate_is_unconditionally_held_after_other_checks_pass():
    errors = V.validate_document(_r11_doc(), R10._schema())
    assert len(errors) == 1
    assert errors[0].startswith(f"{_IMPORT_HOLD}:")


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


def test_r11_legacy_public_registry_remains_green_without_import_hold():
    errors = V.validate_document(R10._doc(), R10._schema())
    assert errors == []
