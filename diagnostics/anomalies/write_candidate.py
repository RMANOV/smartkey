#!/usr/bin/env python3
"""Create a validated append-only anomaly-registry candidate artifact."""

from __future__ import annotations

import argparse
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import secrets
import stat
import sys


HERE = Path(__file__).resolve().parent
DEFAULT_SCHEMA = HERE / "schema.json"
VALIDATOR_PATH = HERE / "validate.py"
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")


class WriterError(Exception):
    def __init__(
        self,
        code: str,
        message: str,
        *,
        output_published: bool = False,
        temp_path: Path | None = None,
    ) -> None:
        super().__init__(message)
        self.code = code
        self.message = message
        self.output_published = output_published
        self.temp_path = temp_path

    def as_dict(self) -> dict:
        result = {
            "code": self.code,
            "message": self.message,
            "output_published": self.output_published,
        }
        if self.temp_path is not None:
            result["temp_path"] = str(self.temp_path)
        return result


def _load_validator():
    spec = importlib.util.spec_from_file_location(
        "smartkey_anomaly_candidate_validator", VALIDATOR_PATH
    )
    if spec is None or spec.loader is None:
        raise WriterError("E_VALIDATOR_LOAD", "validator could not be loaded")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


V = _load_validator()


def _read_regular(path: Path, code: str) -> bytes:
    absolute = Path(os.path.abspath(path))
    try:
        if path.resolve(strict=True) != absolute:
            raise WriterError(code, "input path must not traverse a symlink")
        fd = os.open(
            absolute,
            os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK,
        )
    except OSError as exc:
        raise WriterError(code, "input could not be read") from exc
    try:
        metadata = os.fstat(fd)
        if not stat.S_ISREG(metadata.st_mode):
            raise WriterError(code, "input must be a regular non-symlink file")
        chunks = []
        while True:
            chunk = os.read(fd, 1024 * 1024)
            if not chunk:
                break
            chunks.append(chunk)
        try:
            current = os.stat(absolute, follow_symlinks=False)
        except OSError as exc:
            raise WriterError(code, "input path changed while being read") from exc
        if (current.st_dev, current.st_ino) != (metadata.st_dev, metadata.st_ino):
            raise WriterError(code, "input path changed while being read")
        return b"".join(chunks)
    except OSError as exc:
        raise WriterError(code, "input could not be read") from exc
    finally:
        os.close(fd)


def _record_bytes(record: dict) -> bytes:
    return json.dumps(
        record,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")


def _validate_document(document: dict, schema: dict, code: str) -> None:
    errors = V.validate_document(document, schema)
    if errors:
        raise WriterError(code, errors[0].split(":", 1)[0])


def _open_output_parent(output: Path) -> tuple[Path, str, int]:
    absolute_output = Path(os.path.abspath(output))
    parent = absolute_output.parent
    try:
        if output.parent.resolve(strict=True) != parent:
            raise WriterError(
                "E_OUTPUT_PARENT", "output parent must not traverse a symlink"
            )
        directory_fd = os.open(
            parent,
            os.O_RDONLY | os.O_CLOEXEC | os.O_DIRECTORY | os.O_NOFOLLOW,
        )
    except OSError as exc:
        raise WriterError("E_OUTPUT_PARENT", "output parent is unavailable") from exc
    try:
        metadata = os.fstat(directory_fd)
        if not stat.S_ISDIR(metadata.st_mode) or stat.S_IMODE(metadata.st_mode) != 0o700:
            raise WriterError(
                "E_OUTPUT_PARENT", "output parent must be a mode-0700 directory"
            )
        if absolute_output.parent != parent or output.name in ("", ".", ".."):
            raise WriterError("E_OUTPUT_PARENT", "output must be a direct child")
        return parent, absolute_output.name, directory_fd
    except BaseException:
        os.close(directory_fd)
        raise


def _verify_parent_identity(
    parent: Path,
    directory_fd: int,
    *,
    output_published: bool,
) -> None:
    pinned = os.fstat(directory_fd)
    try:
        current = os.stat(parent, follow_symlinks=False)
    except OSError as exc:
        raise WriterError(
            "E_OUTPUT_PARENT_DRIFT",
            "output parent identity changed",
            output_published=output_published,
        ) from exc
    if not stat.S_ISDIR(current.st_mode) or (current.st_dev, current.st_ino) != (
        pinned.st_dev,
        pinned.st_ino,
    ):
        raise WriterError(
            "E_OUTPUT_PARENT_DRIFT",
            "output parent identity changed",
            output_published=output_published,
        )


def _read_regular_at(directory_fd: int, name: str) -> bytes:
    try:
        fd = os.open(
            name,
            os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK,
            dir_fd=directory_fd,
        )
    except OSError as exc:
        raise WriterError("E_OUTPUT_EXISTS", "existing output is not readable") from exc
    try:
        try:
            metadata = os.fstat(fd)
            if not stat.S_ISREG(metadata.st_mode):
                raise WriterError("E_OUTPUT_EXISTS", "existing output is not regular")
            chunks = []
            while True:
                chunk = os.read(fd, 1024 * 1024)
                if not chunk:
                    break
                chunks.append(chunk)
            current = os.stat(name, dir_fd=directory_fd, follow_symlinks=False)
            if (current.st_dev, current.st_ino) != (metadata.st_dev, metadata.st_ino):
                raise WriterError("E_OUTPUT_DRIFT", "existing output identity changed")
            return b"".join(chunks)
        except OSError as exc:
            raise WriterError("E_OUTPUT_DRIFT", "existing output identity changed") from exc
    finally:
        os.close(fd)


def _publish_no_clobber(output: Path, content: bytes) -> str:
    parent, output_name, directory_fd = _open_output_parent(output)
    temp_name = f".{output_name}.{secrets.token_hex(12)}"
    temp_path = parent / temp_name
    linked = False
    temp_created = False
    fd = -1
    try:
        try:
            fd = os.open(
                temp_name,
                os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC | os.O_NOFOLLOW,
                0o600,
                dir_fd=directory_fd,
            )
            temp_created = True
            with os.fdopen(fd, "wb", closefd=True) as stream:
                fd = -1
                stream.write(content)
                stream.flush()
                os.fsync(stream.fileno())
        except OSError as exc:
            raise WriterError(
                "E_OUTPUT_STAGE",
                "candidate output staging failed",
                output_published=linked,
                temp_path=temp_path if temp_created else None,
            ) from exc
        _verify_parent_identity(
            parent,
            directory_fd,
            output_published=False,
        )
        try:
            os.link(
                temp_name,
                output_name,
                src_dir_fd=directory_fd,
                dst_dir_fd=directory_fd,
                follow_symlinks=False,
            )
            linked = True
        except FileExistsError:
            existing = _read_regular_at(directory_fd, output_name)
            if existing != content:
                try:
                    os.unlink(temp_name, dir_fd=directory_fd)
                except OSError as exc:
                    raise WriterError(
                        "E_TEMP_UNLINK",
                        "different output exists and temporary cleanup failed",
                        temp_path=temp_path,
                    ) from exc
                raise WriterError("E_OUTPUT_EXISTS", "different output already exists")
            _verify_parent_identity(
                parent,
                directory_fd,
                output_published=False,
            )
            try:
                os.unlink(temp_name, dir_fd=directory_fd)
            except OSError as exc:
                raise WriterError(
                    "E_TEMP_UNLINK",
                    "identical output exists and temporary cleanup failed",
                    temp_path=temp_path,
                ) from exc
            return "NOOP_IDENTICAL_OUTPUT"
        except OSError as exc:
            raise WriterError(
                "E_OUTPUT_PUBLISH",
                "candidate output could not be linked",
                temp_path=temp_path,
            ) from exc
        _verify_parent_identity(
            parent,
            directory_fd,
            output_published=True,
        )
        try:
            os.fsync(directory_fd)
        except OSError as exc:
            raise WriterError(
                "E_DIR_FSYNC_AFTER_PUBLISH",
                "output was linked but directory fsync failed",
                output_published=True,
                temp_path=temp_path,
            ) from exc
        try:
            os.unlink(temp_name, dir_fd=directory_fd)
        except OSError as exc:
            raise WriterError(
                "E_TEMP_UNLINK_AFTER_PUBLISH",
                "output was linked but temporary cleanup failed",
                output_published=True,
                temp_path=temp_path,
            ) from exc
        try:
            os.fsync(directory_fd)
        except OSError as exc:
            raise WriterError(
                "E_DIR_FSYNC_AFTER_PUBLISH",
                "output was linked but directory fsync failed",
                output_published=True,
            ) from exc
        return "CREATED"
    except BaseException:
        if fd >= 0:
            os.close(fd)
        raise
    finally:
        os.close(directory_fd)


def prepare_candidate(
    source_registry: Path | str,
    append_records: Path | str,
    schema_path: Path | str,
    output_candidate: Path | str,
    expected_input_sha256: str,
) -> dict:
    source_path = Path(source_registry)
    append_path = Path(append_records)
    schema_file = Path(schema_path)
    output_path = Path(output_candidate)
    if not SHA256_RE.fullmatch(expected_input_sha256):
        raise WriterError("E_INPUT_DIGEST", "expected input digest is malformed")
    if output_path in {source_path, append_path, schema_file}:
        raise WriterError("E_OUTPUT_PATH", "output must be a distinct candidate path")

    source_bytes = _read_regular(source_path, "E_SOURCE_READ")
    source_sha256 = hashlib.sha256(source_bytes).hexdigest()
    if source_sha256 != expected_input_sha256:
        raise WriterError("E_INPUT_DIGEST", "source does not match expected digest")
    append_bytes = _read_regular(append_path, "E_APPEND_READ")
    schema_bytes = _read_regular(schema_file, "E_SCHEMA_READ")
    try:
        source = json.loads(source_bytes)
        additions = json.loads(append_bytes)
        schema = json.loads(schema_bytes)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise WriterError("E_OPERATION_SHAPE", "input JSON is malformed") from exc
    if not isinstance(source, dict) or not isinstance(additions, list):
        raise WriterError("E_OPERATION_SHAPE", "append input must be a JSON array")
    if not isinstance(schema, dict):
        raise WriterError("E_SCHEMA_SHAPE", "schema must be a JSON object")
    if not all(isinstance(record, dict) for record in additions):
        raise WriterError("E_OPERATION_SHAPE", "every append item must be an object")

    _validate_document(source, schema, "E_SOURCE_INVALID")
    source_canonical = V.canonical_text(source).encode("utf-8")
    if source_bytes != source_canonical:
        raise WriterError("E_SOURCE_INVALID", "source is not canonical")
    original_records = source.get("records")
    if not isinstance(original_records, list):
        raise WriterError("E_SOURCE_INVALID", "source records are unavailable")
    original_count = len(original_records)
    try:
        record_schema = schema["$defs"]["record"]
        legacy_keys = set(record_schema["required"])
    except (KeyError, TypeError) as exc:
        raise WriterError("E_SCHEMA_SHAPE", "record schema is unavailable") from exc

    candidate = copy.deepcopy(source)
    seen_content: dict[bytes, str] = {}
    seen_ids: dict[str, bytes] = {}
    seen_dedup: dict[str, bytes] = {}
    for record in candidate["records"]:
        content = _record_bytes(record)
        seen_content[content] = record["id"]
        seen_ids[record["id"]] = content
        seen_dedup[record["dedup_key"]] = content

    appended_ids: list[str] = []
    duplicate_ids: list[str] = []
    for record in additions:
        content = _record_bytes(record)
        if content in seen_content:
            duplicate_ids.append(seen_content[content])
            continue
        if set(record) != legacy_keys:
            if set(record) - legacy_keys:
                raise WriterError("E_R3_IMPORT_HOLD", "enhanced append fields are held")
            raise WriterError("E_OPERATION_SHAPE", "legacy append fields are incomplete")
        shape_errors: list[str] = []
        V.SchemaChecker(schema).check(record_schema, record, "$.append", shape_errors)
        if shape_errors:
            raise WriterError("E_APPEND_INVALID", shape_errors[0].split(":", 1)[0])
        try:
            computed_id = V.compute_id(record)
            computed_dedup = V.compute_dedup_key(record)
        except (KeyError, TypeError, ValueError) as exc:
            raise WriterError("E_OPERATION_SHAPE", "record identity is malformed") from exc
        collision_contents = {
            seen_ids[value]
            for value in (record.get("id"), computed_id)
            if value in seen_ids
        } | {
            seen_dedup[value]
            for value in (record.get("dedup_key"), computed_dedup)
            if value in seen_dedup
        }
        if collision_contents:
            raise WriterError("E_IDENTITY_COLLISION", "identity has different content")
        if record.get("status") != "open_suspected_smartkey":
            raise WriterError("E_NEW_STATUS", "new legacy intake must start open")
        candidate["records"].append(copy.deepcopy(record))
        seen_content[content] = record["id"]
        seen_ids[record["id"]] = content
        seen_dedup[record["dedup_key"]] = content
        appended_ids.append(record["id"])

    _validate_document(candidate, schema, "E_CANDIDATE_INVALID")
    candidate_bytes = V.canonical_text(candidate).encode("utf-8")
    outcome = _publish_no_clobber(output_path, candidate_bytes)
    return {
        "outcome": outcome,
        "source_sha256": source_sha256,
        "candidate_sha256": hashlib.sha256(candidate_bytes).hexdigest(),
        "original_count": original_count,
        "appended_count": len(appended_ids),
        "appended_ids": appended_ids,
        "duplicate_identical_ids": duplicate_ids,
        "diffs": [
            {"op": "append", "path": f"/records/{original_count + index}", "id": rid}
            for index, rid in enumerate(appended_ids)
        ],
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source_registry")
    parser.add_argument("append_records")
    parser.add_argument("output_candidate")
    parser.add_argument("--schema", default=str(DEFAULT_SCHEMA))
    parser.add_argument("--expected-input-sha256", required=True)
    args = parser.parse_args(argv)
    try:
        result = prepare_candidate(
            args.source_registry,
            args.append_records,
            args.schema,
            args.output_candidate,
            args.expected_input_sha256,
        )
    except WriterError as exc:
        print(json.dumps(exc.as_dict(), sort_keys=True), file=sys.stderr)
        return 1
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
