# AI Platform Differential Review — 2026-07-23

## Decision

**APPROVE FOR MERGE** after the corrective working-tree changes in this review are committed. No unresolved P0/P1 security, compatibility, runtime, or side-effect blocker remains in the reviewed diff.

The reviewed baseline is `origin/main` at `9d7c5f18a10f4f8a4963faeaa4378508147b07e1`. The supplied branch started at `c45466f72ab6965646aa9d7a249db5b5d597e15f`.

## Blocking Issues Found And Corrected

1. Three generated Office validator families contained a syntax-breaking indentation edit. All nine copies now compile and use the same hardened `lxml` parser.
2. Image embedding failures could shorten a batch and misalign vectors with inputs. The existing strict ordered batch contract is restored: a failed item fails the batch.
3. Retrieval partial-failure handling unpacked exception objects before checking them and could mishandle cancellation. Results are checked first, strict dense/BM25 modes still fail, hybrid mode can degrade one side, and `CancelledError` propagates.
4. An incomplete oversized SSE event could lose its framing and later persist a successful zero-token usage record. The parser now uses bounded discard-until-boundary state, records unmetered overflow as `error`/`zero_on_failure`, and can recover later valid usage while retaining overflow metadata.
5. Migration DSN resolution had an insecure hardcoded fallback. It now accepts `DATABASE_URL` or `GATEWAY_DATABASE__DSN`, falls back to repository Settings, and otherwise exits without printing caught exception content.
6. The Assistant package used `defusedxml` without declaring it directly. The dependency and lockfile entry are now explicit.
7. Assistant storage fallback logging referenced the wrong exception and could expose configuration-bearing exception text. It now logs only the initialization exception class.
8. The branch accidentally committed the user's local `web/playwright.live.config.ts`. It is removed from the branch delta while the local file remains preserved and untracked.

The background task retention, admission-state locking, independent lease release, XML hardening, and per-result presigned-URL isolation changes were retained after full-file review.

## Verification Evidence

| Check | Result |
| --- | --- |
| Changed Python Ruff lint | Pass |
| Changed Python byte compilation | Pass |
| `uv lock --check` | Pass |
| Focused streaming/billing regression | 37 passed |
| New and directly affected regression set | 61 passed |
| Wider proxy, adapter, middleware, knowledge, and migration suite | 413 passed, 1 Windows-only skip |
| Assistant runtime regression gate | 5/5 groups passed: 81, 80, 30, and 100 tests plus golden gate |
| Isolation without local credentials | 4 passed, 2 credential-dependent skips |
| Credentialed provider-free black-box isolation | 6 passed, 0 skipped; non-stream and SSE Gateway-to-Assistant paths executed |
| Security diff review | 17/17 source-like files have full-file receipts; one low-severity billing candidate was reproduced, fixed, and suppressed; zero unresolved reportable findings |
| Docker ownership | Gateway, frontend, Assistant, and Knowledge labels all resolve to this repository |
| Docker candidate runtime | Current core and Assistant source plus `defusedxml 0.7.1` started healthy; Gateway and Knowledge were hot-updated; all eight services healthy |
| Docker resource state | No OOM; Assistant approximately 117 MiB of 640 MiB after validation |

The canonical local security report is under scan `c45466f_20260723T044245Z`; its deterministic report records zero surviving findings.

## Evidence Boundaries

- No real-provider chat ran because neither the local environment nor the container had a usable Qwen/DashScope credential. The provider-free Stub validates the application and streaming contracts, not model quality.
- The standard Compose build first timed out resolving the remote Dockerfile frontend. A full Dockerfile build without that frontend reached dependency installation but was stopped after external PyPI throughput fell to roughly 50–110 KB/s. A minimal candidate image based on the official image, with the current core/Assistant source and new dependency, passed startup and health checks. The complete release Dockerfile build is therefore not claimed as locally completed.
- No production deployment, production migration, provider credential mutation, image push, cache prune, or destructive Docker cleanup was performed.
- The repository-wide Ruff formatter would reformat historical large files and is not a configured CI gate; only lint was used to avoid unrelated churn.

## Residual Risk And Rollback

The remaining risk is external-environment availability: a release builder still needs working Docker Hub/PyPI access, and a real Qwen credential is required for provider-quality validation. Application rollback is a normal Git revert plus restoration of the last published service images. No schema change or production state mutation is part of this branch.
