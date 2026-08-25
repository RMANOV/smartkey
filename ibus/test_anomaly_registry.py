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
