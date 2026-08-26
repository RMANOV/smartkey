#!/usr/bin/env python3
"""Reject new commit-message metadata that can correlate private sessions.

The command emits one canonical JSON object and never includes matched text or
trailer values in diagnostics.  Exit status is 0 for a clean scan, 1 for
policy violations, and 2 for an input/Git error.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import json
import re
import subprocess
import sys


SCHEMA_VERSION = 1
FORBIDDEN_TRAILER_KEY = "claude-session"

PRIVATE_HOME_PATTERNS = (
    re.compile(r"(?:^|[\s\"'(<])/(?:home|Users)/[^/\s]+/", re.IGNORECASE),
    re.compile(r"\b[A-Z]:[\\/]Users[\\/][^\\/\s]+[\\/]", re.IGNORECASE),
    re.compile(
        r"(?:^|[\s\"'(<])~[\\/](?:\.claude|\.codex|\.local[\\/]state[\\/]smartkey)(?:[\\/]|$)",
        re.IGNORECASE,
    ),
)
PRIVATE_SESSION_PATTERNS = (
    re.compile(
        r"(?:^|[\\/])\.(?:claude|codex)[\\/](?:projects|sessions)(?:[\\/]|$)",
        re.IGNORECASE,
    ),
    re.compile(r"\bclaude\.ai/(?:code/)?session/[^\s]+", re.IGNORECASE),
)
PRIVATE_CORPUS_PATTERNS = (
    re.compile(r"(?:^|[\\/])anomaly-corpus(?:[\\/]|$)", re.IGNORECASE),
    re.compile(r"\banchor-[0-9]{8}T[0-9]{6}(?:[+-][0-9]{4}|Z)\b", re.IGNORECASE),
)
PRIVATE_CORRELATION_PATTERNS = (
    re.compile(r"\bsource_(?:session|record|path)_hash\b\s*[:=]", re.IGNORECASE),
    re.compile(r"\bactive_transcript_cross_check\b", re.IGNORECASE),
    re.compile(r"\bsource_boundary_inventory_sha256\b", re.IGNORECASE),
    re.compile(
        r"\b(?:private|anomaly)[ _-]?corpus\s+receipt\b\s*[:=]?\s*[0-9a-f]{12,64}\b",
        re.IGNORECASE,
    ),
    re.compile(
        r"\bclaude_shard\.jsonl\b[^\r\n]{0,40}\b(?:sha(?:-?256)?|hash)\b\s*[:=]?\s*[0-9a-f]{12,64}\b",
        re.IGNORECASE,
    ),
    re.compile(
        r"\b(?:claude|codex)[ _-]?shard\b[^\r\n]{0,40}\b(?:sha(?:-?256)?|hash|receipt)\b\s*[:=]?\s*[0-9a-f]{12,64}\b",
        re.IGNORECASE,
    ),
)
TRAILER_LINE_RE = re.compile(r"^\s*claude-session\s*[:=]", re.IGNORECASE)


class InputError(Exception):
    """A safe-to-report input failure without private exception details."""


@dataclass(frozen=True)
class MessageSource:
    source_kind: str
    source_id: str
    message: str


def _run_git(arguments: list[str], *, message: str | None = None) -> str:
    process = subprocess.run(
        ["git", *arguments],
        input=message,
        text=True,
        capture_output=True,
        check=False,
    )
    if process.returncode != 0:
        raise InputError("git_operation_failed")
    return process.stdout


def _canonical_trailer_keys(message: str) -> list[str]:
    parsed = _run_git(
        ["-c", "trailer.separators=:=", "interpret-trailers", "--parse"],
        message=message,
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
    if not revision or revision.startswith("-"):
        raise InputError("invalid_revision")


def _commits_from_inputs(ranges: list[str], commits: list[str]) -> list[str]:
    resolved: set[str] = set()
    for revision_range in ranges:
        _reject_option_like_revision(revision_range)
        output = _run_git(["rev-list", "--reverse", "--topo-order", revision_range])
        resolved.update(line for line in output.splitlines() if line)
    for revision in commits:
        _reject_option_like_revision(revision)
        commit = _run_git(["rev-parse", "--verify", f"{revision}^{{commit}}"]).strip()
        if not re.fullmatch(r"[0-9a-f]{40,64}", commit):
            raise InputError("invalid_commit_identity")
        resolved.add(commit)
    return sorted(resolved)


def _collect_sources(args: argparse.Namespace) -> list[MessageSource]:
    sources: list[MessageSource] = []
    if args.stdin:
        sources.append(MessageSource("stdin", "stdin:0", sys.stdin.read()))
    for commit in _commits_from_inputs(args.ranges, args.commits):
        message = _run_git(["show", "-s", "--format=%B", commit, "--"])
        sources.append(MessageSource("commit", commit, message))
    if not sources:
        raise InputError("no_input_selected")
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
    parser = argparse.ArgumentParser(
        description="Reject private session/corpus metadata in commit messages."
    )
    parser.add_argument("--range", dest="ranges", action="append", default=[])
    parser.add_argument("--commit", dest="commits", action="append", default=[])
    parser.add_argument("--stdin", action="store_true")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    try:
        sources = _collect_sources(args)
        violations = [
            violation for source in sources for violation in scan_message(source)
        ]
    except InputError as error:
        _emit(
            {
                "error": str(error),
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
