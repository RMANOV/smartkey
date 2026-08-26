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
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

import pytest

os.environ.setdefault(
    "SMARTKEY_PHASEA_DATA", tempfile.mkdtemp(prefix="smartkey-test-phasea-")
)

_REPO = Path(__file__).resolve().parent.parent
_DIR = _REPO / "diagnostics" / "anomalies"
_REGISTRY = _DIR / "registry.json"
_SCHEMA = _DIR / "schema.json"
_VALIDATOR = _DIR / "validate.py"

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
    spec = importlib.util.spec_from_file_location("smartkey_anomaly_validate", _VALIDATOR)
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


def _find(doc: dict, observed: str, expected: str) -> dict:
    for rec in doc["records"]:
        if rec["observed"]["token"] == observed and rec["expected"]["token"] == expected:
            return rec
    raise AssertionError(f"pair {observed!r} -> {expected!r} missing from registry")


def _refresh_identity(rec: dict) -> None:
    rec["dedup_key"] = V.compute_dedup_key(rec)
    rec["id"] = V.compute_id(rec)


def _synthetic_opaque_ref(label: str) -> str:
    return hashlib.sha256(f"synthetic:{label}".encode("utf-8")).hexdigest()


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
        descriptor = "metadata:" + _synthetic_opaque_ref(
            f"descriptor:{disposition}:{ordinal}"
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
            "ref": f"synthetic:evidence:{ordinal}",
            "build_sha": "unknown",
            "summary": "metadata:" + _synthetic_opaque_ref("evidence:summary"),
        }
        closure_reason = "metadata:" + _synthetic_opaque_ref("closure:reason")
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
                "ref": f"synthetic:legacy:{ordinal}",
                "observed_utc": None,
                "build_sha": None,
                "surface": "synthetic fixture",
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
    "orig:01dddf93a4946369", "orig:03078aa3f0211682",
    "orig:077e507ca5b404c3", "orig:0a1f4eea084f295d",
    "orig:0b54da36ae8fdd6a", "orig:1825fef17b7d6958",
    "orig:19cb8c152dd465d5", "orig:1bf4e08e6c94bbaf",
    "orig:1ce28219e06b7de2", "orig:1fe9e0c8a0b903fa",
    "orig:2072a8f696034a65", "orig:21ca2a4de3d3549a",
    "orig:260d1f3b9c057661", "orig:286349b461143bc3",
    "orig:29b59535f67faf39", "orig:2db970d7241fb6ba",
    "orig:31ace7712b72546c", "orig:32a1249096ca0101",
    "orig:32cd3ab535039553", "orig:347798fcd3dc3674",
    "orig:349b676e76d10fb3", "orig:35853c01ba074901",
    "orig:3cc100e857d1c0eb", "orig:3dda31f67a7e0504",
    "orig:4119a99c3bdbd501", "orig:41ad2b57e9c7fac6",
    "orig:445b1bcb07b1fb2f", "orig:468e0ba3d0ea4a62",
    "orig:471c7a33f9a969ac", "orig:4aeb912add6cdfb5",
    "orig:4cf19f9ecc7feb1b", "orig:4dd4fadeafbfb404",
    "orig:4fcea9ac064464fe", "orig:51c0fa6d3356962d",
    "orig:5ce5d7a27704b57c", "orig:5ea197799c601113",
    "orig:6330045760ba0612", "orig:63482ea1d7333296",
    "orig:6399efd2fbf9f937", "orig:6511e29c9434b123",
    "orig:65c3f0433762c985", "orig:6826ae16029f7972",
    "orig:6831cf6ed6d41185", "orig:6a44807c71050404",
    "orig:6e72417255fd294b", "orig:6edca44b9be0c444",
    "orig:7370ec2b227900fd", "orig:74e5d7e1d7a7aacf",
    "orig:7abc6ead3e07f378", "orig:7c82590873671bec",
    "orig:7d68d95b8291fb41", "orig:7de6b869119c55d5",
    "orig:806d02defb5c17a2", "orig:833b7279e47c1756",
    "orig:8c87cf5a5f68fc6f", "orig:8f262cdc8a46e802",
    "orig:8f7dc21c7efaaead", "orig:8fef3450f841fbc4",
    "orig:90105acdd01e53da", "orig:9666d71f20e59585",
    "orig:97cb5f1c8f386e04", "orig:99515bbe9876abdd",
    "orig:9986c7195ca98755", "orig:9a0f2c0e6352d59b",
    "orig:9c5e6344772055ef", "orig:aa80db451a033ec3",
    "orig:ac9a25456ce09f35", "orig:ad250eb3b3b95d23",
    "orig:ad96597876fccede", "orig:b1d0e80e0941b903",
    "orig:b2c01597a6e2e12d", "orig:b435d3b647a685b0",
    "orig:b4ca378f49780878", "orig:b6167ac3189d8ac5",
    "orig:b9a4ae5205ce3289", "orig:b9b3d5bf3a023f72",
    "orig:ba91d56421d76122", "orig:bdd53ee2538899df",
    "orig:bf50ed6e6a9fb714", "orig:c13bdd0b07a5e87e",
    "orig:c163e3c0880979b2", "orig:c25e29700ec68ab0",
    "orig:c3f31858ed64a47b", "orig:c551228040d71cec",
    "orig:c55a6a1fea77bd58", "orig:c92e246078fc0bcb",
    "orig:cbb5fa8f2c6228c3", "orig:cbc0c1a0517e0255",
    "orig:cc02dd4936f81442", "orig:cc6c5cbf0c6bbc46",
    "orig:cc6e3f51ebdb366d", "orig:d6b1dece032d0060",
    "orig:d6b9e24e1bb841af", "orig:dacc32c611276450",
    "orig:e67ace843f5274b2", "orig:e8cbad930acdae29",
    "orig:edf3c58b92cda9c4", "orig:ee8450ab997022b5",
    "orig:f3dd016012c9d986", "orig:f4a5fd9a8cc730d4",
    "orig:f8c5fabbe999b0dd", "orig:f9b98070d1b72035",
    "orig:fbd3f544727e8d56", "orig:fce8d6c98d84dc4d",
    "orig:fe152ae9873db112", "orig:fecf5af6bd40a120",
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
    return [
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
            "causal_confidence": adj["causal_confidence"],
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
        for adj, target_record_id in sorted(
            adjudications, key=lambda item: item[0]["ref"]
        )
    ]


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


def _r2_adjudication(ref: str, ordinal: int, disposition: str) -> dict:
    grain = "original_candidate" if ref.startswith("orig:") else "supplemental_hypothesis"
    opaque = _synthetic_opaque_ref(f"r2:{ordinal}")
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
            "value_ref": f"value:{opaque}",
        }
        confidence = {
            "anomaly_or_guard_presence": "high",
            "expected_form": "high",
            "runtime_mechanism": "low",
            "smartkey_attribution": "low",
        }
        lane = "f4_core"
        privacy = "public_token"
        sources = [{"kind": "source_record", "ref": opaque}]
    elif disposition == "guard":
        classification = "guard_not_bug_unless_intent_changes"
        normalized_class = "intentional_transliteration_guard"
        expected = {
            "status": "not_applicable",
            "authority": "not_applicable",
            "value_ref": None,
        }
        confidence = {
            "anomaly_or_guard_presence": "high",
            "expected_form": "not_applicable",
            "runtime_mechanism": "not_applicable",
            "smartkey_attribution": "not_applicable",
        }
        lane = "intended_english_quoted_guard"
        privacy = "metadata_only"
        sources = [
            {"kind": "source_record", "ref": opaque},
            {"kind": "ruling", "ref": _synthetic_opaque_ref(f"r2:ruling:{ordinal}")},
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
            "anomaly_or_guard_presence": "high",
            "expected_form": "not_applicable",
            "runtime_mechanism": "not_applicable",
            "smartkey_attribution": "not_applicable",
        }
        lane = "outside_product_source_authorship"
        privacy = "metadata_only"
        sources = [
            {"kind": "source_record", "ref": opaque},
            {"kind": "receipt", "ref": _synthetic_opaque_ref(f"r2:receipt:{ordinal}")},
        ]
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
        "source_event_refs": [f"event:{_synthetic_opaque_ref(f'r2:event:{ordinal}')}"] ,
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
            "ref": _synthetic_opaque_ref(f"r2:legacy-source:{seed}"),
            "observed_utc": None,
            "build_sha": None,
            "surface": f"metadata:{_synthetic_opaque_ref(f'r2:surface:{seed}')}",
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
    bug = [_r2_adjudication(ref, i, "bug_candidate") for i, ref in enumerate(refs[:125])]
    guard = [
        _r2_adjudication(ref, i + 125, "guard")
        for i, ref in enumerate(refs[125:157])
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
        _r2_enhanced_record(index, items)
        for index, items in enumerate(bug_groups)
    )
    guard_groups = [[item] for item in guard[:26]]
    for index, item in enumerate(guard[26:]):
        guard_groups[index].append(item)
    records.extend(
        _r2_enhanced_record(200 + index, items)
        for index, items in enumerate(guard_groups)
    )
    records.extend(_r2_legacy_record(index) for index in range(9))

    doc = {
        "schema_version": 1,
        "registry": "smartkey-anomaly-registry",
        "gate_ref": "af4d6969b2ad",
        "adjudication_contract": {
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
            "projected_record_count": 111,
            "bug_record_count": 76,
            "guard_record_count": 26,
            "legacy_record_count": 9,
            "canonical_ref_set_sha256": "81168506c76776f060aaf9cd71bb4ed0e2fbde31b286ac30b69823bc8a54a5ee",
            "semantic_commitment_algorithm": "sha256-canonical-json-v1",
            "semantic_commitment_sha256": "f" * 64,
        },
        "source_exclusions": exclusions,
        "records": records,
    }
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(doc)
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
        [sys.executable, str(_VALIDATOR), str(_REGISTRY)],
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
        [sys.executable, str(_VALIDATOR), str(bad), "--schema", str(_SCHEMA)],
        capture_output=True,
        text=True,
        check=False,
    )
    assert proc.returncode == 1
    assert "E_DUP" in proc.stdout


def test_cli_exit_two_on_unreadable_input(tmp_path):
    proc = subprocess.run(
        [sys.executable, str(_VALIDATOR), str(tmp_path / "missing.json")],
        capture_output=True,
        text=True,
        check=False,
    )
    assert proc.returncode == 2


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
    assert V.compute_id(rec) == before == "anom-" + __import__("hashlib").sha256(
        b"li|\xd0\xbb\xd0\xb8"
    ).hexdigest()[:12]


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
    assert any("RED test" in e for e in V.validate_document(doc, _schema()) if e.startswith("E_GATE"))


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
    for observed, expected in (("ima[", "имаш"), ("sled", "след"),
                               ("otgowor", "отговор"), ("prewkl", "превкл")):
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
    assert V.validate_document(_synthetic_r2_doc(), _schema()) == []


def test_adjudication_enum_is_strict():
    doc = _synthetic_r2_doc()
    doc["records"][0]["adjudications"][0]["classification"] = "plausible_guess"
    assert "E_ENUM" in _codes(V.validate_document(doc, _schema()))


def test_classification_must_match_coarse_disposition():
    doc = _synthetic_r2_doc()
    doc["records"][0]["adjudications"][0]["disposition"] = "guard"
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(doc)
    assert "E_ADJ_STATE" in _codes(V.validate_document(doc, _schema()))


def test_unique_expected_status_requires_an_opaque_value_commitment():
    doc = _synthetic_r2_doc()
    doc["records"][0]["adjudications"][0]["expected"]["value_ref"] = None
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(doc)
    assert "E_ADJ_EXPECTED" in _codes(V.validate_document(doc, _schema()))


def test_not_applicable_expected_status_rejects_expected_form_confidence():
    doc = _synthetic_r2_doc()
    guard = doc["records"][76]["adjudications"][0]
    guard["causal_confidence"]["expected_form"] = "low"
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(doc)
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
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(doc)
    assert "E_ADJ_PRIVACY" in _codes(V.validate_document(doc, _schema()))


def test_metadata_only_privacy_rejects_semantic_text_encoded_as_a_code():
    doc = _synthetic_r2_doc()
    rec = doc["records"][76]
    rec["closure_reason"] = "metadata:synthetic_private_sentence_fragment"
    assert "E_ADJ_PRIVACY" in _codes(V.validate_document(doc, _schema()))


def test_hash_shaped_metadata_is_not_mistaken_for_a_long_word():
    doc = _synthetic_r2_doc()
    doc["records"][76]["closure_reason"] = "metadata:" + "a" * 64
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
        error.startswith("E_SCHEMA") and "adjudication_contract.artifact_sha256" in error
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


def test_cross_record_source_budget_blocks_reconstructable_payload():
    doc = _synthetic_r2_doc()
    for rec in doc["records"][:5]:
        rec["adjudications"][0]["source_refs"] = [
            {"kind": "source_record", "ref": _synthetic_opaque_ref("source:shared")}
        ]
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(doc)
    assert "E_PRIV_AGGREGATE" in _codes(V.validate_document(doc, _schema()))


def test_source_budget_allows_bounded_isolated_synthetic_tokens():
    doc = _synthetic_r2_doc()
    for rec in doc["records"][:4]:
        rec["adjudications"][0]["source_refs"] = [
            {"kind": "source_record", "ref": _synthetic_opaque_ref("source:bounded")}
        ]
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(doc)
    assert V.validate_document(doc, _schema()) == []


def test_source_budget_does_not_count_opaque_metadata_as_payload():
    doc = _synthetic_r2_doc()
    for ordinal, rec in enumerate(doc["records"][:4]):
        rec["notes"] = "metadata:" + _synthetic_opaque_ref(f"note:{ordinal}")
        rec["adjudications"][0]["source_refs"] = [
            {"kind": "source_record", "ref": _synthetic_opaque_ref("source:bounded")}
        ]
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(doc)
    assert "E_PRIV_AGGREGATE" not in _codes(V.validate_document(doc, _schema()))


# ---------------------------------------- G0 adjudication contract revision 2
def test_g0_r2_exact_159_to_111_shape_validates_without_raw_data():
    doc = _synthetic_r2_doc()
    assert len(doc["records"]) == 111
    assert sum("adjudications" in rec for rec in doc["records"]) == 102
    assert sum(
        len(rec.get("adjudications", ())) for rec in doc["records"]
    ) == 157
    assert len(doc["source_exclusions"]) == 2
    assert V.validate_document(doc, _schema()) == []


def test_g0_r2_schema_is_valid_draft_2020_12_when_jsonschema_is_available():
    jsonschema = pytest.importorskip("jsonschema")
    jsonschema.Draft202012Validator.check_schema(_schema())
    jsonschema.Draft202012Validator(_schema()).validate(_synthetic_r2_doc())


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


def test_g0_r2_count_preserving_target_record_rebinding_breaks_commitment():
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
    assert "E_ADJ_COMMITMENT" in _codes(V.validate_document(doc, _schema()))


def test_g0_r2_ref_set_is_pinned_not_merely_counted():
    doc = _synthetic_r2_doc()
    adjudication = doc["records"][0]["adjudications"][0]
    adjudication["ref"] = "orig:aaaaaaaaaaaaaaaa"
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(doc)
    assert "E_ADJ_REF_SET" in _codes(V.validate_document(doc, _schema()))


def test_g0_r2_bare_and_cross_grain_refs_are_rejected():
    doc = _synthetic_adjudicated_doc()
    assert "E_ADJ_REF" in _codes(V.validate_document(doc, _schema()))

    doc = _synthetic_r2_doc()
    doc["records"][0]["adjudications"][0]["grain"] = "supplemental_hypothesis"
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(doc)
    assert "E_ADJ_REF" in _codes(V.validate_document(doc, _schema()))


def test_g0_r2_all_letter_hex_ref_is_grammatically_valid():
    assert V.canonical_ref_matches_grain(
        "original_candidate", "orig:aaaaaaaaaaaaaaaa"
    )


def test_g0_r2_normalized_class_sentinel_is_grain_specific():
    original = _synthetic_r2_doc()
    original["records"][0]["adjudications"][0]["normalized_class"] = (
        "mechanical_red_candidate"
    )
    original["adjudication_contract"]["semantic_commitment_sha256"] = (
        _r2_semantic_digest(original)
    )
    assert "E_ADJ_NORMALIZED" in _codes(
        V.validate_document(original, _schema())
    )

    supplemental = _synthetic_r2_doc()
    supplemental["records"][30]["adjudications"][1]["normalized_class"] = (
        "not_applicable"
    )
    supplemental["adjudication_contract"]["semantic_commitment_sha256"] = (
        _r2_semantic_digest(supplemental)
    )
    assert "E_ADJ_NORMALIZED" in _codes(
        V.validate_document(supplemental, _schema())
    )

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
    assert adjudications[0]["expected"]["value_ref"] != adjudications[1]["expected"]["value_ref"]
    assert V.validate_document(doc, _schema()) == []


def test_g0_r2_expected_authority_status_matrix_is_strict():
    doc = _synthetic_r2_doc()
    expected = doc["records"][0]["adjudications"][0]["expected"]
    expected["authority"] = "null"
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(doc)
    assert "E_ADJ_EXPECTED" in _codes(V.validate_document(doc, _schema()))


def test_g0_r2_redacted_unique_expected_needs_no_literal_token():
    doc = _synthetic_r2_doc()
    rec = doc["records"][0]
    rec["observed"] = {
        "token": "[redacted]",
        "script": "latin",
        "descriptor": None,
    }
    rec["expected"] = {
        "token": None,
        "script": "unknown",
        "descriptor": f"metadata:{_synthetic_opaque_ref('r2:redacted:expected')}",
    }
    rec["privacy_classification"] = "redacted"
    for adjudication in rec["adjudications"]:
        adjudication["privacy_class"] = "redacted"
    _refresh_identity(rec)
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(doc)
    assert V.validate_document(doc, _schema()) == []


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
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(doc)
    assert "E_ADJ_PRIVACY" in _codes(V.validate_document(doc, _schema()))


def test_g0_r2_public_token_cannot_encode_a_private_path():
    doc = _synthetic_r2_doc()
    rec = doc["records"][0]
    rec["observed"]["token"] = "private/path"
    rec["observed"]["script"] = "latin"
    _refresh_identity(rec)
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(doc)
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
    shared = _synthetic_opaque_ref("r2:shared-source")
    for index, rec in enumerate(doc["records"][:4]):
        rec["adjudications"][0]["source_refs"] = [
            {"kind": "source_record", "ref": shared}
        ]
        rec["environment"]["flags"] = {
            f"synthetic:{_synthetic_opaque_ref(f'r2:flag:{index}')}": "off"
        }
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(doc)
    assert "E_PRIV_AGGREGATE" in _codes(V.validate_document(doc, _schema()))


def test_g0_r2_owner_lane_matrix_rejects_guard_lane_for_mechanical_bug():
    doc = _synthetic_r2_doc()
    doc["records"][0]["adjudications"][0]["owner_lane"] = (
        "intended_english_quoted_guard"
    )
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(doc)
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
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(doc)
    assert "E_ADJ_OWNER" in _codes(V.validate_document(doc, _schema()))


def test_g0_r2_outside_product_lane_is_exclusive_to_source_exclusions():
    doc = _synthetic_r2_doc()
    adjudication = doc["records"][0]["adjudications"][0]
    adjudication["owner_lane"] = "outside_product_source_authorship"
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(doc)
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
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(doc)
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
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(doc)
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
        "ref": f"metadata:{_synthetic_opaque_ref('r2:reproducer')}",
        "build_sha": "unknown",
        "summary": f"metadata:{_synthetic_opaque_ref('r2:reproducer-summary')}",
    }
    rec["red_test"] = {
        "path": f"metadata:{_synthetic_opaque_ref('r2:red-path')}",
        "name": f"metadata:{_synthetic_opaque_ref('r2:red-name')}",
        "commit": "a" * 40,
        **coverage,
    }
    rec["fix_commit"] = "b" * 40
    rec["fix_evidence"] = copy.deepcopy(coverage)
    rec["verification"] = {
        "build_sha": "b" * 40,
        "evidence_refs": [f"metadata:{_synthetic_opaque_ref('r2:verify-ref')}"] ,
        "surface": f"metadata:{_synthetic_opaque_ref('r2:verify-surface')}",
        "summary": f"metadata:{_synthetic_opaque_ref('r2:verify-summary')}",
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
    rec = doc["records"][0]
    covered = [item["ref"] for item in rec["adjudications"]]
    _r2_add_bug_evidence(rec, covered)
    rec["status"] = "verified"
    assert V.validate_document(doc, _schema()) == []


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
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(doc)
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
    doc["adjudication_contract"]["semantic_commitment_sha256"] = _r2_semantic_digest(doc)
    assert "E_ADJ_COUNT" in _codes(V.validate_document(doc, _schema()))
