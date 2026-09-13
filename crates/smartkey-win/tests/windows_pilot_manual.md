# SmartKey Windows experimental pilot evidence form

This form is for a future disposable, standard-user Windows pilot. Packaging is not registration, native acceptance, or daily-readiness evidence. Do not use a daily account, elevation, employer data, private corpora, or real keystrokes.

## Artifact identity and completeness

- GitHub Actions run URL:
- Source SHA (must equal `artifact-source-manifest.json`):
- Artifact name (must be `smartkey-windows-pilot`):
- `smartkey-register.exe` SHA-256:
- `smartkey_win.dll` SHA-256:
- `pilot-runtime-manifest.json` SHA-256:
- `smartkey.json` SHA-256:
- `corpus_en.json` SHA-256:
- `corpus_bg.json` SHA-256:
- `corpus_tech.json` SHA-256:
- `PILOT_README.md` SHA-256:
- `LICENSE` SHA-256:
- `LICENSE-GPL` SHA-256:
- `LICENSE-APACHE` SHA-256:
- Exact allowlist only, with no installer, `personal.json`, private corpus, autostart component, or telemetry: PASS / FAIL
- Runtime DLL dependencies inspected on the built artifact; tool and output attached:
- Dependency inventory completeness: PASS / FAIL / UNVERIFIED

If any required file is missing or a hash differs, stop. A successful build or `corpus_loaded` state does not waive package completeness.

## Account and data staging

- Disposable account identifier (non-sensitive alias only):
- Standard/non-elevated account proof: PASS / FAIL
- Administrator/split-token rejection proof: PASS / FAIL
- Manually created destination: `%APPDATA%\smartkey`
- Manually copied only `smartkey.json`, `corpus_en.json`, `corpus_bg.json`, and `corpus_tech.json`: PASS / FAIL
- No `personal.json`, private data, autostart, scheduled task, service, or network activity introduced: PASS / FAIL

No registration or cleanup command is available from this Task 2 checkpoint. Wait for the separately approved Task 1 nonce-owned probe interface and receipt; do not substitute current `--install`, `--uninstall`, `regsvr32`, or ad-hoc registry commands.

## Phase 0 registration evidence (unavailable until Task 1)

| Phase | Status | HRESULT / aggregate evidence | Residuals |
|---|---|---|---|
| Begin / token gate | NOT RUN |  |  |
| HKCU COM | NOT RUN |  |  |
| Profile | NOT RUN |  |  |
| Keyboard category | NOT RUN |  |  |
| Display-attribute category | NOT RUN |  |  |
| Fresh-process verification | NOT RUN |  |  |
| Nonce-scoped cleanup | NOT RUN |  |  |

## Later native-app acceptance

Use one synthetic, non-sensitive test string consistently. Record aggregate outcomes only; never capture raw production typing.

| Surface | Before restart | After restart | ReplaceWord | Composing | Notes / failure signal |
|---|---|---|---|---|---|
| Scratch text control | NOT RUN | NOT RUN | NOT ACCEPTED | NOT ACCEPTED |  |
| Claude console | NOT RUN | NOT RUN | NOT ACCEPTED | NOT ACCEPTED |  |
| Codex console | NOT RUN | NOT RUN | NOT ACCEPTED | NOT ACCEPTED |  |
| VSCode Insiders editor | NOT RUN | NOT RUN | NOT ACCEPTED | NOT ACCEPTED |  |
| VSCode Insiders integrated terminal | NOT RUN | NOT RUN | NOT ACCEPTED | NOT ACCEPTED |  |

Final pilot status: NOT RUN / PARTIAL / PASS / FAIL

Clean uninstall and post-cleanup inspection: NOT RUN. When Task 1 exists, cleanup must be nonce-scoped to the fixed SmartKey identities; never remove another IME or perform broad HKCU/HKLM deletion. After verified cleanup, manual removal is limited to the staged pilot files under `%APPDATA%\smartkey`.
