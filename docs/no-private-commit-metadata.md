# No-new-private-metadata commit gate

`tools/check_commit_metadata.py` rejects new commit-message metadata that can
correlate public history with private SmartKey work. It scans only explicitly
selected messages, revisions, or supported GitHub events. It never rewrites
history, modifies Git configuration, checks out a pull-request head, or prints
matched text.

The gate rejects:

- the exact canonical trailer key `Claude-Session`, case-insensitively;
- private home and session paths, including standard home aliases;
- private anomaly-corpus paths;
- bounded first-party session/correlation URLs;
- known source, shard-manifest, digest, and corpus-receipt label combinations.

It deliberately permits ordinary commit/checksum SHAs, unrelated URLs and
paths, body prose that merely mentions the trailer name, and standard trailers
such as `Co-Authored-By`. Diagnostics are one deterministic ASCII JSON object.
A violation contains only the rule, source kind, safe commit identity (or the
fixed stdin identity), and line number—never a path, trailer value, receipt,
exception, Git stderr, or surrounding text.

## Mandatory pre-upload gate

The only prevention point in this repository is **before bytes are uploaded**.
Run the scanner manually before every push, or call the same command from a
user-managed global pre-push hook. This repository intentionally does not
install or modify local/global hooks.

For a branch with an upstream:

```bash
base="$(git merge-base HEAD '@{upstream}')"
python3 tools/check_commit_metadata.py --range "$base..HEAD"
```

For a new branch whose complete history is available locally:

```bash
python3 tools/check_commit_metadata.py --range HEAD
```

For one explicitly selected commit or a not-yet-created message:

```bash
python3 tools/check_commit_metadata.py --commit HEAD
python3 tools/check_commit_metadata.py --stdin < message.txt
```

Exit codes are `0` for a clean scan, `1` for a policy violation, and `2` for a
fixed-code input/Git failure. A range over more than 128 commits must be split
into bounded invocations, all of which must pass. A full-history scan fails
closed in a shallow repository; fetch the required history before retrying.

## Trusted pull-request enforcement

`.github/workflows/trusted-commit-metadata.yml` uses
`pull_request_target` only for commit metadata enforcement. GitHub loads that
workflow from the trusted base/default branch. The job then:

1. checks out the exact pull-request base SHA, with credentials not persisted;
2. fetches the base repository's `refs/pull/<number>/head` as Git objects only;
3. never checks out or executes the pull-request head;
4. runs the base copy of the scanner over the exact `base.sha..head.sha` range.

The ordinary `pull_request` job in `ci.yml` may execute head code, so it runs
only the synthetic unit tests. It is deliberately **not** the enforcement
check. Both checkout and Python setup actions used by this policy are pinned to
immutable commit SHAs, permissions are read-only, credentials are not
persisted, secrets are not referenced, and jobs have explicit timeouts.

`CODEOWNERS` assigns every workflow file, the policy scanner, and its tests to
`@RMANOV`. Owning the whole workflow directory also prevents an unreviewed new
workflow from imitating the required job name. CODEOWNERS alone is advisory:
it protects nothing unless the repository rules require code-owner approval
and prevent bypass.

## Required external repository rules

This repository does not mutate GitHub settings. For enforcement rather than
best-effort detection, an active rule/ruleset on every protected target branch
must require all of the following:

- changes merge only through pull requests;
- code-owner review for the policy-owned paths, dismissal of stale approvals,
  and approval of the most recent reviewable push;
- the exact check **`Trusted commit metadata enforcement`**, with GitHub Actions
  as the expected source (or the equivalent required-workflow rule for
  `Trusted Commit Metadata`);
- no direct-push, force-push, branch-deletion, administrator, or other bypass
  that can publish without that trusted check.

A status-check name plus the GitHub Actions source identifies an app/context,
not a workflow file cryptographically. Prefer a required-workflow ruleset when
the repository plan supports it. Otherwise, whole-directory CODEOWNERS and the
no-stale-approval rules above are part of the enforcement boundary; reviewers
must not approve a workflow that imitates the trusted job name.

The push-triggered run is explicitly **post-publication detection**, never
prevention. A clean push result does not prove that an active ruleset blocked
the upload. Deletion events are skipped because they add no commits. A normal
or force push scans the exact `before..after` selection and fails closed when a
nonzero boundary is unavailable. A branch-creation event fails closed because
the push payload does not provide a trustworthy snapshot of all pre-existing
refs; use the pre-upload full-history gate before creating the remote branch.
An exact but empty range is a valid pass.

`merge_group` is intentionally unsupported and the workflow does not subscribe
to it. No merge-queue coverage is promised. Enabling a merge queue requires a
separately reviewed trusted event design and matching required-check rules.

## Resource and privacy bounds

The scanner caps a selection at 128 commits, each message at 256 KiB, an event
payload at 1 MiB, and each Git subprocess at 10 seconds. It preflights commit
object size and bounds Git output. Invalid UTF-8, unavailable objects, shallow
history, subprocess timeout, OS errors, and invalid arguments return fixed JSON
error codes without a traceback or reflected value.

This remains a no-new commit-message gate, not a general secret scanner. It
does not inspect file contents, Git notes, annotated tags, branch names,
issue/PR text, build artifacts, already-published history outside the selected
range, or encoded/obfuscated values. Pattern checks can conservatively reject
synthetic documentation that looks private. Remediation is manual; the tool
never prints the detected value and never rewrites a commit.
