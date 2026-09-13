"""Contract tests for the Git-tracked anomaly registry
(``diagnostics/anomalies``, ANOMALY GATE af4d6969b2ad).

Two halves:
  (a) the real ``registry.json`` validates cleanly and carries the mandated
      first-baseline pairs;
  (b) synthetic bad records (duplicate key, bad status, privacy leak, closure
      without evidence, identity drift) are rejected with the expected code.

Safety: pure JSON bookkeeping.  No native module, no IBus, no Phase-A data
is touched (``SMARTKEY_PHASEA_DATA`` is still pointed at a temp dir as
belt-and-suspenders, matching the other adapter tests).
"""

from __future__ import annotations

import copy
import hashlib
import hmac
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

import pytest
from jsonschema import Draft202012Validator

if "SMARTKEY_PHASEA_DATA" not in os.environ:
    os.environ["SMARTKEY_PHASEA_DATA"] = tempfile.mkdtemp(prefix="smartkey-test-phasea-")

_REPO = Path(__file__).resolve().parent.parent
_DIR = _REPO / "diagnostics" / "anomalies"
_REGISTRY = _DIR / "registry.json"
_SCHEMA = _DIR / "schema.json"
_VALIDATOR = _DIR / "validate.py"
_R4_SYNTHETIC_KEY_ID = "0123456789abcdef0123456789abcdef"

# The eight pairs the gate names as the minimum first baseline.
_MANDATED_PAIRS = {
    ("li", "ли"),
    ("no", "но"),
    ("statiq", "статия"),
    ("statii", "статии"),
    ("draftyt", "драфтът"),
    ("templejtyt", "темплейтът"),
    ("markdaun", "маркдаун"),
    ("bygowe", "бъгове"),
}


def _load_validator():
    spec = importlib.util.spec_from_file_location(
        "smartkey_anomaly_validate", _VALIDATOR
    )
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


V = _load_validator()


def _schema() -> dict:
    return json.loads(_SCHEMA.read_text(encoding="utf-8"))


def _doc() -> dict:
    return json.loads(_REGISTRY.read_text(encoding="utf-8"))


def _codes(errors: list[str]) -> set[str]:
    return {e.split(":", 1)[0] for e in errors}


def _assert_r11a_hold_only(doc: dict) -> None:
    errors = V.validate_document(doc, _schema())
    assert len(errors) == 1
    assert errors[0].startswith("E_R3_IMPORT_HOLD:")


def _find(doc: dict, observed: str, expected: str) -> dict:
    for rec in doc["records"]:
        if (
            rec["observed"]["token"] == observed
            and rec["expected"]["token"] == expected
        ):
            return rec
    raise AssertionError(f"pair {observed!r} -> {expected!r} missing from registry")


def _refresh_identity(rec: dict) -> None:
    rec["dedup_key"] = V.compute_dedup_key(rec)
    rec["id"] = V.compute_id(rec)


def _synthetic_opaque_ref(label: str) -> str:
    return hashlib.sha256(f"synthetic:{label}".encode("utf-8")).hexdigest()


def _synthetic_hmac_ref(domain: str, label: str) -> str:
    """Shape-only fixture ref; never derived from user data or a real HMAC key."""
    digest = hashlib.sha256(
        f"synthetic-hmac-fixture:{domain}:{label}".encode("utf-8")
    ).hexdigest()
    return f"smartkey-g0-hmac-sha256-v1:{domain}:{_R4_SYNTHETIC_KEY_ID}:{digest}"


def _synthetic_adjudicated_record(
    ordinal: int,
    *,
    grain: str,
    disposition: str,
) -> dict:
    """Build one privacy-clean synthetic record without copying registry data."""
    is_bug = disposition == "bug_candidate"
    if is_bug:
        observed = f"syntheticobserved{ordinal}"
        expected = f"syntheticexpected{ordinal}"
        observed_side = {"token": observed, "script": "latin", "descriptor": None}
        expected_side = {"token": expected, "script": "latin", "descriptor": None}
        dedup_key = f"{observed}|{expected}"
        category = "unknown"
        status = "open_suspected_smartkey"
        privacy = "public_token"
        classification = "mechanical_red_candidate"
        expected_form_status = "unique_authoritative_spelling"
        owner_lane = "f4_core"
        expected_confidence = "high"
        mechanism_confidence = "low"
        attribution_confidence = "low"
        reproducer = None
        closure_reason = None
    else:
        descriptor = _synthetic_hmac_ref(
            "metadata", f"descriptor:{disposition}:{ordinal}"
        )
        observed_side = {"token": None, "script": "unknown", "descriptor": descriptor}
        expected_side = {"token": None, "script": "unknown", "descriptor": None}
        dedup_key = f"desc:{descriptor}|null"
        category = "unknown"
        status = "not_smartkey"
        privacy = "unknown"
        expected_form_status = "not_applicable"
        expected_confidence = "not_applicable"
        mechanism_confidence = "not_applicable"
        attribution_confidence = "not_applicable"
        reproducer = {
            "kind": "unit_test",
            "ref": _synthetic_hmac_ref("metadata", f"evidence:{ordinal}"),
            "build_sha": "unknown",
            "summary": _synthetic_hmac_ref("metadata", "evidence:summary"),
        }
        closure_reason = _synthetic_hmac_ref("metadata", "closure:reason")
        if disposition == "guard":
            classification = "guard_not_bug_unless_intent_changes"
            owner_lane = "intended_english_quoted_guard"
        else:
            classification = "source_authorship_exclusion"
            owner_lane = "outside_product_source_authorship"

    adjudication_ref = _synthetic_opaque_ref(f"{grain}:{ordinal}")
    return {
        "id": "anom-" + hashlib.sha256(dedup_key.encode("utf-8")).hexdigest()[:12],
        "dedup_key": dedup_key,
        "category": category,
        "observed": observed_side,
        "expected": expected_side,
        "needs_operator_confirmation": False,
        "minimal_context": {"before": None, "after": None},
        "recorded_utc": "2026-08-26T00:00Z",
        "first_seen_utc": None,
        "last_seen_utc": None,
        "repeat": False,
        "occurrence_count": 1,
        "environment": {
            "app_surface": "synthetic validator fixture",
            "os": "synthetic",
            "build_sha": "unknown",
            "build_label": "synthetic validator fixture",
            "flags": None,
        },
        "source_refs": [
            {
                "kind": "other",
                "ref": _synthetic_hmac_ref("metadata", f"legacy:{ordinal}"),
                "observed_utc": None,
                "build_sha": None,
                "surface": _synthetic_hmac_ref("metadata", f"legacy-surface:{ordinal}"),
                "note": None,
            }
        ],
        "status": status,
        "suspected_layer": "unknown",
        "reproducer": reproducer,
        "red_test": None,
        "red_test_waiver": None,
        "fix_commit": None,
        "verification": None,
        "closure_reason": closure_reason,
        "privacy_classification": privacy,
        "notes": None,
        "adjudications": [
            {
                "grain": grain,
                "ref": adjudication_ref,
                "classification": classification,
                "disposition": disposition,
                "expected_form_status": expected_form_status,
                "causal_confidence": {
                    "anomaly_or_guard_presence": "high",
                    "expected_form": expected_confidence,
                    "runtime_mechanism": mechanism_confidence,
                    "smartkey_attribution": attribution_confidence,
                },
                "owner_lane": owner_lane,
                "privacy_class": "public_token" if is_bug else "metadata_only",
                "source_refs": [
                    {
                        "kind": "source_record",
                        "ref": _synthetic_opaque_ref(f"source:{ordinal}"),
                    }
                ],
            }
        ],
    }


def _synthetic_adjudicated_doc() -> dict:
    """Exact ratified 159-ID count contract, using synthetic values only."""
    records = []
    dispositions = ["bug_candidate"] * 125 + ["guard"] * 32 + ["source_exclusion"] * 2
    for ordinal, disposition in enumerate(dispositions):
        grain = "original_candidate" if ordinal < 106 else "supplemental_hypothesis"
        records.append(
            _synthetic_adjudicated_record(
                ordinal,
                grain=grain,
                disposition=disposition,
            )
        )
    return {
        "schema_version": 1,
        "registry": "smartkey-anomaly-registry",
        "gate_ref": "af4d6969b2ad",
        "adjudication_contract": {
            "ruling_ref": "6e0704632fef",
            "artifact_sha256": "92c6dd612be8095d54dc385044c60e9d33e91d0ed9e9f62f6701c87722edbb72",
            "original_count": 106,
            "supplemental_count": 53,
            "bug_candidate_count": 125,
            "guard_count": 32,
            "source_exclusion_count": 2,
        },
        "records": records,
    }


# Opaque namespace from the ratified 106-unit original adjudication set.  These
# identifiers reveal no typed form or source sentence; keeping them here lets a
# synthetic fixture exercise the exact production namespace without importing
# the private corpus.
_CANONICAL_ORIGINAL_REFS = (
    "orig:01dddf93a4946369",
    "orig:03078aa3f0211682",
    "orig:077e507ca5b404c3",
    "orig:0a1f4eea084f295d",
    "orig:0b54da36ae8fdd6a",
    "orig:1825fef17b7d6958",
    "orig:19cb8c152dd465d5",
    "orig:1bf4e08e6c94bbaf",
    "orig:1ce28219e06b7de2",
    "orig:1fe9e0c8a0b903fa",
    "orig:2072a8f696034a65",
    "orig:21ca2a4de3d3549a",
    "orig:260d1f3b9c057661",
    "orig:286349b461143bc3",
    "orig:29b59535f67faf39",
    "orig:2db970d7241fb6ba",
    "orig:31ace7712b72546c",
    "orig:32a1249096ca0101",
    "orig:32cd3ab535039553",
    "orig:347798fcd3dc3674",
    "orig:349b676e76d10fb3",
    "orig:35853c01ba074901",
    "orig:3cc100e857d1c0eb",
    "orig:3dda31f67a7e0504",
    "orig:4119a99c3bdbd501",
    "orig:41ad2b57e9c7fac6",
    "orig:445b1bcb07b1fb2f",
    "orig:468e0ba3d0ea4a62",
    "orig:471c7a33f9a969ac",
    "orig:4aeb912add6cdfb5",
    "orig:4cf19f9ecc7feb1b",
    "orig:4dd4fadeafbfb404",
    "orig:4fcea9ac064464fe",
    "orig:51c0fa6d3356962d",
    "orig:5ce5d7a27704b57c",
    "orig:5ea197799c601113",
    "orig:6330045760ba0612",
    "orig:63482ea1d7333296",
    "orig:6399efd2fbf9f937",
    "orig:6511e29c9434b123",
    "orig:65c3f0433762c985",
    "orig:6826ae16029f7972",
    "orig:6831cf6ed6d41185",
    "orig:6a44807c71050404",
    "orig:6e72417255fd294b",
    "orig:6edca44b9be0c444",
    "orig:7370ec2b227900fd",
    "orig:74e5d7e1d7a7aacf",
    "orig:7abc6ead3e07f378",
    "orig:7c82590873671bec",
    "orig:7d68d95b8291fb41",
    "orig:7de6b869119c55d5",
    "orig:806d02defb5c17a2",
    "orig:833b7279e47c1756",
    "orig:8c87cf5a5f68fc6f",
    "orig:8f262cdc8a46e802",
    "orig:8f7dc21c7efaaead",
    "orig:8fef3450f841fbc4",
    "orig:90105acdd01e53da",
    "orig:9666d71f20e59585",
    "orig:97cb5f1c8f386e04",
    "orig:99515bbe9876abdd",
    "orig:9986c7195ca98755",
    "orig:9a0f2c0e6352d59b",
    "orig:9c5e6344772055ef",
    "orig:aa80db451a033ec3",
    "orig:ac9a25456ce09f35",
    "orig:ad250eb3b3b95d23",
    "orig:ad96597876fccede",
    "orig:b1d0e80e0941b903",
    "orig:b2c01597a6e2e12d",
    "orig:b435d3b647a685b0",
    "orig:b4ca378f49780878",
    "orig:b6167ac3189d8ac5",
    "orig:b9a4ae5205ce3289",
    "orig:b9b3d5bf3a023f72",
    "orig:ba91d56421d76122",
    "orig:bdd53ee2538899df",
    "orig:bf50ed6e6a9fb714",
    "orig:c13bdd0b07a5e87e",
    "orig:c163e3c0880979b2",
    "orig:c25e29700ec68ab0",
    "orig:c3f31858ed64a47b",
    "orig:c551228040d71cec",
    "orig:c55a6a1fea77bd58",
    "orig:c92e246078fc0bcb",
    "orig:cbb5fa8f2c6228c3",
    "orig:cbc0c1a0517e0255",
    "orig:cc02dd4936f81442",
    "orig:cc6c5cbf0c6bbc46",
    "orig:cc6e3f51ebdb366d",
    "orig:d6b1dece032d0060",
    "orig:d6b9e24e1bb841af",
    "orig:dacc32c611276450",
    "orig:e67ace843f5274b2",
    "orig:e8cbad930acdae29",
    "orig:edf3c58b92cda9c4",
    "orig:ee8450ab997022b5",
    "orig:f3dd016012c9d986",
    "orig:f4a5fd9a8cc730d4",
    "orig:f8c5fabbe999b0dd",
    "orig:f9b98070d1b72035",
    "orig:fbd3f544727e8d56",
    "orig:fce8d6c98d84dc4d",
    "orig:fe152ae9873db112",
    "orig:fecf5af6bd40a120",
)


def _canonical_supplemental_refs() -> tuple[str, ...]:
    refs = []
    hypothesis = 1
    for group in range(1, 53):
        count = 2 if group == 46 else 1
        for _ in range(count):
            refs.append(f"supp:G{group:03d}:H{hypothesis:03d}")
            hypothesis += 1
    return tuple(refs)


_CANONICAL_ADJUDICATION_REFS = (
    *_CANONICAL_ORIGINAL_REFS,
    *_canonical_supplemental_refs(),
)


def _r2_semantic_payload(doc: dict) -> list[dict]:
    adjudications = [
        (adjudication, rec["id"])
        for rec in doc["records"]
        for adjudication in rec.get("adjudications", ())
    ]
    adjudications.extend(
        (adjudication, "top_level_source_exclusion")
        for adjudication in doc.get("source_exclusions", ())
    )
    projected = []
    for adj, target_record_id in sorted(adjudications, key=lambda item: item[0]["ref"]):
        confidence = copy.deepcopy(adj["causal_confidence"])
        if adj["grain"] == "original_candidate":
            confidence["anomaly_or_guard_presence"] = "high"
        projected.append(
            {
                "ref": adj["ref"],
                "grain": adj["grain"],
                "classification": adj["classification"],
                "disposition": adj["disposition"],
                "normalized_class": adj["normalized_class"],
                "expected": {
                    "status": adj["expected"]["status"],
                    "authority": adj["expected"]["authority"],
                    "value_commitment": adj["expected"]["value_ref"],
                },
                "causal_confidence": confidence,
                "owner_lane": adj["owner_lane"],
                "privacy_class": adj["privacy_class"],
                "source_binding": {
                    "target_record_id": target_record_id,
                    "source_refs": sorted(
                        f"{source['kind']}:{source['ref']}"
                        for source in adj["source_refs"]
                    ),
                    "source_event_refs": sorted(adj["source_event_refs"]),
                },
            }
        )
    return projected


def _r2_semantic_digest(doc: dict) -> str:
    payload = json.dumps(
        _r2_semantic_payload(doc),
        ensure_ascii=True,
        separators=(",", ":"),
        sort_keys=True,
    )
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()


def _r2_ref_set_digest(doc: dict) -> str:
    refs = [item["ref"] for item in _r2_semantic_payload(doc)]
    payload = json.dumps(
        sorted(refs),
        ensure_ascii=True,
        separators=(",", ":"),
    )
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()


def _r3_legacy_projection_digest(doc: dict) -> str:
    records = sorted(
        (copy.deepcopy(rec) for rec in doc["records"] if not rec.get("adjudications")),
        key=lambda rec: rec["id"],
    )
    payload = json.dumps(
        records,
        ensure_ascii=True,
        separators=(",", ":"),
        sort_keys=True,
    )
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()


def _r3_registry_projection_digest(doc: dict) -> str:
    payload = copy.deepcopy(doc)
    payload["adjudication_contract"].pop("registry_projection_sha256", None)
    encoded = json.dumps(
        payload,
        ensure_ascii=True,
        separators=(",", ":"),
        sort_keys=True,
    )
    return hashlib.sha256(encoded.encode("utf-8")).hexdigest()


def _r4_baseline_record_payload(rec: dict) -> dict:
    payload = copy.deepcopy(rec)
    for field in (
        "adjudications",
        "fix_evidence",
        "record_origin",
        "baseline_record_sha256",
    ):
        payload.pop(field, None)
    for field in ("reproducer", "red_test", "verification"):
        evidence = payload.get(field)
        if isinstance(evidence, dict):
            evidence.pop("covered_adjudication_refs", None)
            evidence.pop("covered_source_event_refs", None)
    return payload


def _r4_baseline_record_digest(rec: dict) -> str:
    encoded = json.dumps(
        _r4_baseline_record_payload(rec),
        ensure_ascii=True,
        separators=(",", ":"),
        sort_keys=True,
    )
    return hashlib.sha256(encoded.encode("utf-8")).hexdigest()


def _r4_baseline_set_digest(doc: dict) -> str:
    payload = sorted(
        (
            {
                "record_id": rec["id"],
                "baseline_record_sha256": rec["baseline_record_sha256"],
            }
            for rec in doc["records"]
            if rec.get("record_origin") == "preexisting_public_baseline"
        ),
        key=lambda item: item["record_id"],
    )
    encoded = json.dumps(
        payload,
        ensure_ascii=True,
        separators=(",", ":"),
        sort_keys=True,
    )
    return hashlib.sha256(encoded.encode("utf-8")).hexdigest()


def _refresh_r3_commitments(doc: dict) -> None:
    contract = doc["adjudication_contract"]
    contract["semantic_commitment_sha256"] = _r2_semantic_digest(doc)
    hmac_receipt = contract.get("hmac_external_receipt")
    if isinstance(hmac_receipt, dict):
        hmac_receipt["semantic_commitment_sha256"] = contract[
            "semantic_commitment_sha256"
        ]
    contract["legacy_projection_sha256"] = _r3_legacy_projection_digest(doc)
    contract["registry_projection_sha256"] = _r3_registry_projection_digest(doc)


def _r2_adjudication(ref: str, ordinal: int, disposition: str) -> dict:
    grain = (
        "original_candidate" if ref.startswith("orig:") else "supplemental_hypothesis"
    )
    source_ref = _synthetic_hmac_ref("source_record", f"r2:{ordinal}")
    if disposition == "bug_candidate":
        classification = "mechanical_red_candidate"
        normalized_class = (
            "not_applicable"
            if grain == "original_candidate"
            else "f4_early_lock_full_word_crossover"
        )
        expected = {
            "status": "unique_authoritative_spelling",
            "authority": "authoritative",
            "value_ref": _synthetic_hmac_ref("value", f"r2:{ordinal}"),
        }
        confidence = {
            "expected_form": "high",
            "runtime_mechanism": "low",
            "smartkey_attribution": "low",
        }
        lane = "f4_core"
        privacy = "public_token"
        sources = [{"kind": "source_record", "ref": source_ref}]
    elif disposition == "guard":
        classification = "guard_not_bug_unless_intent_changes"
        normalized_class = "intentional_transliteration_guard"
        expected = {
            "status": "not_applicable",
            "authority": "not_applicable",
            "value_ref": None,
        }
        confidence = {
            "expected_form": "not_applicable",
            "runtime_mechanism": "not_applicable",
            "smartkey_attribution": "not_applicable",
        }
        lane = "intended_english_quoted_guard"
        privacy = "metadata_only"
        sources = [
            {"kind": "source_record", "ref": source_ref},
            {
                "kind": "ruling",
                "ref": _synthetic_hmac_ref("metadata", f"r2:ruling:{ordinal}"),
            },
        ]
    else:
        classification = "source_authorship_exclusion"
        normalized_class = "source_harness_framing"
        expected = {
            "status": "not_applicable",
            "authority": "not_applicable",
            "value_ref": None,
        }
        confidence = {
            "expected_form": "not_applicable",
            "runtime_mechanism": "not_applicable",
            "smartkey_attribution": "not_applicable",
        }
        lane = "outside_product_source_authorship"
        privacy = "metadata_only"
        sources = [
            {"kind": "source_record", "ref": source_ref},
            {
                "kind": "receipt",
                "ref": _synthetic_hmac_ref("metadata", f"r2:receipt:{ordinal}"),
            },
        ]
    if grain == "supplemental_hypothesis":
        confidence["anomaly_or_guard_presence"] = "high"
    return {
        "grain": grain,
        "ref": ref,
        "classification": classification,
        "disposition": disposition,
        "normalized_class": normalized_class,
        # Deprecated bridge stays present in the fixture to prove additive
        # migration; the validator requires equality with expected.status.
        "expected_form_status": expected["status"],
        "expected": expected,
        "causal_confidence": confidence,
        "owner_lane": lane,
        "privacy_class": privacy,
        "source_refs": sources,
        "source_event_refs": [_synthetic_hmac_ref("event", f"r2:event:{ordinal}")],
    }


def _r2_enhanced_record(seed: int, adjudications: list[dict]) -> dict:
    disposition = adjudications[0]["disposition"]
    base = _synthetic_adjudicated_record(
        seed,
        grain="original_candidate",
        disposition="bug_candidate" if disposition == "bug_candidate" else "guard",
    )
    base["adjudications"] = adjudications
    base["environment"] = {
        "app_surface": "synthetic",
        "os": "synthetic",
        "build_sha": "unknown",
        "build_label": "synthetic",
        "flags": None,
    }
    base["source_refs"] = [
        {
            "kind": "other",
            "ref": _synthetic_hmac_ref("metadata", f"r2:legacy-source:{seed}"),
            "observed_utc": None,
            "build_sha": None,
            "surface": _synthetic_hmac_ref("metadata", f"r2:surface:{seed}"),
            "note": None,
        }
    ]
    events = {
        event
        for adjudication in adjudications
        for event in adjudication["source_event_refs"]
    }
    base["occurrence_count"] = len(events)
    base["repeat"] = len(events) > 1
    base["fix_evidence"] = None
    if disposition == "guard":
        base["reproducer"] = None
    return base


def _r2_legacy_record(ordinal: int) -> dict:
    rec = _synthetic_adjudicated_record(
        1000 + ordinal,
        grain="original_candidate",
        disposition="bug_candidate",
    )
    rec.pop("adjudications")
    return rec


def _synthetic_r2_doc() -> dict:
    refs = list(_CANONICAL_ADJUDICATION_REFS)
    bug = [
        _r2_adjudication(ref, i, "bug_candidate") for i, ref in enumerate(refs[:125])
    ]
    guard = [
        _r2_adjudication(ref, i + 125, "guard") for i, ref in enumerate(refs[125:157])
    ]
    exclusions = [
        _r2_adjudication(ref, i + 157, "source_exclusion")
        for i, ref in enumerate(refs[157:])
    ]

    records = []
    bug_groups = [[item] for item in bug[:76]]
    for index, item in enumerate(bug[76:]):
        bug_groups[index].append(item)
    records.extend(
        _r2_enhanced_record(index, items) for index, items in enumerate(bug_groups)
    )
    guard_groups = [[item] for item in guard[:26]]
    for index, item in enumerate(guard[26:]):
        guard_groups[index].append(item)
    records.extend(
        _r2_enhanced_record(200 + index, items)
        for index, items in enumerate(guard_groups)
    )
    records.extend(_r2_legacy_record(index) for index in range(9))

    for index, rec in enumerate(records[:102]):
        if index < 20:
            rec["record_origin"] = "preexisting_public_baseline"
            rec["baseline_record_sha256"] = None
        else:
            rec["record_origin"] = "new"
            rec["baseline_record_sha256"] = None
            if index < 76:
                rec["observed"] = {
                    "token": None,
                    "script": "unknown",
                    "descriptor": _synthetic_hmac_ref(
                        "metadata", f"r4:redacted-observed:{index}"
                    ),
                }
                rec["expected"] = {
                    "token": None,
                    "script": "unknown",
                    "descriptor": _synthetic_hmac_ref(
                        "metadata", f"r4:redacted-expected:{index}"
                    ),
                }
                rec["privacy_classification"] = "redacted"
                for adjudication in rec["adjudications"]:
                    adjudication["privacy_class"] = "redacted"
                _refresh_identity(rec)

    for rec in records[:20]:
        rec["baseline_record_sha256"] = _r4_baseline_record_digest(rec)
    baseline_set_digest = _r4_baseline_set_digest({"records": records})

    doc = {
        "schema_version": 1,
        "registry": "smartkey-anomaly-registry",
        "gate_ref": "af4d6969b2ad",
        "adjudication_contract": {
            "state": "sealed",
            "ruling_ref": "6e0704632fef",
            "artifact_sha256": "92c6dd612be8095d54dc385044c60e9d33e91d0ed9e9f62f6701c87722edbb72",
            "mapping_crosswalk_sha256": (
                "dff41e032dab140ed77b3d0339787e354a5772354f60093b0c6b8153bbe384d6"
            ),
            "audit_contract": "g0-r11a-preseal-crosswalk",
            "original_count": 106,
            "supplemental_count": 53,
            "bug_candidate_count": 125,
            "guard_count": 32,
            "source_exclusion_count": 2,
            "projected_record_count": 111,
            "bug_record_count": 76,
            "guard_record_count": 26,
            "legacy_record_count": 9,
            "baseline_update_record_count": 20,
            "new_record_count": 82,
            "canonical_ref_set_sha256": "81168506c76776f060aaf9cd71bb4ed0e2fbde31b286ac30b69823bc8a54a5ee",
            "normalized_tuple_matrix_sha256": "4c32653fa93b8dbe2ca48b5ee42cb4d733aec6ebc374c2ca712f558203237a04",
            "reconstruction_fragment_policy_version": (
                "smartkey-g0-source-ref-scoped-cap-v1"
            ),
            "crosswalk_projection_profile": ("smartkey-g0-crosswalk-target-binding-v2"),
            "source_scope_placement_profile": ("smartkey-g0-source-scope-placement-v1"),
            "source_scope_placement_sha256": (
                "c4f2a3a642a15c0a62ce9bb5904adfdec43721e88af65be1577f68d7e122f119"
            ),
            "original_presence_derivation_version": (
                "smartkey-g0-original-retained-exactly-once-presence-v1"
            ),
            "original_presence_derivation_ruling_ref": "4e0198426714",
            "original_ref_set_sha256": (
                "6a2d167b00ece704a369772c76b231821513b0da5602ab960c16ac9bed78a3f2"
            ),
            "original_presence_external_receipt": {
                "state": "externally_verified",
                "policy_version": (
                    "smartkey-g0-original-retained-exactly-once-presence-v1"
                ),
                "ruling_ref": "4e0198426714",
                "original_count": 106,
                "derived_presence": "high",
                "original_ref_set_sha256": (
                    "6a2d167b00ece704a369772c76b231821513b0da5602ab960c16ac9bed78a3f2"
                ),
                "crosswalk_projection_profile": (
                    "smartkey-g0-crosswalk-target-binding-v2"
                ),
                "target_binding_crosswalk_sha256": (
                    "dff41e032dab140ed77b3d0339787e354a5772354f60093b0c6b8153bbe384d6"
                ),
                "audit_contract": "g0-r11a-preseal-crosswalk",
                "receipt_sha256": (
                    "6e9d2931631a284fb25e0f60e354eb42e42f026a5032d5f58db3a1a7e07b96ee"
                ),
            },
            "semantic_commitment_algorithm": "sha256-canonical-json-v1",
            "semantic_commitment_sha256": "f" * 64,
            "legacy_projection_sha256": "e" * 64,
            "registry_projection_sha256": "d" * 64,
            "baseline_commitment_algorithm": "sha256-canonical-json-v1",
            "baseline_record_set_sha256": baseline_set_digest,
            "baseline_external_receipt": {
                "state": "externally_verified",
                "record_count": 20,
                "approved_record_set_sha256": baseline_set_digest,
                "receipt_sha256": _synthetic_opaque_ref("r4:baseline-receipt"),
            },
            "hmac_scheme": "smartkey-g0-hmac-sha256-v1",
            "hmac_min_key_bytes": 32,
            "hmac_contract_version": "smartkey-g0-hmac-byte-contract-v4",
            "hmac_domains": ["value", "event", "metadata", "source_record"],
            "hmac_domain_payload_profiles": {
                "value": "scalar_utf8",
                "event": "project_canonical_json_v1",
                "metadata": "project_canonical_json_v1",
                "source_record": (
                    "smartkey-g0-source-record-adjudication-scoped-semantic-v2"
                ),
            },
            "hmac_domain_root_types": {
                "value": "exact_str",
                "event": "exact_dict",
                "metadata": "exact_dict",
                "source_record": "exact_dict",
            },
            "hmac_resource_contract_version": ("smartkey-g0-hmac-resource-contract-v1"),
            "hmac_resource_limits": {
                "max_raw_utf8_bytes": 262_144,
                "max_canonical_payload_bytes": 65_536,
                "max_lexical_nesting_depth": 32,
                "max_typed_nesting_depth": 32,
                "max_total_nodes": 4_096,
                "max_array_members": 1_024,
                "max_object_members": 512,
                "max_string_utf8_bytes": 65_528,
                "max_key_utf8_bytes": 1_024,
                "max_integer_token_digits": 16,
            },
            "hmac_resource_contract_sha256": (
                "50c430ef4de54c935e9bbbc4e6929dc7fbd28ba0f549da843526dcaf0f271bab"
            ),
            "hmac_scalar_payload_encoding": ("exact-utf8-scalar-no-normalization-v1"),
            "hmac_structured_payload_encoding": "smartkey-g0-canonical-json-v1",
            "hmac_input_frame": ("ascii-scheme-nul-domain-nul-u64be-length-payload-v1"),
            "hmac_vector_set_sha256": (
                "8f6644ed05a898bd650f4e42dca4bc278e19db601523ed5442fcc20dd5ab0b3c"
            ),
            "hmac_key_id": _R4_SYNTHETIC_KEY_ID,
            "hmac_external_receipt": {
                "state": "externally_verified",
                "scheme": "smartkey-g0-hmac-sha256-v1",
                "key_id": _R4_SYNTHETIC_KEY_ID,
                "contract_version": "smartkey-g0-hmac-byte-contract-v4",
                "resource_contract_version": ("smartkey-g0-hmac-resource-contract-v1"),
                "resource_contract_sha256": (
                    "50c430ef4de54c935e9bbbc4e6929dc7fbd28ba0f549da843526dcaf0f271bab"
                ),
                "vector_set_sha256": (
                    "8f6644ed05a898bd650f4e42dca4bc278e19db601523ed5442fcc20dd5ab0b3c"
                ),
                "coverage": (
                    "all-refs-domain-serialization-resource-key-id-adjudication-"
                    "scope-uniqueness-whole-source-private-recomputation-v3"
                ),
                "source_scope_policy_version": ("smartkey-g0-source-ref-scoped-cap-v1"),
                "crosswalk_projection_profile": (
                    "smartkey-g0-crosswalk-target-binding-v2"
                ),
                "source_scope_placement_profile": (
                    "smartkey-g0-source-scope-placement-v1"
                ),
                "audit_contract": "g0-r11a-preseal-crosswalk",
                "registry_schema_version": 1,
                "source_scope_binding_count": 159,
                "mapping_crosswalk_sha256": (
                    "dff41e032dab140ed77b3d0339787e354a5772354f60093b0c6b8153bbe384d6"
                ),
                "source_scope_placement_sha256": (
                    "c4f2a3a642a15c0a62ce9bb5904adfdec43721e88af65be1577f68d7e122f119"
                ),
                "semantic_commitment_sha256": "d" * 64,
                "receipt_sha256": _synthetic_opaque_ref("r4:hmac-receipt"),
            },
        },
        "source_exclusions": exclusions,
        "records": records,
    }
    _refresh_r3_commitments(doc)
    return doc


# ---------------------------------------------------------------- real file
def test_real_registry_validates_cleanly():
    assert V.validate_file(_REGISTRY, _SCHEMA) == []


def test_real_registry_is_canonically_formatted():
    text = _REGISTRY.read_text(encoding="utf-8")
    assert text == V.canonical_text(json.loads(text))


def test_real_registry_contains_mandated_baseline_pairs():
    pairs = {
        (r["observed"]["token"], r["expected"]["token"])
        for r in _doc()["records"]
        if r["observed"]["token"] is not None
    }
    assert _MANDATED_PAIRS <= pairs


def test_open_is_the_default_and_every_record_has_a_source():
    for rec in _doc()["records"]:
        assert rec["status"] in V.STATUSES
        assert rec["source_refs"], rec["id"]


def test_cli_exit_zero_on_real_registry():
    proc = subprocess.run(
        [sys.executable, "-I", "-S", "-B", str(_VALIDATOR), str(_REGISTRY)],
        capture_output=True,
        text=True,
        check=False,
    )
    assert proc.returncode == 0, proc.stdout + proc.stderr
    assert proc.stdout.startswith("OK:")


def test_cli_exit_nonzero_on_bad_file(tmp_path):
    doc = _doc()
    doc["records"].append(copy.deepcopy(doc["records"][0]))
    bad = tmp_path / "registry.json"
    bad.write_text(V.canonical_text(doc), encoding="utf-8")
    proc = subprocess.run(
        [
            sys.executable,
            "-I",
            "-S",
            "-B",
            str(_VALIDATOR),
            str(bad),
            "--schema",
            str(_SCHEMA),
        ],
        capture_output=True,
        text=True,
        check=False,
    )
    assert proc.returncode == 1
    assert "E_DUP" in proc.stdout


def test_cli_exit_two_on_unreadable_input(tmp_path):
    proc = subprocess.run(
        [
            sys.executable,
            "-I",
            "-S",
            "-B",
            str(_VALIDATOR),
            str(tmp_path / "missing.json"),
        ],
        capture_output=True,
        text=True,
        check=False,
    )
    assert proc.returncode == 2


def test_cli_children_are_isolated_before_external_execution(monkeypatch, tmp_path):
    """Fail closed if any validator child loses the audited interpreter flags."""
    calls = []

    def deny_unisolated_child(argv, **kwargs):
        assert argv[1:4] == ["-I", "-S", "-B"], argv
        calls.append((argv, kwargs))
        if "--schema" in argv:
            return subprocess.CompletedProcess(argv, 1, "E_DUP\n", "")
        if argv[-1].endswith("missing.json"):
            return subprocess.CompletedProcess(argv, 2, "", "E_READ\n")
        return subprocess.CompletedProcess(argv, 0, "OK: 29 record(s)\n", "")

    monkeypatch.setattr(subprocess, "run", deny_unisolated_child)
    test_cli_exit_zero_on_real_registry()
    test_cli_exit_nonzero_on_bad_file(tmp_path)
    test_cli_exit_two_on_unreadable_input(tmp_path)

    assert len(calls) == 3
    assert all(call[1] == {
        "capture_output": True,
        "text": True,
        "check": False,
    } for call in calls)


def test_non_canonical_format_is_rejected(tmp_path):
    bad = tmp_path / "registry.json"
    bad.write_text(json.dumps(_doc(), indent=4, ensure_ascii=True), encoding="utf-8")
    assert "E_FORMAT" in _codes(V.validate_file(bad, _SCHEMA))


# ----------------------------------------------------------- synthetic bad
def test_duplicate_dedup_key_rejected():
    doc = _doc()
    doc["records"].append(copy.deepcopy(doc["records"][0]))
    assert "E_DUP" in _codes(V.validate_document(doc, _schema()))


def test_same_pair_with_different_case_is_still_a_duplicate():
    doc = _doc()
    dup = copy.deepcopy(_find(doc, "li", "ли"))
    dup["observed"]["token"] = "LI"
    dup["expected"]["token"] = "ЛИ"
    _refresh_identity(dup)
    doc["records"].append(dup)
    assert "E_DUP" in _codes(V.validate_document(doc, _schema()))


def test_bad_status_rejected():
    doc = _doc()
    doc["records"][0]["status"] = "wontfix"
    assert "E_ENUM" in _codes(V.validate_document(doc, _schema()))


def test_bad_layer_and_privacy_enum_rejected():
    doc = _doc()
    doc["records"][0]["suspected_layer"] = "gremlins"
    doc["records"][1]["privacy_classification"] = "secret"
    assert _codes(V.validate_document(doc, _schema())) == {"E_ENUM"}


def test_unknown_field_rejected():
    doc = _doc()
    doc["records"][0]["full_text"] = "x"
    assert "E_SCHEMA" in _codes(V.validate_document(doc, _schema()))


def test_missing_required_field_rejected():
    doc = _doc()
    del doc["records"][0]["source_refs"]
    assert "E_SCHEMA" in _codes(V.validate_document(doc, _schema()))


def test_id_drift_rejected():
    doc = _doc()
    doc["records"][0]["id"] = "anom-000000000000"
    assert "E_ID" in _codes(V.validate_document(doc, _schema()))


def test_dedup_key_drift_rejected():
    doc = _doc()
    doc["records"][0]["dedup_key"] = "x|y"
    codes = _codes(V.validate_document(doc, _schema()))
    assert "E_DEDUP_KEY" in codes and "E_ID" not in codes


def test_id_is_deterministic_and_independent_of_layer():
    rec = copy.deepcopy(_find(_doc(), "li", "ли"))
    before = V.compute_id(rec)
    rec["suspected_layer"] = "unknown"
    rec["status"] = "open_suspected_smartkey"
    assert (
        V.compute_id(rec)
        == before
        == "anom-"
        + __import__("hashlib").sha256(b"li|\xd0\xbb\xd0\xb8").hexdigest()[:12]
    )


@pytest.mark.parametrize(
    ("field", "value", "code"),
    [
        ("notes", "mail me at someone@example.org", "E_PRIV_AT"),
        ("notes", "see http site for the trace", "E_PRIV_URL"),
        ("notes", "ticket 12345678 has the dump", "E_PRIV_DIGITS"),
        ("notes", "a" * 41, "E_PRIV_LONGRUN"),
        ("notes", "line one\nline two", "E_PRIV_MULTILINE"),
    ],
)
def test_privacy_leak_in_free_text_rejected(field, value, code):
    doc = _doc()
    doc["records"][0][field] = value
    assert code in _codes(V.validate_document(doc, _schema()))


def test_privacy_leak_in_source_note_still_rejected_for_at_and_digits():
    doc = _doc()
    doc["records"][0]["source_refs"][0]["note"] = "from x@y, id 9876543210"
    codes = _codes(V.validate_document(doc, _schema()))
    assert {"E_PRIV_AT", "E_PRIV_DIGITS"} <= codes


def test_url_is_allowed_only_inside_source_refs():
    doc = _doc()
    doc["records"][0]["source_refs"][0]["note"] = "http reference kept here"
    assert "E_PRIV_URL" not in _codes(V.validate_document(doc, _schema()))


def test_multi_token_context_rejected():
    doc = _doc()
    doc["records"][0]["minimal_context"]["before"] = "two words"
    assert "E_PRIV_CONTEXT" in _codes(V.validate_document(doc, _schema()))


def test_multi_token_observed_rejected():
    doc = _doc()
    rec = doc["records"][0]
    rec["observed"]["token"] = "two words"
    _refresh_identity(rec)
    assert "E_TOKEN_WS" in _codes(V.validate_document(doc, _schema()))


def test_over_long_token_rejected_by_schema():
    doc = _doc()
    rec = doc["records"][0]
    rec["observed"]["token"] = "x" * 41
    _refresh_identity(rec)
    assert "E_SCHEMA" in _codes(V.validate_document(doc, _schema()))


def test_short_sha_in_build_field_rejected():
    doc = _doc()
    doc["records"][0]["environment"]["build_sha"] = "7b9901a"
    assert "E_SCHEMA" in _codes(V.validate_document(doc, _schema()))


def test_script_class_mismatch_rejected():
    doc = _doc()
    doc["records"][0]["observed"]["script"] = "cyrillic"
    assert "E_SCRIPT" in _codes(V.validate_document(doc, _schema()))


def test_null_token_requires_descriptor_and_unknown_privacy():
    doc = _doc()
    rec = doc["records"][0]
    rec["observed"] = {"token": None, "script": "unknown", "descriptor": None}
    _refresh_identity(rec)
    assert "E_OBSERVED" in _codes(V.validate_document(doc, _schema()))


def test_unknown_expected_requires_operator_confirmation():
    doc = _doc()
    rec = doc["records"][0]
    rec["expected"] = {"token": None, "script": "unknown", "descriptor": None}
    rec["needs_operator_confirmation"] = False
    _refresh_identity(rec)
    assert "E_EXPECTED" in _codes(V.validate_document(doc, _schema()))


def test_unconfirmed_expectation_cannot_be_fixed():
    doc = _doc()
    rec = _find(doc, "ima[", "имаш")  # a fixed record
    rec["needs_operator_confirmation"] = True
    assert "E_EXPECTED" in _codes(V.validate_document(doc, _schema()))


def test_repeat_flag_must_match_occurrence_count():
    doc = _doc()
    rec = doc["records"][0]
    rec["occurrence_count"] = 1
    rec["repeat"] = True
    assert "E_REPEAT" in _codes(V.validate_document(doc, _schema()))


def test_first_seen_after_last_seen_rejected():
    doc = _doc()
    rec = doc["records"][0]
    rec["first_seen_utc"] = "2026-08-24"
    rec["last_seen_utc"] = "2026-08-23T18:07Z"
    assert "E_TIME" in _codes(V.validate_document(doc, _schema()))


# ------------------------------------------------------------ closure gates
@pytest.mark.parametrize("status", ["verified", "closed"])
def test_closure_without_evidence_rejected(status):
    doc = _doc()
    rec = _find(doc, "draftyt", "драфтът")  # open, no evidence at all
    rec["status"] = status
    errors = V.validate_document(doc, _schema())
    gate = [e for e in errors if e.startswith("E_GATE")]
    assert any("fix_commit" in e for e in gate)
    assert any("verification" in e for e in gate)
    assert any("reproducer" in e for e in gate)
    if status == "closed":
        assert any("closure_reason" in e for e in gate)


def test_red_tested_requires_red_test():
    doc = _doc()
    rec = _find(doc, "li", "ли")  # reproduced, no red test
    rec["status"] = "red_tested"
    assert any(
        "RED test" in e
        for e in V.validate_document(doc, _schema())
        if e.startswith("E_GATE")
    )


def test_fixed_requires_fix_commit_and_test_or_waiver():
    doc = _doc()
    rec = _find(doc, "li", "ли")
    rec["status"] = "fixed"
    gate = [e for e in V.validate_document(doc, _schema()) if e.startswith("E_GATE")]
    assert any("fix_commit" in e for e in gate)
    assert any("regression test" in e for e in gate)


def test_not_smartkey_requires_closure_reason_and_evidence():
    doc = _doc()
    rec = _find(doc, "draftyt", "драфтът")
    rec["status"] = "not_smartkey"
    gate = [e for e in V.validate_document(doc, _schema()) if e.startswith("E_GATE")]
    assert any("closure_reason" in e for e in gate)
    assert any("reproducer" in e for e in gate)


def test_open_record_cannot_carry_a_fix_commit():
    doc = _doc()
    rec = _find(doc, "draftyt", "драфтът")
    rec["fix_commit"] = "0" * 40
    assert "E_STATE" in _codes(V.validate_document(doc, _schema()))


def test_verification_without_fix_commit_rejected():
    doc = _doc()
    rec = _find(doc, "ima[", "имаш")  # fixed: fix_commit is mandatory
    rec["fix_commit"] = None
    codes = _codes(V.validate_document(doc, _schema()))
    assert "E_GATE" in codes


def test_fixed_b9_records_carry_fix_chain_but_no_verification():
    # Ruling 5394d2c2ad28: aggregate structural evidence cannot verify a
    # single token — the four B9 records are 'fixed', never 'verified'.
    doc = _doc()
    # fmt: off
    for observed, expected in (("ima[", "имаш"), ("sled", "след"),
                               ("otgowor", "отговор"), ("prewkl", "превкл")):
    # fmt: on
        rec = _find(doc, observed, expected)
        assert rec["status"] == "fixed"
        assert rec["reproducer"] and rec["red_test"] and rec["fix_commit"]
        assert rec["verification"] is None
        assert V.SHA_RE.match(rec["fix_commit"])
    assert not [r for r in doc["records"] if r["status"] == "verified"]


def test_promoting_fixed_to_verified_requires_verification_evidence():
    doc = _doc()
    rec = _find(doc, "ima[", "имаш")
    rec["status"] = "verified"
    errors = V.validate_document(doc, _schema())
    assert any(e.startswith("E_GATE") and "verification" in e for e in errors)


def test_schema_enum_drift_is_detected():
    schema = _schema()
    schema["$defs"]["record"]["properties"]["status"]["enum"].append("maybe")
    assert "E_SCHEMA_DRIFT" in _codes(V.validate_document(_doc(), schema))


# -------------------------------------------- G0 adjudication contract (RED)
def test_exact_synthetic_adjudication_contract_validates_cleanly():
    _assert_r11a_hold_only(_synthetic_r2_doc())


def test_adjudication_enum_is_strict():
    doc = _synthetic_r2_doc()
    doc["records"][0]["adjudications"][0]["classification"] = "plausible_guess"
    assert "E_ENUM" in _codes(V.validate_document(doc, _schema()))


def test_classification_must_match_coarse_disposition():
    doc = _synthetic_r2_doc()
    doc["records"][0]["adjudications"][0]["disposition"] = "guard"
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(
        doc
    )
    assert "E_ADJ_STATE" in _codes(V.validate_document(doc, _schema()))


def test_unique_expected_status_requires_an_opaque_value_commitment():
    doc = _synthetic_r2_doc()
    doc["records"][0]["adjudications"][0]["expected"]["value_ref"] = None
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(
        doc
    )
    assert "E_ADJ_EXPECTED" in _codes(V.validate_document(doc, _schema()))


def test_not_applicable_expected_status_rejects_expected_form_confidence():
    doc = _synthetic_r2_doc()
    guard = doc["records"][76]["adjudications"][0]
    guard["causal_confidence"]["expected_form"] = "low"
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(
        doc
    )
    assert "E_ADJ_CONFIDENCE" in _codes(V.validate_document(doc, _schema()))


def test_all_guard_record_uses_not_smartkey_lifecycle():
    doc = _synthetic_r2_doc()
    rec = doc["records"][76]
    rec["status"] = "open_suspected_smartkey"
    assert "E_ADJ_STATE" in _codes(V.validate_document(doc, _schema()))


def test_bug_candidate_record_cannot_use_not_smartkey_lifecycle():
    doc = _synthetic_r2_doc()
    rec = doc["records"][0]
    rec["status"] = "not_smartkey"
    rec["reproducer"] = {
        "kind": "offline_probe",
        "ref": "metadata:" + _synthetic_opaque_ref("bug:evidence:ref"),
        "build_sha": "unknown",
        "summary": "metadata:" + _synthetic_opaque_ref("bug:evidence"),
    }
    rec["closure_reason"] = "metadata:" + _synthetic_opaque_ref("bug:closure")
    assert "E_ADJ_STATE" in _codes(V.validate_document(doc, _schema()))


def test_metadata_only_privacy_rejects_token_payload():
    doc = _synthetic_r2_doc()
    doc["records"][0]["adjudications"][0]["privacy_class"] = "metadata_only"
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(
        doc
    )
    assert "E_ADJ_PRIVACY" in _codes(V.validate_document(doc, _schema()))


def test_metadata_only_privacy_rejects_semantic_text_encoded_as_a_code():
    doc = _synthetic_r2_doc()
    rec = doc["records"][76]
    rec["closure_reason"] = "metadata:synthetic_private_sentence_fragment"
    assert "E_ADJ_PRIVACY" in _codes(V.validate_document(doc, _schema()))


def test_hash_shaped_metadata_is_not_mistaken_for_a_long_word():
    doc = _synthetic_r2_doc()
    doc["records"][76]["closure_reason"] = _synthetic_hmac_ref(
        "metadata", "hash-shaped-metadata"
    )
    _refresh_r3_commitments(doc)
    assert "E_PRIV_LONGRUN" not in _codes(V.validate_document(doc, _schema()))


def test_opaque_source_ref_rejects_paths_and_whitespace():
    doc = _synthetic_r2_doc()
    ref = doc["records"][0]["adjudications"][0]["source_refs"][0]
    ref["ref"] = "source/private transcript"
    errors = V.validate_document(doc, _schema())
    assert any(
        error.startswith("E_SCHEMA") and "adjudications[0].source_refs[0].ref" in error
        for error in errors
    )


def test_opaque_source_ref_rejects_semantic_text_encoded_with_underscores():
    doc = _synthetic_r2_doc()
    ref = doc["records"][0]["adjudications"][0]["source_refs"][0]
    ref["ref"] = "synthetic_private_sentence_fragment"
    errors = V.validate_document(doc, _schema())
    assert any(
        error.startswith("E_SCHEMA") and "adjudications[0].source_refs[0].ref" in error
        for error in errors
    )


def test_adjudication_id_is_mapped_exactly_once_across_records():
    doc = _synthetic_r2_doc()
    first = doc["records"][0]["adjudications"][0]
    duplicate = doc["records"][1]["adjudications"][0]
    duplicate["grain"] = first["grain"]
    duplicate["ref"] = first["ref"]
    assert "E_ADJ_DUP" in _codes(V.validate_document(doc, _schema()))


def test_ratified_mapping_counts_are_derived_from_unique_ids():
    doc = _synthetic_r2_doc()
    doc["records"].pop()
    assert "E_ADJ_COUNT" in _codes(V.validate_document(doc, _schema()))


def test_contract_rejects_an_unratified_artifact_receipt():
    doc = _synthetic_r2_doc()
    doc["adjudication_contract"]["artifact_sha256"] = "b" * 64
    errors = V.validate_document(doc, _schema())
    assert any(
        error.startswith("E_SCHEMA")
        and "adjudication_contract.artifact_sha256" in error
        for error in errors
    )


def test_contract_provenance_pins_survive_schema_and_document_tampering():
    doc = _synthetic_r2_doc()
    schema = _schema()
    replacement = "b" * 64
    doc["adjudication_contract"]["artifact_sha256"] = replacement
    schema["$defs"]["adjudication_contract"]["properties"]["artifact_sha256"] = {
        "const": replacement
    }
    assert "E_SCHEMA_DRIFT" in _codes(V.validate_document(doc, schema))


def test_cross_record_source_scope_reuse_is_rejected_before_aggregation():
    doc = _synthetic_r2_doc()
    for rec in doc["records"][:5]:
        rec["adjudications"][0]["source_refs"] = [
            {
                "kind": "source_record",
                "ref": _synthetic_hmac_ref("source_record", "source:shared"),
            }
        ]
    codes = _codes(V.validate_document(doc, _schema()))
    assert {"E_ADJ_SOURCE_SCOPE", "E_SOURCE_SCOPE_PLACEMENT"} <= codes


def test_source_scope_reuse_is_rejected_even_when_fragment_budget_is_bounded():
    doc = _synthetic_r2_doc()
    for rec in doc["records"][:4]:
        rec["adjudications"][0]["source_refs"] = [
            {
                "kind": "source_record",
                "ref": _synthetic_hmac_ref("source_record", "source:bounded"),
            }
        ]
    codes = _codes(V.validate_document(doc, _schema()))
    assert {"E_ADJ_SOURCE_SCOPE", "E_SOURCE_SCOPE_PLACEMENT"} <= codes


def test_source_budget_does_not_count_opaque_metadata_as_payload():
    doc = _synthetic_r2_doc()
    for ordinal, rec in enumerate(doc["records"][20:24]):
        rec["notes"] = _synthetic_hmac_ref("metadata", f"note:{ordinal}")
        rec["adjudications"][0]["source_refs"] = [
            {
                "kind": "source_record",
                "ref": _synthetic_hmac_ref("source_record", "source:bounded"),
            }
        ]
    _refresh_r3_commitments(doc)
    assert "E_PRIV_AGGREGATE" not in _codes(V.validate_document(doc, _schema()))


# ---------------------------------------- G0 adjudication contract revision 2
def test_g0_r2_exact_159_to_111_shape_validates_without_raw_data():
    doc = _synthetic_r2_doc()
    assert len(doc["records"]) == 111
    assert sum("adjudications" in rec for rec in doc["records"]) == 102
    assert sum(len(rec.get("adjudications", ())) for rec in doc["records"]) == 157
    assert len(doc["source_exclusions"]) == 2
    _assert_r11a_hold_only(doc)


def test_g0_r2_schema_is_valid_draft_2020_12():
    Draft202012Validator.check_schema(_schema())
    Draft202012Validator(_schema()).validate(_synthetic_r2_doc())


def test_g0_r2_legacy_registry_may_omit_adjudication_contract():
    doc = _doc()
    assert "adjudication_contract" not in doc
    assert V.validate_document(doc, _schema()) == []


def test_g0_r2_enhanced_data_without_sealed_contract_is_rejected():
    doc = _synthetic_r2_doc()
    del doc["adjudication_contract"]
    assert "E_ADJ_CONTRACT" in _codes(V.validate_document(doc, _schema()))


def test_g0_r2_enhanced_contract_state_must_be_sealed():
    doc = _synthetic_r2_doc()
    doc["adjudication_contract"]["state"] = "draft"
    assert "E_ADJ_CONTRACT" in _codes(V.validate_document(doc, _schema()))


def test_g0_r2_zero_semantic_commitment_is_rejected():
    doc = _synthetic_r2_doc()
    doc["adjudication_contract"]["semantic_commitment_sha256"] = "0" * 64
    assert "E_ADJ_COMMITMENT" in _codes(V.validate_document(doc, _schema()))


def test_g0_r2_count_preserving_tuple_substitution_breaks_commitment():
    doc = _synthetic_r2_doc()
    doc["records"][0]["adjudications"][0]["owner_lane"] = "punctuation_boundary"
    assert "E_ADJ_COMMITMENT" in _codes(V.validate_document(doc, _schema()))


def test_g0_r2_count_preserving_target_rebinding_breaks_presence_and_crosswalk():
    doc = _synthetic_r2_doc()
    source = doc["records"][0]
    target = doc["records"][1]
    target["adjudications"].append(source["adjudications"].pop())
    for rec in (source, target):
        events = {
            event
            for adjudication in rec["adjudications"]
            for event in adjudication["source_event_refs"]
        }
        rec["occurrence_count"] = len(events)
        rec["repeat"] = len(events) > 1
    codes = _codes(V.validate_document(doc, _schema()))
    assert {"E_ADJ_CROSSWALK", "E_ADJ_PRESENCE"} <= codes


def test_g0_r2_ref_set_is_pinned_not_merely_counted():
    doc = _synthetic_r2_doc()
    adjudication = doc["records"][0]["adjudications"][0]
    adjudication["ref"] = "orig:aaaaaaaaaaaaaaaa"
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(
        doc
    )
    assert "E_ADJ_REF_SET" in _codes(V.validate_document(doc, _schema()))


def test_g0_r2_bare_and_cross_grain_refs_are_rejected():
    doc = _synthetic_adjudicated_doc()
    assert "E_ADJ_REF" in _codes(V.validate_document(doc, _schema()))

    doc = _synthetic_r2_doc()
    doc["records"][0]["adjudications"][0]["grain"] = "supplemental_hypothesis"
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(
        doc
    )
    assert "E_ADJ_REF" in _codes(V.validate_document(doc, _schema()))


def test_g0_r2_all_letter_hex_ref_is_grammatically_valid():
    assert V.canonical_ref_matches_grain("original_candidate", "orig:aaaaaaaaaaaaaaaa")


def test_g0_r2_normalized_class_sentinel_is_grain_specific():
    original = _synthetic_r2_doc()
    original["records"][0]["adjudications"][0]["normalized_class"] = (
        "mechanical_red_candidate"
    )
    original["adjudication_contract"]["semantic_commitment_sha256"] = (
        _r2_semantic_digest(original)
    )
    assert "E_ADJ_NORMALIZED" in _codes(V.validate_document(original, _schema()))

    supplemental = _synthetic_r2_doc()
    supplemental["records"][30]["adjudications"][1]["normalized_class"] = (
        "not_applicable"
    )
    supplemental["adjudication_contract"]["semantic_commitment_sha256"] = (
        _r2_semantic_digest(supplemental)
    )
    assert "E_ADJ_NORMALIZED" in _codes(V.validate_document(supplemental, _schema()))

    exclusion = _synthetic_r2_doc()
    exclusion["source_exclusions"][0]["normalized_class"] = "not_applicable"
    exclusion["adjudication_contract"]["semantic_commitment_sha256"] = (
        _r2_semantic_digest(exclusion)
    )
    assert "E_ADJ_NORMALIZED" in _codes(V.validate_document(exclusion, _schema()))


def test_g0_r2_missing_normalized_class_is_rejected():
    doc = _synthetic_r2_doc()
    del doc["records"][0]["adjudications"][0]["normalized_class"]
    assert "E_ADJ_NORMALIZED" in _codes(V.validate_document(doc, _schema()))


def test_g0_r2_expected_envelope_is_per_adjudication_for_fan_in():
    doc = _synthetic_r2_doc()
    adjudications = doc["records"][0]["adjudications"]
    assert len(adjudications) == 2
    assert (
        adjudications[0]["expected"]["value_ref"]
        != adjudications[1]["expected"]["value_ref"]
    )
    _assert_r11a_hold_only(doc)


def test_g0_r2_expected_authority_status_matrix_is_strict():
    doc = _synthetic_r2_doc()
    expected = doc["records"][0]["adjudications"][0]["expected"]
    expected["authority"] = "null"
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(
        doc
    )
    assert "E_ADJ_EXPECTED" in _codes(V.validate_document(doc, _schema()))


def test_g0_r2_redacted_unique_expected_needs_no_literal_token():
    doc = _synthetic_r2_doc()
    rec = doc["records"][20]
    assert rec["observed"]["token"] is None
    assert rec["expected"]["token"] is None
    assert rec["privacy_classification"] == "redacted"
    assert all(
        adjudication["privacy_class"] == "redacted"
        for adjudication in rec["adjudications"]
    )
    _assert_r11a_hold_only(doc)


def test_g0_r2_redaction_applies_to_expected_side_too():
    doc = _synthetic_r2_doc()
    rec = doc["records"][0]
    rec["observed"] = {
        "token": "[redacted]",
        "script": "latin",
        "descriptor": None,
    }
    rec["privacy_classification"] = "redacted"
    for adjudication in rec["adjudications"]:
        adjudication["privacy_class"] = "redacted"
    _refresh_identity(rec)
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(
        doc
    )
    assert "E_ADJ_PRIVACY" in _codes(V.validate_document(doc, _schema()))


def test_g0_r2_public_token_cannot_encode_a_private_path():
    doc = _synthetic_r2_doc()
    rec = doc["records"][0]
    rec["observed"]["token"] = "private/path"
    rec["observed"]["script"] = "latin"
    _refresh_identity(rec)
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(
        doc
    )
    assert "E_ADJ_PRIVACY" in _codes(V.validate_document(doc, _schema()))


def test_g0_r2_metadata_only_rejects_tokens_on_either_side():
    doc = _synthetic_r2_doc()
    guard = doc["records"][76]
    guard["expected"]["token"] = "synthetic"
    guard["expected"]["script"] = "latin"
    _refresh_identity(guard)
    assert "E_ADJ_PRIVACY" in _codes(V.validate_document(doc, _schema()))


@pytest.mark.parametrize(
    ("field", "value"),
    [
        ("app_surface", "private label"),
        ("os", "private platform"),
        ("build_label", "private build label"),
    ],
)
def test_g0_r2_enhanced_environment_strings_use_closed_allowlist(field, value):
    doc = _synthetic_r2_doc()
    doc["records"][0]["environment"][field] = value
    assert "E_ADJ_PRIVACY" in _codes(V.validate_document(doc, _schema()))


def test_g0_r2_enhanced_dynamic_dictionary_keys_use_closed_allowlist():
    doc = _synthetic_r2_doc()
    doc["records"][0]["environment"]["flags"] = {"private_flag": "off"}
    assert "E_ADJ_PRIVACY" in _codes(V.validate_document(doc, _schema()))


def test_g0_r2_legacy_source_and_evidence_strings_use_closed_allowlist():
    doc = _synthetic_r2_doc()
    rec = doc["records"][0]
    rec["source_refs"][0]["ref"] = "private/source"
    rec["source_refs"][0]["surface"] = "private surface"
    rec["reproducer"] = {
        "kind": "offline_probe",
        "ref": "private reproducer",
        "build_sha": "unknown",
        "summary": "private summary",
    }
    rec["red_test"] = {
        "path": "private/test/path",
        "name": "private test name",
        "commit": "a" * 40,
    }
    rec["verification"] = {
        "build_sha": "a" * 40,
        "evidence_refs": ["private evidence"],
        "surface": "private surface",
        "summary": "private verification",
    }
    assert "E_ADJ_PRIVACY" in _codes(V.validate_document(doc, _schema()))


def test_g0_r2_metadata_descriptors_require_full_sha256():
    doc = _synthetic_r2_doc()
    doc["records"][76]["closure_reason"] = "metadata:" + "a" * 12
    assert "E_ADJ_PRIVACY" in _codes(V.validate_document(doc, _schema()))


def test_g0_r2_aggregate_budget_includes_dynamic_keys_and_values():
    doc = _synthetic_r2_doc()
    shared = _synthetic_hmac_ref("source_record", "r2:shared-source")
    dynamic_keys = (
        "alphafragment",
        "betafragment",
        "gammafragment",
        "deltafragment",
    )
    for index, rec in enumerate(doc["records"][:4]):
        rec["adjudications"][0]["source_refs"] = [
            {"kind": "source_record", "ref": shared}
        ]
        rec["environment"]["flags"] = {dynamic_keys[index]: "off"}
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(
        doc
    )
    assert "E_PRIV_AGGREGATE" in _codes(V.validate_document(doc, _schema()))


def test_g0_r2_owner_lane_matrix_rejects_guard_lane_for_mechanical_bug():
    doc = _synthetic_r2_doc()
    doc["records"][0]["adjudications"][0]["owner_lane"] = (
        "intended_english_quoted_guard"
    )
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(
        doc
    )
    assert "E_ADJ_OWNER" in _codes(V.validate_document(doc, _schema()))


@pytest.mark.parametrize(
    ("record_index", "classification", "owner_lane"),
    [
        (0, "language_quality_feature_candidate", "f4_core"),
        (76, "guard_not_bug_unless_intent_changes", "f4_core"),
    ],
)
def test_g0_r2_language_quality_and_guard_lanes_are_exclusive(
    record_index, classification, owner_lane
):
    doc = _synthetic_r2_doc()
    adjudication = doc["records"][record_index]["adjudications"][0]
    adjudication["classification"] = classification
    adjudication["owner_lane"] = owner_lane
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(
        doc
    )
    assert "E_ADJ_OWNER" in _codes(V.validate_document(doc, _schema()))


def test_g0_r2_outside_product_lane_is_exclusive_to_source_exclusions():
    doc = _synthetic_r2_doc()
    adjudication = doc["records"][0]["adjudications"][0]
    adjudication["owner_lane"] = "outside_product_source_authorship"
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(
        doc
    )
    assert "E_ADJ_OWNER" in _codes(V.validate_document(doc, _schema()))


def test_g0_r2_occurrence_count_equals_distinct_source_events():
    doc = _synthetic_r2_doc()
    rec = doc["records"][0]
    rec["occurrence_count"] += 1
    rec["repeat"] = True
    assert "E_ADJ_EVENT" in _codes(V.validate_document(doc, _schema()))


def test_g0_r2_source_event_cannot_reconcile_to_two_records():
    doc = _synthetic_r2_doc()
    first = doc["records"][0]["adjudications"][0]["source_event_refs"][0]
    doc["records"][1]["adjudications"][0]["source_event_refs"] = [first]
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(
        doc
    )
    assert "E_ADJ_EVENT" in _codes(V.validate_document(doc, _schema()))


def test_g0_r2_source_event_ids_use_canonical_sha256_namespace():
    doc = _synthetic_r2_doc()
    doc["records"][0]["adjudications"][0]["source_event_refs"] = ["event:abc"]
    assert "E_ADJ_EVENT" in _codes(V.validate_document(doc, _schema()))


def test_g0_r2_malformed_event_value_fails_closed_without_validator_crash():
    doc = _synthetic_r2_doc()
    doc["records"][0]["adjudications"][0]["source_event_refs"] = [{}]
    codes = _codes(V.validate_document(doc, _schema()))
    assert {"E_ADJ_EVENT", "E_SCHEMA"} <= codes


def test_g0_r2_duplicate_source_event_does_not_increment_occurrence():
    doc = _synthetic_r2_doc()
    rec = doc["records"][0]
    event = rec["adjudications"][0]["source_event_refs"][0]
    rec["adjudications"][0]["source_event_refs"].append(event)
    rec["occurrence_count"] += 1
    rec["repeat"] = True
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(
        doc
    )
    assert "E_ADJ_EVENT" in _codes(V.validate_document(doc, _schema()))


def _r2_add_bug_evidence(rec: dict, covered_refs: list[str]) -> None:
    event_refs = sorted(
        {
            event
            for adjudication in rec["adjudications"]
            if adjudication["ref"] in covered_refs
            for event in adjudication["source_event_refs"]
        }
    )
    coverage = {
        "covered_adjudication_refs": covered_refs,
        "covered_source_event_refs": event_refs,
    }
    rec["reproducer"] = {
        "kind": "offline_probe",
        "ref": _synthetic_hmac_ref("metadata", "r2:reproducer"),
        "build_sha": "unknown",
        "summary": _synthetic_hmac_ref("metadata", "r2:reproducer-summary"),
        **coverage,
    }
    rec["red_test"] = {
        "path": _synthetic_hmac_ref("metadata", "r2:red-path"),
        "name": _synthetic_hmac_ref("metadata", "r2:red-name"),
        "commit": "a" * 40,
        **coverage,
    }
    rec["fix_commit"] = "b" * 40
    rec["fix_evidence"] = copy.deepcopy(coverage)
    rec["verification"] = {
        "build_sha": "b" * 40,
        "evidence_refs": [_synthetic_hmac_ref("metadata", "r2:verify-ref")],
        "surface": _synthetic_hmac_ref("metadata", "r2:verify-surface"),
        "summary": _synthetic_hmac_ref("metadata", "r2:verify-summary"),
        **coverage,
    }


def test_g0_r2_red_evidence_requires_explicit_mapping_coverage():
    doc = _synthetic_r2_doc()
    rec = doc["records"][0]
    rec["status"] = "red_tested"
    rec["reproducer"] = {
        "kind": "offline_probe",
        "ref": f"metadata:{_synthetic_opaque_ref('r2:probe')}",
        "build_sha": "unknown",
        "summary": f"metadata:{_synthetic_opaque_ref('r2:probe-summary')}",
    }
    rec["red_test"] = {
        "path": f"metadata:{_synthetic_opaque_ref('r2:path')}",
        "name": f"metadata:{_synthetic_opaque_ref('r2:name')}",
        "commit": "a" * 40,
    }
    assert "E_ADJ_COVERAGE" in _codes(V.validate_document(doc, _schema()))


def test_g0_r2_partial_multi_mapping_closure_is_rejected():
    doc = _synthetic_r2_doc()
    rec = doc["records"][0]
    covered = [rec["adjudications"][0]["ref"]]
    _r2_add_bug_evidence(rec, covered)
    rec["status"] = "verified"
    assert "E_ADJ_COVERAGE" in _codes(V.validate_document(doc, _schema()))


def test_g0_r2_full_multi_mapping_closure_coverage_is_valid():
    doc = _synthetic_r2_doc()
    rec = doc["records"][20]
    covered = [item["ref"] for item in rec["adjudications"]]
    _r2_add_bug_evidence(rec, covered)
    rec["status"] = "verified"
    _refresh_r3_commitments(doc)
    _assert_r11a_hold_only(doc)


def test_g0_r2_guard_uses_ruling_receipt_not_fabricated_unit_test():
    doc = _synthetic_r2_doc()
    guard = doc["records"][76]
    guard["reproducer"] = {
        "kind": "unit_test",
        "ref": f"metadata:{_synthetic_opaque_ref('r2:fake-guard')}",
        "build_sha": "unknown",
        "summary": f"metadata:{_synthetic_opaque_ref('r2:fake-guard-summary')}",
    }
    assert "E_ADJ_GUARD_EVIDENCE" in _codes(V.validate_document(doc, _schema()))


def test_g0_r2_partial_or_missing_top_level_source_exclusions_fail_counts():
    doc = _synthetic_r2_doc()
    doc["source_exclusions"].pop()
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(
        doc
    )
    assert "E_ADJ_COUNT" in _codes(V.validate_document(doc, _schema()))


def test_g0_r2_record_projection_breakdown_is_derived_not_trusted():
    doc = _synthetic_r2_doc()
    removed = doc["records"][76].pop("adjudications")
    doc["records"][77]["adjudications"].extend(removed)
    events = {
        event
        for adjudication in doc["records"][77]["adjudications"]
        for event in adjudication["source_event_refs"]
    }
    doc["records"][77]["occurrence_count"] = len(events)
    doc["records"][77]["repeat"] = len(events) > 1
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(
        doc
    )
    assert "E_ADJ_COUNT" in _codes(V.validate_document(doc, _schema()))


# ---------------------------------------- G0 adjudication contract revision 3
@pytest.mark.parametrize(
    "secret",
    ["a" * 12, "b" * 16, "c" * 40, "d" * 64],
)
def test_g0_r3_bare_hex_is_rejected_in_generic_string_fields(secret):
    doc = _synthetic_r2_doc()
    rec = doc["records"][0]
    for field, parent in (
        ("notes", rec),
        ("app_surface", rec["environment"]),
        ("surface", rec["source_refs"][0]),
    ):
        candidate = copy.deepcopy(doc)
        candidate_rec = candidate["records"][0]
        if parent is rec:
            candidate_rec[field] = secret
        elif parent is rec["environment"]:
            candidate_rec["environment"][field] = secret
        else:
            candidate_rec["source_refs"][0][field] = secret
        assert "E_ADJ_PRIVACY" in _codes(V.validate_document(candidate, _schema()))


def test_g0_r3_bare_hex_is_rejected_as_a_dynamic_dictionary_key():
    doc = _synthetic_r2_doc()
    doc["records"][0]["environment"]["flags"] = {"a" * 64: "off"}
    assert "E_ADJ_PRIVACY" in _codes(V.validate_document(doc, _schema()))


@pytest.mark.parametrize(
    "field",
    [
        "descriptor",
        "record_source_ref",
        "reproducer_ref",
        "reproducer_summary",
        "red_path",
        "red_name",
        "verification_ref",
        "verification_surface",
        "verification_summary",
    ],
)
def test_g0_r3_sha_shaped_values_are_rejected_across_generic_field_types(
    field,
):
    doc = _synthetic_r2_doc()
    rec = doc["records"][0]
    secret = "e" * 64
    rec["reproducer"] = {
        "kind": "offline_probe",
        "ref": _synthetic_hmac_ref("metadata", "r3:typed-reproducer"),
        "build_sha": "unknown",
        "summary": _synthetic_hmac_ref("metadata", "r3:typed-summary"),
    }
    rec["red_test"] = {
        "path": _synthetic_hmac_ref("metadata", "r3:typed-path"),
        "name": _synthetic_hmac_ref("metadata", "r3:typed-name"),
        "commit": "a" * 40,
    }
    rec["verification"] = {
        "build_sha": "b" * 40,
        "evidence_refs": [_synthetic_hmac_ref("metadata", "r3:typed-evidence")],
        "surface": _synthetic_hmac_ref("metadata", "r3:typed-surface"),
        "summary": _synthetic_hmac_ref("metadata", "r3:typed-verification"),
    }
    targets = {
        "descriptor": (rec["observed"], "descriptor"),
        "record_source_ref": (rec["source_refs"][0], "ref"),
        "reproducer_ref": (rec["reproducer"], "ref"),
        "reproducer_summary": (rec["reproducer"], "summary"),
        "red_path": (rec["red_test"], "path"),
        "red_name": (rec["red_test"], "name"),
        "verification_surface": (rec["verification"], "surface"),
        "verification_summary": (rec["verification"], "summary"),
    }
    if field == "verification_ref":
        rec["verification"]["evidence_refs"][0] = secret
    else:
        parent, key = targets[field]
        parent[key] = secret
    assert "E_ADJ_PRIVACY" in _codes(V.validate_document(doc, _schema()))


@pytest.mark.parametrize(
    "token",
    [
        "a" * 40,
        "AbCdefghijKLMNopqrstUVWX12345678",
    ],
)
def test_g0_r3_new_public_token_requires_baseline_authority(
    token,
):
    doc = _synthetic_r2_doc()
    rec = doc["records"][20]
    rec["observed"]["token"] = token
    rec["observed"]["script"] = "latin"
    rec["privacy_classification"] = "public_token"
    for adjudication in rec["adjudications"]:
        adjudication["privacy_class"] = "public_token"
    _refresh_identity(rec)
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(
        doc
    )
    assert "E_ADJ_ORIGIN" in _codes(V.validate_document(doc, _schema()))


@pytest.mark.parametrize(
    ("target", "unsalted"),
    [
        ("source_record", "a" * 64),
        ("event", "event:" + "b" * 64),
        ("value", "value:" + "c" * 64),
        ("metadata", "metadata:" + "d" * 64),
    ],
)
def test_g0_r3_plain_unsalted_sha256_refs_are_rejected(target, unsalted):
    doc = _synthetic_r2_doc()
    rec = doc["records"][0]
    adjudication = rec["adjudications"][0]
    if target == "source_record":
        adjudication["source_refs"][0]["ref"] = unsalted
    elif target == "event":
        adjudication["source_event_refs"][0] = unsalted
    elif target == "value":
        adjudication["expected"]["value_ref"] = unsalted
    else:
        rec["notes"] = unsalted
    assert {
        "E_SCHEMA",
        "E_ADJ_PRIVACY",
        "E_ADJ_REF_PRIVACY",
    } & _codes(V.validate_document(doc, _schema()))


def test_g0_r3_typed_hmac_schema_pattern_drift_is_rejected():
    schema = _schema()
    schema["$defs"]["typed_hmac_ref"]["pattern"] = "^.*$"
    assert "E_SCHEMA_DRIFT" in _codes(V.validate_document(_synthetic_r2_doc(), schema))


@pytest.mark.parametrize(
    ("source_index", "wrong_domain"),
    [
        (0, "metadata"),
        (1, "source_record"),
    ],
)
def test_g0_r3_adjudication_source_kind_requires_its_hmac_domain(
    source_index, wrong_domain
):
    doc = _synthetic_r2_doc()
    guard = doc["records"][76]["adjudications"][0]
    guard["source_refs"][source_index]["ref"] = _synthetic_hmac_ref(
        wrong_domain, f"r3:wrong-domain:{source_index}"
    )
    assert "E_ADJ_REF_PRIVACY" in _codes(V.validate_document(doc, _schema()))


@pytest.mark.parametrize(
    ("field", "wrong_domain"),
    [
        ("expected", "event"),
        ("event", "value"),
    ],
)
def test_g0_r3_expected_and_event_refs_reject_cross_domain_hmacs(field, wrong_domain):
    doc = _synthetic_r2_doc()
    adjudication = doc["records"][0]["adjudications"][0]
    wrong_ref = _synthetic_hmac_ref(wrong_domain, f"r3:wrong-{field}")
    if field == "expected":
        adjudication["expected"]["value_ref"] = wrong_ref
    else:
        adjudication["source_event_refs"][0] = wrong_ref
    assert "E_SCHEMA" in _codes(V.validate_document(doc, _schema()))


def test_g0_r3_projection_digest_schema_omission_is_rejected():
    schema = _schema()
    contract = schema["$defs"]["adjudication_contract"]
    contract["required"].remove("legacy_projection_sha256")
    assert "E_SCHEMA_DRIFT" in _codes(V.validate_document(_synthetic_r2_doc(), schema))


def test_g0_r3_registry_projection_binds_enhanced_record_content():
    doc = _synthetic_r2_doc()
    doc["records"][0]["notes"] = _synthetic_hmac_ref(
        "metadata", "r3:enhanced-note-substitution"
    )
    codes = _codes(V.validate_document(doc, _schema()))
    assert "E_ADJ_PROJECTION" in codes
    assert "E_ADJ_PRIVACY" not in codes


def test_g0_r3_synthetic_projection_digests_are_independently_recomputed():
    doc = _synthetic_r2_doc()
    contract = doc["adjudication_contract"]
    assert contract["legacy_projection_sha256"] == _r3_legacy_projection_digest(doc)
    assert contract["legacy_projection_sha256"] == (V.legacy_projection_sha256(doc))
    assert contract["registry_projection_sha256"] == (
        _r3_registry_projection_digest(doc)
    )
    assert contract["registry_projection_sha256"] == (V.registry_projection_sha256(doc))
    assert contract["legacy_projection_sha256"] != "0" * 64
    assert contract["registry_projection_sha256"] != "0" * 64


def test_g0_r3_legacy_projection_binds_identity_and_content():
    for mutation in ("identity", "content"):
        doc = _synthetic_r2_doc()
        legacy = doc["records"][-1]
        if mutation == "identity":
            legacy["observed"]["token"] = "syntheticlegacysubstitution"
            legacy["observed"]["script"] = "latin"
            _refresh_identity(legacy)
        else:
            legacy["notes"] = _synthetic_hmac_ref(
                "metadata", "r3:legacy-note-substitution"
            )
        codes = _codes(V.validate_document(doc, _schema()))
        assert "E_ADJ_PROJECTION" in codes
        assert "E_ADJ_PRIVACY" not in codes


@pytest.mark.parametrize(
    "field",
    ["registry_projection_sha256", "legacy_projection_sha256"],
)
def test_g0_r3_projection_commitments_reject_zero_digest(field):
    doc = _synthetic_r2_doc()
    doc["adjudication_contract"][field] = "0" * 64
    assert "E_ADJ_PROJECTION" in _codes(V.validate_document(doc, _schema()))


def test_g0_r3_mixed_bug_guard_record_needs_ruling_per_guard_mapping():
    doc = _synthetic_r2_doc()
    adjudication = doc["records"][0]["adjudications"][1]
    adjudication["classification"] = "guard_not_bug_unless_intent_changes"
    adjudication["disposition"] = "guard"
    adjudication["owner_lane"] = "intended_english_quoted_guard"
    adjudication["expected"] = {
        "status": "not_applicable",
        "authority": "not_applicable",
        "value_ref": None,
    }
    adjudication["expected_form_status"] = "not_applicable"
    adjudication["causal_confidence"] = {
        "anomaly_or_guard_presence": "high",
        "expected_form": "not_applicable",
        "runtime_mechanism": "not_applicable",
        "smartkey_attribution": "not_applicable",
    }
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(
        doc
    )
    assert "E_ADJ_GUARD_EVIDENCE" in _codes(V.validate_document(doc, _schema()))


def test_g0_r3_reproduced_multi_mapping_requires_exact_reproducer_coverage():
    doc = _synthetic_r2_doc()
    rec = doc["records"][0]
    rec["status"] = "reproduced"
    rec["reproducer"] = {
        "kind": "offline_probe",
        "ref": _synthetic_hmac_ref("metadata", "r3:probe"),
        "build_sha": "unknown",
        "summary": _synthetic_hmac_ref("metadata", "r3:probe-summary"),
    }
    codes = _codes(V.validate_document(doc, _schema()))
    assert "E_ADJ_COVERAGE" in codes
    assert "E_ADJ_PRIVACY" not in codes


def test_g0_r3_normalized_class_must_match_classification_register_and_lane():
    doc = _synthetic_r2_doc()
    adjudication = doc["records"][30]["adjudications"][1]
    adjudication["normalized_class"] = "grammar_definite_article"
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(
        doc
    )
    assert "E_ADJ_NORMALIZED" in _codes(V.validate_document(doc, _schema()))


@pytest.mark.parametrize(
    ("normalized_class", "classification", "disposition", "owner_lane"),
    [
        (
            "same_script_character_omission",
            "language_quality_feature_candidate",
            "bug_candidate",
            "future_language_quality_layer",
        ),
        (
            "intentional_transliteration_guard",
            "mechanical_red_candidate",
            "bug_candidate",
            "f4_core",
        ),
    ],
)
def test_g0_r3_mechanical_language_and_guard_normalized_lanes_do_not_mix(
    normalized_class, classification, disposition, owner_lane
):
    doc = _synthetic_r2_doc()
    adjudication = doc["records"][30]["adjudications"][1]
    adjudication["normalized_class"] = normalized_class
    adjudication["classification"] = classification
    adjudication["disposition"] = disposition
    adjudication["owner_lane"] = owner_lane
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(
        doc
    )
    assert "E_ADJ_NORMALIZED" in _codes(V.validate_document(doc, _schema()))


def test_g0_r3_normalized_tuple_contract_covers_every_enum_member():
    matrix = getattr(V, "NORMALIZED_CLASS_ALLOWED_TUPLES", {})
    assert set(matrix) == set(V.NORMALIZED_CLASSES)
    assert all(matrix[normalized_class] for normalized_class in V.NORMALIZED_CLASSES)
    for normalized_class, allowed_tuples in matrix.items():
        grain = (
            "original_candidate"
            if normalized_class == "not_applicable"
            else "supplemental_hypothesis"
        )
        assert all(
            V.normalized_tuple_is_allowed(
                normalized_class,
                grain,
                classification,
                disposition,
                owner_lane,
            )
            for classification, disposition, owner_lane in allowed_tuples
        )


def test_g0_r3_user_derived_values_are_never_reflected_in_diagnostics():
    sentinel = "DO_NOT_ECHO_SENTINEL"
    doc = _synthetic_r2_doc()
    rec = doc["records"][0]
    rec["observed"]["token"] = sentinel
    rec["observed"]["script"] = "latin"
    rec["notes"] = sentinel
    rec["environment"]["flags"] = {sentinel: "off"}
    errors = V.validate_document(doc, _schema())
    assert errors
    assert all(sentinel.casefold() not in error.casefold() for error in errors)


def test_g0_r3_structural_diagnostics_do_not_echo_values_or_dynamic_keys():
    sentinel = "DO_NOT_ECHO_STRUCTURAL_SENTINEL"
    candidates = []

    unknown_key = _synthetic_r2_doc()
    unknown_key["records"][0][sentinel] = None
    candidates.append(unknown_key)

    enum_value = _synthetic_r2_doc()
    enum_value["records"][0]["adjudications"][0]["classification"] = sentinel
    candidates.append(enum_value)

    shaped_ref = _synthetic_r2_doc()
    shaped_ref["records"][0]["adjudications"][0]["source_refs"][0]["ref"] = sentinel
    candidates.append(shaped_ref)

    for candidate in candidates:
        errors = V.validate_document(candidate, _schema())
        assert errors
        assert all(sentinel.casefold() not in error.casefold() for error in errors)


def test_g0_r3_enhanced_red_test_waiver_is_forbidden():
    doc = _synthetic_r2_doc()
    rec = doc["records"][0]
    covered = [item["ref"] for item in rec["adjudications"]]
    _r2_add_bug_evidence(rec, covered)
    rec["status"] = "fixed"
    rec["red_test"] = None
    rec["red_test_waiver"] = _synthetic_hmac_ref("metadata", "r3:waiver")
    codes = _codes(V.validate_document(doc, _schema()))
    assert "E_ADJ_WAIVER" in codes
    assert "E_ADJ_PRIVACY" not in codes


def test_g0_r3_whole_fixture_validates_under_external_draft_2020_12():
    validator = Draft202012Validator(_schema())
    validator.validate(_synthetic_r2_doc())


# ---------------------------------------- G0 adjudication contract revision 4


def _r4_shaped_ref(
    domain: str,
    label: str,
    *,
    key_id: str = _R4_SYNTHETIC_KEY_ID,
    suffix: str | None = None,
) -> str:
    mac = (
        suffix
        or hashlib.sha256(f"r4-shape-only:{domain}:{label}".encode("utf-8")).hexdigest()
    )
    return f"smartkey-g0-hmac-sha256-v1:{domain}:{key_id}:{mac}"


@pytest.mark.parametrize(
    "credential",
    [
        "AlphabeticCredentialMaterialOnly",
        "QWxhZGRpbjpPcGVuU2VzYW1lPQ==",
        "a" * 32,
    ],
)
def test_g0_r4_new_enhanced_records_forbid_every_public_literal_shape(
    credential,
):
    doc = _synthetic_r2_doc()
    rec = doc["records"][20]
    rec["record_origin"] = "new"
    rec["baseline_record_sha256"] = None
    rec["observed"] = {
        "token": credential,
        "script": "latin",
        "descriptor": None,
    }
    rec["expected"] = {
        "token": "SyntheticExpectedLiteral",
        "script": "latin",
        "descriptor": None,
    }
    rec["privacy_classification"] = "public_token"
    for adjudication in rec["adjudications"]:
        adjudication["privacy_class"] = "public_token"
    _refresh_identity(rec)
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(
        doc
    )
    assert "E_ADJ_ORIGIN" in _codes(V.validate_document(doc, _schema()))


def test_g0_r4_baseline_literal_mutation_fails_after_local_reseal():
    doc = _synthetic_r2_doc()
    rec = doc["records"][0]
    rec["observed"]["token"] = "SyntheticMutatedBaselineLiteral"
    rec["observed"]["script"] = "latin"
    _refresh_identity(rec)
    _refresh_r3_commitments(doc)
    assert "E_BASELINE_AUTHORITY" in _codes(V.validate_document(doc, _schema()))


def test_g0_r4_baseline_extra_literal_field_fails_after_local_reseal():
    doc = _synthetic_r2_doc()
    doc["records"][0]["notes"] = "SyntheticExtraBaselineLiteral"
    _refresh_r3_commitments(doc)
    codes = _codes(V.validate_document(doc, _schema()))
    assert {"E_BASELINE_AUTHORITY", "E_ADJ_PRIVACY"} <= codes


def test_g0_r4_baseline_receipt_stays_external_to_local_digest_reseal():
    doc = _synthetic_r2_doc()
    rec = doc["records"][0]
    rec["observed"]["token"] = "SyntheticLocallyResealedLiteral"
    rec["observed"]["script"] = "latin"
    _refresh_identity(rec)
    rec["baseline_record_sha256"] = _r4_baseline_record_digest(rec)
    doc["adjudication_contract"]["baseline_record_set_sha256"] = (
        _r4_baseline_set_digest(doc)
    )
    _refresh_r3_commitments(doc)
    assert "E_BASELINE_AUTHORITY" in _codes(V.validate_document(doc, _schema()))


def test_g0_r4_baseline_commitments_are_independently_recomputed():
    doc = _synthetic_r2_doc()
    baseline = [
        rec
        for rec in doc["records"]
        if rec.get("record_origin") == "preexisting_public_baseline"
    ]
    assert len(baseline) == 20
    for rec in baseline:
        assert rec["baseline_record_sha256"] == _r4_baseline_record_digest(rec)
        assert rec["baseline_record_sha256"] == V.baseline_record_sha256(rec)
    digest = _r4_baseline_set_digest(doc)
    assert digest == V.baseline_record_set_sha256(doc)
    assert doc["adjudication_contract"]["baseline_record_set_sha256"] == digest
    assert (
        doc["adjudication_contract"]["baseline_external_receipt"][
            "approved_record_set_sha256"
        ]
        == digest
    )


def test_g0_r4_baseline_receipt_cannot_be_transplanted_to_new_record():
    doc = _synthetic_r2_doc()
    source = doc["records"][0]
    target = doc["records"][20]
    target["record_origin"] = "preexisting_public_baseline"
    target["baseline_record_sha256"] = source.get("baseline_record_sha256")
    assert "E_BASELINE_AUTHORITY" in _codes(V.validate_document(doc, _schema()))


def test_g0_r4_fixture_has_exact_20_update_82_new_9_keep_actions():
    doc = _synthetic_r2_doc()
    origins = [
        rec.get("record_origin") for rec in doc["records"] if rec.get("adjudications")
    ]
    assert origins.count("preexisting_public_baseline") == 20
    assert origins.count("new") == 82
    assert sum(not rec.get("adjudications") for rec in doc["records"]) == 9


@pytest.mark.parametrize(
    ("field", "borrowed_enum"),
    [
        ("notes", "fixed"),
        ("reproducer_summary", "public_token"),
        ("source_surface", "not_applicable"),
        ("closure_reason", "high"),
    ],
)
def test_g0_r4_cross_field_enum_substitutions_are_rejected(field, borrowed_enum):
    doc = _synthetic_r2_doc()
    rec = doc["records"][20]
    rec["reproducer"] = {
        "kind": "offline_probe",
        "ref": _synthetic_hmac_ref("metadata", "r4:probe"),
        "build_sha": "unknown",
        "summary": _synthetic_hmac_ref("metadata", "r4:summary"),
    }
    targets = {
        "notes": (rec, "notes"),
        "reproducer_summary": (rec["reproducer"], "summary"),
        "source_surface": (rec["source_refs"][0], "surface"),
        "closure_reason": (rec, "closure_reason"),
    }
    parent, key = targets[field]
    parent[key] = borrowed_enum
    assert "E_ADJ_PRIVACY" in _codes(V.validate_document(doc, _schema()))


def test_g0_r4_synthetic_literal_is_fixture_only_not_generic_metadata():
    doc = _synthetic_r2_doc()
    doc["records"][20]["notes"] = "synthetic"
    assert "E_ADJ_PRIVACY" in _codes(V.validate_document(doc, _schema()))


def test_g0_r4_cross_record_enum_covert_channel_is_rejected_and_budgeted():
    doc = _synthetic_r2_doc()
    shared = _synthetic_hmac_ref("source_record", "r4:shared-source")
    symbols = (
        "fixed",
        "verified",
        "closed",
        "operator",
        "authoritative",
        "legacy_bridge",
        "medium",
        "source_record",
        "punctuation_boundary",
    )
    for rec, symbol in zip(doc["records"][20:29], symbols):
        rec["notes"] = symbol
        rec["adjudications"][0]["source_refs"] = [
            {"kind": "source_record", "ref": shared}
        ]
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(
        doc
    )
    codes = _codes(V.validate_document(doc, _schema()))
    assert {"E_ADJ_PRIVACY", "E_PRIV_AGGREGATE"} <= codes


def test_g0_r4_contract_declares_domain_inside_mac_input():
    doc = _synthetic_r2_doc()
    doc["adjudication_contract"]["hmac_input_frame"] = (
        "ascii-scheme-nul-u64be-length-payload-v1"
    )
    assert "E_HMAC_CONTRACT" in _codes(V.validate_document(doc, _schema()))


def test_g0_r4_contract_cannot_omit_domain_bound_input_frame():
    doc = _synthetic_r2_doc()
    doc["adjudication_contract"].pop("hmac_input_frame")
    assert "E_HMAC_CONTRACT" in _codes(V.validate_document(doc, _schema()))


def test_g0_r4_contract_requires_exact_domain_set():
    doc = _synthetic_r2_doc()
    doc["adjudication_contract"]["hmac_domains"] = [
        "value",
        "event",
        "metadata",
    ]
    assert "E_HMAC_CONTRACT" in _codes(V.validate_document(doc, _schema()))


def test_g0_r4_contract_requires_exact_scalar_encoding():
    doc = _synthetic_r2_doc()
    doc["adjudication_contract"]["hmac_scalar_payload_encoding"] = "normalized-utf8"
    assert "E_HMAC_CONTRACT" in _codes(V.validate_document(doc, _schema()))


def test_g0_r4_contract_rejects_wrong_structured_serialization_declaration():
    doc = _synthetic_r2_doc()
    doc["adjudication_contract"]["hmac_structured_payload_encoding"] = "json-defaults"
    assert "E_HMAC_CONTRACT" in _codes(V.validate_document(doc, _schema()))


def test_g0_r4_contract_requires_external_private_receipt_declaration():
    doc = _synthetic_r2_doc()
    doc["adjudication_contract"].pop("hmac_external_receipt", None)
    assert "E_HMAC_CONTRACT" in _codes(V.validate_document(doc, _schema()))


def test_g0_r4_ref_with_wrong_key_id_is_rejected():
    doc = _synthetic_r2_doc()
    source = doc["records"][0]["adjudications"][0]["source_refs"][0]
    source["ref"] = _r4_shaped_ref("source_record", "wrong-key", key_id="f" * 32)
    assert "E_HMAC_KEY_ID" in _codes(V.validate_document(doc, _schema()))


def test_g0_r4_dynamic_metadata_key_uses_the_sealed_key_epoch():
    doc = _synthetic_r2_doc()
    doc["records"][20]["environment"]["flags"] = {
        _r4_shaped_ref("metadata", "flag-key", key_id="f" * 32): "on"
    }
    assert "E_HMAC_KEY_ID" in _codes(V.validate_document(doc, _schema()))


def test_g0_r4_external_receipt_must_match_the_key_epoch():
    doc = _synthetic_r2_doc()
    doc["adjudication_contract"]["hmac_external_receipt"]["key_id"] = "f" * 32
    assert "E_HMAC_CONTRACT" in _codes(V.validate_document(doc, _schema()))


def test_g0_r4_schema_cannot_relax_hmac_receipt_coverage_pin():
    doc = _synthetic_r2_doc()
    schema = _schema()
    schema["$defs"]["hmac_external_receipt"]["properties"]["coverage"] = {
        "type": "string"
    }
    assert "E_SCHEMA_DRIFT" in _codes(V.validate_document(doc, schema))


def test_g0_r4_legacy_opaque_prefix_is_rejected():
    doc = _synthetic_r2_doc()
    source = doc["records"][0]["adjudications"][0]["source_refs"][0]
    source["ref"] = "hmac-sha256-v1:source_record:" + "a" * 64
    assert "E_HMAC_LEGACY" in _codes(V.validate_document(doc, _schema()))


def test_g0_r4_same_mac_suffix_cannot_be_reused_across_domains():
    doc = _synthetic_r2_doc()
    adjudication = doc["records"][0]["adjudications"][0]
    suffix = "a" * 64
    adjudication["expected"]["value_ref"] = _r4_shaped_ref(
        "value", "shared", suffix=suffix
    )
    adjudication["source_event_refs"][0] = _r4_shaped_ref(
        "event", "shared", suffix=suffix
    )
    assert "E_HMAC_SUFFIX" in _codes(V.validate_document(doc, _schema()))


# ---------------------------------------- G0 adjudication contract revision 5
_R5_PUBLIC_VECTOR_KEY = b"smartkey-g0-public-synthetic-vector-key-v1"
_R5_DOMAIN_PROFILES = {
    "value": "scalar_utf8",
    "event": "project_canonical_json_v1",
    "metadata": "project_canonical_json_v1",
    "source_record": "smartkey-g0-source-record-semantic-v1",
}
_R5_VECTORS = (
    {
        "name": "value_non_ascii",
        "domain": "value",
        "value": "café",
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
        "value": {
            "active": True,
            "count": 2,
            "event": "synthetic",
            "parts": ["alpha", None, {"ok": False}],
            "unit_separator": "\x1f",
            "\ue000": "bmp",
            "\U00010000": "astral",
        },
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
        "value": {
            "control": "\b\t\n\f\r\x00",
            "label": "café",
            "quote": '"\\/',
        },
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
        "value": {
            "profile": "smartkey-g0-source-record-semantic-v1",
            "merged_id": (
                "c0045ec8791a7f514f4addc5982ecfbdd9e28ab80bdaea91f5e6fbc6dfbf395e"
            ),
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
            "raw_sha256": (
                "56d890f43577f16f03358ea0c94bb7bd7e1e6864b908585e24be54ae2734cd56"
            ),
        },
        "payload_hex": (
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
        ),
        "frame_hex": (
            "736d6172746b65792d67302d686d61632d7368613235362d763100736f757263"
            "655f7265636f72640000000000000004627b22617574686f72736869705f636f"
            "6e666964656e6365223a2273796e7468657469632d636f6e666964656e636522"
            "2c226578636c756465645f7365676d656e7473223a5b7b22617574686f727368"
            "69705f636f6e666964656e6365223a2273796e7468657469632d616c70686122"
            "2c22656e645f63686172223a342c22726561736f6e223a2273796e7468657469"
            "632d616c706861222c2273746172745f63686172223a307d2c7b22617574686f"
            "72736869705f636f6e666964656e6365223a2273796e7468657469632d616c70"
            "6861222c22656e645f63686172223a342c22726561736f6e223a2273796e7468"
            "657469632d7a756c75222c2273746172745f63686172223a307d2c7b22617574"
            "686f72736869705f636f6e666964656e6365223a2273796e7468657469632d62"
            "657461222c22656e645f63686172223a342c22726561736f6e223a2273796e74"
            "68657469632d616c706861222c2273746172745f63686172223a307d2c7b2261"
            "7574686f72736869705f636f6e666964656e6365223a2273796e746865746963"
            "2d7a756c75222c22656e645f63686172223a342c22726561736f6e223a227379"
            "6e7468657469632d7a756c75222c2273746172745f63686172223a307d2c7b22"
            "617574686f72736869705f636f6e666964656e6365223a2273796e7468657469"
            "632d616c706861222c22656e645f63686172223a362c22726561736f6e223a22"
            "73796e7468657469632d616c706861222c2273746172745f63686172223a307d"
            "2c7b22617574686f72736869705f636f6e666964656e6365223a2273796e7468"
            "657469632d616161222c22656e645f63686172223a31322c22726561736f6e22"
            "3a2273796e7468657469632d616161222c2273746172745f63686172223a387d"
            "2c7b22617574686f72736869705f636f6e666964656e6365223a22ee8080222c"
            "22656e645f63686172223a32302c22726561736f6e223a2273796e7468657469"
            "632d7a756c75222c2273746172745f63686172223a31367d2c7b22617574686f"
            "72736869705f636f6e666964656e6365223a22f0908080222c22656e645f6368"
            "6172223a32302c22726561736f6e223a2273796e7468657469632d616c706861"
            "222c2273746172745f63686172223a31367d5d2c226d65726765645f6964223a"
            "226330303435656338373931613766353134"
            "6634616464633539383265636662646439653238616238306264616561393166"
            "356536666263366466626633393565222c22706c6174666f726d223a2273796e"
            "7468657469632d706c6174666f726d222c2270726f66696c65223a22736d6172"
            "746b65792d67302d736f757263652d7265636f72642d73656d616e7469632d76"
            "31222c227261775f736861323536223a22353664383930663433353737663136"
            "6630333335386561306339346262376264376531653638363462393038353835"
            "6532346265353461653237333463643536222c22736f757263655f736368656d"
            "615f76657273696f6e223a2273796e7468657469632d736f757263652d763122"
            "7d"
        ),
        "mac_sha256": (
            "dec2460826116e6f9e907387f617f9d26371b024202b4f8a064ed3a22b6869e4"
        ),
    },
)
_R5_VECTOR_SET_SHA256 = (
    "2fa017fb46cfc08512ca0f9f6c8b4dafc56b3b28dab0bc8ab2fa9b45ae3c7192"
)


def _r5_key_errors(doc: dict, key: str) -> list[str]:
    code = "E_ADJ_KEYS"
    return [
        error
        for error in V.validate_document(doc, _schema())
        if error.startswith(code) and error.split(":", 2)[1].endswith(f".{key}")
    ]


@pytest.mark.parametrize("key", ["record_origin", "baseline_record_sha256"])
def test_g0_r5_each_legacy_keep_forbids_enhanced_key_even_when_null(key):
    doc = _synthetic_r2_doc()
    for rec in doc["records"][102:]:
        rec[key] = None
    assert len(_r5_key_errors(doc, key)) == 9


@pytest.mark.parametrize("key", ["record_origin", "baseline_record_sha256"])
def test_g0_r5_every_enhanced_record_requires_both_origin_keys(key):
    doc = _synthetic_r2_doc()
    for rec in doc["records"][:102]:
        rec.pop(key)
    assert len(_r5_key_errors(doc, key)) == 102


def test_g0_r5_new_record_requires_explicit_null_baseline_digest():
    doc = _synthetic_r2_doc()
    rec = doc["records"][20]
    rec.pop("baseline_record_sha256")
    assert len(_r5_key_errors(doc, "baseline_record_sha256")) == 1


@pytest.mark.parametrize("replacement", [None, "0" * 64, "f" * 64])
def test_g0_r5_baseline_record_requires_nonzero_matching_independent_digest(
    replacement,
):
    doc = _synthetic_r2_doc()
    rec = doc["records"][0]
    rec["baseline_record_sha256"] = replacement
    codes = _codes(V.validate_document(doc, _schema()))
    assert "E_BASELINE_AUTHORITY" in codes
    assert replacement != _r4_baseline_record_digest(rec)


def test_g0_r5_baseline_digest_key_cannot_be_omitted():
    doc = _synthetic_r2_doc()
    doc["records"][0].pop("baseline_record_sha256")
    assert len(_r5_key_errors(doc, "baseline_record_sha256")) == 1


@pytest.mark.parametrize(
    "field",
    [
        "hmac_contract_version",
        "hmac_domain_payload_profiles",
        "hmac_vector_set_sha256",
    ],
)
def test_g0_r5_sealed_contract_rejects_missing_serialization_pin(field):
    doc = _synthetic_r2_doc()
    doc["adjudication_contract"].pop(field, None)
    assert "E_HMAC_CONTRACT" in _codes(V.validate_document(doc, _schema()))


def test_g0_r5_domain_payload_inventory_is_exact_and_float_free():
    assert hasattr(V, "HMAC_DOMAIN_PAYLOAD_PROFILES")
    assert {
        domain: V.HMAC_DOMAIN_PAYLOAD_PROFILES[domain]
        for domain in ("value", "event", "metadata")
    } == {
        domain: _R5_DOMAIN_PROFILES[domain] for domain in ("value", "event", "metadata")
    }
    assert V.HMAC_DOMAIN_PAYLOAD_PROFILES["source_record"] == (
        "smartkey-g0-source-record-adjudication-scoped-semantic-v2"
    )
    assert V.HMAC_STRUCTURED_PAYLOAD_ENCODING == "smartkey-g0-canonical-json-v1"


def _r5_canonical_bytes(value) -> bytes:
    assert hasattr(V, "canonical_structured_payload_bytes")
    return V.canonical_structured_payload_bytes(value)


def _r5_frame_bytes(domain: str, value) -> bytes:
    assert hasattr(V, "hmac_frame_bytes")
    return V.hmac_frame_bytes(domain, value)


def test_g0_r5_project_canonical_json_has_exact_nested_bytes():
    event = _R5_VECTORS[1]
    assert _r5_canonical_bytes(event["value"]).hex() == event["payload_hex"]


def test_g0_r5_non_ascii_literal_and_escape_have_identical_canonical_bytes():
    assert hasattr(V, "parse_project_canonical_json")
    literal = V.parse_project_canonical_json('{"label":"café"}')
    escaped = V.parse_project_canonical_json(r'{"label":"caf\u00e9"}')
    expected = b'{"label":"caf\xc3\xa9"}'
    assert _r5_canonical_bytes(literal) == expected
    assert _r5_canonical_bytes(escaped) == expected


@pytest.mark.parametrize(
    "value",
    [
        -0.0,
        1.5,
        float("inf"),
        float("-inf"),
        float("nan"),
        9_007_199_254_740_992,
        -9_007_199_254_740_992,
        "\ud800",
    ],
)
def test_g0_r5_project_canonical_json_rejects_unsupported_values(value):
    assert hasattr(V, "CanonicalJsonError")
    with pytest.raises(V.CanonicalJsonError):
        _r5_canonical_bytes(value)


@pytest.mark.parametrize(
    "encoded",
    [
        "-0",
        "1.0",
        "1e0",
        "9007199254740992",
        "-9007199254740992",
        "NaN",
        "Infinity",
        "-Infinity",
        r'"\ud800"',
        '{"duplicate":1,"duplicate":2}',
    ],
)
def test_g0_r5_project_canonical_json_parser_rejects_ambiguous_input(encoded):
    assert hasattr(V, "CanonicalJsonError")
    assert hasattr(V, "parse_project_canonical_json")
    with pytest.raises(V.CanonicalJsonError):
        V.parse_project_canonical_json(encoded)


def test_g0_r5_all_domain_vectors_pin_payload_frame_mac_and_receipt():
    assert len(_R5_PUBLIC_VECTOR_KEY) >= 32
    assert getattr(V, "HMAC_VECTOR_SET_SHA256", None) == (
        "8f6644ed05a898bd650f4e42dca4bc278e19db601523ed5442fcc20dd5ab0b3c"
    )
    receipt_items = []
    for vector in _R5_VECTORS:
        if vector["domain"] == "source_record":
            with pytest.raises(ValueError, match="^E_HMAC_CONTRACT$"):
                V.hmac_payload_bytes(vector["domain"], vector["value"])
        else:
            payload = V.hmac_payload_bytes(vector["domain"], vector["value"])
            frame = _r5_frame_bytes(vector["domain"], vector["value"])
            mac = hmac.new(_R5_PUBLIC_VECTOR_KEY, frame, hashlib.sha256).hexdigest()
            assert payload.hex() == vector["payload_hex"]
            assert frame.hex() == vector["frame_hex"]
            assert mac == vector["mac_sha256"]
        receipt_items.append(
            {
                "name": vector["name"],
                "domain": vector["domain"],
                "profile": _R5_DOMAIN_PROFILES[vector["domain"]],
                "key_id": _R4_SYNTHETIC_KEY_ID,
                "key_hex": _R5_PUBLIC_VECTOR_KEY.hex(),
                "payload_hex": vector["payload_hex"],
                "frame_hex": vector["frame_hex"],
                "mac_sha256": vector["mac_sha256"],
            }
        )
    encoded = json.dumps(
        receipt_items,
        ensure_ascii=True,
        separators=(",", ":"),
        sort_keys=True,
    ).encode("utf-8")
    assert hashlib.sha256(encoded).hexdigest() == _R5_VECTOR_SET_SHA256


def test_g0_r5_receipt_binds_contract_version_profiles_and_vector_set():
    doc = _synthetic_r2_doc()
    receipt = doc["adjudication_contract"]["hmac_external_receipt"]
    assert receipt.get("contract_version") == getattr(V, "HMAC_CONTRACT_VERSION", None)
    assert receipt.get("vector_set_sha256") == V.HMAC_VECTOR_SET_SHA256


def test_g0_r5_local_projection_reseal_cannot_relax_payload_profiles():
    doc = _synthetic_r2_doc()
    doc["adjudication_contract"]["hmac_domain_payload_profiles"] = {
        domain: "ambiguous_json" for domain in _R5_DOMAIN_PROFILES
    }
    _refresh_r3_commitments(doc)
    assert "E_HMAC_CONTRACT" in _codes(V.validate_document(doc, _schema()))


# ---------------------------------------- G0 adjudication contract revision 6
_R6_ERROR_TYPE = "canonical_type"
_R6_ERROR_SYNTAX = "canonical_syntax"
_R6_ERROR_BOUNDS = "canonical_bounds"
_R6_ERROR_RESOURCE = "canonical_resource"
_R6_ERROR_DEPTH = "canonical_depth"
_R6_ERROR_CYCLE = "canonical_cycle"
_R6_ERROR_ROOT = "canonical_root"
_R6_ERROR_INTERNAL = "canonical_internal"
_R6_CONTRACT_VERSION = "smartkey-g0-hmac-byte-contract-v4"
_R6_RESOURCE_VERSION = "smartkey-g0-hmac-resource-contract-v1"
_R6_RESOURCE_LIMITS = {
    "max_raw_utf8_bytes": 262_144,
    "max_canonical_payload_bytes": 65_536,
    "max_lexical_nesting_depth": 32,
    "max_typed_nesting_depth": 32,
    "max_total_nodes": 4_096,
    "max_array_members": 1_024,
    "max_object_members": 512,
    "max_string_utf8_bytes": 65_528,
    "max_key_utf8_bytes": 1_024,
    "max_integer_token_digits": 16,
}
_R6_DOMAIN_ROOT_TYPES = {
    "value": "exact_str",
    "event": "exact_dict",
    "metadata": "exact_dict",
    "source_record": "exact_dict",
}
_R6_RESOURCE_SHA256 = "50c430ef4de54c935e9bbbc4e6929dc7fbd28ba0f549da843526dcaf0f271bab"
_R6_VECTOR_SET_SHA256 = (
    "8f6644ed05a898bd650f4e42dca4bc278e19db601523ed5442fcc20dd5ab0b3c"
)
_R6_EVENT_VALUE = {
    "active": True,
    "count": 2,
    "event": "synthetic",
    "parts": ["alpha", None, {"ok": False}],
    "unit_separator": "\x1f",
    "\ue000": "bmp",
    "\U00010000": "astral",
}
_R6_EVENT_PAYLOAD_HEX = (
    "7b22616374697665223a747275652c22636f756e74223a322c226576656e7422"
    "3a2273796e746865746963222c227061727473223a5b22616c706861222c6e75"
    "6c6c2c7b226f6b223a66616c73657d5d2c22756e69745f736570617261746f"
    "72223a225c7530303166222c22ee8080223a22626d70222c22f0908080223a22"
    "61737472616c227d"
)
_R6_EVENT_FRAME_HEX = (
    "736d6172746b65792d67302d686d61632d7368613235362d7631006576656e74"
    "0000000000000000877b22616374697665223a747275652c22636f756e74223a"
    "322c226576656e74223a2273796e746865746963222c227061727473223a5b22"
    "616c706861222c6e756c6c2c7b226f6b223a66616c73657d5d2c22756e6974"
    "5f736570617261746f72223a225c7530303166222c22ee8080223a22626d7022"
    "2c22f0908080223a2261737472616c227d"
)
_R6_EVENT_MAC = "800c5952070ce32356a99537c95d2eeec515a854a7356379623d5ae4001ae94d"


class _R6BenignInt(int):
    pass


class _R6BenignStr(str):
    pass


class _R6BenignList(list):
    pass


class _R6BenignDict(dict):
    pass


class _R6HostileInt(int):
    def __new__(cls, value):
        instance = int.__new__(cls, value)
        instance.touched = False
        return instance

    def _trip(self, *_args, **_kwargs):
        self.touched = True
        raise RuntimeError("hostile integer override executed")

    __abs__ = _trip
    __int__ = _trip
    __index__ = _trip
    __str__ = _trip


class _R6HostileStr(str):
    def __new__(cls, value, *, armed=True):
        instance = str.__new__(cls, value)
        instance.armed = armed
        instance.touched = False
        return instance

    def _trip(self, *_args, **_kwargs):
        if self.armed:
            self.touched = True
            raise RuntimeError("hostile string override executed")
        return None

    def __iter__(self):
        self._trip()
        return str.__iter__(self)

    def __len__(self):
        self._trip()
        return str.__len__(self)

    def encode(self, *args, **kwargs):
        self._trip()
        return str.encode(self, *args, **kwargs)

    def __hash__(self):
        if self.armed:
            self._trip()
        return str.__hash__(self)

    def __lt__(self, other):
        self._trip()
        return str.__lt__(self, other)


class _R6HostileList(list):
    def __init__(self, value):
        list.__init__(self, value)
        self.touched = False

    def _trip(self, *_args, **_kwargs):
        self.touched = True
        raise RuntimeError("hostile list override executed")

    __iter__ = _trip
    __len__ = _trip
    __getitem__ = _trip


class _R6HostileDict(dict):
    def __init__(self, value):
        dict.__init__(self, value)
        self.touched = False

    def _trip(self, *_args, **_kwargs):
        self.touched = True
        raise RuntimeError("hostile dict override executed")

    __iter__ = _trip
    __len__ = _trip
    __getitem__ = _trip
    items = _trip
    keys = _trip


def _r6_value_contains_marker(value, marker: str, seen: set[int] | None = None) -> bool:
    if seen is None:
        seen = set()
    identity = id(value)
    if identity in seen:
        return False
    seen.add(identity)
    if isinstance(value, str):
        return str.__contains__(value, marker)
    if isinstance(value, bytes):
        return bytes.__contains__(value, marker.encode("utf-8"))
    if isinstance(value, tuple):
        return any(_r6_value_contains_marker(item, marker, seen) for item in value)
    if isinstance(value, list):
        return any(
            _r6_value_contains_marker(item, marker, seen)
            for item in list.__iter__(value)
        )
    if isinstance(value, dict):
        return any(
            _r6_value_contains_marker(key, marker, seen)
            or _r6_value_contains_marker(item, marker, seen)
            for key, item in dict.items(value)
        )
    try:
        attributes = object.__getattribute__(value, "__dict__")
    except (AttributeError, TypeError):
        return False
    return _r6_value_contains_marker(attributes, marker, seen)


def _r6_clean_error(call, expected_code: str, marker: str | None = None):
    with pytest.raises(V.CanonicalJsonError) as caught:
        call()
    error = caught.value
    assert error.args == (expected_code,)
    assert error.__cause__ is None
    assert error.__context__ is None
    if marker is not None:
        assert not _r6_value_contains_marker(error.args, marker)
        assert not _r6_value_contains_marker(vars(error), marker)
        traceback_cursor = error.__traceback__
        while traceback_cursor is not None:
            frame = traceback_cursor.tb_frame
            if Path(frame.f_code.co_filename).resolve() != Path(__file__).resolve():
                assert not _r6_value_contains_marker(frame.f_locals, marker)
            traceback_cursor = traceback_cursor.tb_next
    return error


def _r6_clean_hmac_contract_error(call, marker: str | None = None):
    with pytest.raises(ValueError) as caught:
        call()
    error = caught.value
    assert type(error) is ValueError
    assert error.args == ("E_HMAC_CONTRACT",)
    assert error.__cause__ is None
    assert error.__context__ is None
    if marker is not None:
        assert not _r6_value_contains_marker(error.args, marker)
        assert not _r6_value_contains_marker(vars(error), marker)
    return error


@pytest.mark.parametrize(
    "value",
    [
        _R6BenignInt(1),
        _R6BenignStr("synthetic"),
        _R6BenignList([None]),
        _R6BenignDict({"key": None}),
    ],
)
def test_g0_r6_typed_encoder_rejects_every_benign_builtin_subclass(value):
    _r6_clean_error(lambda: V.canonical_structured_payload_bytes(value), _R6_ERROR_TYPE)


def test_g0_r6_raw_parser_requires_exact_builtin_string():
    _r6_clean_error(
        lambda: V.parse_project_canonical_json(_R6BenignStr("{}")),
        _R6_ERROR_TYPE,
    )


def test_g0_r6_scalar_value_requires_exact_builtin_string():
    _r6_clean_error(
        lambda: V.hmac_payload_bytes("value", _R6BenignStr("synthetic")),
        _R6_ERROR_TYPE,
    )


@pytest.mark.parametrize(
    "factory",
    [
        lambda: _R6HostileInt(1),
        lambda: _R6HostileStr("private_hostile_marker"),
        lambda: _R6HostileList([None]),
        lambda: _R6HostileDict({"key": None}),
    ],
)
def test_g0_r6_hostile_typed_subclass_override_is_never_invoked(factory):
    value = factory()
    _r6_clean_error(
        lambda: V.canonical_structured_payload_bytes(value),
        _R6_ERROR_TYPE,
        "private_hostile_marker",
    )
    assert value.touched is False


def test_g0_r6_hostile_raw_string_override_is_never_invoked():
    value = _R6HostileStr('{"private_hostile_marker":null}')
    _r6_clean_error(
        lambda: V.parse_project_canonical_json(value),
        _R6_ERROR_TYPE,
        "private_hostile_marker",
    )
    assert value.touched is False


def test_g0_r6_hostile_scalar_string_override_is_never_invoked():
    value = _R6HostileStr("private_hostile_marker")
    _r6_clean_error(
        lambda: V.hmac_payload_bytes("value", value),
        _R6_ERROR_TYPE,
        "private_hostile_marker",
    )
    assert value.touched is False


def test_g0_r6_hostile_dictionary_key_override_is_never_invoked():
    key = _R6HostileStr("private_hostile_marker", armed=False)
    value = {key: None}
    key.armed = True
    _r6_clean_error(
        lambda: V.canonical_structured_payload_bytes(value),
        _R6_ERROR_TYPE,
        "private_hostile_marker",
    )
    assert key.touched is False


@pytest.mark.parametrize(
    ("raw", "expected_code"),
    [
        ('{"private_raw_marker":', _R6_ERROR_SYNTAX),
        ('{"key":"private_raw_marker","key":null}', _R6_ERROR_SYNTAX),
        ("-0", _R6_ERROR_BOUNDS),
        ("1.0", _R6_ERROR_BOUNDS),
        ("NaN", _R6_ERROR_BOUNDS),
        ("Infinity", _R6_ERROR_BOUNDS),
        ("-Infinity", _R6_ERROR_BOUNDS),
        ("9007199254740992", _R6_ERROR_BOUNDS),
    ],
)
def test_g0_r6_raw_failures_are_fixed_nonreflective_errors(raw, expected_code):
    marker = "private_raw_marker" if "private_raw_marker" in raw else None
    _r6_clean_error(lambda: V.parse_project_canonical_json(raw), expected_code, marker)


def test_g0_r6_typed_failure_drops_sensitive_traceback_locals():
    value = {"key": "private_typed_marker\ud800"}
    _r6_clean_error(
        lambda: V.canonical_structured_payload_bytes(value),
        _R6_ERROR_TYPE,
        "private_typed_marker",
    )


def test_g0_r6_cycle_failure_is_normalized_and_nonreflective():
    value = {"private_cycle_marker": []}
    value["private_cycle_marker"].append(value)
    _r6_clean_error(
        lambda: V.canonical_structured_payload_bytes(value),
        _R6_ERROR_CYCLE,
        "private_cycle_marker",
    )


def test_g0_r6_unexpected_scanner_failure_is_normalized_and_nonreflective(monkeypatch):
    def fail_scanner(_text):
        raise RuntimeError("private_internal_marker")

    monkeypatch.setattr(V, "_scan_raw_resources", fail_scanner)
    _r6_clean_error(
        lambda: V.parse_project_canonical_json('{"private_internal_marker":null}'),
        _R6_ERROR_INTERNAL,
        "private_internal_marker",
    )


def test_g0_r6_unexpected_encoder_failure_is_normalized_and_nonreflective(monkeypatch):
    def fail_encoder(_value, _output, _state, _depth):
        raise RuntimeError("private_internal_marker")

    monkeypatch.setattr(V, "_encode_typed_value", fail_encoder)
    _r6_clean_error(
        lambda: V.canonical_structured_payload_bytes({"private_internal_marker": None}),
        _R6_ERROR_INTERNAL,
        "private_internal_marker",
    )


def test_g0_r6_unexpected_scalar_failure_is_normalized_and_nonreflective(monkeypatch):
    def fail_scalar(_char):
        raise RuntimeError("private_internal_marker")

    monkeypatch.setattr(V, "ord", fail_scalar, raising=False)
    _r6_clean_error(
        lambda: V.hmac_payload_bytes("value", "private_internal_marker"),
        _R6_ERROR_INTERNAL,
        "private_internal_marker",
    )


def _r6_nested_dict(depth: int):
    value = None
    for _index in range(depth):
        value = {"x": value}
    return value


def _r6_node_boundary(extra_node: bool):
    lengths = [1_024, 1_024, 1_024, 1_015 + int(extra_node)]
    return {f"k{index}": [None] * length for index, length in enumerate(lengths)}


def test_g0_r6_resource_contract_constants_are_exact():
    assert V.HMAC_CONTRACT_VERSION == _R6_CONTRACT_VERSION
    assert V.HMAC_RESOURCE_CONTRACT_VERSION == _R6_RESOURCE_VERSION
    assert V.HMAC_RESOURCE_LIMITS == _R6_RESOURCE_LIMITS
    assert V.HMAC_DOMAIN_ROOT_TYPES == _R6_DOMAIN_ROOT_TYPES
    assert V.HMAC_RESOURCE_CONTRACT_SHA256 == _R6_RESOURCE_SHA256
    encoded = json.dumps(
        {
            "version": _R6_RESOURCE_VERSION,
            "domain_root_types": _R6_DOMAIN_ROOT_TYPES,
            "limits": _R6_RESOURCE_LIMITS,
        },
        ensure_ascii=True,
        separators=(",", ":"),
        sort_keys=True,
    ).encode("ascii")
    assert hashlib.sha256(encoded).hexdigest() == _R6_RESOURCE_SHA256


def test_g0_r6_raw_utf8_byte_boundary_passes_and_plus_one_fails():
    maximum = _R6_RESOURCE_LIMITS["max_raw_utf8_bytes"]
    at_limit = "{}" + " " * (maximum - 2)
    assert V.parse_project_canonical_json(at_limit) == {}
    _r6_clean_error(
        lambda: V.parse_project_canonical_json(at_limit + " "), _R6_ERROR_RESOURCE
    )
    multibyte_value = "é" * 32_764
    multibyte = json.dumps(multibyte_value, ensure_ascii=False)
    multibyte += " " * (maximum - len(multibyte.encode("utf-8")))
    assert len(multibyte.encode("utf-8")) == maximum
    assert V.parse_project_canonical_json(multibyte) == multibyte_value
    _r6_clean_error(
        lambda: V.parse_project_canonical_json(multibyte + " "),
        _R6_ERROR_RESOURCE,
    )


def test_g0_r6_string_and_canonical_payload_boundary_passes_plus_one_fails():
    maximum = _R6_RESOURCE_LIMITS["max_string_utf8_bytes"]
    payload = V.canonical_structured_payload_bytes({"x": "a" * maximum})
    assert len(payload) == _R6_RESOURCE_LIMITS["max_canonical_payload_bytes"]
    assert V.parse_project_canonical_json(payload.decode("ascii")) == {
        "x": "a" * maximum
    }
    _r6_clean_error(
        lambda: V.canonical_structured_payload_bytes({"x": "a" * (maximum + 1)}),
        _R6_ERROR_RESOURCE,
    )

    at_payload_limit = {"a": "a" * 32_760, "b": "b" * 32_761}
    assert len(V.canonical_structured_payload_bytes(at_payload_limit)) == 65_536
    over_payload_limit = {"a": "a" * 32_760, "b": "b" * 32_762}
    _r6_clean_error(
        lambda: V.canonical_structured_payload_bytes(over_payload_limit),
        _R6_ERROR_RESOURCE,
    )


def test_g0_r6_key_byte_boundary_passes_and_plus_one_fails():
    maximum = _R6_RESOURCE_LIMITS["max_key_utf8_bytes"]
    assert V.canonical_structured_payload_bytes({"k" * maximum: None})
    raw_at_limit = json.dumps({"k" * maximum: None}, separators=(",", ":"))
    assert V.parse_project_canonical_json(raw_at_limit) == {"k" * maximum: None}
    _r6_clean_error(
        lambda: V.canonical_structured_payload_bytes({"k" * (maximum + 1): None}),
        _R6_ERROR_RESOURCE,
    )


def test_g0_r6_array_member_boundary_passes_and_plus_one_fails_raw_and_typed():
    maximum = _R6_RESOURCE_LIMITS["max_array_members"]
    assert V.canonical_structured_payload_bytes({"items": [None] * maximum})
    raw_at_limit = "[" + ",".join("null" for _index in range(maximum)) + "]"
    assert len(V.parse_project_canonical_json(raw_at_limit)) == maximum
    _r6_clean_error(
        lambda: V.canonical_structured_payload_bytes({"items": [None] * (maximum + 1)}),
        _R6_ERROR_RESOURCE,
    )
    raw = "[" + ",".join("null" for _index in range(maximum + 1)) + "]"
    _r6_clean_error(lambda: V.parse_project_canonical_json(raw), _R6_ERROR_RESOURCE)


def test_g0_r6_object_member_boundary_passes_and_plus_one_fails_raw_and_typed():
    maximum = _R6_RESOURCE_LIMITS["max_object_members"]
    at_limit = {f"k{index}": None for index in range(maximum)}
    assert V.canonical_structured_payload_bytes(at_limit)
    raw_at_limit = json.dumps(at_limit, separators=(",", ":"), sort_keys=True)
    assert V.parse_project_canonical_json(raw_at_limit) == at_limit
    over_limit = {f"k{index}": None for index in range(maximum + 1)}
    _r6_clean_error(
        lambda: V.canonical_structured_payload_bytes(over_limit),
        _R6_ERROR_RESOURCE,
    )
    raw = json.dumps(over_limit, separators=(",", ":"), sort_keys=True)
    _r6_clean_error(lambda: V.parse_project_canonical_json(raw), _R6_ERROR_RESOURCE)


def test_g0_r6_total_node_boundary_passes_and_plus_one_fails():
    at_limit = _r6_node_boundary(False)
    assert V.canonical_structured_payload_bytes(at_limit)
    assert (
        V.parse_project_canonical_json(json.dumps(at_limit, separators=(",", ":")))
        == at_limit
    )
    _r6_clean_error(
        lambda: V.canonical_structured_payload_bytes(_r6_node_boundary(True)),
        _R6_ERROR_RESOURCE,
    )
    raw_over_limit = json.dumps(_r6_node_boundary(True), separators=(",", ":"))
    _r6_clean_error(
        lambda: V.parse_project_canonical_json(raw_over_limit), _R6_ERROR_RESOURCE
    )


def test_g0_r6_depth_boundary_passes_and_plus_one_fails_raw_and_typed():
    maximum = _R6_RESOURCE_LIMITS["max_typed_nesting_depth"]
    at_limit = _r6_nested_dict(maximum)
    assert V.canonical_structured_payload_bytes(at_limit)
    _r6_clean_error(
        lambda: V.canonical_structured_payload_bytes(_r6_nested_dict(maximum + 1)),
        _R6_ERROR_DEPTH,
    )
    assert (
        V.parse_project_canonical_json(json.dumps(at_limit, separators=(",", ":")))
        == at_limit
    )
    raw_over_limit = json.dumps(_r6_nested_dict(maximum + 1), separators=(",", ":"))
    _r6_clean_error(
        lambda: V.parse_project_canonical_json(raw_over_limit), _R6_ERROR_DEPTH
    )


@pytest.mark.parametrize("digits", [4_300, 4_301])
def test_g0_r6_python_integer_threshold_inputs_are_preflight_bounded(digits):
    raw = "9" * digits
    _r6_clean_error(lambda: V.parse_project_canonical_json(raw), _R6_ERROR_RESOURCE)


def test_g0_r6_safe_integer_boundary_passes_and_next_value_is_bounded():
    assert V.parse_project_canonical_json("9007199254740991") == 9_007_199_254_740_991
    _r6_clean_error(
        lambda: V.parse_project_canonical_json("9007199254740992"),
        _R6_ERROR_BOUNDS,
    )


def test_g0_r6_shallow_huge_inputs_fail_with_contract_errors():
    raw = " " * 1_000_000
    _r6_clean_error(lambda: V.parse_project_canonical_json(raw), _R6_ERROR_RESOURCE)
    huge_string = "x" * 1_000_000
    _r6_clean_error(
        lambda: V.parse_project_canonical_json(json.dumps(huge_string)),
        _R6_ERROR_RESOURCE,
    )
    _r6_clean_error(
        lambda: V.canonical_structured_payload_bytes(huge_string),
        _R6_ERROR_RESOURCE,
    )
    typed = [None] * 100_000
    _r6_clean_error(
        lambda: V.canonical_structured_payload_bytes(typed), _R6_ERROR_RESOURCE
    )


@pytest.mark.parametrize("domain", ["event", "metadata", "source_record"])
@pytest.mark.parametrize("value", [None, True, 1, "synthetic", []])
def test_g0_r6_structured_hmac_domains_require_exact_dict_root(domain, value):
    if domain == "source_record":
        _r6_clean_hmac_contract_error(lambda: V.hmac_payload_bytes(domain, value))
    else:
        _r6_clean_error(lambda: V.hmac_payload_bytes(domain, value), _R6_ERROR_ROOT)


@pytest.mark.parametrize("domain", ["event", "metadata", "source_record"])
def test_g0_r6_structured_hmac_root_rejects_hostile_dict_subclass(domain):
    value = _R6HostileDict({"private_hostile_marker": None})
    if domain == "source_record":
        _r6_clean_hmac_contract_error(
            lambda: V.hmac_payload_bytes(domain, value),
            "private_hostile_marker",
        )
    else:
        _r6_clean_error(
            lambda: V.hmac_payload_bytes(domain, value),
            _R6_ERROR_ROOT,
            "private_hostile_marker",
        )
    assert value.touched is False


@pytest.mark.parametrize("domain", ["event", "metadata", "source_record"])
def test_g0_r6_structured_hmac_domains_accept_nested_scalars_and_arrays(domain):
    if domain == "source_record":
        value = copy.deepcopy(_R5_VECTORS[3]["value"])
        value["profile"] = V.SOURCE_RECORD_HMAC_PROFILE
        value["scope_ref"] = "orig:0123456789abcdef"
        assert V.hmac_payload_bytes(domain, value) == (
            V.canonical_structured_payload_bytes(value)
        )
    else:
        value = {"nested": [None, True, 1, "synthetic", {"ok": False}]}
        assert V.hmac_payload_bytes(domain, value).startswith(b'{"nested":[')


def test_g0_r6_event_vector_pins_codepoint_order_control_escape_frame_and_mac():
    payload = V.hmac_payload_bytes("event", _R6_EVENT_VALUE)
    frame = V.hmac_frame_bytes("event", _R6_EVENT_VALUE)
    assert payload.hex() == _R6_EVENT_PAYLOAD_HEX
    assert frame.hex() == _R6_EVENT_FRAME_HEX
    assert hmac.new(_R5_PUBLIC_VECTOR_KEY, frame, hashlib.sha256).hexdigest() == (
        _R6_EVENT_MAC
    )
    assert payload.index("\ue000".encode("utf-8")) < payload.index(
        "\U00010000".encode("utf-8")
    )
    assert b"\\u001f" in payload


def test_g0_r6_node_explicit_codepoint_sort_matches_and_default_sort_fails():
    script = r"""
const value = {
  active: true,
  count: 2,
  event: "synthetic",
  parts: ["alpha", null, {ok: false}],
  unit_separator: "\u001f",
  "\uE000": "bmp",
  "\u{10000}": "astral",
};
const compareCodePoints = (left, right) => {
  const a = Array.from(left, char => char.codePointAt(0));
  const b = Array.from(right, char => char.codePointAt(0));
  for (let index = 0; index < Math.min(a.length, b.length); index++) {
    if (a[index] !== b[index]) return a[index] - b[index];
  }
  return a.length - b.length;
};
const encode = (item, comparator) => {
  if (item === null || typeof item !== "object") return JSON.stringify(item);
  if (Array.isArray(item)) return "[" + item.map(v => encode(v, comparator)).join(",") + "]";
  return "{" + Object.keys(item).sort(comparator).map(
    key => JSON.stringify(key) + ":" + encode(item[key], comparator)
  ).join(",") + "}";
};
process.stdout.write(
  Buffer.from(encode(value, compareCodePoints)).toString("hex") + "\n" +
  Buffer.from(encode(value, undefined)).toString("hex") + "\n"
);
"""
    result = subprocess.run(
        ["node", "-e", script], check=True, capture_output=True, text=True
    )
    explicit_hex, default_hex = result.stdout.splitlines()
    assert explicit_hex == _R6_EVENT_PAYLOAD_HEX
    assert default_hex != _R6_EVENT_PAYLOAD_HEX


def test_g0_r6_vector_set_and_sealed_resource_receipt_are_pinned():
    assert V.HMAC_VECTOR_SET_SHA256 == _R6_VECTOR_SET_SHA256
    doc = _synthetic_r2_doc()
    contract = doc["adjudication_contract"]
    assert contract.get("hmac_contract_version") == _R6_CONTRACT_VERSION
    assert contract.get("hmac_resource_contract_version") == _R6_RESOURCE_VERSION
    assert contract.get("hmac_resource_limits") == _R6_RESOURCE_LIMITS
    assert contract.get("hmac_domain_root_types") == _R6_DOMAIN_ROOT_TYPES
    assert contract.get("hmac_resource_contract_sha256") == _R6_RESOURCE_SHA256
    assert contract.get("hmac_vector_set_sha256") == _R6_VECTOR_SET_SHA256
    receipt = contract["hmac_external_receipt"]
    assert receipt.get("contract_version") == _R6_CONTRACT_VERSION
    assert receipt.get("resource_contract_version") == _R6_RESOURCE_VERSION
    assert receipt.get("resource_contract_sha256") == _R6_RESOURCE_SHA256
    assert receipt.get("vector_set_sha256") == _R6_VECTOR_SET_SHA256


@pytest.mark.parametrize(
    ("field", "replacement"),
    [
        ("hmac_resource_contract_version", "unversioned"),
        ("hmac_resource_limits", {**_R6_RESOURCE_LIMITS, "max_total_nodes": 4_097}),
        ("hmac_domain_root_types", {**_R6_DOMAIN_ROOT_TYPES, "event": "any"}),
        ("hmac_resource_contract_sha256", "f" * 64),
        ("hmac_vector_set_sha256", "f" * 64),
    ],
)
def test_g0_r6_local_projection_reseal_cannot_tamper_contract(field, replacement):
    doc = _synthetic_r2_doc()
    doc["adjudication_contract"][field] = replacement
    _refresh_r3_commitments(doc)
    assert "E_HMAC_CONTRACT" in _codes(V.validate_document(doc, _schema()))


@pytest.mark.parametrize(
    ("field", "replacement"),
    [
        ("contract_version", "unversioned"),
        ("resource_contract_version", "unversioned"),
        ("resource_contract_sha256", "f" * 64),
        ("vector_set_sha256", "f" * 64),
    ],
)
def test_g0_r6_external_receipt_cannot_tamper_resource_or_vector_contract(
    field, replacement
):
    doc = _synthetic_r2_doc()
    doc["adjudication_contract"]["hmac_external_receipt"][field] = replacement
    _refresh_r3_commitments(doc)
    assert "E_HMAC_CONTRACT" in _codes(V.validate_document(doc, _schema()))


# ---------------------------------------- G0 adjudication contract revision 7
def _r7_spy_json_loads(monkeypatch):
    real_loads = V.json.loads
    calls = []

    def spy(text, *args, **kwargs):
        calls.append(len(text))
        return real_loads(text, *args, **kwargs)

    monkeypatch.setattr(V.json, "loads", spy)
    return calls


def _r7_mixed_escaped_content(decoded_utf8_bytes: int) -> str:
    fixed = "a" + r"\/" + r"\u0062" + r"\ud83d\ude00"
    fixed_decoded_utf8_bytes = 7
    assert decoded_utf8_bytes >= fixed_decoded_utf8_bytes
    return fixed + "c" * (decoded_utf8_bytes - fixed_decoded_utf8_bytes)


def _r7_mutate_on_first_append(monkeypatch, fragment: bytes, mutation):
    real_append = V._append_output
    state = {"calls": 0}

    def mutate_after_append(output, current):
        code = real_append(output, current)
        if current == fragment and state["calls"] == 0:
            state["calls"] += 1
            mutation()
        return code

    monkeypatch.setattr(V, "_append_output", mutate_after_append)
    return state


def _r7_mutate_on_first_sort(monkeypatch, mutation):
    real_sorted = sorted
    state = {"calls": 0}

    def mutate_before_sort(iterable, *args, **kwargs):
        if state["calls"] == 0:
            state["calls"] += 1
            mutation()
        return real_sorted(iterable, *args, **kwargs)

    monkeypatch.setattr(V, "sorted", mutate_before_sort, raising=False)
    return state


def test_g0_r7_simple_escape_decoded_limit_is_enforced_before_json_loads(
    monkeypatch,
):
    maximum = _R6_RESOURCE_LIMITS["max_string_utf8_bytes"]
    exact = '"' + r"\/" * maximum + '"'
    calls = _r7_spy_json_loads(monkeypatch)

    assert V.parse_project_canonical_json(exact) == "/" * maximum
    assert len(calls) == 1

    _r6_clean_error(
        lambda: V.parse_project_canonical_json('"' + r"\/" * (maximum + 1) + '"'),
        _R6_ERROR_RESOURCE,
    )
    assert len(calls) == 1


def test_g0_r7_simple_escape_overflow_has_clean_error_without_calling_json_loads(
    monkeypatch,
):
    maximum = _R6_RESOURCE_LIMITS["max_string_utf8_bytes"]
    marker = "private_escape_marker"
    raw = '"' + marker + r"\/" * (maximum + 1 - len(marker)) + '"'
    calls = _r7_spy_json_loads(monkeypatch)

    _r6_clean_error(
        lambda: V.parse_project_canonical_json(raw),
        _R6_ERROR_RESOURCE,
        marker,
    )
    assert calls == []


@pytest.mark.parametrize(
    ("as_key", "maximum"),
    [
        (False, _R6_RESOURCE_LIMITS["max_string_utf8_bytes"]),
        (True, _R6_RESOURCE_LIMITS["max_key_utf8_bytes"]),
    ],
)
def test_g0_r7_mixed_escape_boundary_is_preflight_bounded(monkeypatch, as_key, maximum):
    content = _r7_mixed_escaped_content(maximum)
    exact = '{"' + content + '":null}' if as_key else '"' + content + '"'
    overflow = (
        '{"' + content + r"\/" + '":null}' if as_key else '"' + content + r"\/" + '"'
    )
    calls = _r7_spy_json_loads(monkeypatch)

    parsed = V.parse_project_canonical_json(exact)
    parsed_text = next(iter(parsed)) if as_key else parsed
    assert len(parsed_text.encode("utf-8")) == maximum
    assert len(calls) == 1

    _r6_clean_error(
        lambda: V.parse_project_canonical_json(overflow),
        _R6_ERROR_RESOURCE,
    )
    assert len(calls) == 1


@pytest.mark.parametrize("final_size", [1_025, 4_095])
def test_g0_r7_list_snapshot_is_stable_when_live_list_grows(monkeypatch, final_size):
    source = [0] * 1_024
    expected = b"[" + b",".join([b"0"] * 1_024) + b"]"
    state = _r7_mutate_on_first_append(
        monkeypatch,
        b"[",
        lambda: source.extend([1] * (final_size - len(source))),
    )

    started = time.monotonic()
    actual = V.canonical_structured_payload_bytes(source)
    elapsed = time.monotonic() - started

    assert actual == expected
    assert elapsed < 5.0
    assert state["calls"] == 1
    assert len(source) == final_size


def test_g0_r7_dict_snapshot_is_stable_when_live_size_changes(monkeypatch):
    source = {f"k{index:03d}": 0 for index in range(512)}
    expected = json.dumps(source, separators=(",", ":"), sort_keys=True).encode()
    state = _r7_mutate_on_first_sort(
        monkeypatch,
        lambda: source.__setitem__("overflow", 1),
    )

    assert V.canonical_structured_payload_bytes(source) == expected
    assert state["calls"] == 1
    assert len(source) == 513


def test_g0_r7_dict_snapshot_never_rereads_live_values(monkeypatch):
    source = {"key": "before"}
    state = _r7_mutate_on_first_append(
        monkeypatch,
        b"{",
        lambda: source.__setitem__("key", "after"),
    )

    assert V.canonical_structured_payload_bytes(source) == b'{"key":"before"}'
    assert state["calls"] == 1
    assert source == {"key": "after"}


def test_g0_r7_nested_container_is_deeply_snapshotted_before_sort(monkeypatch):
    nested = [0]
    source = {"nested": nested}
    state = _r7_mutate_on_first_sort(monkeypatch, lambda: nested.append(1))

    assert V.canonical_structured_payload_bytes(source) == b'{"nested":[0]}'
    assert state["calls"] == 1
    assert nested == [0, 1]


def test_g0_r7_repeated_alias_reuses_first_completed_snapshot(monkeypatch):
    shared = {"value": 0}
    source = [shared, shared]
    state = _r7_mutate_on_first_sort(
        monkeypatch,
        lambda: shared.__setitem__("value", 1),
    )

    assert V.canonical_structured_payload_bytes(source) == (
        b'[{"value":0},{"value":0}]'
    )
    assert state["calls"] == 1
    assert shared == {"value": 1}


def test_g0_r7_overlimit_key_is_rejected_before_sort(monkeypatch):
    marker = "private_snapshot_marker"
    source = {marker + "k" * (1_025 - len(marker)): None}
    state = {"calls": 0}

    def forbidden_sort(*_args, **_kwargs):
        state["calls"] += 1
        raise RuntimeError(marker)

    monkeypatch.setattr(V, "sorted", forbidden_sort, raising=False)
    _r6_clean_error(
        lambda: V.canonical_structured_payload_bytes(source),
        _R6_ERROR_RESOURCE,
        marker,
    )
    assert state["calls"] == 0


def test_g0_r7_sort_failure_is_normalized_without_source_retention(monkeypatch):
    marker = "private_snapshot_marker"
    source = {marker: None}

    def fail_sort(*_args, **_kwargs):
        raise RuntimeError(marker)

    monkeypatch.setattr(V, "sorted", fail_sort, raising=False)
    _r6_clean_error(
        lambda: V.canonical_structured_payload_bytes(source),
        _R6_ERROR_INTERNAL,
        marker,
    )


def test_g0_r7_dict_capture_failure_is_normalized_without_source_retention(
    monkeypatch,
):
    marker = "private_snapshot_marker"
    source = {marker: None}

    def fail_capture(*_args, **_kwargs):
        raise RuntimeError(marker)

    monkeypatch.setattr(V, "islice", fail_capture)
    _r6_clean_error(
        lambda: V.canonical_structured_payload_bytes(source),
        _R6_ERROR_INTERNAL,
        marker,
    )


def test_g0_r7_snapshot_preserves_cycle_rejection_and_static_bytes():
    static = {"b": [None, True, 1, "x"], "a": {"z": False}}
    assert V.canonical_structured_payload_bytes(static) == (
        b'{"a":{"z":false},"b":[null,true,1,"x"]}'
    )

    cyclic = []
    cyclic.append(cyclic)
    _r6_clean_error(
        lambda: V.canonical_structured_payload_bytes(cyclic),
        _R6_ERROR_CYCLE,
    )
