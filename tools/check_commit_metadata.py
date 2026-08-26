#!/usr/bin/env python3
"""Reject new commit metadata that can correlate private sessions.

The command emits one canonical JSON object and never includes matched text,
paths, trailer values, Git stderr, or exception strings in diagnostics. Exit
status is 0 for a clean scan, 1 for policy violations, and 2 for a fixed-code
input or Git failure.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import json
from pathlib import Path
import re
import subprocess
import sys
from typing import BinaryIO


SCHEMA_VERSION = 1
FORBIDDEN_TRAILER_KEY = "claude-session"

MAX_COMMITS = 128
MAX_MESSAGE_BYTES = 256 * 1024
MAX_COMMIT_OBJECT_BYTES = MAX_MESSAGE_BYTES + (128 * 1024)
MAX_EVENT_BYTES = 1024 * 1024
MAX_GIT_OUTPUT_BYTES = MAX_COMMIT_OBJECT_BYTES
GIT_TIMEOUT_SECONDS = 10

OBJECT_ID_RE = re.compile(r"[0-9a-f]{40}(?:[0-9a-f]{24})?")
EVENT_OBJECT_ID_RE = re.compile(r"[0-9a-f]{40}", re.IGNORECASE)
ZERO_EVENT_OBJECT_ID = "0" * 40
PATH_PREFIX = r"(?:^|[\s\"'(<`=,;\[\]]|(?<![\\/][A-Za-z]):)"
FILE_URI_AUTHORITY = r"(?:[A-Za-z0-9._~!$&'()*+,;=:%@\[\]-]+)?"
FILE_URI_PREFIX = r"(?<![A-Za-z0-9_./\\-])file://" + FILE_URI_AUTHORITY + r"/"
HEX_IDENTIFIER = r"(?<![0-9a-f])[0-9a-f]{12,64}(?![0-9a-f])"
FULL_HEX_IDENTIFIER = r"(?<![0-9a-f])[0-9a-f]{40}(?:[0-9a-f]{24})?(?![0-9a-f])"
COMPONENT_LEFT_BOUNDARY = r"(?<![A-Za-z0-9_.-])"
SHARD_TEXT_LABEL = r"(?:claude|codex)[ _.-]shard(?:[ _.-]manifest)?"
SHARD_FILENAME = r"(?:claude|codex)[_.-]shard(?:[_.-]manifest)?\.jsonl?"
SHARD_REFERENCE = r"(?:" + SHARD_FILENAME + r"|" + SHARD_TEXT_LABEL + r")"
SHARD_RIGHT_BOUNDARY = r"(?![A-Za-z0-9_.-])"
BOUNDED_SHARD_REFERENCE = (
    COMPONENT_LEFT_BOUNDARY + SHARD_REFERENCE + SHARD_RIGHT_BOUNDARY
)
BOUNDED_SHARD_FILENAME = COMPONENT_LEFT_BOUNDARY + SHARD_FILENAME + SHARD_RIGHT_BOUNDARY
FILESYSTEM_SEPARATOR = r"[\\/]"
FILESYSTEM_COMPONENT = r"(?:\.\.?|[A-Za-z0-9_][A-Za-z0-9_.-]{0,31})"
RELATIVE_CHECKSUM_PREFIX = (
    r"(?:" + FILESYSTEM_COMPONENT + FILESYSTEM_SEPARATOR + r"){1,8}"
)
ROOTED_CHECKSUM_PREFIX = (
    FILESYSTEM_SEPARATOR
    + r"(?:"
    + FILESYSTEM_COMPONENT
    + FILESYSTEM_SEPARATOR
    + r"){0,8}"
)
DRIVE_CHECKSUM_PREFIX = (
    r"[A-Za-z]:"
    + FILESYSTEM_SEPARATOR
    + r"(?:"
    + FILESYSTEM_COMPONENT
    + FILESYSTEM_SEPARATOR
    + r"){0,8}"
)
UNC_CHECKSUM_PREFIX = (
    r"(?:"
    + FILESYSTEM_SEPARATOR
    + r"){2}"
    + FILESYSTEM_COMPONENT
    + FILESYSTEM_SEPARATOR
    + FILESYSTEM_COMPONENT
    + FILESYSTEM_SEPARATOR
    + r"(?:"
    + FILESYSTEM_COMPONENT
    + FILESYSTEM_SEPARATOR
    + r"){0,6}"
)
BOUNDED_CHECKSUM_PATH_PREFIX = (
    r"(?:(?:"
    + RELATIVE_CHECKSUM_PREFIX
    + r"|"
    + ROOTED_CHECKSUM_PREFIX
    + r"|"
    + DRIVE_CHECKSUM_PREFIX
    + r"|"
    + UNC_CHECKSUM_PREFIX
    + r"))?"
)
SOURCE_LABEL = r"source[-_](?:session|record|path)(?:[-_](?:hash|digest))?"
DIGEST_LABEL = r"(?:sha(?:-?256)?|digest|hash|receipt)"
LABEL_SEPARATOR = r"(?:[^\S\r\n]|[_:=()/.`-])"

PRIVATE_HOME_PATTERNS = (
    re.compile(
        FILE_URI_PREFIX + r"(?:(?:home|Users)/[^/\s`]+|root)(?:[\\/]|$)",
        re.IGNORECASE,
    ),
    re.compile(
        FILE_URI_PREFIX + r"[A-Z]:/Users/[^/\s`]+(?:/|$)",
        re.IGNORECASE,
    ),
    re.compile(
        PATH_PREFIX + r"/(?:(?:home|Users)/[^/\s`]+|root)(?:[\\/]|$)",
        re.IGNORECASE,
    ),
    re.compile(
        PATH_PREFIX + r"[A-Z]:[\\/]Users[\\/][^\\/\s`]+(?:[\\/]|$)",
        re.IGNORECASE,
    ),
    re.compile(
        PATH_PREFIX + r"(?:\$HOME|\$\{HOME\}|%USERPROFILE%|~)[\\/]",
        re.IGNORECASE,
    ),
)
PRIVATE_SESSION_PATTERNS = (
    re.compile(
        COMPONENT_LEFT_BOUNDARY
        + r"\.(?:claude|codex)[\\/](?:projects|sessions)(?:[\\/]|$)",
        re.IGNORECASE,
    ),
    re.compile(
        r"(?<![A-Za-z0-9.-])(?:https?://)?(?:www\.)?claude\.ai/"
        r"(?:code/[A-Za-z0-9][A-Za-z0-9_-]{11,127}|"
        r"(?:code/)?session/[A-Za-z0-9][A-Za-z0-9_-]{11,127})"
        r"(?![A-Za-z0-9_-])",
        re.IGNORECASE,
    ),
)
PRIVATE_CORPUS_PATTERNS = (
    re.compile(
        COMPONENT_LEFT_BOUNDARY + r"anomaly-corpus(?:[\\/]|$)",
        re.IGNORECASE,
    ),
    re.compile(r"\banchor-[0-9]{8}T[0-9]{6}(?:[+-][0-9]{4}|Z)\b", re.IGNORECASE),
)
PRIVATE_CORRELATION_PATTERNS = (
    re.compile(
        r"\bsource[-_](?:session|record|path)[-_](?:hash|digest)\b\s*[:=]",
        re.IGNORECASE,
    ),
    re.compile(r"\bactive[-_]transcript[-_]cross[-_]check\b", re.IGNORECASE),
    re.compile(r"\bsource[-_]boundary[-_]inventory[-_]sha256\b", re.IGNORECASE),
    re.compile(
        r"\b(?:private|anomaly)[ _-]?corpus[ _-]+receipt\b\s*[:=]?\s*" + HEX_IDENTIFIER,
        re.IGNORECASE,
    ),
    re.compile(
        BOUNDED_SHARD_REFERENCE
        + r"[^\r\n]{0,47}"
        + LABEL_SEPARATOR
        + DIGEST_LABEL
        + r"\b\s*[:=]?\s*"
        + HEX_IDENTIFIER,
        re.IGNORECASE,
    ),
    re.compile(
        COMPONENT_LEFT_BOUNDARY
        + SHARD_REFERENCE
        + LABEL_SEPARATOR
        + DIGEST_LABEL
        + r"\b\s*[:=]?\s*"
        + HEX_IDENTIFIER,
        re.IGNORECASE,
    ),
    re.compile(
        r"\b(?:sha(?:-?256)?|digest)\s*\(\s*(?:"
        + SOURCE_LABEL
        + r"|"
        + COMPONENT_LEFT_BOUNDARY
        + SHARD_REFERENCE
        + r")\s*\)\s*[:=]\s*"
        + HEX_IDENTIFIER,
        re.IGNORECASE,
    ),
    re.compile(
        BOUNDED_SHARD_FILENAME + r"\s*[:=]\s*" + FULL_HEX_IDENTIFIER,
        re.IGNORECASE,
    ),
    re.compile(
        r"\b(?:sha(?:-?256)?|digest|hash|checksum)\b\s*[:=]?\s*"
        + FULL_HEX_IDENTIFIER
        + r"[^\r\n]{0,47}"
        + BOUNDED_SHARD_FILENAME,
        re.IGNORECASE,
    ),
    re.compile(
        FULL_HEX_IDENTIFIER
        + r"[^\S\r\n]+\*?"
        + BOUNDED_CHECKSUM_PATH_PREFIX
        + BOUNDED_SHARD_FILENAME,
        re.IGNORECASE,
    ),
)
TRAILER_LINE_RE = re.compile(r"^\s*claude-session\s*[:=]", re.IGNORECASE)


class InputError(Exception):
    """A fixed-code input failure safe to report without exception details."""

    def __init__(self, code: str) -> None:
        super().__init__(code)
        self.code = code


class SafeArgumentParser(argparse.ArgumentParser):
    """Convert parser failures into non-reflective machine-readable errors."""

    def error(self, message: str) -> None:  # noqa: ARG002
        raise InputError("invalid_arguments")


@dataclass(frozen=True)
class MessageSource:
    source_kind: str
    source_id: str
    message: str


@dataclass(frozen=True)
class EventSelection:
    ranges: tuple[str, ...] = ()
    skip: bool = False


def _run_git(
    arguments: list[str],
    *,
    input_bytes: bytes | None = None,
    max_output_bytes: int = MAX_GIT_OUTPUT_BYTES,
) -> bytes:
    try:
        process = subprocess.run(
            ["git", *arguments],
            input=input_bytes,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            check=False,
            timeout=GIT_TIMEOUT_SECONDS,
        )
    except subprocess.TimeoutExpired as error:
        raise InputError("git_timeout") from error
    except OSError as error:
        raise InputError("git_unavailable") from error
    if process.returncode != 0:
        raise InputError("git_operation_failed")
    if len(process.stdout) > max_output_bytes:
        raise InputError("git_output_too_large")
    return process.stdout


def _decode_utf8(value: bytes) -> str:
    try:
        return value.decode("utf-8", errors="strict")
    except UnicodeDecodeError as error:
        raise InputError("invalid_utf8") from error


def _decode_ascii(value: bytes, error_code: str) -> str:
    try:
        return value.decode("ascii", errors="strict")
    except UnicodeDecodeError as error:
        raise InputError(error_code) from error


def _read_bounded(stream: BinaryIO, limit: int, error_code: str) -> bytes:
    try:
        value = stream.read(limit + 1)
    except OSError as error:
        raise InputError("input_unavailable") from error
    if len(value) > limit:
        raise InputError(error_code)
    return value


def _canonical_trailer_keys(message: str) -> list[str]:
    parsed = _decode_utf8(
        _run_git(
            ["-c", "trailer.separators=:=", "interpret-trailers", "--parse"],
            input_bytes=message.encode("utf-8"),
            max_output_bytes=MAX_MESSAGE_BYTES,
        )
    )
    keys: list[str] = []
    for line in parsed.splitlines():
        key, separator, _value = line.partition(":")
        if separator:
            keys.append(key.strip().casefold())
    return keys


def _trailer_line(message: str) -> int:
    matches = [
        line_number
        for line_number, line in enumerate(message.splitlines(), start=1)
        if TRAILER_LINE_RE.match(line)
    ]
    return matches[-1] if matches else 0


def _violation(source: MessageSource, rule: str, line: int) -> dict[str, object]:
    return {
        "line": line,
        "rule": rule,
        "source_id": source.source_id,
        "source_kind": source.source_kind,
    }


def scan_message(source: MessageSource) -> list[dict[str, object]]:
    violations: list[dict[str, object]] = []
    if FORBIDDEN_TRAILER_KEY in _canonical_trailer_keys(source.message):
        violations.append(
            _violation(source, "forbidden_trailer_key", _trailer_line(source.message))
        )

    rules = (
        ("private_home_path", PRIVATE_HOME_PATTERNS),
        ("private_session_path", PRIVATE_SESSION_PATTERNS),
        ("private_corpus_path", PRIVATE_CORPUS_PATTERNS),
        ("private_corpus_correlation", PRIVATE_CORRELATION_PATTERNS),
    )
    for line_number, line in enumerate(source.message.splitlines(), start=1):
        for rule, patterns in rules:
            if any(pattern.search(line) for pattern in patterns):
                violations.append(_violation(source, rule, line_number))

    return sorted(
        {tuple(sorted(item.items())): item for item in violations}.values(),
        key=lambda item: (
            item["source_kind"],
            item["source_id"],
            item["line"],
            item["rule"],
        ),
    )


def _reject_option_like_revision(revision: str) -> None:
    if (
        not revision
        or revision.startswith("-")
        or any(character.isspace() for character in revision)
    ):
        raise InputError("invalid_revision")


def _resolve_commit(revision: str, unavailable_code: str) -> str:
    _reject_option_like_revision(revision)
    try:
        output = _run_git(
            ["rev-parse", "--verify", f"{revision}^{{commit}}"],
            max_output_bytes=256,
        )
    except InputError as error:
        if error.code == "git_operation_failed":
            raise InputError(unavailable_code) from error
        raise
    commit = _decode_ascii(output, "invalid_commit_identity").strip()
    if not OBJECT_ID_RE.fullmatch(commit):
        raise InputError("invalid_commit_identity")
    return commit


def _is_shallow_repository() -> bool:
    output = _decode_ascii(
        _run_git(
            ["rev-parse", "--is-shallow-repository"],
            max_output_bytes=16,
        ),
        "git_operation_failed",
    ).strip()
    if output not in {"true", "false"}:
        raise InputError("git_operation_failed")
    return output == "true"


def _commits_for_range(revision_range: str) -> list[str]:
    _reject_option_like_revision(revision_range)
    if "..." in revision_range:
        raise InputError("invalid_revision")

    if ".." in revision_range:
        parts = revision_range.split("..")
        if len(parts) != 2 or not all(parts):
            raise InputError("invalid_revision")
        before = _resolve_commit(parts[0], "boundary_unavailable")
        after = _resolve_commit(parts[1], "boundary_unavailable")
        if before == after:
            return []
        if _is_shallow_repository():
            raise InputError("shallow_history_unavailable")
        normalized_range = f"{before}..{after}"
    else:
        if _is_shallow_repository():
            raise InputError("shallow_history_unavailable")
        normalized_range = _resolve_commit(revision_range, "revision_unavailable")

    try:
        output = _run_git(
            [
                "rev-list",
                "--reverse",
                "--topo-order",
                f"--max-count={MAX_COMMITS + 1}",
                normalized_range,
                "--",
            ],
            max_output_bytes=(MAX_COMMITS + 1) * 66,
        )
    except InputError as error:
        if error.code == "git_operation_failed":
            raise InputError("boundary_unavailable") from error
        raise
    commits = [
        line
        for line in _decode_ascii(output, "invalid_commit_identity").splitlines()
        if line
    ]
    if len(commits) > MAX_COMMITS:
        raise InputError("commit_limit_exceeded")
    if any(not OBJECT_ID_RE.fullmatch(commit) for commit in commits):
        raise InputError("invalid_commit_identity")
    return commits


def _read_event(path_text: str) -> dict[str, object]:
    try:
        with Path(path_text).open("rb") as stream:
            raw = _read_bounded(stream, MAX_EVENT_BYTES, "event_too_large")
    except OSError as error:
        raise InputError("event_unavailable") from error
    try:
        value = json.loads(_decode_utf8(raw))
    except (ValueError, RecursionError) as error:
        raise InputError("invalid_event") from error
    if not isinstance(value, dict):
        raise InputError("invalid_event")
    return value


def _event_object_id(value: object) -> str:
    if not isinstance(value, str) or not EVENT_OBJECT_ID_RE.fullmatch(value):
        raise InputError("invalid_event")
    return value.casefold()


def _event_boolean(payload: dict[str, object], key: str) -> bool:
    if key not in payload or type(payload[key]) is not bool:
        raise InputError("invalid_event")
    return bool(payload[key])


def _nested_object_id(payload: dict[str, object], *keys: str) -> str:
    value: object = payload
    for key in keys:
        if not isinstance(value, dict) or key not in value:
            raise InputError("invalid_event")
        value = value[key]
    return _event_object_id(value)


def _select_event_range(event_name: str, payload: dict[str, object]) -> EventSelection:
    if event_name == "push":
        created = _event_boolean(payload, "created")
        deleted = _event_boolean(payload, "deleted")
        before = _event_object_id(payload.get("before"))
        after = _event_object_id(payload.get("after"))
        before_is_zero = before == ZERO_EVENT_OBJECT_ID
        after_is_zero = after == ZERO_EVENT_OBJECT_ID

        if created and deleted:
            raise InputError("invalid_event")
        if deleted:
            if created or before_is_zero or not after_is_zero:
                raise InputError("invalid_event")
            return EventSelection(skip=True)
        if created:
            if deleted or not before_is_zero or after_is_zero:
                raise InputError("invalid_event")
            # Push creation lacks a trustworthy pre-existing-ref snapshot in the
            # payload. Failing closed avoids silently scanning older history.
            raise InputError("creation_boundary_unprovable")
        if before_is_zero or after_is_zero:
            raise InputError("invalid_event")
        return EventSelection(ranges=(f"{before}..{after}",))

    if event_name == "pull_request_target":
        base = _nested_object_id(payload, "pull_request", "base", "sha")
        head = _nested_object_id(payload, "pull_request", "head", "sha")
        return EventSelection(ranges=(f"{base}..{head}",))

    # The workflow does not subscribe to merge_group and promises no check for it.
    raise InputError("unsupported_event")


def _commits_from_inputs(ranges: list[str], commits: list[str]) -> list[str]:
    if len(ranges) + len(commits) > MAX_COMMITS:
        raise InputError("commit_limit_exceeded")
    resolved: list[str] = []
    seen: set[str] = set()
    for revision_range in ranges:
        for commit in _commits_for_range(revision_range):
            if commit not in seen:
                seen.add(commit)
                resolved.append(commit)
                if len(resolved) > MAX_COMMITS:
                    raise InputError("commit_limit_exceeded")
    for revision in commits:
        commit = _resolve_commit(revision, "commit_unavailable")
        if commit not in seen:
            seen.add(commit)
            resolved.append(commit)
            if len(resolved) > MAX_COMMITS:
                raise InputError("commit_limit_exceeded")
    return resolved


def _read_commit_message(commit: str) -> str:
    size_output = _decode_ascii(
        _run_git(["cat-file", "-s", commit], max_output_bytes=32),
        "invalid_commit_object",
    ).strip()
    try:
        object_size = int(size_output)
    except ValueError as error:
        raise InputError("invalid_commit_object") from error
    if object_size < 0 or object_size > MAX_COMMIT_OBJECT_BYTES:
        raise InputError("message_too_large")
    commit_object = _run_git(
        ["cat-file", "commit", commit], max_output_bytes=MAX_COMMIT_OBJECT_BYTES
    )
    header, separator, message = commit_object.partition(b"\n\n")
    if not separator or not header:
        raise InputError("invalid_commit_object")
    if len(message) > MAX_MESSAGE_BYTES:
        raise InputError("message_too_large")
    return _decode_utf8(message)


def _collect_sources(args: argparse.Namespace) -> list[MessageSource]:
    event_selected = args.event_file is not None or args.event_name is not None
    direct_selected = bool(args.stdin or args.ranges or args.commits)
    if event_selected and direct_selected:
        raise InputError("invalid_arguments")
    if event_selected and (args.event_file is None or args.event_name is None):
        raise InputError("invalid_arguments")
    if not event_selected and not direct_selected:
        raise InputError("no_input_selected")

    ranges = list(args.ranges)
    commits = list(args.commits)
    if event_selected:
        selection = _select_event_range(args.event_name, _read_event(args.event_file))
        if selection.skip:
            return []
        ranges.extend(selection.ranges)

    sources: list[MessageSource] = []
    if args.stdin:
        message_bytes = _read_bounded(
            sys.stdin.buffer, MAX_MESSAGE_BYTES, "message_too_large"
        )
        sources.append(MessageSource("stdin", "stdin:0", _decode_utf8(message_bytes)))
    for commit in _commits_from_inputs(ranges, commits):
        sources.append(MessageSource("commit", commit, _read_commit_message(commit)))
    return sources


def _emit(payload: dict[str, object]) -> None:
    print(
        json.dumps(
            payload,
            ensure_ascii=True,
            sort_keys=True,
            separators=(",", ":"),
        )
    )


def _parser() -> argparse.ArgumentParser:
    parser = SafeArgumentParser(
        description="Reject private session/corpus metadata in commit messages."
    )
    parser.add_argument("--range", dest="ranges", action="append", default=[])
    parser.add_argument("--commit", dest="commits", action="append", default=[])
    parser.add_argument("--stdin", action="store_true")
    parser.add_argument("--event-name")
    parser.add_argument("--event-file")
    return parser


def main(argv: list[str] | None = None) -> int:
    try:
        args = _parser().parse_args(argv)
        sources = _collect_sources(args)
        violations = [
            violation for source in sources for violation in scan_message(source)
        ]
    except InputError as error:
        _emit(
            {
                "error": error.code,
                "schema_version": SCHEMA_VERSION,
                "status": "error",
            }
        )
        return 2

    payload: dict[str, object] = {
        "schema_version": SCHEMA_VERSION,
        "scanned_messages": len(sources),
        "status": "fail" if violations else "pass",
        "violation_count": len(violations),
        "violations": violations,
    }
    _emit(payload)
    return 1 if violations else 0


if __name__ == "__main__":
    sys.exit(main())
