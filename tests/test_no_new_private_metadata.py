"""Behavioral tests for the no-new-private-metadata commit-message gate."""

from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent
TOOL = ROOT / "tools" / "check_commit_metadata.py"


class CommitMetadataGateTests(unittest.TestCase):
    maxDiff = None

    def run_gate(
        self,
        *arguments: str,
        message: str | None = None,
        cwd: Path = ROOT,
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(TOOL), *arguments],
            cwd=cwd,
            input=message,
            text=True,
            capture_output=True,
            check=False,
            env={**os.environ, "PYTHONDONTWRITEBYTECODE": "1"},
        )

    def assert_payload(self, process: subprocess.CompletedProcess[str]) -> dict:
        self.assertTrue(process.stdout, process.stderr)
        try:
            return json.loads(process.stdout)
        except json.JSONDecodeError as error:
            self.fail(f"stdout was not one deterministic JSON object: {error}")

    def test_exact_forbidden_trailer_key_is_case_insensitive(self) -> None:
        for trailer in (
            "Claude-Session: synthetic-value",
            "cLaUdE-SeSsIoN = another-synthetic-value",
        ):
            with self.subTest(trailer=trailer.split(maxsplit=1)[0]):
                process = self.run_gate(
                    "--stdin",
                    message=f"Synthetic subject\n\nSynthetic body.\n\n{trailer}\n",
                )
                self.assertEqual(process.returncode, 1, process.stderr)
                payload = self.assert_payload(process)
                self.assertEqual(payload["status"], "fail")
                self.assertIn(
                    "forbidden_trailer_key",
                    {item["rule"] for item in payload["violations"]},
                )

    def test_body_text_and_similar_trailer_keys_are_not_rejected(self) -> None:
        messages = (
            "Synthetic subject\n\nClaude-Session: prose, not a final trailer\nMore body.\n",
            "Synthetic subject\n\nClaude-Session-Note: benign synthetic note\n",
            (
                "Synthetic subject\n\nBody.\n\n"
                "Co-Authored-By: Synthetic Contributor <contributor@example.invalid>\n"
            ),
        )
        for message in messages:
            with self.subTest(message_number=messages.index(message)):
                process = self.run_gate("--stdin", message=message)
                self.assertEqual(process.returncode, 0, process.stderr)
                self.assertEqual(self.assert_payload(process)["status"], "pass")

    def test_private_home_session_and_corpus_path_variants_fail(self) -> None:
        synthetic_paths = (
            "/" + "home/sample-account/.claude/projects/sample/session.jsonl",
            "C:\\Users\\sample-account\\.codex\\sessions\\sample.jsonl",
            "~/.local/state/smartkey/" + "anomaly-corpus/anchor-synthetic",
        )
        for synthetic_path in synthetic_paths:
            with self.subTest(path_kind=synthetic_paths.index(synthetic_path)):
                process = self.run_gate(
                    "--stdin",
                    message=f"Synthetic subject\n\nReference: {synthetic_path}\n",
                )
                self.assertEqual(process.returncode, 1, process.stderr)
                payload = self.assert_payload(process)
                self.assertTrue(
                    {item["rule"] for item in payload["violations"]}
                    & {
                        "private_home_path",
                        "private_session_path",
                        "private_corpus_path",
                    }
                )

    def test_private_correlation_labels_fail_but_ordinary_shas_pass(self) -> None:
        synthetic_correlation_messages = (
            "Synthetic subject\n\nsource_session_hash: " + "a" * 64 + "\n",
            "Synthetic subject\n\nprivate corpus receipt: " + "b" * 12 + "\n",
            "Synthetic subject\n\nclaude_shard.jsonl sha256: " + "c" * 64 + "\n",
            "Synthetic subject\n\nClaude shard SHA-256: " + "f" * 64 + "\n",
        )
        for message in synthetic_correlation_messages:
            process = self.run_gate("--stdin", message=message)
            self.assertEqual(process.returncode, 1, process.stderr)
            payload = self.assert_payload(process)
            self.assertIn(
                "private_corpus_correlation",
                {item["rule"] for item in payload["violations"]},
            )

        benign = (
            "Synthetic subject\n\nOrdinary commit SHA: "
            + "d" * 40
            + "\nArtifact checksum: "
            + "e" * 64
            + "\n"
        )
        process = self.run_gate("--stdin", message=benign)
        self.assertEqual(process.returncode, 0, process.stderr)
        self.assertEqual(self.assert_payload(process)["status"], "pass")

    def test_diagnostics_are_deterministic_and_never_echo_raw_matches(self) -> None:
        sentinel = "SYNTHETIC_PRIVATE_SENTINEL_DO_NOT_ECHO"
        message = (
            "Synthetic subject\n\n"
            + "/home/sample-account/.claude/projects/"
            + sentinel
            + "/session.jsonl\n\nClaude-Session: "
            + sentinel
            + "\n"
        )
        first = self.run_gate("--stdin", message=message)
        second = self.run_gate("--stdin", message=message)
        self.assertEqual(first.returncode, 1, first.stderr)
        self.assertEqual(second.returncode, 1, second.stderr)
        self.assertEqual(first.stdout, second.stdout)
        self.assertNotIn(sentinel, first.stdout)
        self.assertNotIn(sentinel, first.stderr)
        payload = self.assert_payload(first)
        self.assertEqual(
            set(payload),
            {
                "schema_version",
                "scanned_messages",
                "status",
                "violation_count",
                "violations",
            },
        )
        for violation in payload["violations"]:
            self.assertEqual(
                set(violation), {"line", "rule", "source_id", "source_kind"}
            )

    def test_commit_range_excludes_boundary_and_includes_new_commits(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            repository = Path(temporary)
            self.init_repository(repository)
            boundary = self.commit(repository, "Synthetic root")
            rejected = self.commit(
                repository,
                "Synthetic child\n\nClaude-Session: synthetic-child-value",
            )
            head = self.commit(repository, "Synthetic head")

            process = self.run_gate("--range", f"{boundary}..{head}", cwd=repository)
            self.assertEqual(process.returncode, 1, process.stderr)
            payload = self.assert_payload(process)
            self.assertEqual(payload["scanned_messages"], 2)
            self.assertEqual(
                {item["source_id"] for item in payload["violations"]}, {rejected}
            )

            boundary_only = self.run_gate("--commit", boundary, cwd=repository)
            self.assertEqual(boundary_only.returncode, 0, boundary_only.stderr)
            self.assertEqual(self.assert_payload(boundary_only)["scanned_messages"], 1)

    def test_root_commit_range_and_explicit_commit_list_are_supported(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            repository = Path(temporary)
            self.init_repository(repository)
            root = self.commit(
                repository,
                "Synthetic root\n\nClaude-Session: synthetic-root-value",
            )
            child = self.commit(repository, "Synthetic child")

            root_range = self.run_gate("--range", root, cwd=repository)
            self.assertEqual(root_range.returncode, 1, root_range.stderr)
            root_payload = self.assert_payload(root_range)
            self.assertEqual(root_payload["scanned_messages"], 1)
            self.assertEqual(
                {item["source_id"] for item in root_payload["violations"]}, {root}
            )

            explicit = self.run_gate(
                "--commit", child, "--commit", root, "--commit", child, cwd=repository
            )
            self.assertEqual(explicit.returncode, 1, explicit.stderr)
            explicit_payload = self.assert_payload(explicit)
            self.assertEqual(explicit_payload["scanned_messages"], 2)
            self.assertEqual(
                {item["source_id"] for item in explicit_payload["violations"]},
                {root},
            )

    def init_repository(self, repository: Path) -> None:
        subprocess.run(
            ["git", "init", "-q", "-b", "main"],
            cwd=repository,
            check=True,
        )
        subprocess.run(
            ["git", "config", "user.name", "Synthetic Test"],
            cwd=repository,
            check=True,
        )
        subprocess.run(
            ["git", "config", "user.email", "synthetic@example.invalid"],
            cwd=repository,
            check=True,
        )

    def commit(self, repository: Path, message: str) -> str:
        subprocess.run(
            ["git", "commit", "-q", "--allow-empty", "-F", "-"],
            cwd=repository,
            input=message,
            text=True,
            check=True,
            env={
                **os.environ,
                "GIT_AUTHOR_DATE": "2026-01-01T00:00:00Z",
                "GIT_COMMITTER_DATE": "2026-01-01T00:00:00Z",
            },
        )
        return subprocess.run(
            ["git", "rev-parse", "HEAD"],
            cwd=repository,
            text=True,
            capture_output=True,
            check=True,
        ).stdout.strip()


if __name__ == "__main__":
    unittest.main()
