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


def _capture_exact_error(call, code: str) -> ValueError:
    """Require one exact public error without retaining inputs between calls."""
    error = None
    try:
        call()
    except Exception as caught:
        error = caught
    assert type(error) is ValueError
    assert error.args == (code,)
    assert vars(error) == {}
    assert error.__cause__ is None
    assert error.__context__ is None
    return error


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


@pytest.mark.parametrize("include_optional_key", [False, True])
def test_r11a_a_exact_allowed_authority_field_set_is_required_at_valid_sizes(
    include_optional_key,
):
    allowed_fields_11 = (
        "grain",
        "ref",
        "classification",
        "disposition",
        "normalized_class",
        "expected",
        "causal_confidence",
        "owner_lane",
        "privacy_class",
        "source_refs",
        "source_event_refs",
    )
    allowed_fields_12 = (
        "grain",
        "ref",
        "classification",
        "disposition",
        "normalized_class",
        "expected",
        "causal_confidence",
        "owner_lane",
        "privacy_class",
        "source_refs",
        "source_event_refs",
        "expected_form_status",
    )
    assert len(allowed_fields_11) == len(set(allowed_fields_11)) == 11
    assert len(allowed_fields_12) == len(set(allowed_fields_12)) == 12
    assert allowed_fields_12[:-1] == allowed_fields_11

    allowed_fields = allowed_fields_12 if include_optional_key else allowed_fields_11
    expected_case_count = (1 << len(allowed_fields)) - 1
    assert expected_case_count == (4095 if include_optional_key else 2047)
    representative_masks = {1, 3, expected_case_count}
    base_adjudication = R11._authoritative_adjudication(R11._ORIGINAL_SCOPE)
    if include_optional_key:
        base_adjudication["expected_form_status"] = "synthetic-ignored-status"
    else:
        base_adjudication.pop("expected_form_status", None)
    assert frozenset(dict.__iter__(base_adjudication)) == frozenset(allowed_fields)
    source_input = R11._source_input()
    source_input_before = copy.deepcopy(source_input)
    executed_case_count = 0

    for subset_mask in range(1, expected_case_count + 1):
        adjudication = dict(base_adjudication)
        replacement_markers = []
        for field_index, field in enumerate(allowed_fields):
            if not subset_mask & (1 << field_index):
                continue
            marker = (
                "synthetic_unknown_authority_field_"
                f"{len(allowed_fields):02d}_{subset_mask:04x}_{field_index:02d}"
            )
            replaced_value = adjudication.pop(field)
            adjudication[marker] = replaced_value
            replacement_markers.append(marker)

        assert len(adjudication) == len(allowed_fields)
        assert len(replacement_markers) == subset_mask.bit_count()
        assert len(replacement_markers) == len(set(replacement_markers))
        assert all(type(key) is str for key in dict.__iter__(adjudication))
        assert all(
            field not in adjudication
            for field_index, field in enumerate(allowed_fields)
            if subset_mask & (1 << field_index)
        )

        error = _capture_exact_error(
            lambda: V.source_record_identity_envelope(source_input, adjudication),
            _HMAC_ERROR,
        )

        if subset_mask in representative_masks:
            for marker in replacement_markers:
                _assert_fixed_nonreflective_error(error, _HMAC_ERROR, marker)
        error.__traceback__ = None
        executed_case_count += 1

    assert executed_case_count == (4095 if include_optional_key else 2047)
    assert source_input == source_input_before
    if include_optional_key:
        marker = "synthetic_unknown_authority_field_moderate_13th"
        adjudication = dict(base_adjudication)
        assert (
            tuple(field for field in allowed_fields_12 if field not in adjudication)
            == ()
        )
        adjudication[marker] = "synthetic-moderate-thirteenth-slot"
        assert len(adjudication) == 13

        error = _capture_exact_error(
            lambda: V.source_record_identity_envelope(source_input, adjudication),
            _HMAC_ERROR,
        )

        _assert_fixed_nonreflective_error(error, _HMAC_ERROR, marker)
        error.__traceback__ = None


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


def test_r11a_c_nested_only_markers_in_final_record_emit_one_hold():
    markers = (
        "adjudications",
        "fix_evidence",
        "record_origin",
        "baseline_record_sha256",
    )
    assert len(markers) == len(set(markers)) == 4
    marker_values = (None, {}, [], "")
    expected_record_count = 20

    def fresh_nested_doc():
        doc = _legacy_doc()
        assert "adjudication_contract" not in doc
        assert "source_exclusions" not in doc
        assert len(doc["records"]) == expected_record_count
        assert all(
            marker not in record for record in doc["records"] for marker in markers
        )
        return doc

    subset_executed_count = 0
    subset_exact_hold_count = 0
    for subset_mask in range(1, 1 << len(markers)):
        doc = fresh_nested_doc()
        for marker_index, marker in enumerate(markers):
            if subset_mask & (1 << marker_index):
                doc["records"][-1][marker] = None
        errors = V.validate_document(doc, R10._schema())
        subset_exact_hold_count += len(_hold_errors(errors)) == 1
        subset_executed_count += 1

    position_executed_count = 0
    position_exact_hold_count = 0
    for marker in markers:
        for marker_value in marker_values:
            for record_index in range(expected_record_count):
                doc = fresh_nested_doc()
                doc["records"][record_index][marker] = copy.deepcopy(marker_value)
                errors = V.validate_document(doc, R10._schema())
                position_exact_hold_count += len(_hold_errors(errors)) == 1
                position_executed_count += 1

    combined_doc = fresh_nested_doc()
    for marker in markers:
        combined_doc["records"][-1][marker] = None
    combined_errors = V.validate_document(combined_doc, R10._schema())

    distributed_doc = fresh_nested_doc()
    distributed_indices = (0, 6, 13, 19)
    assert len(distributed_indices) == len(set(distributed_indices)) == len(markers)
    for record_index, marker in zip(distributed_indices, markers, strict=True):
        distributed_doc["records"][record_index][marker] = None
    distributed_errors = V.validate_document(distributed_doc, R10._schema())

    assert subset_executed_count == 15
    assert position_executed_count == 320
    assert {
        "subset_cases_with_exactly_one_hold": subset_exact_hold_count,
        "position_cases_with_exactly_one_hold": position_exact_hold_count,
        "combined_hold_count": len(_hold_errors(combined_errors)),
        "distributed_hold_count": len(_hold_errors(distributed_errors)),
    } == {
        "subset_cases_with_exactly_one_hold": 15,
        "position_cases_with_exactly_one_hold": 320,
        "combined_hold_count": 1,
        "distributed_hold_count": 1,
    }


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

    class ArmedRecords(list):
        def __init__(self, values, present_key):
            self.armed = False
            self.touched = False
            list.__init__(self, values)
            self.middle_index = list.__len__(self) // 2
            middle_record = list.__getitem__(self, self.middle_index)
            dict.__setitem__(middle_record, present_key, None)
            self.armed = True

        def _trip(self):
            if self.armed:
                self.touched = True
                raise AssertionError(marker)

        def __iter__(self):
            self._trip()
            return list.__iter__(self)

        def __len__(self):
            self._trip()
            return list.__len__(self)

        def __getitem__(self, key):
            self._trip()
            return list.__getitem__(self, key)

        def __contains__(self, value):
            self._trip()
            return list.__contains__(self, value)

        def __reversed__(self):
            self._trip()
            return list.__reversed__(self)

        def copy(self):
            self._trip()
            return list.copy(self)

        def count(self, value):
            self._trip()
            return list.count(self, value)

        def index(self, value, start=0, stop=None):
            self._trip()
            if stop is None:
                return list.index(self, value, start)
            return list.index(self, value, start, stop)

    hostile_top_level = ArmedMapping(_legacy_doc(), "adjudication_contract")
    assert dict.__contains__(hostile_top_level, "adjudication_contract")

    nested_doc = _legacy_doc()
    hostile_record = ArmedMapping(nested_doc["records"][0], "record_origin")
    assert dict.__contains__(hostile_record, "record_origin")
    nested_doc["records"][0] = hostile_record

    hostile_records_doc = _legacy_doc()
    hostile_records = ArmedRecords(hostile_records_doc["records"], "record_origin")
    middle_record = list.__getitem__(hostile_records, hostile_records.middle_index)
    assert dict.__contains__(middle_record, "record_origin")
    hostile_records_doc["records"] = hostile_records

    outcomes = []
    for doc, hostile_mapping in (
        (hostile_top_level, hostile_top_level),
        (nested_doc, hostile_record),
        (hostile_records_doc, hostile_records),
    ):
        errors = []
        escaped = None
        try:
            V._check_enhanced_preflight(doc, errors)
        except AssertionError as caught:
            escaped = caught
        outcomes.append((escaped, hostile_mapping.touched, errors))

    assert [escaped is None for escaped, _touched, _errors in outcomes] == [
        True,
        True,
        True,
    ]
    assert [touched for _escaped, touched, _errors in outcomes] == [
        False,
        False,
        False,
    ]
    assert [len(_hold_errors(errors)) for _escaped, _touched, errors in outcomes] == [
        1,
        1,
        1,
    ]
