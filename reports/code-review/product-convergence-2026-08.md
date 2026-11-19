# Product Convergence — Final Verification Report

**Date:** 2026-08-14 · **Program:** [`deploy/runbooks/product-convergence/`](../../deploy/runbooks/product-convergence/README.md)
**Branch:** `product-convergence/main` (local, not pushed) · **Scope:** review findings #1–#5 + #8 plus the
product decisions on Confluence (connectors) and quiz (assistant plugin).

## What changed

| Finding | Result |
| --- | --- |
| #1 First-run dead end | `GET /api/v1/setup/state` (JWT + api-key auth) reports `{configured, missing, mode, default_model}`; the console shows a dismissible setup banner above every page and a three-step dashboard checklist until a provider is configured; README quickstart no longer promises a nonexistent "web setup". |
| #2 Product identity (exams/quiz/confluence) | Confluence fossil deleted (gateway 503 REST surface, zero-reference sync code, `/tasks` Confluence tab, knowledge sync cards); exams surface deleted; the console now shows only the connector stack for third-party data. |
| #3 Confluence 503 | Entire fossil removed; `ConnectorsPanel` re-pointed at the connector catalog/OAuth/MCP APIs (`src/api/v1/connectors.py`). |
| #4 Flat nav | Sidebar regrouped 使用/构建/治理 (dashboard ungrouped on top), playground labelled 模型调试, "why you are here" hints on assistant/playground/agents empty states; model-tester rail stays flat. |
| #5 Hardcoded `qwen3.7-plus` | `DEFAULT_MODEL` env var is the single deployment default (assistant + gateway); **zero** occurrences remain under `sdk/` (code, docs, examples); web console falls back to the server default; create-agent form accepts empty model_id. |
| #8 i18n not in CI | `pnpm -C web i18n:check` added to the CI frontend job. |
| Connectors productization | Admin CRUD + toggle + mode column (migration 084) + `/settings/connectors` page; catalog-model connector capability binding validated gateway-side (enabled config + connected user, stable deny codes) and documented in the agent-studio contract; `src/connectors` → `src/transports`, `ConnectorType` → `TransportType`. |
| Quiz → plugin | `agent-plugins/ai-quiz` (data-only: skill documents the built-in `generate_quiz` tool, inert quiz-expert agent); quiz generation/orchestration moved from `packages/ai-gateway-core` into `apps/assistant-service/core/quiz/`; kind-generic `artifact_shares` (migration 083, backfilled from `quiz_shares`) with public `/quiz/shared/*` aliases keeping legacy links valid; `POST /assistant/quiz/generate` deleted. |

## Verification evidence

All commands run on `product-convergence/main` @ `c24a989` (working tree clean).

| Gate | Command | Result |
| --- | --- | --- |
| Full Python suite | `uv run --all-packages --extra test pytest -q --no-cov tests/` | **6032 passed, 20 skipped** (all skips pre-existing: DSN/test-data not configured) |
| Ruff | on every touched path per phase | clean (2 import-order + TC auto-fixes in sdk/python committed) |
| OpenAPI compat | `tests/contract/test_openapi_schema_compat.py` after snapshot regeneration | 137 passed |
| Isolation | `make test-isolation` | 4 passed, 2 skipped (unreachable localhost gateway) |
| Env contract | `make validate-example-config` | OK |
| Harness contract | `make harness-check` | OK (0 warnings) |
| Frontend | `pnpm -C web type-check && lint && build && i18n:check` | green (lint 0 errors; 10 pre-existing warnings) |
| E2E smoke | `pnpm -C web e2e:opensource` | **46 passed** (incl. new first-run + nav-group specs) |
| Terminal sweeps | `rg 'qwen3.7-plus' sdk/` | zero |
| | `rg -i exams src/ web/src/` | zero |
| | `rg 'src.connectors\|ConnectorType' src/ apps/ packages/ tests/` | zero (vendored XSD exempt) |
| | `rg -i confluence src/ web/src/` | connector-stack + data-compat hits only (verified individually) |

## Not verified locally (reported honestly)

| Item | Why | Manual command |
| --- | --- | --- |
| `make verify-agent-studio` | Requires a live stack | Run after `make quickstart`: `make verify-agent-studio` |
| `make migrate-status` + migration 083/084 apply | No local Postgres running | `make migrate` then `make migrate-status` |
| Full e2e suite (quiz-workflow, quiz-history, chat-experience, model-tester) | Requires live stack + provider keys; quiz specs re-pointed at the in-chat generation path (needs a real model) | `pnpm -C web e2e` against a running stack |
| Real OAuth callback (connectors) | Requires real Atlassian/Outlook client ids | Connect a provider at `/settings/connectors` |
| Dart SDK build | dart toolchain not installed | `dart analyze` / `dart test` in `sdk/dart` |
| Java SDK build | Maven/Gradle not installed | `mvn test` in `sdk/java` |
| Python SDK live tests | Require `SDK_TEST_API_KEY` | `pytest sdk/python/tests/ -v` |

## Suggested PR split

1. `refactor: remove Confluence fossil and standalone exams surface` (P1, `4bd021c`)
2. `feat: DEFAULT_MODEL deployment default across SDKs and console` (Track C, `d1d91fd`)
3. `feat: first-run setup state, nav groups, i18n CI gate` (Track D, `9fa4269`)
4. `feat: connector catalog admin, modes, capability validation, transports rename` (Track B, `b40f898`)
5. `feat: ai-quiz plugin, artifact shares, retire quiz generation API` (P3, `d78fc87`)
6. `chore: openapi snapshots, dead settings cleanup, e2e spec alignment` (P4, `c24a989`)

Commits are already ordered this way on the branch; each was individually gated during its phase.
