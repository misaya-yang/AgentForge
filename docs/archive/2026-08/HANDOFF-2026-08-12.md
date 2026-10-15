# AI Gateway Assistant × KB session handoff

Updated: 2026-08-08 (America/New_York)

## Read first

1. Read `AGENTS.md` and follow its repository, Docker, secret, verification, and Git rules.
2. Read this file before opening the implementation. Do not repeat the completed UAO work unless new evidence shows a regression.
3. The authoritative completed Harness is outside the repository at
   `/Users/yang/study_data/general-assistant-optimization-harness-2026-07-18`.
   Its `README.md`, `loop-state.json`, `HANDOFF.md`, `init.sh`, and active phase
   are the continuity contract for the completed optimization program.

## Current state

- Repository: `/Users/yang/projects/AI--Platfform`
- Branch: `main`
- Baseline commit: `5703e5c5e7ecf1fdf40e3bdd116e73e653cf1250`
- Remote state before creating this handoff: `HEAD == origin/main`; the working
  tree was clean.
- Product posture: General AI Assistant plus KB, not a coding-agent product.
- Harness state: UAO-00 through UAO-14 are `done`; all 15 declared features
  (sparse IDs from UAO-F001 through UAO-F028) pass; `Blockers: none`;
  `Decision: done`.
- This handoff and its README link are intentionally local, uncommitted changes.
  Re-check `git status --short --branch` before any edit or staging action.

## Completed work

The current baseline implements the Assistant × KB capability and reliability
plan without changing the required public Assistant, Responses, SSE, approval,
artifact, trace, or KB contracts. The main delivered areas are:

- result-level live Assistant and Assistant × KB capability tests;
- controlled pre-delta model failover with explicit server-side mappings;
- strict tool argument schema validation and one correction opportunity;
- optional, redacted large tool-output spill into existing artifact storage;
- staged, anti-thrashing context compaction and external-content trust envelopes;
- explicit MCP operation metadata with unknown tools treated as potential writes;
- tenant-scoped provider/model resolution for Assistant and KB;
- a compact, non-conflicting General Assistant system prompt hierarchy;
- Agent Plugins 1.0.0 support in skills-only mode, with no bundled script or
  `mcp.json` execution;
- quickstart, Docker distribution, frontend, security, isolation, and rollback
  coverage.

Important implementation entry points:

- `apps/assistant-service/src/assistant_service/core/agent/agent_loop.py`
- `apps/assistant-service/src/assistant_service/core/prompts/system_prompt_v2.py`
- `apps/assistant-service/src/assistant_service/core/models/model_failover.py`
- `apps/assistant-service/src/assistant_service/core/agent/middlewares/tool_output_spill.py`
- `apps/assistant-service/src/assistant_service/core/runtime/context/external_content.py`
- `apps/assistant-service/src/assistant_service/core/tool_invoker.py`
- `apps/assistant-service/src/assistant_service/core/mcp/`
- `apps/knowledge-service/src/knowledge_service/services/knowledge/`
- `packages/ai-gateway-core/src/ai_gateway_core/agent_plugins.py`
- `src/services/eval/assistant_capability.py`
- `tests/integration/test_assistant_api_e2e_live.py`
- `tests/integration/test_assistant_kb_capability_live.py`

## Feature switches and promotion posture

Verified against `.env.example` and the in-code `_env_flag` defaults on
2026-08-12. Two of these have been promoted since this list was first written,
so do not treat "all optional capabilities are off" as still true:

| Flag | `.env.example` | code default | note |
| --- | --- | --- | --- |
| `ASSISTANT_RUNTIME_FAILOVER_V2` | `false` | `False` | consistent |
| `ASSISTANT_STAGED_COMPACTION_ENABLED` | `false` | `False` | consistent |
| `ASSISTANT_SUBAGENTS_ENABLED` | `false` | `False` | consistent |
| `ASSISTANT_TOOL_OUTPUT_SPILL_ENABLED` | `true` | `True` | **promoted — now on** |
| `ASSISTANT_RUNTIME_SKILLS` | `true` | `False` | **drift: env enables, code default disables** |

`ASSISTANT_RUNTIME_SKILLS` is the one real inconsistency: a deployment that
copies `.env.example` runs with Skills on, while any code path that resolves
the flag without the env var present runs with Skills off. Pick one and make
the two agree before relying on Skills behaviour in evaluation.

`ASSISTANT_MODEL_FALLBACKS_JSON={}` is an explicit server-side mapping, not a
model-supplied policy. Do not enable Skills, subagents, or retrieval variants by
default until result-level evaluation demonstrates benefit without permission,
cancellation, isolation, cost, or answer-quality regressions.

## Verification evidence

Freshly checked while preparing this handoff:

- `python3 /Users/yang/.codex/skills/prd-phase-harness/scripts/validate_harness_prd.py /Users/yang/study_data/general-assistant-optimization-harness-2026-07-18 --strict --claim-check`
  passed. This validates Harness structure and claim metadata; it does not rerun
  the cited tests.
- Eight `ai-gateway-*` containers were healthy, and every running container's
  `com.docker.compose.project.working_dir` was
  `/Users/yang/projects/AI--Platfform`.

Accepted evidence recorded in the Harness, not rerun for this handoff edit:

- `make verify-assistant-runtime-dev`: 5/5 groups passed.
- `make eval-e1-gate`: 200 tests passed; Assistant golden 25/25; stateful 7/7;
  recorded RAG recall/MRR/nDCG were 1.0.
- `make test-isolation`: 6/6 passed.
- Focused prompt/context/docgen/subagent suite: 201 passed.
- Real `qwen3.7-plus` Assistant capability: 3/3 result-level trials passed.
- Real Assistant × KB lifecycle: 3/3 trials passed, including grounding,
  follow-up, cross-user denial, and deletion.
- Native Chromium live suite: 24 passed and one conditional Playground skip.
- Frontend lint: 0 errors; type-check and production build passed.
- Promotion/rollback focused suite: 55 passed.
- Security diff scan `b560af4b-fc17-41c4-b7a3-55c7dfba7323`: 72/72 source/config
  receipts, 10/10 reviewed security surfaces, 0 findings, 0 deferred.

Before making new completion claims, rerun checks proportional to the changed
surface. The stable offline gates are:

```bash
make verify-assistant-runtime-dev
make eval-e1-gate
make test-isolation
corepack pnpm@10.33.0 -C web lint
corepack pnpm@10.33.0 -C web type-check
corepack pnpm@10.33.0 -C web build
```

For Docker or live testing, inspect ownership and health first. Keep the
low-memory setting and do not rebuild images for source-only changes:

```bash
export COMPOSE_PARALLEL_LIMIT=1
docker ps
docker compose ps
make status
```

Never print provider keys or generated `.env` values. Real-provider tests use
the configured environment and the default `qwen3.7-plus`; disable failover for
a primary-model baseline.

## Known issues / current breakage

### 2026-08-09 local migration addendum

- The explicitly authorized local per-service migration applied all six current
  SQL files and an immediate rerun skipped all six as already applied.
- Compose now gives Gateway, Assistant, and Knowledge service-owned search
  paths. The migrated local environment sets `GATEWAY_DATABASE_AUTO_INIT=false`
  so Gateway cannot recreate shadow Assistant/Knowledge tables.
- The legacy runner now anchors its ledger in `public.schema_migrations` and
  recognizes schema-split base tables instead of reapplying `schema.sql`.
- The migrated local PostgreSQL run completed the formerly skipped memory test:
  full Assistant result `1943 passed, 0 skipped`. Post-migration real
  `qwen3.7-plus` E2E passed in `188.73s`.
- This is local evidence only; no production migration, deployment, commit, or
  push was performed.

- No reproducible P0/P1 code breakage is known at this checkpoint.
- Node 24 produces an engine warning because the repository declares Node 22;
  the recorded frontend type-check and production build still passed. Use Node
  22 for release reproduction.
- The Codex in-app browser and Chrome extension returned
  `ERR_BLOCKED_BY_CLIENT` before loading local HTTP. The repository-native
  Chromium Playwright suite supplied the browser evidence instead.

## Unverified and out of scope

- The external answer-quality judge was not run; recorded RAG metrics are not a
  substitute for that judge.
- Published multi-architecture release images were not recertified against this
  exact commit after the final source changes.
- Production, external dashboards, production migration, production canary,
  deployment, and rollback execution were not performed.
- No deployment or destructive operation is authorized by this handoff.

## Recommended continuation

The UAO implementation itself is complete. A new session should not invent more
runtime abstraction or reopen every phase. Start by confirming the user's next
scope. If the intent is to close the remaining release-evidence gaps, use this
order:

1. Re-check Git status, Harness strict validation, Compose ownership, service
   readiness, and usable Qwen configuration without revealing secrets.
2. Run the external answer-quality judge in a temporary report directory and
   keep it separate from offline fixtures and real-provider capability receipts.
3. Recertify the published `linux/amd64` and `linux/arm64` image references
   against the open-source quickstart contract; do not rebuild or overwrite the
   published tags silently.
4. Only with explicit user authorization, prepare a production canary and
   rollback exercise. Deployment, migration, image publication, commit, push,
   force operations, and cleanup remain separate authority boundaries.

If the next task is a product feature instead, preserve the current contracts
and use the smallest change plus focused result-level tests. Do not count
"non-empty output" as Assistant or KB capability success.

## Ready-to-paste prompt for the next session

```text
继续 /Users/yang/projects/AI--Platfform 的 Assistant × KB 工作。先完整读取 AGENTS.md、根目录 HANDOFF.md，以及 /Users/yang/study_data/general-assistant-optimization-harness-2026-07-18 的 README.md、loop-state.json、HANDOFF.md、init.sh 和当前 phase。当前 main 基线为 5703e5c5e7ecf1fdf40e3bdd116e73e653cf1250，UAO-00 至 UAO-14 已完成且 strict claim-check 已通过，不要重复重构或把 coding-agent 架构强加给 General Assistant。先核对 git 状态、Docker Compose working_dir、服务健康和证据层级，再按我接下来给出的具体目标做最小修改和结果级验证。不得打印密钥；未明确授权不得 commit、push、deploy、迁移、清理容器/卷或执行其他不可逆操作。
```

## Decision

Handoff ready. The completed UAO baseline is usable; the next session must
receive or confirm a new concrete scope before modifying it.
