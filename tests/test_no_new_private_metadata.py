"""Behavioral tests for the no-new-private-metadata commit-message gate."""

from __future__ import annotations

import json
import importlib.util
import io
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest
from unittest import mock
from contextlib import redirect_stdout


ROOT = Path(__file__).resolve().parent.parent
TOOL = ROOT / "tools" / "check_commit_metadata.py"
TRUSTED_WORKFLOW = ROOT / ".github" / "workflows" / "trusted-commit-metadata.yml"
PINNED_CHECKOUT = "actions/checkout@fbc6f3992d24b796d5a048ff273f7fcc4a7b6c09"
PINNED_SETUP_PYTHON = "actions/setup-python@a26af69be951a213d495a4c3e4e4022e16d87065"


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

    def run_gate_bytes(
        self,
        *arguments: str,
        message: bytes,
        cwd: Path = ROOT,
    ) -> subprocess.CompletedProcess[bytes]:
        return subprocess.run(
            [sys.executable, str(TOOL), *arguments],
            cwd=cwd,
            input=message,
            text=False,
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

    def assert_bytes_payload(self, process: subprocess.CompletedProcess[bytes]) -> dict:
        self.assertTrue(process.stdout, process.stderr)
        try:
            return json.loads(process.stdout.decode("ascii"))
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            self.fail(f"stdout was not one deterministic ASCII JSON object: {error}")

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
            "/Users/sample-account/.codex/sessions/sample/session.jsonl",
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

    def test_private_home_aliases_and_delimiters_fail_with_url_guards(self) -> None:
        synthetic_paths = (
            "$HOME/Documents/synthetic-private-note.md",
            "${HOME}/Documents/synthetic-private-note.md",
            "%USERPROFILE%\\Documents\\synthetic-private-note.md",
            "~/Documents/synthetic-private-note.md",
            "/root/synthetic-private-note.md",
            "/Users/sample-account/Documents/synthetic-private-note.md",
            "path=/home/sample-account/synthetic-private-note.md",
            "path:/home/sample-account/synthetic-private-note.md",
            "`/home/sample-account/synthetic-private-note.md`",
        )
        for synthetic_path in synthetic_paths:
            with self.subTest(path_kind=synthetic_paths.index(synthetic_path)):
                process = self.run_gate(
                    "--stdin",
                    message=f"Synthetic subject\n\nReference: {synthetic_path}\n",
                )
                self.assertEqual(process.returncode, 1, process.stderr)
                self.assertIn(
                    "private_home_path",
                    {
                        item["rule"]
                        for item in self.assert_payload(process)["violations"]
                    },
                )

        benign_locations = (
            "https://example.invalid/home/sample-account/public",
            "https://example.invalid/code/syntheticopaque1234",
            "/opt/root/public",
            "docs/home/sample-account/public",
        )
        for benign in benign_locations:
            with self.subTest(benign=benign):
                process = self.run_gate(
                    "--stdin", message=f"Synthetic subject\n\nReference: {benign}\n"
                )
                self.assertEqual(process.returncode, 0, process.stderr)

    def test_private_correlation_labels_fail_but_ordinary_shas_pass(self) -> None:
        synthetic_correlation_messages = (
            "Synthetic subject\n\nsource_session_hash: " + "a" * 64 + "\n",
            "Synthetic subject\n\nprivate corpus receipt: " + "b" * 12 + "\n",
            "Synthetic subject\n\nclaude_shard.jsonl sha256: " + "c" * 64 + "\n",
            "Synthetic subject\n\nClaude shard SHA-256: " + "f" * 64 + "\n",
            "Synthetic subject\n\nsource-session-hash=" + "1" * 64 + "\n",
            "Synthetic subject\n\nsource_record_digest: " + "2" * 64 + "\n",
            "Synthetic subject\n\nprivate-corpus-receipt=" + "6" * 64 + "\n",
            "Synthetic subject\n\nanomaly_corpus_receipt: " + "7" * 64 + "\n",
            (
                "Synthetic subject\n\nclaude-shard-manifest.json digest: "
                + "3" * 64
                + "\n"
            ),
            "Synthetic subject\n\nsha256(source-session): " + "4" * 64 + "\n",
            "Synthetic subject\n\ndigest(codex_shard_manifest)=" + "5" * 64 + "\n",
            "Synthetic subject\n\nclaude_shard_manifest_digest: " + "8" * 64 + "\n",
            "Synthetic subject\n\ncodex_shard_manifest_sha256=" + "9" * 64 + "\n",
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

    def test_known_first_party_code_correlation_is_bounded(self) -> None:
        rejected = (
            "https://claude.ai/code/syntheticopaque1234",
            "claude.ai/code/abcdef0123456789_example",
        )
        for reference in rejected:
            process = self.run_gate(
                "--stdin", message=f"Synthetic subject\n\nReference: {reference}\n"
            )
            self.assertEqual(process.returncode, 1, process.stderr)
            self.assertIn(
                "private_session_path",
                {item["rule"] for item in self.assert_payload(process)["violations"]},
            )

        benign = (
            "https://example.invalid/code/syntheticopaque1234",
            "https://claude.ai/code/docs",
            "https://claude.ai/code/" + "a" * 129,
            "https://notclaude.ai/code/syntheticopaque1234",
        )
        for reference in benign:
            process = self.run_gate(
                "--stdin", message=f"Synthetic subject\n\nReference: {reference}\n"
            )
            self.assertEqual(process.returncode, 0, process.stderr)

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

    def test_push_event_range_matrix_is_exact_and_fails_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            repository = Path(temporary)
            self.init_repository(repository)
            root = self.commit(repository, "Synthetic root")
            rejected = self.commit(
                repository,
                "Synthetic rejected\n\nClaude-Session: synthetic-event-value",
            )
            head = self.commit(repository, "Synthetic head")

            exact = self.run_event(
                repository,
                "push",
                {"before": root, "after": head, "created": False, "deleted": False},
            )
            self.assertEqual(exact.returncode, 1, exact.stderr)
            exact_payload = self.assert_payload(exact)
            self.assertEqual(exact_payload["scanned_messages"], 2)
            self.assertEqual(
                {item["source_id"] for item in exact_payload["violations"]},
                {rejected},
            )

            empty = self.run_event(
                repository,
                "push",
                {"before": head, "after": head, "created": False, "deleted": False},
            )
            self.assertEqual(empty.returncode, 0, empty.stderr)
            self.assertEqual(self.assert_payload(empty)["scanned_messages"], 0)

            deleted = self.run_event(
                repository,
                "push",
                {
                    "before": head,
                    "after": "0" * 40,
                    "created": False,
                    "deleted": True,
                },
            )
            self.assertEqual(deleted.returncode, 0, deleted.stderr)
            self.assertEqual(self.assert_payload(deleted)["scanned_messages"], 0)

            unresolved = self.run_event(
                repository,
                "push",
                {
                    "before": "a" * 40,
                    "after": head,
                    "created": False,
                    "deleted": False,
                },
            )
            self.assert_safe_error(unresolved, "boundary_unavailable")

            creation = self.run_event(
                repository,
                "push",
                {
                    "before": "0" * 40,
                    "after": head,
                    "created": True,
                    "deleted": False,
                },
            )
            self.assert_safe_error(creation, "creation_boundary_unprovable")

    def test_force_push_scans_after_only_and_pull_request_target_scans_base_head(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            repository = Path(temporary)
            self.init_repository(repository)
            root = self.commit(repository, "Synthetic root")
            old_head = self.commit(
                repository,
                "Synthetic old head\n\nClaude-Session: synthetic-old-value",
            )
            subprocess.run(
                ["git", "reset", "--hard", root],
                cwd=repository,
                text=True,
                capture_output=True,
                check=True,
            )
            new_head = self.commit(repository, "Synthetic replacement head")

            forced = self.run_event(
                repository,
                "push",
                {
                    "before": old_head,
                    "after": new_head,
                    "created": False,
                    "deleted": False,
                    "forced": True,
                },
            )
            self.assertEqual(forced.returncode, 0, forced.stderr)
            self.assertEqual(self.assert_payload(forced)["scanned_messages"], 1)

            pr_head = self.commit(
                repository,
                "Synthetic PR head\n\nClaude-Session: synthetic-pr-value",
            )
            pull_request_target = self.run_event(
                repository,
                "pull_request_target",
                {
                    "pull_request": {
                        "base": {"sha": new_head},
                        "head": {"sha": pr_head},
                    }
                },
            )
            self.assertEqual(
                pull_request_target.returncode, 1, pull_request_target.stderr
            )
            payload = self.assert_payload(pull_request_target)
            self.assertEqual(payload["scanned_messages"], 1)
            self.assertEqual(
                {item["source_id"] for item in payload["violations"]}, {pr_head}
            )

            unsupported = self.run_event(repository, "merge_group", {})
            self.assert_safe_error(unsupported, "unsupported_event")

    def test_shallow_full_history_and_missing_boundaries_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            area = Path(temporary)
            origin = area / "origin"
            shallow = area / "shallow"
            origin.mkdir()
            self.init_repository(origin)
            root = self.commit(origin, "Synthetic root")
            self.commit(
                origin,
                "Synthetic hidden middle\n\nClaude-Session: synthetic-hidden-value",
            )
            head = self.commit(origin, "Synthetic head")
            subprocess.run(["git", "branch", "boundary", root], cwd=origin, check=True)
            subprocess.run(
                ["git", "clone", "-q", "--depth", "1", f"file://{origin}", shallow],
                check=True,
            )

            history = self.run_gate("--range", "HEAD", cwd=shallow)
            self.assert_safe_error(history, "shallow_history_unavailable")

            event = self.run_event(
                shallow,
                "push",
                {"before": root, "after": head, "created": False, "deleted": False},
            )
            self.assert_safe_error(event, "boundary_unavailable")

            subprocess.run(
                [
                    "git",
                    "fetch",
                    "-q",
                    "--depth",
                    "1",
                    "origin",
                    "boundary:refs/remotes/origin/boundary",
                ],
                cwd=shallow,
                check=True,
            )
            disconnected = self.run_event(
                shallow,
                "push",
                {"before": root, "after": head, "created": False, "deleted": False},
            )
            self.assert_safe_error(disconnected, "shallow_history_unavailable")

            explicit_head = self.run_gate("--commit", head, cwd=shallow)
            self.assertEqual(explicit_head.returncode, 0, explicit_head.stderr)

    def test_invalid_bytes_argument_failures_and_limits_are_non_reflective(
        self,
    ) -> None:
        invalid = self.run_gate_bytes(
            "--stdin", message=b"Synthetic subject\n\xffPRIVATE_SENTINEL"
        )
        self.assertEqual(invalid.returncode, 2, invalid.stderr)
        self.assertNotIn(b"PRIVATE_SENTINEL", invalid.stdout + invalid.stderr)
        self.assertEqual(self.assert_bytes_payload(invalid)["error"], "invalid_utf8")

        oversized = self.run_gate_bytes("--stdin", message=b"x" * 1_100_000)
        self.assertEqual(oversized.returncode, 2, oversized.stderr)
        self.assertEqual(
            self.assert_bytes_payload(oversized)["error"], "message_too_large"
        )

        invalid_arguments = self.run_gate("--synthetic-unknown-option")
        self.assertEqual(invalid_arguments.returncode, 2, invalid_arguments.stderr)
        self.assertEqual(
            self.assert_payload(invalid_arguments)["error"], "invalid_arguments"
        )
        self.assertNotIn("synthetic-unknown-option", invalid_arguments.stderr)

        with tempfile.TemporaryDirectory() as temporary:
            repository = Path(temporary)
            self.init_repository(repository)
            tree = subprocess.run(
                ["git", "mktree"],
                cwd=repository,
                input=b"",
                capture_output=True,
                check=True,
            ).stdout.strip()
            invalid_commit = (
                b"tree "
                + tree
                + b"\nauthor Synthetic Test <synthetic@example.invalid> 0 +0000"
                + b"\ncommitter Synthetic Test <synthetic@example.invalid> 0 +0000"
                + b"\n\nSynthetic subject\n\xffPRIVATE_SENTINEL"
            )
            commit = (
                subprocess.run(
                    ["git", "hash-object", "-t", "commit", "-w", "--stdin"],
                    cwd=repository,
                    input=invalid_commit,
                    capture_output=True,
                    check=True,
                )
                .stdout.decode("ascii")
                .strip()
            )
            invalid_commit_result = self.run_gate("--commit", commit, cwd=repository)
            self.assert_safe_error(invalid_commit_result, "invalid_utf8")
            self.assertNotIn(
                "PRIVATE_SENTINEL",
                invalid_commit_result.stdout + invalid_commit_result.stderr,
            )

    def test_git_oserror_and_timeout_are_fixed_non_reflective_errors(self) -> None:
        module = self.load_tool_module()
        sentinel = "SYNTHETIC_EXCEPTION_SENTINEL"
        cases = (
            (OSError(sentinel), "git_unavailable"),
            (
                subprocess.TimeoutExpired(["git", sentinel], timeout=1),
                "git_timeout",
            ),
        )
        for failure, expected in cases:
            with self.subTest(expected=expected):
                stdout = io.StringIO()
                stderr = io.StringIO()
                with (
                    mock.patch.object(module.subprocess, "run", side_effect=failure),
                    redirect_stdout(stdout),
                    mock.patch("sys.stderr", stderr),
                ):
                    returncode = module.main(["--commit", "HEAD"])
                self.assertEqual(returncode, 2)
                self.assertEqual(json.loads(stdout.getvalue())["error"], expected)
                self.assertNotIn(sentinel, stdout.getvalue() + stderr.getvalue())

    def test_event_read_and_decode_errors_never_reflect_input(self) -> None:
        sentinel = "SYNTHETIC_EVENT_SENTINEL"
        with tempfile.TemporaryDirectory() as temporary:
            event_path = Path(temporary) / "synthetic-event.json"
            event_path.write_bytes(b'{"value":"\xff' + sentinel.encode() + b'"}')
            invalid_utf8 = self.run_gate(
                "--event-name",
                "push",
                "--event-file",
                str(event_path),
            )
            self.assert_safe_error(invalid_utf8, "invalid_utf8")
            self.assertNotIn(sentinel, invalid_utf8.stdout + invalid_utf8.stderr)

            event_path.write_text('{"value":' + ("9" * 5_000) + "}", encoding="utf-8")
            invalid_json_value = self.run_gate(
                "--event-name",
                "push",
                "--event-file",
                str(event_path),
            )
            self.assert_safe_error(invalid_json_value, "invalid_event")

            unavailable = self.run_gate(
                "--event-name",
                "push",
                "--event-file",
                str(Path(temporary) / sentinel),
            )
            self.assert_safe_error(unavailable, "event_unavailable")
            self.assertNotIn(sentinel, unavailable.stdout + unavailable.stderr)

    def test_commit_count_limit_fails_before_message_scanning(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            repository = Path(temporary)
            self.init_repository(repository)
            root = self.commit(repository, "Synthetic root")
            for index in range(129):
                self.commit(repository, f"Synthetic child {index:03d}")
            process = self.run_gate("--range", f"{root}..HEAD", cwd=repository)
            self.assert_safe_error(process, "commit_limit_exceeded")

    def test_trusted_workflow_cannot_be_replaced_by_pull_request_head(self) -> None:
        workflow = json.loads(TRUSTED_WORKFLOW.read_text(encoding="utf-8"))
        self.assertIn("pull_request_target", workflow["on"])
        self.assertNotIn("pull_request", workflow["on"])
        self.assertNotIn("merge_group", workflow["on"])
        self.assertEqual(
            set(workflow["on"]["pull_request_target"]["types"]),
            {"opened", "reopened", "synchronize", "edited"},
        )
        self.assertEqual(workflow["permissions"], {"contents": "read"})

        job = workflow["jobs"]["trusted-commit-metadata"]
        self.assertEqual(job["permissions"], {"contents": "read"})
        self.assertLessEqual(job["timeout-minutes"], 10)
        serialized = json.dumps(job, sort_keys=True)
        self.assertNotIn("secrets", serialized.casefold())

        checkout_steps = [
            step
            for step in job["steps"]
            if step.get("uses", "").startswith("actions/checkout@")
        ]
        self.assertEqual(len(checkout_steps), 2)
        self.assertEqual({step["uses"] for step in checkout_steps}, {PINNED_CHECKOUT})
        for step in checkout_steps:
            self.assertIs(step["with"]["persist-credentials"], False)
        pr_checkout = next(
            step
            for step in checkout_steps
            if "pull_request_target" in step.get("if", "")
        )
        self.assertEqual(
            pr_checkout["with"]["ref"], "${{ github.event.pull_request.base.sha }}"
        )
        push_checkout = next(
            step
            for step in checkout_steps
            if "github.event_name == 'push'" == step["if"]
        )
        self.assertEqual(push_checkout["with"]["ref"], "${{ github.sha }}")

        setup_steps = [
            step
            for step in job["steps"]
            if step.get("uses", "").startswith("actions/setup-python@")
        ]
        self.assertEqual({step["uses"] for step in setup_steps}, {PINNED_SETUP_PYTHON})
        uses_steps = [step for step in job["steps"] if "uses" in step]
        self.assertEqual(len(uses_steps), 3)
        self.assertEqual(
            {step["uses"] for step in uses_steps},
            {PINNED_CHECKOUT, PINNED_SETUP_PYTHON},
        )
        run_commands = "\n".join(step.get("run", "") for step in job["steps"])
        self.assertIn("refs/pull/$PR_NUMBER/head", run_commands)
        self.assertIn("--no-write-fetch-head", run_commands)
        self.assertIn("--filter=blob:none", run_commands)
        self.assertNotRegex(run_commands, r"\bgit\s+(?:checkout|switch)\b")
        self.assertNotIn("FETCH_HEAD", run_commands)
        self.assertEqual(run_commands.count("git fetch"), 2)
        self.assertEqual(run_commands.count("python "), 1)
        self.assertIn("tools/check_commit_metadata.py", run_commands)
        self.assertIn("--event-file", run_commands)

    def test_codeowners_and_untrusted_head_job_do_not_claim_enforcement(self) -> None:
        codeowners = (ROOT / ".github" / "CODEOWNERS").read_text(encoding="utf-8")
        for protected_path in (
            "/.github/CODEOWNERS",
            "/.github/workflows/",
            "/tools/check_commit_metadata.py",
            "/tests/test_no_new_private_metadata.py",
        ):
            self.assertRegex(
                codeowners,
                rf"(?m)^{re.escape(protected_path)}\s+@RMANOV\s*$",
            )

        ci_text = (ROOT / ".github" / "workflows" / "ci.yml").read_text(
            encoding="utf-8"
        )
        self.assertIn("Commit metadata unit tests (untrusted head)", ci_text)
        self.assertNotIn("Scan pull-request commits", ci_text)
        self.assertNotIn("Scan pushed commits", ci_text)
        action_refs = set(
            re.findall(r"actions/(?:checkout|setup-python)@[0-9A-Za-z._-]+", ci_text)
        )
        self.assertEqual(
            action_refs,
            {
                PINNED_CHECKOUT,
                PINNED_SETUP_PYTHON,
            },
        )
        self.assertEqual(
            ci_text.count(PINNED_CHECKOUT),
            ci_text.count("persist-credentials: false"),
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

    def run_event(
        self,
        repository: Path,
        event_name: str,
        payload: dict[str, object],
    ) -> subprocess.CompletedProcess[str]:
        event_path = repository / "synthetic-event.json"
        event_path.write_text(json.dumps(payload), encoding="utf-8")
        return self.run_gate(
            "--event-name",
            event_name,
            "--event-file",
            str(event_path),
            cwd=repository,
        )

    def assert_safe_error(
        self, process: subprocess.CompletedProcess[str], expected: str
    ) -> None:
        self.assertEqual(process.returncode, 2, process.stderr)
        payload = self.assert_payload(process)
        self.assertEqual(
            payload,
            {"error": expected, "schema_version": 1, "status": "error"},
        )
        self.assertNotIn("Traceback", process.stderr)

    def load_tool_module(self):
        spec = importlib.util.spec_from_file_location(
            "synthetic_commit_metadata_gate", TOOL
        )
        self.assertIsNotNone(spec)
        self.assertIsNotNone(spec.loader)
        module = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = module
        spec.loader.exec_module(module)
        self.addCleanup(sys.modules.pop, spec.name, None)
        return module


if __name__ == "__main__":
    unittest.main()
