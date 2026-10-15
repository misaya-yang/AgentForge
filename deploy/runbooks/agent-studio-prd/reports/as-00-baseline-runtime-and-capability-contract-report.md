# AS-00 Baseline Runtime and Capability Contract Report

- **Phase:** AS-00 - Baseline Runtime and Capability Contract
- **Feature:** AS-F001
- **Status:** passed
- **Date:** 2026-07-18
- **Plan:** `docs/agent-studio-prd/reports/as-00-baseline-runtime-and-capability-contract-plan.md`
- **Independent Critic:** approved in `docs/agent-studio-prd/reports/as-00-critic-verdict.md`

## Outcome

The bounded allowlist seam and deterministic AS-F001 tests are implemented. `CapabilityAllowlist` is an immutable internal runtime value: `None` preserves the current built-in Assistant tool surface, while an explicit object, including an empty one, is a hard upper bound. It is applied by `RegistryToolInvoker`, reapplied after Connector visibility merging and before `select_tools`, and checked again before invocation or cached-result return. Tenant tool-policy uncertainty now hides/denies tools rather than failing open.

The user then explicitly authorized local dev-runtime and password/container changes while excluding real API Key changes. A memory-bounded runtime was brought up from the current checkout: Docker ran only PostgreSQL and Redis, while Gateway and Assistant ran as host processes against a temporary localhost OpenAI-compatible model stub. The first live gate run exposed a missing local admin (`2 failed, 4 passed`); the admin was created through the repository `DatabaseStorage` interface, and the repeated required command completed with `6 passed` and no skips. Docker RSS was about 79 MiB and the Gateway/Assistant process tree about 200 MiB at the Actor evidence sample. The fresh independent Critic then reran every required group and approved AS-F001 with `13 passed`, `88 passed`, `6 passed`, Ruff and diff hygiene all successful and no required skips. The orchestrator subsequently ran the strict AS-00 completion gate, which passed with quality score 100. AS-00 is verified and AS-01 is unlocked; this is not a claim about AS-F002 through AS-F010 or the terminal whole-demand gate.

## Plan followed

1. Revalidated the current branch and locally recorded `origin/main` without fetching, syncing, switching, stashing, or resetting.
2. Traced the specified Assistant/Gateway composition roots, runtime filters, management routes, and the selected AS-F001 item.
3. Added the smallest internal allowlist boundary in `agent_loop.py` and `tool_invoker.py`; no public request/API/schema was changed.
4. Added the two Phase-named test files and ran required plus adjacent regression checks.
5. Resumed only after explicit runtime authorization, used the smallest local runtime that could exercise the real Gateway-to-Assistant HTTP path, and refreshed Actor evidence without treating the earlier skips or the first 401 run as passes. A fresh independent Critic reran the final commands and approved the Phase evidence; completion-gate output remains a separate orchestrator row, not actor self-approval.

## Branch baseline

The Phase `branch-baseline` command established:

- `HEAD`: `945eb2225d644093802bf5f9d75ca4d9dbad6a8d` (`main`)
- locally recorded `origin/main` and `origin/HEAD`: the same commit
- `git log HEAD..origin/main`: empty
- `git diff --name-only HEAD..origin/main`: empty
- pre-edit worktree: clean

This is a comparison with the existing local remote-tracking ref; no network fetch was run because the Phase permits read-only `git status/log/diff` and requires approval for branch-changing sync actions.

## Capability matrix

| Source type | Production registration / execution point | Reachable management API | Tenant / caller filter | Setup and health state | Verified reachability on this checkout |
| --- | --- | --- | --- | --- | --- |
| Platform-native tools | Assistant `main.py` calls `register_builtin_tools`, document/PPTX/image, task-list/context and optional primitive registrations into `ToolRegistry`; `RegistryToolInvoker` executes them | Gateway `GET /api/v1/assistant/tools` proxies Assistant `GET /api/v1/assistant/tools` | Tool permission checks plus the AS-00 allowlist; optional `TenantToolPolicyService` exists but is not injected by the current composition root | Registered during Assistant startup; no uniform per-tool health contract | Yes at code/test level. Legacy `None`, explicit subset, and explicit empty behavior passed. The live Gateway and Assistant health checks were ready and the black-box chat contract passed; no full-container or production claim is made. |
| Model-native search | `ModelRegistry.NATIVE_SEARCH_CAPABLE` derives Qwen `enable_search`, Gemini `google_search`, and Anthropic `web_search_20250305`; provider request builders apply it | `GET /api/v1/assistant/models` lists models but currently omits `supports_native_search`/native-search config | Model access tier and request search preference; the AS-00 tool-name allowlist does not represent provider-native search | Depends on configured provider/model; no credential or provider call was used in AS-00 | Reachable code path and Qwen classification test passed; live provider behavior is not claimed. Agent/channel intersection remains AS-02. |
| MCP | `core/mcp/manager.py` can register `ToolCategory.MCP` definitions, but Assistant `main.py` does not initialize an MCP manager; Gateway sets `app.state.mcp_manager = None` | `GET /api/v1/assistant/mcp/servers`, `/tools`, and refresh routes exist but manager-dependent calls return 503 when state is `None` | `TenantMCPConfigService`; `create_tool_invoker()` defaults MCP to deny when no configured policy is injected | Not initialized in the production composition root | Not currently reachable as a production MCP runtime. Static config/code is not treated as runtime proof. Owner: AS-03. |
| Skills | When enabled, AgentLoop loads DB metadata and `SkillToolBridge` registers `skill_*` tools in the global registry | Gateway `/api/v1/skills` upload/list/detail/update/delete/test/enable/disable routes | DB load accepts tenant/user, but the registry/cache is process-global and exact immutable version content is not loaded | Lifecycle flags exist; no complete runtime health/version binding | Partially wired. Current AgentLoop obtains the tool snapshot before bridge registration, so a newly loaded Skill cannot enter the same-turn selection set and may appear only on a later request. Owner: AS-04. |
| Connectors | Assistant `main.py` registers DB-backed Confluence definitions/executors and `ConnectorRegistry` adds them per request only after its tenant predicate succeeds | Gateway `/api/v1/connectors/*` and `/api/v1/confluence/*` | Active-connection predicate by tenant; executor resolves per-call tenant credentials; AS-00 allowlist is reapplied after Connector merge | Conditional on DB and active connection; predicate failure hides tools | Reachable composition path is verified statically and post-merge non-expansion passed. No live Connector credentials were used. Dual Connector/MCP product path remains AS-03. |
| Knowledge | Assistant creates `KBProxyClient`; `register_builtin_tools` registers `search_knowledge_base`; invocation injects configured Dataset IDs | Gateway `/api/v1/knowledge/{path}` proxies Knowledge Service; Assistant `/datasets` exposes visible datasets | User context plus Dataset IDs/Knowledge Service ACL; AS-00 allowlist can remove the tool | Proxy construction is present; live Knowledge Service health was not exercised | Runtime code path and allowlist/cache denial passed. Exact Agent Version binding, runtime revalidation and revision provenance remain AS-04. |

The matrix confirms the architecture is mixed. MCP is one source, not a synonym for every capability.

## Stable runtime boundary produced for downstream phases

- Type: `assistant_service.core.tool_invoker.CapabilityAllowlist` with immutable `tool_names`.
- Compatibility: `AgentLoopConfig.capability_allowlist is None` and `ToolInvocationContext.capability_allowlist is None` preserve the legacy set.
- Explicit-empty semantics: `CapabilityAllowlist()` exposes and invokes no tools.
- Selection order: registry/user filtering -> tenant/MCP filtering -> Connector visibility merge -> allowlist recheck -> relevance selection.
- Invocation order: cancellation -> allowlist denial -> tenant/MCP authorization -> cached-result return -> rate/execution path.
- Failure semantics: an injected tenant policy read/filter exception hides or denies tools; it cannot return a previously cached Knowledge result.
- AS-02 consumer: the Gateway Capability Resolver must construct this allowlist from the signed effective capability set; clients must never supply it directly.

## Files changed

### Runtime and tests

- `apps/assistant-service/src/assistant_service/core/tool_invoker.py`
- `apps/assistant-service/src/assistant_service/core/agent/agent_loop.py`
- `tests/services/assistant/test_agent_capability_allowlist.py`
- `tests/integration/test_assistant_capability_wiring.py`

### AS-00 evidence and continuity

- `docs/agent-studio-prd/reports/as-00-baseline-runtime-and-capability-contract-plan.md`
- `docs/agent-studio-prd/reports/as-00-baseline-runtime-and-capability-contract-report.md`
- targeted AS-00 rows/facts in `source-packet.md`, `continuity-ledger.md`, `progress-log.md`, `agent-handoff.md`, `next-window-prompt.md`, `loop-state.json`, and `feature-oracle.json`

The root `docs/*` ignore rule still makes this Harness local/ignored. This report does not claim the evidence is Git-tracked or committed.

### Local runtime artifacts and mutations

- Ignored `.env` created by `scripts/new/init-env.sh` with generated local infrastructure secrets; values were not printed.
- Local PostgreSQL and Redis containers created by the repository dev workflow; both have no Compose project/working-dir label and no pre-existing Compose containers were mutated.
- Local database schema initialized by Gateway and one `admin@example.com` dev user created for the black-box gate.
- Temporary localhost model stub plus host Gateway/Assistant processes; no real provider API Key was read, persisted, or changed.
- Qdrant image pull was cancelled before a Qdrant container started because `kb_mode=off` makes it unnecessary for this Phase.

## Validation evidence

| Check | Exact command | Result | Gate status |
| --- | --- | --- | --- |
| Branch baseline | `git status --short --branch && git log --oneline --decorate HEAD..origin/main --max-count=30 && git diff --name-only HEAD..origin/main` | Same local commit/ref, no commit/path delta; clean before edits | passed |
| Capability tests | `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agent_capability_allowlist.py tests/integration/test_assistant_capability_wiring.py` | `13 passed`, one third-party deprecation warning | passed |
| Assistant isolation, initial authorized run | `make test-isolation` with local credentials/runtime | `2 failed, 4 passed`; both live tests reached Gateway but login returned 401 because the fresh database had no local admin | corrected, not counted as pass |
| Assistant isolation, final authorized run | `make test-isolation` with local credentials/runtime | Exit 0; `6 passed`, one third-party deprecation warning; both live Gateway-to-Assistant cases ran without skips | passed |
| Required lint | `uv run ruff check apps/assistant-service/src/assistant_service/core/agent/agent_loop.py apps/assistant-service/src/assistant_service/core/tool_invoker.py tests/services/assistant/test_agent_capability_allowlist.py tests/integration/test_assistant_capability_wiring.py` | `All checks passed!` | passed |
| Adjacent tool/streaming regression | `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/tools/test_mcp_capability_policy.py tests/services/assistant/tools/test_tool_runtime_safety.py tests/services/assistant/test_tool_orchestrator.py tests/services/assistant/test_agentloop_streaming_first_contract.py` | `88 passed`, one third-party deprecation warning | supplemental passed |
| Diff hygiene | `git diff --check` | Exit 0 | passed |
| Harness structure | `python3 /Users/yang/.codex/skills/prd-phase-harness/scripts/validate_harness_prd.py docs/agent-studio-prd --strict --quality-score` | Harness validation passed; quality score 100 | passed for structure only |
| Independent Critic | Fresh-context review in `docs/agent-studio-prd/reports/as-00-critic-verdict.md` | Approved after independent `13 passed`, `88 passed`, live `6 passed`, Ruff and diff checks; no required skip | passed |
| AS-00 completion gate | `python3 /Users/yang/.codex/skills/prd-phase-harness/scripts/validate_harness_prd.py docs/agent-studio-prd --strict --completion-gate --phase AS-00 --quality-score` | Exit 0; `Harness validation passed`; quality score `100 (excellent)`. The earlier pre-runtime run correctly failed with score 49 and is not counted as a pass. | passed |
| Browser | Not applicable: no UI or route shape changed; OpenAPI superset check passed inside `make test-isolation` | no screenshots required by AS-00 | not applicable |
| Live provider / AI eval | Temporary localhost OpenAI-compatible endpoint returned deterministic Qwen-shaped responses while the real Gateway/Assistant HTTP, auth, model-selection and SSE paths executed | transport/contract evidence only; no real Qwen availability or model-quality claim | passed for AS-00 scope |

## Regression and security assessment

- Built-in Assistant compatibility is covered deterministically by `None` visibility/invocation tests and the existing 28-test streaming-first suite.
- Empty and subset allowlists deny omitted tools before selection, invocation, and cached result reuse.
- Connector-visible definitions are filtered again after the conditional merge.
- Tenant-policy read/filter failure is fail-closed. MCP missing/error policy remains fail-closed.
- Tool approval/audit/orchestrator behavior is covered by the adjacent 88-test regression set.
- The user explicitly authorized local dev container/database/password changes. Local infrastructure secrets were generated by the repository initializer and never printed; no real API Key was read or changed. No deployment or production state was touched.

## Feature Oracle Updates

- AS-F001 is transitioned from `failing` to `passing` only after the fresh Critic independently reran and approved every required command with no skips.
- Evidence remains split between this Actor report and the separate Critic verdict; neither the localhost model stub nor this Phase is represented as real-provider or whole-demand release proof.
- AS-F002 through AS-F010 remain `failing`; this AS-00 report does not claim any downstream feature complete.

## Minimal Change Scope and Rollback

The implementation is limited to the two Phase-listed runtime files plus the two exact required test files. No public API, committed database schema, Agent CRUD, UI, provider configuration, or deployment file changed. Documentation changes are evidence-only; ignored `.env`, local schema/user rows and temporary runtime processes are validation artifacts. Rollback is to remove `CapabilityAllowlist`, its two propagation/filter sites, the policy/cache ordering hardening, and the two tests; the branch/capability inventory remains valid read-only evidence. Local runtime cleanup is to stop the two host services/stub and the repository-owned PostgreSQL/Redis dev containers without deleting images or data.

## Blockers, deviations, and handoff

- **Runtime deviation:** the AGENTS.md-designated Compose owner path `/Users/misaya.yanghejazfs.com.au/misaya_project/AI--Platfform` does not exist on this host. Read-only inspection found no containers. To avoid creating wrong-owner Compose application containers or building images, the repository-local dev workflow ran only PostgreSQL/Redis without Compose labels and the current checkout ran Gateway/Assistant on the host. This still exercised the production HTTP boundary named by the test while preserving the ownership guard.
- **Provider boundary:** the shell and generated `.env` had no user-supplied provider key. The gate used a temporary localhost stub and therefore proves API/auth/proxy/SSE compatibility, not real Qwen readiness. No API Key waiver or modification occurred.
- **Fresh-database warning:** Gateway startup reports missing eval trace/outbox tables in background workers after applying its normal auto-init set. Those warnings did not affect the AS-00 request path and are not relabeled as passed functionality; AS-09 whole-demand regression remains responsible for aggregate release health.
- **AS-01 unlock:** the strict AS-00 completion gate exited zero with quality score 100 after the Actor report, passing Oracle, and approved independent Critic were durable.
- **Next orchestrator action:** stop the temporary low-memory runtime without deleting images/data, then cold-start AS-01 from its context profile, loop state, and Phase contract.
