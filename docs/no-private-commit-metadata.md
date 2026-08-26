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

Literal protected components use the same left boundary: start, punctuation,
or a path separator is accepted, while alphanumeric, underscore, dot, and
hyphen attachment is not. This applies to `anomaly-corpus`,
`.claude/projects`, and `.codex/sessions`; their existing right path boundary
is unchanged.

It deliberately permits ordinary commit/checksum SHAs, unrelated URLs and
paths, body prose that merely mentions the trailer name, and standard trailers
such as `Co-Authored-By`. Diagnostics are one deterministic ASCII JSON object.
A violation contains only the rule, source kind, safe commit identity (or the
fixed stdin identity), and line number—never a path, trailer value, receipt,
exception, Git stderr, or surrounding text.

## Mandatory pre-upload gate

The mandatory local prevention point is **before bytes are uploaded**. Run the
scanner manually before every push, or call the same command from a
user-managed global pre-push hook. This repository intentionally does not
install or modify local/global hooks. Repository-side enforcement is an
additional layer and is not active until the external bootstrap below passes.

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
`pull_request_target` only for commit metadata enforcement. GitHub loads the
workflow definition and in-job `GITHUB_SHA` from the trusted default-branch
context. Independently, the scanner policy is checked out from the exact
pull-request base SHA: the job:

1. checks out the exact pull-request base SHA, with credentials not persisted;
2. fetches the base repository's `refs/pull/<number>/head` as Git objects only;
3. never checks out or executes the pull-request head;
4. runs the base copy of the scanner over the exact `base.sha..head.sha` range.

The ordinary `pull_request` job in `ci.yml` may execute head code, so it runs
only the synthetic unit tests. It is deliberately **not** the enforcement
check. All third-party actions in both workflows are pinned to immutable commit
SHAs, permissions are read-only, checkout credentials are not persisted,
secrets are not referenced, and the metadata jobs have explicit timeouts.

These two SHA concepts must not be conflated:

- the workflow definition and in-job `GITHUB_SHA` come from the trusted
  default-branch context;
- the scanner checkout is the exact pull-request base SHA;
- GitHub's native workflow run and check suite can still be associated with the
  pull-request **head** SHA. Public REST observations of
  `pull_request_target` runs (for example, dotnet/skills run `32952853192` and
  Homebrew/brew run `32943547911`) showed
  `workflow_run.head_sha == check_suite.head_sha == PR head`.

That public evidence supports the architecture, but it does not prove this
repository's ruleset, context, or merge-blocking behavior. The target
repository must pass the live bootstrap gate below before this job is treated
as active enforcement. No final status-context or merge-blocking claim is made
until that live bootstrap passes.

`CODEOWNERS` assigns every workflow file, the policy scanner, and its tests to
`@RMANOV`. Owning the whole workflow directory also prevents an unreviewed new
workflow from imitating the required job name. CODEOWNERS alone is advisory:
it protects nothing unless the repository rules require code-owner approval
and prevent bypass.

## External activation and live bootstrap

This repository does not mutate GitHub settings. The committed artifact is
ready for review-branch publication, but **enforcement is not active** until an
external operator both configures the target rules and records a successful
read-back/bootstrap. On every controlled target branch, the rules must require
pull requests, code-owner review for policy-owned paths, dismissal of stale
approvals, approval of the latest reviewable push, and no direct/force/delete,
administrator, or other bypass.

Use a harmless bootstrap pull request to prove all of the following before
making the metadata job required:

1. the native workflow run and check suite are attached to the exact PR head
   SHA, while logs separately confirm that the workflow definition and in-job
   `GITHUB_SHA` came from the trusted default-branch context and the scanner
   checkout used the exact PR base SHA;
2. the observed context name and source are the values the repository rules
   can require—do not infer them from this file;
3. a synthetic forbidden commit makes the trusted check fail **and** the rules
   block merge;
4. replacing it with a clean synthetic commit makes the check succeed **and**
   the rules unblock merge;
5. a same-name workflow added or changed in PR-head code cannot satisfy or
   bypass the required context, and workflow/CODEOWNERS changes cannot bypass
   code-owner, stale-approval, latest-push, or no-bypass rules;
6. the final ruleset and branch-protection read-back exactly match the tested
   branch scope and context.

No fork coverage is claimed until a separate fork-origin bootstrap proves the
same properties. A required check name/source is not, by itself, evidence that
the correct workflow is required; the negative and positive merge tests are
part of activation.

### Optional server-side commit-message rules

A public personal repository can additionally use branch-ruleset
`commit_message_pattern` rules with `must_not_match`. This is defense in depth,
not a replacement for the trusted check or local pre-push scan, and it is not
active merely because patterns are documented here. Add narrow rules
separately so each can be read back and tested. RE2-compatible starting
patterns include:

```text
(?im)^[\t ]*claude-session[\t ]*[:=]
(?i)(^|[^A-Za-z0-9_.-])anomaly-corpus([/\\]|$)
(?i)(^|[[:space:]"'(<`=,;\[\]])(/(home|Users)/[^/[:space:]`]+|/root)([/\\]|$)
(?i)(^|[[:space:]"'(<`=,;\[\]])[A-Z]:[/\\]Users[/\\][^/\\[:space:]`]+([/\\]|$)
(?i)(^|[[:space:]"'(<`=,;\[\]])(\$HOME|\$\{HOME\}|%USERPROFILE%|~)[/\\]
(?i)(^|[^A-Za-z0-9_./\\-])file://([A-Za-z0-9._~:@!$&'()*+,;=%\[\]-]+)?/((home|Users)/[^/[:space:]`]+|root)(/|$)
(?i)(^|[^A-Za-z0-9_./\\-])file://([A-Za-z0-9._~:@!$&'()*+,;=%\[\]-]+)?/[A-Z]:/Users/[^/[:space:]`]+(/|$)
(?i)(^|[^A-Za-z0-9.-])(https?://)?(www\.)?claude\.ai/code/[A-Za-z0-9][A-Za-z0-9_-]{11,127}([^A-Za-z0-9_-]|$)
(?i)(private|anomaly)[ _-]?corpus[ _-]+receipt[\t ]*[:=]?[\t ]*[0-9a-f]{12,64}
(?i)(^|[^A-Za-z0-9_.-])(claude|codex)[_.-]shard([_.-]manifest)?\.jsonl?[\t ]*[:=][\t ]*[0-9a-f]{40}([0-9a-f]{24})?([^0-9a-f]|$)
(?i)(^|[^0-9a-f])[0-9a-f]{40}([0-9a-f]{24})?[\t ]+\*?(claude|codex)[_.-]shard([_.-]manifest)?\.jsonl?([^A-Za-z0-9_.-]|$)
```

RE2 has no look-around, so the file-URI examples use a consuming predecessor
group equivalent to the scanner's one-character negative predecessor check. A
regex rule cannot reproduce Git's canonical trailer parsing or every URL/path
boundary without unacceptable false positives. The patterns above
intentionally cover only bounded forms; the repository scanner remains the
richer policy. Audit existing controlled-branch history first, account for
legacy matches, activate rules externally, exercise both rejection and
acceptance on a bootstrap branch, and read back the exact rule
mode/pattern/scope. Repository code cannot activate or verify that state.

The bare-checksum RE2 starter above intentionally covers an adjacent basename
only. The repository scanner additionally accepts bounded filesystem-shaped
relative, rooted, drive, and UNC prefixes. That grammar allows at most eight
path components of at most 32 characters each; a UNC server and share count as
two of the eight components. The server-side starter is not equivalent path
coverage and must not be represented as such without a separate RE2 rule and
bootstrap matrix.

The push-triggered run is explicitly **post-publication detection**, never
prevention. A clean push result does not prove that an active ruleset blocked
the upload. Event parsing requires exact boolean `created`/`deleted` flags,
40-hex `before`/`after` identities, and a consistent creation/deletion/normal
tuple before any skip. A genuine deletion is skipped because it adds no
commits. A normal or force push scans the exact `before..after` selection and
fails closed when a nonzero boundary is unavailable. A branch-creation event
fails closed because the push payload does not provide a trustworthy snapshot
of all pre-existing refs; use the pre-upload full-history gate before creating
the remote branch. An exact but empty range is a valid pass.

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
range, or encoded/obfuscated values. File-URI coverage specifically requires a
literal `file://` scheme and literal home-directory components; encoded or
otherwise obfuscated spellings remain out of contract. Pattern checks can
conservatively reject synthetic documentation that looks private. Remediation
is manual; the tool never prints the detected value and never rewrites a
commit.
