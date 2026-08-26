# No-new-private-metadata commit gate

`tools/check_commit_metadata.py` rejects commit-message metadata that can
correlate public history with private SmartKey work. It scans only supplied
messages and commit revisions; it never rewrites history or modifies Git
configuration.

The gate rejects:

- the exact canonical trailer key `Claude-Session`, case-insensitively;
- absolute or home-relative private user/session paths;
- private anomaly-corpus paths;
- private-corpus receipt and source-correlation labels coupled to identifiers.

It deliberately permits ordinary commit/checksum SHAs, body prose that merely
mentions the trailer name, and standard trailers such as `Co-Authored-By`.
Diagnostics are one deterministic JSON object. A violation contains only the
rule, source kind, safe commit identity (or a fixed stdin identity), and line
number—never the matched path, trailer value, receipt, or surrounding text.

## Manual use before pushing

With an upstream branch:

```bash
base="$(git merge-base HEAD '@{upstream}')"
python3 tools/check_commit_metadata.py --range "$base..HEAD"
```

For one explicitly selected commit:

```bash
python3 tools/check_commit_metadata.py --commit HEAD
```

For a message supplied without adding it to Git:

```bash
python3 tools/check_commit_metadata.py --stdin < message.txt
```

Exit codes are `0` for a clean scan, `1` for policy violations, and `2` for a
revision/input failure. No hook is installed by the repository; the first
command is the documented manual pre-push gate.

## Remote CI range selection

CI scans the pull-request base-to-head range and the push before-to-after
range. If a push has no resolvable previous boundary, it scans the complete
history reachable from the new head, including a root commit.

## Security boundary

This is a no-new commit-message gate, not a general secret scanner. It does
not inspect file contents, Git notes, annotated tags, branch names, issue/PR
text, build artifacts, already-published history outside the selected range,
or encoded/obfuscated values. Pattern checks can conservatively reject a
synthetic or documentation path that looks private. Remediation is manual;
the tool never prints the detected value and never rewrites a commit.
