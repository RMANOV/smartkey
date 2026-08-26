"""Targeted R11a GREEN-audit REDs using only public synthetic fixtures.

No private candidate or corpus data is read.  These tests isolate bounded
container inspection and marker-presence semantics without changing the
already-audited R11a RED v3 unit.
"""

from __future__ import annotations

import copy
import tracemalloc
from pathlib import Path

import pytest

from ibus import test_anomaly_registry as R10
from ibus import test_anomaly_registry_r11_preflight_red as R11


V = R11.V

_ALLOCATION_CEILING = 8 * V.HMAC_RESOURCE_LIMITS["max_canonical_payload_bytes"]
_HMAC_ERROR = "E_HMAC_CONTRACT"
_PLACEMENT_ERROR = "E_SOURCE_SCOPE_PLACEMENT"
_HOLD = "E_R3_IMPORT_HOLD"


def _capture_fixed_error(call) -> tuple[ValueError, int]:
    """Measure only the production call; all large inputs predate tracing."""
    error = None
    tracemalloc.start()
    tracemalloc.reset_peak()
    try:
        try:
            call()
        except ValueError as caught:
            error = caught
        _current_bytes, peak_bytes = tracemalloc.get_traced_memory()
    finally:
        tracemalloc.stop()
    assert error is not None, "audit RED: malformed input must fail closed"
    return error, peak_bytes


def _assert_fixed_nonreflective_error(
    error: ValueError,
    code: str,
    marker: str,
) -> None:
    assert type(error) is ValueError
    assert error.args == (code,)
    assert vars(error) == {}
    assert error.__cause__ is None
    assert error.__context__ is None
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


def _placement_probe_doc(*, records: list, source_exclusions: list) -> dict:
    return {
        "records": records,
        "source_exclusions": source_exclusions,
    }


def _hold_errors(errors: list[str]) -> list[str]:
    return [error for error in errors if error.startswith(f"{_HOLD}:")]


def _legacy_doc() -> dict:
    return R10._doc()


# ---------------------------------------------------- A: authoritative keys


@pytest.mark.parametrize("include_optional_key", [False, True])
def test_r11a_a_valid_exact_11_and_12_key_authority_shapes_remain_green(
    include_optional_key,
):
    adjudication = R11._authoritative_adjudication(R11._ORIGINAL_SCOPE)
    if include_optional_key:
        adjudication["expected_form_status"] = "synthetic-ignored-status"
        expected_size = 12
    else:
        adjudication.pop("expected_form_status", None)
        expected_size = 11
    assert type(adjudication) is dict
    assert len(adjudication) == expected_size
    assert V.source_record_identity_envelope(
        R11._source_input(), adjudication
    ) == R11._reference_scoped_envelope(R11._ORIGINAL_SCOPE)


def test_r11a_a_oversized_exact_authority_keyset_fails_with_bounded_peak():
    marker = "synthetic-oversized-authority-key-marker"
    adjudication = R11._authoritative_adjudication(R11._ORIGINAL_SCOPE)
    adjudication.update(
        {f"synthetic-extra-key-{index:05d}": None for index in range(22_000)}
    )
    adjudication[marker] = None
    assert type(adjudication) is dict
    assert all(type(key) is str for key in dict.__iter__(adjudication))
    assert len(adjudication) > 20_000

    error, peak_bytes = _capture_fixed_error(
        lambda: V.source_record_identity_envelope(R11._source_input(), adjudication)
    )

    _assert_fixed_nonreflective_error(error, _HMAC_ERROR, marker)
    assert peak_bytes <= _ALLOCATION_CEILING


def test_r11a_a_armed_string_subclass_key_is_never_hashed_or_compared():
    marker = "synthetic-hostile-authority-key-marker"

    class ArmedKey(str):
        def __new__(cls, value):
            instance = str.__new__(cls, value)
            instance.armed = False
            instance.touched = False
            return instance

        def _trip(self):
            if self.armed:
                self.touched = True
                raise AssertionError(marker)

        def __hash__(self):
            self._trip()
            return str.__hash__(self)

        def __eq__(self, other):
            self._trip()
            return str.__eq__(self, other)

    hostile_key = ArmedKey(marker)
    adjudication = R11._authoritative_adjudication(R11._ORIGINAL_SCOPE)
    adjudication.pop("expected_form_status", None)
    assert len(adjudication) == 11
    adjudication[hostile_key] = None
    # Twelve is an allowed authoritative-shape cardinality, so a bounded
    # implementation must inspect exact keys without hashing this subclass.
    assert len(tuple(dict.__iter__(adjudication))) == 12
    hostile_key.armed = True

    error, peak_bytes = _capture_fixed_error(
        lambda: V.source_record_identity_envelope(R11._source_input(), adjudication)
    )

    _assert_fixed_nonreflective_error(error, _HMAC_ERROR, marker)
    assert hostile_key.touched is False
    assert peak_bytes <= _ALLOCATION_CEILING


# ------------------------------------------------ placement cardinality


def test_r11a_b_oversized_exact_per_record_list_fails_with_bounded_peak():
    marker = "synthetic-oversized-record-placement-marker"
    item = {"synthetic_marker": marker}
    oversized = [item] * 100_000
    doc = _placement_probe_doc(
        records=[{"adjudications": oversized}],
        source_exclusions=[],
    )
    assert type(oversized) is list

    error, peak_bytes = _capture_fixed_error(
        lambda: V.source_scope_placement_projection(doc)
    )

    _assert_fixed_nonreflective_error(error, _PLACEMENT_ERROR, marker)
    assert peak_bytes <= _ALLOCATION_CEILING


def test_r11a_b_oversized_exact_source_exclusions_fails_before_copying():
    marker = "synthetic-oversized-exclusion-placement-marker"
    item = {"synthetic_marker": marker}
    oversized = [item] * 22_000
    doc = _placement_probe_doc(records=[], source_exclusions=oversized)
    assert type(oversized) is list
    assert len(oversized) > 20_000

    error, peak_bytes = _capture_fixed_error(
        lambda: V.source_scope_placement_projection(doc)
    )

    _assert_fixed_nonreflective_error(error, _PLACEMENT_ERROR, marker)
    assert peak_bytes <= _ALLOCATION_CEILING


def test_r11a_b_cumulative_bounded_lists_stop_at_global_159_budget():
    marker = "synthetic-cumulative-placement-marker"
    item = {"synthetic_marker": marker}
    bounded = [item] * V.HMAC_RESOURCE_LIMITS["max_array_members"]
    records = [{"adjudications": bounded} for _index in range(111)]
    doc = _placement_probe_doc(records=records, source_exclusions=[])
    assert all(
        len(record["adjudications"]) == V.HMAC_RESOURCE_LIMITS["max_array_members"]
        for record in records
    )

    error, peak_bytes = _capture_fixed_error(
        lambda: V.source_scope_placement_projection(doc)
    )

    _assert_fixed_nonreflective_error(error, _PLACEMENT_ERROR, marker)
    assert peak_bytes <= _ALLOCATION_CEILING


def test_r11a_b_exact_159_unique_placement_projection_remains_green():
    doc = R11._r11_doc()
    projection = V.source_scope_placement_projection(doc)
    assert projection == R11._reference_placement_projection(doc)
    assert len(projection["placements"]) == 159
    assert len(
        {placement["source_record_ref"] for placement in projection["placements"]}
    ) == len(projection["placements"])

    concentrated = copy.deepcopy(doc)
    record_adjudications = [
        item
        for record in concentrated["records"]
        for item in record.get("adjudications", ())
    ]
    assert len(record_adjudications) == 157
    for record in concentrated["records"]:
        if "adjudications" in record:
            record["adjudications"] = []
    concentrated["records"][0]["adjudications"] = record_adjudications
    assert len(concentrated["source_exclusions"]) == 2

    concentrated_projection = V.source_scope_placement_projection(concentrated)
    assert concentrated_projection == R11._reference_placement_projection(concentrated)
    assert len(concentrated_projection["placements"]) == 159
    assert len(
        {
            placement["source_record_ref"]
            for placement in concentrated_projection["placements"]
        }
    ) == len(concentrated_projection["placements"])


# ----------------------------------------------------- C: HOLD key presence


def test_r11a_c_exact_legacy_registry_remains_clean_without_hold():
    errors = V.validate_document(_legacy_doc(), R10._schema())
    assert errors == []
    assert _hold_errors(errors) == []

    near_miss = _legacy_doc()
    near_miss["adjudication_contracts"] = None
    near_miss["source_exclusion"] = []
    near_miss["records"][0].update(
        {
            "adjudication": None,
            "fix_evidences": None,
            "record_origins": None,
            "baseline_record_sha25": None,
        }
    )
    near_miss_errors = V.validate_document(near_miss, R10._schema())
    assert _hold_errors(near_miss_errors) == []


@pytest.mark.parametrize(
    "value_kind",
    ["valid", "none", "empty_dict", "empty_list", "empty_scalar"],
)
def test_r11a_c_contract_key_presence_always_emits_exactly_one_hold(value_kind):
    values = {
        "valid": R11._r11_doc()["adjudication_contract"],
        "none": None,
        "empty_dict": {},
        "empty_list": [],
        "empty_scalar": "",
    }
    doc = _legacy_doc()
    doc["adjudication_contract"] = copy.deepcopy(values[value_kind])

    errors = V.validate_document(doc, R10._schema())

    assert len(_hold_errors(errors)) == 1


@pytest.mark.parametrize(
    "value_kind",
    ["valid", "none", "empty_dict", "empty_list", "empty_scalar"],
)
def test_r11a_c_source_exclusions_key_presence_always_emits_exactly_one_hold(
    value_kind,
):
    values = {
        "valid": R11._r11_doc()["source_exclusions"],
        "none": None,
        "empty_dict": {},
        "empty_list": [],
        "empty_scalar": "",
    }
    doc = _legacy_doc()
    doc["source_exclusions"] = copy.deepcopy(values[value_kind])

    errors = V.validate_document(doc, R10._schema())

    assert len(_hold_errors(errors)) == 1


@pytest.mark.parametrize(
    "record_marker",
    ["adjudications", "fix_evidence", "record_origin", "baseline_record_sha256"],
)
@pytest.mark.parametrize("marker_value", [None, {}, [], ""])
def test_r11a_c_record_marker_presence_always_emits_exactly_one_hold(
    record_marker,
    marker_value,
):
    doc = _legacy_doc()
    doc["records"][0][record_marker] = copy.deepcopy(marker_value)

    errors = V.validate_document(doc, R10._schema())

    assert len(_hold_errors(errors)) == 1


def test_r11a_c_multiple_enhanced_markers_deduplicate_to_one_hold():
    doc = _legacy_doc()
    doc["adjudication_contract"] = None
    doc["source_exclusions"] = []
    for marker in (
        "adjudications",
        "fix_evidence",
        "record_origin",
        "baseline_record_sha256",
    ):
        doc["records"][0][marker] = None

    errors = V.validate_document(doc, R10._schema())

    assert len(_hold_errors(errors)) == 1

    without_contract = _legacy_doc()
    assert "adjudication_contract" not in without_contract
    without_contract["source_exclusions"] = copy.deepcopy(
        R11._r11_doc()["source_exclusions"]
    )
    assert without_contract["source_exclusions"]
    for marker in (
        "adjudications",
        "fix_evidence",
        "record_origin",
        "baseline_record_sha256",
    ):
        without_contract["records"][0][marker] = None

    without_contract_errors = V.validate_document(without_contract, R10._schema())
    assert len(_hold_errors(without_contract_errors)) == 1


def test_r11a_c_hostile_mapping_overrides_cannot_hide_present_marker():
    marker = "synthetic-hostile-hold-presence-marker"

    class ArmedMapping(dict):
        def __init__(self, value, present_key):
            self.armed = False
            self.touched = False
            dict.__init__(self, value)
            dict.__setitem__(self, present_key, None)
            self.armed = True

        def _trip(self):
            if self.armed:
                self.touched = True
                raise AssertionError(marker)

        def __contains__(self, key):
            self._trip()
            return dict.__contains__(self, key)

        def __getitem__(self, key):
            self._trip()
            return dict.__getitem__(self, key)

        def __iter__(self):
            self._trip()
            return dict.__iter__(self)

        def __len__(self):
            self._trip()
            return dict.__len__(self)

        def get(self, key, default=None):
            self._trip()
            return dict.get(self, key, default)

        def items(self):
            self._trip()
            return dict.items(self)

        def keys(self):
            self._trip()
            return dict.keys(self)

        def values(self):
            self._trip()
            return dict.values(self)

    hostile_top_level = ArmedMapping(_legacy_doc(), "adjudication_contract")
    assert dict.__contains__(hostile_top_level, "adjudication_contract")

    nested_doc = _legacy_doc()
    hostile_record = ArmedMapping(nested_doc["records"][0], "record_origin")
    assert dict.__contains__(hostile_record, "record_origin")
    nested_doc["records"][0] = hostile_record

    outcomes = []
    for doc, hostile_mapping in (
        (hostile_top_level, hostile_top_level),
        (nested_doc, hostile_record),
    ):
        errors = []
        escaped = None
        try:
            V._check_enhanced_preflight(doc, errors)
        except AssertionError as caught:
            escaped = caught
        outcomes.append((escaped, hostile_mapping.touched, errors))

    assert [escaped is None for escaped, _touched, _errors in outcomes] == [True, True]
    assert [touched for _escaped, touched, _errors in outcomes] == [False, False]
    assert [len(_hold_errors(errors)) for _escaped, _touched, errors in outcomes] == [
        1,
        1,
    ]
