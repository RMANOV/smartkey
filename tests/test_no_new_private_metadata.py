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
import time
import unittest
from unittest import mock
from contextlib import redirect_stdout


ROOT = Path(__file__).resolve().parent.parent
TOOL = ROOT / "tools" / "check_commit_metadata.py"
POLICY_DOC = ROOT / "docs" / "no-private-commit-metadata.md"
TRUSTED_WORKFLOW = ROOT / ".github" / "workflows" / "trusted-commit-metadata.yml"
PINNED_CHECKOUT = "actions/checkout@fbc6f3992d24b796d5a048ff273f7fcc4a7b6c09"
PINNED_SETUP_PYTHON = "actions/setup-python@a26af69be951a213d495a4c3e4e4022e16d87065"
PINNED_RUST_TOOLCHAIN = (
    "dtolnay/rust-toolchain@4360b52568e2003a75bf9bc1d59f33a8e3fc893c"
)
PINNED_RUST_CACHE = "Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6"
PINNED_UPLOAD_ARTIFACT = (
    "actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02"
)


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

    def assert_correlation_table(
        self,
        cases: list[tuple[str, str, bool]],
    ) -> None:
        self.assert_rule_table(cases, "private_corpus_correlation")

    def assert_rule_table(
        self,
        cases: list[tuple[str, str, bool]],
        rule: str,
    ) -> None:
        process = self.run_gate(
            "--stdin",
            message=(
                "Synthetic subject\n\n"
                + "\n".join(value for _name, value, _reject in cases)
                + "\n"
            ),
        )
        expected = {
            (line_number, rule)
            for line_number, (_name, _value, reject) in enumerate(cases, start=3)
            if reject
        }
        self.assertEqual(process.returncode, 1 if expected else 0, process.stderr)
        payload = self.assert_payload(process)
        actual = {(item["line"], item["rule"]) for item in payload["violations"]}
        self.assertEqual(actual, expected, [name for name, _value, _reject in cases])

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
            ",/home/sample-account/synthetic-private-note.md",
            "[/Users/sample-account/synthetic-private-note.md]",
            ";C:\\Users\\sample-account\\synthetic-private-note.md",
            "file:///home/sample-account/synthetic-private-note.md",
            "file:///Users/sample-account/synthetic-private-note.md",
            "file:///C:/Users/sample-account/synthetic-private-note.md",
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
            "https://example.invalid/Users/sample-account/public",
            "https://example.invalid/C:/Users/sample-account/public",
            "file:///opt/public",
            "file:///tmp/Users-guide.txt",
            "/opt/root/public",
            "docs/home/sample-account/public",
        )
        for benign in benign_locations:
            with self.subTest(benign=benign):
                process = self.run_gate(
                    "--stdin", message=f"Synthetic subject\n\nReference: {benign}\n"
                )
                self.assertEqual(process.returncode, 0, process.stderr)

    def test_file_uri_authority_private_homes_fail_without_url_broadening(
        self,
    ) -> None:
        private_file_uris = (
            "file://host.example/home/sample-account/private-note.md",
            "file://host.example/Users/sample-account/private-note.md",
            "file://host.example/C:/Users/sample-account/private-note.md",
        )
        for private_file_uri in private_file_uris:
            with self.subTest(private_file_uri=private_file_uri):
                process = self.run_gate(
                    "--stdin",
                    message=(f"Synthetic subject\n\nReference: {private_file_uri}\n"),
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
            "file://host.example/opt/public",
            "file://host.example/tmp/Users-guide.txt",
            "docs/file://host.example/home/sample-account/relative",
            "https://host.example/home/sample-account/public",
            "https://host.example/Users/sample-account/public",
            "https://host.example/C:/Users/sample-account/public",
        )
        for benign in benign_locations:
            with self.subTest(benign=benign):
                process = self.run_gate(
                    "--stdin",
                    message=f"Synthetic subject\n\nReference: {benign}\n",
                )
                self.assertEqual(process.returncode, 0, process.stderr)

    def test_file_uri_boundary_accepts_punctuation_but_not_path_prefixes(
        self,
    ) -> None:
        for delimiter in (")", ">", "}", "?", "#", "&"):
            with self.subTest(delimiter=delimiter):
                process = self.run_gate(
                    "--stdin",
                    message=(
                        "Synthetic subject\n\nReference: "
                        f"{delimiter}file://host.example/home/sample-account/private\n"
                    ),
                )
                self.assertEqual(process.returncode, 1, process.stderr)
                self.assertIn(
                    "private_home_path",
                    {
                        item["rule"]
                        for item in self.assert_payload(process)["violations"]
                    },
                )

        benign = (
            "notfile://host.example/home/sample-account/relative",
            "docs/file://host.example/home/sample-account/relative",
            (
                "https://host.example/path/"
                "file://nested.example/home/sample-account/public"
            ),
        )
        for value in benign:
            with self.subTest(benign=value[:24]):
                process = self.run_gate(
                    "--stdin",
                    message=f"Synthetic subject\n\nReference: {value}\n",
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
            "Synthetic subject\n\nclaude_shard_manifest.json=" + "a" * 40 + "\n",
            "Synthetic subject\n\ncodex-shard.jsonl: " + "b" * 64 + "\n",
            (
                "Synthetic subject\n\nsha256: "
                + "c" * 64
                + " claude_shard_manifest.json\n"
            ),
            (
                "Synthetic subject\n\nchecksum="
                + "d" * 40
                + " codex-shard-manifest.json\n"
            ),
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

        benign_correlations = (
            "Synthetic subject\n\nclaude_shard_manifest.json\n",
            "Synthetic subject\n\nordinary_manifest.json=" + "a" * 64 + "\n",
            ("Synthetic subject\n\nsha256: " + "b" * 64 + " ordinary_manifest.json\n"),
            "Synthetic subject\n\ncodex-shard-manifest.json=abc123\n",
        )
        for message in benign_correlations:
            with self.subTest(message=message[:32]):
                process = self.run_gate("--stdin", message=message)
                self.assertEqual(process.returncode, 0, process.stderr)

    def test_dotted_shard_manifest_correlations_fail(self) -> None:
        messages = (
            "Synthetic subject\n\nclaude.shard.manifest.json=" + "a" * 40 + "\n",
            (
                "Synthetic subject\n\nsha256: "
                + "b" * 64
                + " claude.shard.manifest.json\n"
            ),
        )
        for message in messages:
            with self.subTest(message_number=messages.index(message)):
                process = self.run_gate("--stdin", message=message)
                self.assertEqual(process.returncode, 1, process.stderr)
                self.assertIn(
                    "private_corpus_correlation",
                    {
                        item["rule"]
                        for item in self.assert_payload(process)["violations"]
                    },
                )

    def test_bare_checksum_first_known_shard_manifest_fails(self) -> None:
        messages = (
            "Synthetic subject\n\n" + "c" * 40 + "  codex-shard.jsonl\n",
            ("Synthetic subject\n\n" + "d" * 64 + " *claude_shard_manifest.json\n"),
        )
        for message in messages:
            with self.subTest(message_number=messages.index(message)):
                process = self.run_gate("--stdin", message=message)
                self.assertEqual(process.returncode, 1, process.stderr)
                self.assertIn(
                    "private_corpus_correlation",
                    {
                        item["rule"]
                        for item in self.assert_payload(process)["violations"]
                    },
                )

        benign = (
            "Synthetic subject\n\n" + "e" * 64 + "\n",
            "Synthetic subject\n\nchecksum: " + "f" * 64 + "\n",
            "Synthetic subject\n\n" + "1" * 64 + " ordinary_manifest.json\n",
        )
        for message in benign:
            with self.subTest(benign_number=benign.index(message)):
                process = self.run_gate("--stdin", message=message)
                self.assertEqual(process.returncode, 0, process.stderr)

    def test_bare_checksum_supports_bounded_filesystem_prefixes(self) -> None:
        path_prefixes = (
            ("adjacent", ""),
            ("relative", "docs/"),
            ("dot-relative", "./"),
            ("parent-relative", "../"),
            ("multi-relative", "one/two/three/"),
            ("posix-root", "/var/tmp/"),
            ("windows-drive", "C:\\work\\tmp\\"),
            ("unc", "\\\\server\\share\\dir\\"),
        )
        separators = (("whitespace", "  "), ("star", " *"))
        cases = []
        for path_name, path_prefix in path_prefixes:
            for size in (40, 64):
                for separator_name, separator in separators:
                    cases.append(
                        (
                            f"{path_name}-{size}-{separator_name}",
                            (
                                ("a" * size)
                                + separator
                                + path_prefix
                                + "claude_shard_manifest.json"
                            ),
                            True,
                        )
                    )
        self.assert_correlation_table(cases)

    def test_bare_checksum_path_bounds_and_near_misses(self) -> None:
        max_component = "a" * 32
        max_prefix = "/".join([max_component] * 8) + "/"
        too_many_components = "/".join(["a"] * 9) + "/"
        windows_max_prefix = "Q:\\" + "\\".join([max_component] * 8) + "\\"
        windows_too_many = "Q:\\" + "\\".join(["a"] * 9) + "\\"
        unc_max_prefix = "\\\\server\\share\\" + "\\".join([max_component] * 6) + "\\"
        unc_too_many = "\\\\server\\share\\" + "\\".join(["a"] * 7) + "\\"
        too_long_component = ("b" * 33) + "/"
        filename = "claude_shard_manifest.json"
        cases = [
            ("relative-maximum", ("a" * 40) + "  " + max_prefix + filename, True),
            (
                "relative-too-many",
                ("b" * 40) + "  " + too_many_components + filename,
                False,
            ),
            (
                "rooted-maximum",
                ("c" * 40) + "  /" + max_prefix + filename,
                True,
            ),
            (
                "rooted-too-many",
                ("d" * 40) + "  /" + too_many_components + filename,
                False,
            ),
            (
                "drive-maximum",
                ("e" * 64) + " *" + windows_max_prefix + filename,
                True,
            ),
            (
                "drive-too-many",
                ("f" * 64) + " *" + windows_too_many + filename,
                False,
            ),
            (
                "unc-maximum",
                ("1" * 40) + "  " + unc_max_prefix + filename,
                True,
            ),
            (
                "unc-too-many",
                ("2" * 40) + "  " + unc_too_many + filename,
                False,
            ),
            (
                "too-long-component",
                ("3" * 64) + " *" + too_long_component + filename,
                False,
            ),
        ]
        for size in (39, 41, 63, 65):
            cases.append(
                (
                    f"invalid-digest-{size}",
                    ("d" * size) + "  docs/" + filename,
                    False,
                )
            )
        for suffix in (".bak", ".jsonx", "-extra", "_extra", ".old"):
            cases.append(
                (
                    f"near-suffix-{suffix}",
                    ("e" * 40) + "  docs/" + filename + suffix,
                    False,
                )
            )
        cases.extend(
            (
                (
                    "unrelated-basename",
                    ("f" * 64) + "  docs/ordinary_manifest.json",
                    False,
                ),
                (
                    "near-prefix-basename",
                    ("1" * 40) + "  docs/not-" + filename,
                    False,
                ),
                ("path-without-basename", ("2" * 40) + "  docs/", False),
            )
        )
        self.assert_correlation_table(cases)

    def test_repeated_bare_checksum_paths_are_bounded(self) -> None:
        filename = "claude_shard_manifest.json "
        tokens = (
            ("3" * 40) + "  a/b/c/d/e/f/g/h/i/" + filename,
            ("4" * 40) + "  /a/b/c/d/e/f/g/h/i/" + filename,
            ("5" * 40) + "  Q:\\a\\b\\c\\d\\e\\f\\g\\h\\i\\" + filename,
            ("6" * 40) + "  \\\\server\\share\\a\\b\\c\\d\\e\\f\\g\\" + filename,
        )
        token = "".join(tokens)
        target_bytes = 240 * 1024
        body = (token * ((target_bytes // len(token)) + 1))[:target_bytes]
        started = time.monotonic()
        process = self.run_gate(
            "--stdin",
            message="Synthetic subject\n\n" + body + "\n",
        )
        elapsed = time.monotonic() - started
        self.assertEqual(process.returncode, 0, process.stderr)
        self.assertEqual(self.assert_payload(process)["status"], "pass")
        self.assertLess(elapsed, 5.0)

    def test_shard_label_near_misses_do_not_correlate(self) -> None:
        digest = "a" * 64
        near_misses = (
            f"claude_shard_manifest.json.bak digest: {digest}",
            f"claude_shard_manifest.jsonx digest: {digest}",
            f"claude shardiness digest: {digest}",
            f"claude_shard_manifestation digest: {digest}",
        )
        for value in near_misses:
            with self.subTest(value=value[:36]):
                process = self.run_gate(
                    "--stdin",
                    message=f"Synthetic subject\n\nReference: {value}\n",
                )
                self.assertEqual(process.returncode, 0, process.stderr)

    def test_bounded_textual_and_exact_shard_labels_still_correlate(self) -> None:
        digest = "b" * 64
        correlations = (
            f"claude_shard_manifest_digest: {digest}",
            f"codex-shard-manifest-hash={digest}",
            f"claude.shard.manifest.json digest: {digest}",
            f"codex-shard-manifest.jsonl sha256: {digest}",
            f"Claude shard SHA-256: {digest}",
        )
        for value in correlations:
            with self.subTest(value=value[:36]):
                process = self.run_gate(
                    "--stdin",
                    message=f"Synthetic subject\n\nReference: {value}\n",
                )
                self.assertEqual(process.returncode, 1, process.stderr)
                self.assertIn(
                    "private_corpus_correlation",
                    {
                        item["rule"]
                        for item in self.assert_payload(process)["violations"]
                    },
                )

    def test_all_policy_separators_bind_both_shard_reference_classes(self) -> None:
        separators = (" ", "\t", "_", ":", "=", "(", ")", "/", ".", "`", "-")
        references = (
            ("text", "codex_shard_manifest"),
            ("filename", "claude_shard_manifest.json"),
        )
        digest_labels = ("digest", "sha", "sha256", "sha-256", "hash", "receipt")
        cases = []
        for reference_name, reference in references:
            for index, separator in enumerate(separators):
                digest_label = digest_labels[index % len(digest_labels)]
                cases.append(
                    (
                        f"{reference_name}-separator-{index}",
                        f"{reference}{separator}{digest_label}: {'a' * 64}",
                        True,
                    )
                )
        self.assert_correlation_table(cases)

    def test_shard_reference_left_boundaries_are_consistent_by_form(self) -> None:
        references = (
            ("text", "codex_shard_manifest"),
            ("filename", "claude_shard_manifest.json"),
        )
        prefixes = (
            ("standalone", "", True),
            ("path", "docs/", True),
            ("word", "not", False),
            ("underscore", "not_", False),
            ("hyphen", "not-", False),
            ("dot", "not.", False),
            ("identifier", "identifier9", False),
        )
        digest = "b" * 64
        cases = []
        for prefix_name, prefix, reject in prefixes:
            for reference_name, reference in references:
                cases.extend(
                    (
                        (
                            f"immediate-{reference_name}-{prefix_name}",
                            f"{prefix}{reference}_digest: {digest}",
                            reject,
                        ),
                        (
                            f"context-{reference_name}-{prefix_name}",
                            f"{prefix}{reference} evidence digest: {digest}",
                            reject,
                        ),
                    )
                )

            filename = f"{prefix}claude_shard_manifest.json"
            cases.extend(
                (
                    (
                        f"direct-filename-{prefix_name}",
                        f"{filename}={'c' * 40}",
                        reject,
                    ),
                    (
                        f"labelled-first-filename-{prefix_name}",
                        f"sha256: {'d' * 40} {filename}",
                        reject,
                    ),
                    (
                        f"bare-filename-{prefix_name}",
                        f"{'e' * 40}  {filename}",
                        reject,
                    ),
                )
            )
        self.assert_correlation_table(cases)

    def test_shard_reference_suffix_near_misses_are_consistent_by_form(self) -> None:
        references = (
            ("text", "codex_shard_manifest"),
            ("filename", "claude_shard_manifest.json"),
        )
        suffixes = (".bak", ".jsonx", "-extra", "_extra", ".old")
        digest = "f" * 64
        cases = []
        for reference_name, reference in references:
            for suffix in suffixes:
                cases.append(
                    (
                        f"context-{reference_name}-{suffix}",
                        f"{reference}{suffix} evidence digest: {digest}",
                        False,
                    )
                )

        for suffix in suffixes:
            filename = f"claude_shard_manifest.json{suffix}"
            cases.extend(
                (
                    (f"direct-filename-{suffix}", f"{filename}={'1' * 40}", False),
                    (
                        f"labelled-first-filename-{suffix}",
                        f"sha256: {'2' * 40} {filename}",
                        False,
                    ),
                    (
                        f"bare-filename-{suffix}",
                        f"{'3' * 40}  {filename}",
                        False,
                    ),
                )
            )
        self.assert_correlation_table(cases)

    def test_shard_digest_lengths_are_preserved_by_form(self) -> None:
        references = (
            ("text", "codex_shard_manifest"),
            ("filename", "claude_shard_manifest.json"),
        )
        sizes = (39, 40, 41, 63, 64, 65)
        cases = []
        for reference_name, reference in references:
            for size in sizes:
                labelled_reject = size <= 64
                digest = "4" * size
                cases.extend(
                    (
                        (
                            f"immediate-{reference_name}-{size}",
                            f"{reference}_digest: {digest}",
                            labelled_reject,
                        ),
                        (
                            f"context-{reference_name}-{size}",
                            f"{reference} evidence digest: {digest}",
                            labelled_reject,
                        ),
                    )
                )

        for size in sizes:
            full_digest_reject = size in {40, 64}
            digest = "5" * size
            cases.extend(
                (
                    (
                        f"direct-filename-{size}",
                        f"claude_shard_manifest.json={digest}",
                        full_digest_reject,
                    ),
                    (
                        f"labelled-first-filename-{size}",
                        f"sha256: {digest} claude_shard_manifest.json",
                        full_digest_reject,
                    ),
                    (
                        f"bare-filename-{size}",
                        f"{digest}  claude_shard_manifest.json",
                        full_digest_reject,
                    ),
                )
            )
        self.assert_correlation_table(cases)

    def test_direct_manifest_re2_example_has_exact_digest_length(self) -> None:
        candidates = [
            line
            for line in POLICY_DOC.read_text(encoding="utf-8").splitlines()
            if "(claude|codex)[_.-]shard" in line and "[:=]" in line
        ]
        self.assertEqual(len(candidates), 1)
        pattern = re.compile(candidates[0])
        self.assertTrue(pattern.pattern.startswith("(?i)(^|"))
        self.assertNotIn("(?<", pattern.pattern)

        for size in (40, 64):
            with self.subTest(valid_size=size):
                value = "claude_shard_manifest.json=" + ("c" * size)
                self.assertIsNotNone(pattern.search(value))

        for size in (41, 65):
            with self.subTest(oversized=size):
                value = "claude_shard_manifest.json=" + ("d" * size)
                self.assertIsNone(pattern.search(value))

        prefixes = (
            ("standalone", "", True),
            ("path", "docs/", True),
            ("word", "not", False),
            ("underscore", "not_", False),
            ("hyphen", "not-", False),
            ("dot", "not.", False),
            ("identifier", "identifier9", False),
        )
        for prefix_name, prefix, should_match in prefixes:
            with self.subTest(prefix=prefix_name):
                value = prefix + "claude_shard_manifest.json=" + ("e" * 40)
                self.assertEqual(pattern.search(value) is not None, should_match)

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

    def test_protected_components_share_the_same_left_boundary(self) -> None:
        allowed_prefixes = (
            ("start", ""),
            ("space", " "),
            ("tab", "\t"),
            ("colon", ":"),
            ("equals", "="),
            ("quote", '"'),
            ("slash", "/"),
            ("backslash", "\\"),
            ("relative-path", "docs/"),
            ("windows-path", "C:\\work\\"),
        )
        blocked_prefixes = (
            ("word", "not"),
            ("underscore", "not_"),
            ("hyphen", "not-"),
            ("dot", "not."),
            ("alphanumeric", "x9"),
        )
        targets = (
            ("anomaly-corpus", "anomaly-corpus/entry", "private_corpus_path"),
            ("claude-projects", ".claude/projects/entry", "private_session_path"),
            ("codex-sessions", ".codex/sessions/entry", "private_session_path"),
        )
        for target_name, target, rule in targets:
            cases = [
                (f"{target_name}-{prefix_name}", prefix + target, True)
                for prefix_name, prefix in allowed_prefixes
            ]
            cases.extend(
                (f"{target_name}-{prefix_name}", prefix + target, False)
                for prefix_name, prefix in blocked_prefixes
            )
            self.assert_rule_table(cases, rule)

    def test_anomaly_corpus_re2_example_matches_component_boundary(self) -> None:
        candidates = [
            line
            for line in POLICY_DOC.read_text(encoding="utf-8").splitlines()
            if line.startswith("(?i)") and "anomaly-corpus" in line
        ]
        self.assertEqual(len(candidates), 1)
        pattern = re.compile(candidates[0])
        self.assertTrue(pattern.pattern.startswith("(?i)(^|"))
        self.assertNotIn("(?<", pattern.pattern)

        prefixes = (
            ("start", "", True),
            ("space", " ", True),
            ("tab", "\t", True),
            ("colon", ":", True),
            ("equals", "=", True),
            ("quote", '"', True),
            ("slash", "/", True),
            ("backslash", "\\", True),
            ("relative-path", "docs/", True),
            ("word", "not", False),
            ("underscore", "not_", False),
            ("hyphen", "not-", False),
            ("dot", "not.", False),
            ("alphanumeric", "x9", False),
        )
        for prefix_name, prefix, should_match in prefixes:
            with self.subTest(prefix=prefix_name):
                value = prefix + "anomaly-corpus/entry"
                self.assertEqual(pattern.search(value) is not None, should_match)

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

    def test_malformed_event_payload_matrix_fails_closed_before_any_skip(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            repository = Path(temporary)
            self.init_repository(repository)
            head = self.commit(repository, "Synthetic head")
            zero = "0" * 40
            normal = {
                "before": head,
                "after": head,
                "created": False,
                "deleted": False,
            }

            malformed_pushes = {
                "missing_before": {
                    key: value for key, value in normal.items() if key != "before"
                },
                "missing_after": {
                    key: value for key, value in normal.items() if key != "after"
                },
                "missing_created": {
                    key: value for key, value in normal.items() if key != "created"
                },
                "missing_deleted": {
                    key: value for key, value in normal.items() if key != "deleted"
                },
                "created_string": {**normal, "created": "false"},
                "deleted_string": {**normal, "deleted": "true"},
                "created_integer": {**normal, "created": 0},
                "deleted_integer": {**normal, "deleted": 1},
                "both_flags_true": {**normal, "created": True, "deleted": True},
                "deletion_after_nonzero": {**normal, "deleted": True},
                "deletion_before_zero": {
                    "before": zero,
                    "after": zero,
                    "created": False,
                    "deleted": True,
                },
                "deletion_invalid_before": {
                    "before": "not-an-object-id",
                    "after": zero,
                    "created": False,
                    "deleted": True,
                },
                "creation_before_nonzero": {**normal, "created": True},
                "creation_after_zero": {
                    "before": zero,
                    "after": zero,
                    "created": True,
                    "deleted": False,
                },
                "normal_before_zero": {**normal, "before": zero},
                "normal_after_zero": {**normal, "after": zero},
                "before_64_hex": {**normal, "before": "a" * 64},
                "after_64_hex": {**normal, "after": "b" * 64},
                "before_non_hex": {**normal, "before": "g" * 40},
                "after_non_hex": {**normal, "after": "z" * 40},
                "before_short": {**normal, "before": "a" * 39},
                "after_long": {**normal, "after": "b" * 41},
            }
            for case, payload in malformed_pushes.items():
                with self.subTest(case=case):
                    process = self.run_event(repository, "push", payload)
                    self.assert_safe_error(process, "invalid_event")

            malformed_pull_requests = {
                "missing_base": {"pull_request": {"head": {"sha": head}}},
                "missing_head": {"pull_request": {"base": {"sha": head}}},
                "base_64_hex": {
                    "pull_request": {
                        "base": {"sha": "a" * 64},
                        "head": {"sha": head},
                    }
                },
                "head_short": {
                    "pull_request": {
                        "base": {"sha": head},
                        "head": {"sha": "b" * 39},
                    }
                },
            }
            for case, payload in malformed_pull_requests.items():
                with self.subTest(case=case):
                    process = self.run_event(repository, "pull_request_target", payload)
                    self.assert_safe_error(process, "invalid_event")

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

    def test_codeowners_and_all_workflow_actions_are_immutable(self) -> None:
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
        self.assertRegex(ci_text, r"(?m)^permissions:\n  contents: read$")

        workflow_text = "\n".join(
            path.read_text(encoding="utf-8")
            for path in sorted((ROOT / ".github" / "workflows").glob("*.yml"))
        )
        action_refs = set(
            re.findall(r'(?:"uses"\s*:\s*"|uses:\s*)([^"\s]+)', workflow_text)
        )
        self.assertEqual(
            action_refs,
            {
                PINNED_CHECKOUT,
                PINNED_SETUP_PYTHON,
                PINNED_RUST_TOOLCHAIN,
                PINNED_RUST_CACHE,
                PINNED_UPLOAD_ARTIFACT,
            },
        )
        for action_ref in action_refs:
            self.assertRegex(action_ref, r"^[^@\s]+@[0-9a-f]{40}$")
        self.assertEqual(
            workflow_text.count(PINNED_CHECKOUT),
            workflow_text.count("persist-credentials: false")
            + workflow_text.count('"persist-credentials": false'),
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
