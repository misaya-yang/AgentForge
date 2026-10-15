# AS-00 Independent Critic Verdict

**Phase:** AS-00 - Baseline Runtime and Capability Contract  
**Feature:** AS-F001  
**Critic:** independent fresh context reviewer (`/root/as00_runtime_critic`)  
**Critic Verdict:** approved  
**Actor Report:** `docs/agent-studio-prd/reports/as-00-baseline-runtime-and-capability-contract-report.md`  
**Date:** 2026-07-18

## Inputs Reviewed

- Phase contract: `docs/agent-studio-prd/phase-00-baseline-runtime-and-capability-contract.md`
- Oracle item: AS-F001 only, selected with `jq`
- Actor report: `docs/agent-studio-prd/reports/as-00-baseline-runtime-and-capability-contract-report.md`
- Diff/changed files: `git status --short --branch --untracked-files=all`, tracked `git diff`, and `git diff --no-index -- /dev/null` for each untracked test
- Validation evidence: all required commands were rerun independently in this Critic session; no Actor test result was accepted without rerun
- Browser/runtime/eval evidence: browser validation is not applicable because no UI or route shape changed; local Gateway/Assistant health, isolation gate, Docker ownership/status, and Docker resource samples were independently checked
- Security/migration/rollback evidence: implementation diff, Actor report security and rollback sections, allowlist/policy/cache tests, and adjacent MCP/tool-orchestrator regressions

## Findings

| ID | Severity | Requirement/gate | Finding | Required correction |
| --- | --- | --- | --- | --- |
| C-01 | low | Runtime evidence boundary | The live gate used the already-running localhost OpenAI-compatible stub. It proves the real local auth, Gateway proxy, Assistant non-stream/SSE, and OpenAPI/isolation contracts, but it does not prove real DashScope/Qwen availability or answer quality. The Actor report states this boundary accurately and does not inflate it into provider evidence. | None for AS-00. Preserve this limitation until a later real-provider/release gate supplies provider evidence. |
| C-02 | low | Harness unlock discipline | AS-F001 was still `failing` when reviewed and the Actor report remained `partial`, intentionally awaiting this independent verdict. Critic approval alone is not the AS-00 completion gate and is not permission to claim the whole demand complete. | None in the implementation. Update AS-F001 and run the strict AS-00 completion gate through the normal Actor/harness transition before unlocking AS-01. |

No critical, high, or medium finding was identified.

## Requirement Coverage

- **R1 / AS-F001 step 1 - branch-accurate baseline:** the Critic independently resolved both `HEAD` and local `origin/main` to `945eb2225d644093802bf5f9d75ca4d9dbad6a8d`; `HEAD..origin/main` had no commits or changed paths. The post-validation dirty scope remained exactly two modified runtime files and two untracked Phase-named tests.
- **R2 / AS-F001 steps 2 and 4 - honest capability inventory:** the Actor matrix separately classifies platform-native, model-native, MCP, Skill, Connector, and Knowledge capability families, names their registration/management/filter/health boundaries, and does not equate all tools with MCP. It records MCP as not production-initialized, Skills as partially wired, Connector visibility as tenant-conditional, and Knowledge/provider reachability without claiming unexecuted external services.
- **R3 / AS-F001 step 3 - non-expanding allowlist:** the diff establishes immutable `CapabilityAllowlist` semantics. `None` preserves the legacy set; an explicit empty object exposes and invokes nothing; a subset can only reduce definitions and calls. In `AgentLoop._get_streaming_tools`, the allowlist is reapplied after Connector visibility merging and immediately before schema hashing and `select_tools`. In `RegistryToolInvoker.invoke`, denial occurs before cache lookup, tenant/MCP authorization occurs before any cached result is returned, and execution receives the same propagated allowlist.
- **R4 - compatibility and handoff:** deterministic legacy-`None`, empty, subset, Connector, invocation, policy-failure, and cache-denial tests passed. The existing isolation/OpenAPI suite and 88 adjacent MCP/tool/orchestrator/streaming regressions passed. MCP/Connector and Skill/Knowledge wiring limitations remain explicitly assigned to AS-03 and AS-04, while the future resolver consumer is assigned to AS-02.

All four AS-F001 steps have substantive evidence in the Actor report and independent command results. The remaining Oracle status transition and completion-gate run are harness bookkeeping after this verdict, not missing implementation evidence.

## Test and Regression Assessment

The Critic independently ran the required commands from `/Users/yang/projects/AI--Platfform`:

| Check | Result |
| --- | --- |
| `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_agent_capability_allowlist.py tests/integration/test_assistant_capability_wiring.py` | Exit 0; **13 passed**, one third-party deprecation warning, no skips |
| `uv run ruff check apps/assistant-service/src/assistant_service/core/agent/agent_loop.py apps/assistant-service/src/assistant_service/core/tool_invoker.py tests/services/assistant/test_agent_capability_allowlist.py tests/integration/test_assistant_capability_wiring.py` | Exit 0; `All checks passed!` |
| `git diff --check` | Exit 0; no output |
| `uv run --package assistant-service pytest -q --no-cov tests/services/assistant/tools/test_mcp_capability_policy.py tests/services/assistant/tools/test_tool_runtime_safety.py tests/services/assistant/test_tool_orchestrator.py tests/services/assistant/test_agentloop_streaming_first_contract.py` | Exit 0; **88 passed**, one third-party deprecation warning, no skips |
| prescribed `load_env` + localhost `make test-isolation` live gate | Exit 0; **6 passed**, one third-party deprecation warning, no skips |

The live gate exercised both live Gateway-to-Assistant cases as well as isolation, OpenAPI, core isolation, and Gateway boot contracts. Before and after the gate, `http://127.0.0.1:8080/health` returned healthy and `http://127.0.0.1:8093/health/ready` returned ready with startup, drain, database, and KB proxy checks healthy/configured.

Read-only runtime inspection found only `ai-gateway-postgres` and `ai-gateway-redis`; both had an empty `com.docker.compose.project.working_dir` label and `docker compose ps --format json` returned no Compose services. The designated owner checkout did not exist on this host. The post-gate Docker sample was approximately 51.73 MiB for PostgreSQL and 10 MiB for Redis. This supports the Actor's stated host-process/dev-runtime deviation without turning it into Compose or production evidence.

The regression scope is sufficient for AS-00: legacy built-in behavior, filtered selection, invocation, cached Knowledge denial, Connector non-expansion, tenant/MCP failure behavior, tool orchestration, streaming-first contracts, auth/proxy, and API isolation were all exercised. There were no failures or skips, so the Phase rule forbidding approval on a failed or skipped required command is satisfied.

## Security, Privacy, and Failure Assessment

- Allowlist enforcement is fail-closed at both visibility and invocation boundaries. A Connector admitted by its own tenant predicate is filtered again before model selection.
- Invocation checks the allowlist before cache lookup. Tenant policy and MCP authorization are evaluated before returning a cached result, preventing a stale cached Knowledge/tool result from bypassing a newly unavailable policy.
- Tenant policy listing exceptions hide all tools, and invocation exceptions return a denied result. Existing MCP configuration exceptions continue to hide or deny MCP tools.
- `None` compatibility is explicit rather than inferred; it does not construct an empty policy accidentally. Explicit empty and subset behavior are both tested.
- No public Agent CRUD shape, database schema, UI, deployment configuration, or provider configuration is changed. No migration is involved.
- The Critic did not inspect, print, or modify any API Key value. The prescribed gate referenced local credential variable names without exposing their values. The localhost stub boundary means no real provider call is claimed.

## Minimal-Change Assessment

The Git-visible implementation scope is limited to the four Phase allowlisted paths:

- `apps/assistant-service/src/assistant_service/core/agent/agent_loop.py`
- `apps/assistant-service/src/assistant_service/core/tool_invoker.py`
- `tests/services/assistant/test_agent_capability_allowlist.py`
- `tests/integration/test_assistant_capability_wiring.py`

The tracked source diff is 132 insertions and 36 deletions across two files. The larger `tool_invoker.py` change is directly explained by the required enforcement order: introducing the typed value, propagating visibility semantics, moving cache return behind authorization, and making tenant-policy uncertainty deny. The two new tests are focused contract coverage rather than production refactoring. No unrelated source, schema, UI, dependency, Docker, or deployment change appeared in the dirty scope. The change is minimal for the stated R3 and compliance gates.

## Rollback and Handoff Assessment

The Actor rollback is concrete: remove the typed allowlist, its AgentLoop propagation/post-Connector filter, invocation/list filtering and policy/cache ordering hardening, then remove the two focused tests while retaining the read-only inventory. Local runtime cleanup is separately described and does not require deleting images or data.

The downstream contract is sufficiently durable for AS-01/AS-02: AS-02 must derive the internal allowlist from a signed effective capability set rather than a client-supplied list; AS-03 owns MCP/Connector lifecycle gaps; AS-04 owns Skill/Knowledge binding gaps. AS-01 may unlock only after AS-F001 is transitioned according to this verdict and the strict AS-00 completion gate passes.

## Whole-Demand Regression Assessment

AS-00 is an early baseline Phase, not the terminal AS-09 release gate. This review covers AS-F001 and inherited Assistant/tool regressions only. Same-build aggregate Oracle regression, real-provider/release evidence where required, and the terminal whole-demand completion gate remain pending for AS-09 and must not be inferred from this approval.

## Verdict Rationale

**Approved.** The actual diff places the allowlist at every required non-expansion boundary: after Connector merge and before relevance selection, again before invocation, and before cached results can escape policy checks. Independent tests prove `None`, explicit empty, subset, Connector, tenant failure, cache, and legacy Assistant semantics. All five required command groups passed with no skips, the live services remained healthy, and the worktree contains no unrelated implementation changes. The Actor report also keeps the localhost stub, non-Compose dev runtime, and unfinished whole-demand evidence within honest boundaries. The two low-severity findings are explicit downstream evidence/bookkeeping constraints, not AS-00 defects.
