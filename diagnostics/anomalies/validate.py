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
4. lifecycle gates: what evidence each ``status`` must carry;
5. privacy walk over every string in every record: no ``@``, no ``http``
   outside ``source_refs``, no digit run longer than six characters outside
   SHA/reference fields, no letter run longer than 40 characters, no
   newlines, one whitespace-free token per side of the context;
6. canonical on-disk format (indent 2, non-ASCII preserved, trailing newline).

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

SHA_RE = re.compile(r"^[0-9a-f]{40}$")
DIGIT_RUN_RE = re.compile(r"[0-9]{7,}")
LETTER_RUN_RE = re.compile(r"[A-Za-zЀ-ӿ]{41,}")
CYRILLIC_RE = re.compile(r"[Ѐ-ӿ]")
LATIN_RE = re.compile(r"[A-Za-z]")
# Fields whose values are commit SHAs (40 hex or "unknown") by contract.
SHA_KEYS = frozenset({"build_sha", "commit", "fix_commit"})
# Fields whose values are ledger/ruling/test identifiers or the derived id;
# they may legitimately contain long digit runs and are pattern-checked instead.
IDENTIFIER_KEYS = frozenset({"id", "ref", "evidence_refs"})


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
    if expected["token"] is None and not expected["descriptor"] and not rec["needs_operator_confirmation"]:
        errors.append(f"E_EXPECTED: {path}: expected form is null, needs_operator_confirmation must be true")
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
        is_identifier = is_sha_field or last_key in IDENTIFIER_KEYS
        if "@" in text:
            errors.append(f"E_PRIV_AT: {where}: '@' is never allowed (addresses/handles)")
        if "http" in text.lower() and not in_source_refs:
            errors.append(f"E_PRIV_URL: {where}: URLs only inside source_refs")
        if "\n" in text or "\r" in text:
            errors.append(f"E_PRIV_MULTILINE: {where}: multi-line text looks like a transcript")
        if LETTER_RUN_RE.search(text):
            errors.append(f"E_PRIV_LONGRUN: {where}: letter run longer than 40 characters")
        if not is_identifier and DIGIT_RUN_RE.search(text):
            errors.append(f"E_PRIV_DIGITS: {where}: digit run longer than 6 outside SHA/reference fields")
        if is_sha_field and text != "unknown" and not SHA_RE.match(text):
            errors.append(f"E_SHA: {where}: SHA field must be 40 hex or 'unknown'")
    for side in ("before", "after"):
        token = rec["minimal_context"][side]
        if isinstance(token, str) and token.split() != [token]:
            errors.append(f"E_PRIV_CONTEXT: {path}.minimal_context.{side}: at most one whitespace-free token per side")


def _check_schema_drift(schema: dict, errors: list[str]) -> None:
    props = schema.get("$defs", {}).get("record", {}).get("properties", {})
    if tuple(props.get("status", {}).get("enum", ())) != STATUSES:
        errors.append("E_SCHEMA_DRIFT: schema status enum differs from validate.STATUSES")
    if tuple(props.get("privacy_classification", {}).get("enum", ())) != PRIVACY_CLASSES:
        errors.append("E_SCHEMA_DRIFT: schema privacy enum differs from validate.PRIVACY_CLASSES")


def validate_document(doc, schema: dict) -> list[str]:
    """Return every violation for an in-memory registry document."""
    errors: list[str] = []
    _check_schema_drift(schema, errors)
    SchemaChecker(schema).check(schema, doc, "$", errors)
    if errors:
        return errors  # semantic checks assume the structural contract holds
    seen_keys: dict[str, str] = {}
    seen_ids: dict[str, str] = {}
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
