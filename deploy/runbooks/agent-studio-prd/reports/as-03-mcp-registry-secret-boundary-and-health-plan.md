# AS-03 MCP Registry, Secret Boundary, and Health Plan

- **Phase:** AS-03 - MCP and Connector Registry, Credential Principals, Secret Boundary, and Health
- **Feature Oracle:** AS-F004 only
- **Status:** fixed Phase-contract execution record; Actor implementation complete, independent Critic pending
- **Date:** 2026-07-18
- **Scope rule:** execute the existing AS-03 contract without replanning, shrinking, or entering AS-04/AS-05 product work

This plan transcribes the already-approved Phase contract into the repository
after the operator instructed the Actor to begin implementation directly. It
does not replace or modify the PRD architecture, dependencies, acceptance gates,
or completion rules.

## Dependency and Runtime Baseline

- AS-02/AS-F003 is `passing` with an approved independent Critic and strict
  completion gate score 100.
- The repository-owned eight-service Compose stack is available. Before every
  mutation, the Gateway and Assistant labels must resolve
  `com.docker.compose.project.working_dir` to this repository.
- The operator's stop line is 3.5 GiB. Docker work uses
  `COMPOSE_PARALLEL_LIMIT=1`, serial service changes, no image build, and
  `docker stats` sampling.
- Provider API keys and production secrets are out of scope. Mock MCP/OAuth and
  explicit offline LLM transport are valid protocol evidence; real third-party
  OAuth remains an explicitly reportable external smoke blocker.

## Requirement-to-Change Map

| Contract | Bounded implementation | Primary evidence |
| --- | --- | --- |
| R1 tenant registry and secret boundary | Add migration 074 and one shared tenant-safe repository for Server, connection, tool/snapshot/diff, channel-grant, and existing Connector-principal resources. Persist only opaque `secret_ref`; redact every public response and validation error. | PostgreSQL migration tests, MCP/Connector API tests, live CRUD/redaction smoke |
| R2 protocol and network security | Permit tenant-configurable Streamable HTTP only; enforce TLS, URL/userinfo, DNS/private-address, rebinding, redirect, Origin, response-stream limits, session/request identity, OAuth resource/audience and PKCE. | `tests/security/test_mcp_security.py`, mock no-auth/bearer/OAuth runtime tests, live fail-closed discovery |
| R3 explicit credential principals | Separate Server definition from service-account or current-user delegated connection; authorize exact tenant/user/channel/scope/audience and deny anonymous/cross-user fallback. Apply the same repository authorization to Confluence Agent runtime. | principal policy and Connector tests, live Connector principal CRUD, AgentLoop/ToolInvoker checks |
| R4 immutable discovery and exact runtime | Store pure JSON schema hash plus immutable risk/read-only contract hash and diffs; validate exact bound tool/hash at publication and invocation; enforce timeout, global connection concurrency, circuit and revocation. | migration-backed discovery/publish tests, runtime tests, exact AS-02 allowlist integration |
| R5 honest degradation | Return stable redacted failures; isolate an unhealthy bound capability; keep native tools available when MCP is disabled; emit request/audit references for mutations and discovery. | rollback test, security tests, live Gateway-to-Assistant failure path, OpenAPI inspection |

## Bounded File Groups

- `database/migrations/074_agent_mcp_registry.sql`
- `packages/ai-gateway-core/.../repositories/mcp_repository.py` and the
  AS-01 Agent Version binding validation hook
- `src/api/{schemas/mcp.py,v1/mcp.py,v1/connectors.py,router.py}` plus the
  redacted validation route and Gateway composition root
- `apps/assistant-service/.../core/mcp`, internal MCP route, composition root,
  ToolInvoker/AgentLoop, and existing Confluence/Connector authorization path
- Phase-owned API, runtime, Connector-principal, security, and PostgreSQL tests
- AS-03 reports, security matrix, Oracle, continuity, source, progress, and
  handoff artifacts

No Skill/Knowledge implementation, Studio UI, publication workflow, Hosted or
Embed channel implementation, Runtime API product surface, new Connector type,
tenant `stdio`, production MCP configuration, deployment, commit, or push is in
this Phase.

## Fixed Execution Sequence

1. Add the additive tenant-safe schema and repository contracts; prove
   idempotence, tenant composite references, immutable snapshots, exact publish
   binding, risk-only drift, revocation, grants, and circuit state in PostgreSQL.
2. Add closed Gateway schemas/routes with RBAC, redaction, mutation audit
   references, compatibility routes, feature flag, and Assistant-signed
   discovery proxy.
3. Implement fail-closed Streamable HTTP and OAuth clients plus Secret resolver,
   discovery, invocation, timeout/concurrency/circuit behavior.
4. Feed only exact MCP/Connector bindings through the passed AS-02 resolver and
   ToolInvoker; preserve the legacy built-in Assistant when no Agent allowlist
   is present.
5. Run the four required Phase commands, supplemental migration tests, real
   container/PostgreSQL/API checks, and explicit offline transport isolation.
6. Freeze Actor evidence and request a fresh independent Critic. Keep AS-F004
   non-passing until that approval and the strict AS-03 completion gate.

## Required Validation Gates

| Gate | Exact Phase command | Required outcome |
| --- | --- | --- |
| MCP API/runtime | `uv run pytest -q --no-cov tests/api/test_mcp_registry_api.py && uv run --package assistant-service pytest -q --no-cov tests/services/assistant/test_mcp_runtime.py tests/services/assistant/tools/test_mcp_capability_policy.py tests/services/assistant/test_connector_credential_principal.py` | Tenant CRUD/redaction, protocol modes, principals, exact binding, disable/revoke, timeout/concurrency/circuit pass. |
| MCP security | `uv run pytest -q --no-cov tests/security/test_mcp_security.py` | SSRF/DNS/redirect/Origin/audience/PKCE/size/session/leakage cases fail closed. |
| Lint | The exact Ruff command in the AS-03 Phase file | Ruff exits zero on the named implementation and test paths. |
| Assistant regression | `make test-isolation && make verify-assistant-runtime-dev` | Existing Gateway/Assistant contract and AHR groups pass; any initial skip/failure is not completion evidence. |

Supplemental evidence is the real PostgreSQL migration test, live Compose
ownership/health/memory, live registry and Connector CRUD, fail-closed signed
discovery, OpenAPI secret/audit inspection, and the independent Critic.

## Rollback and Stop Conditions

- `AGENT_STUDIO_MCP_ENABLED=false` disables MCP registry/runtime adapters while
  preserving native Assistant tools and stored audit/schema history.
- Revoke connections/principals and invalidate runtime state; do not delete
  immutable snapshots or audit rows.
- Stop on plaintext secret persistence in the Agent Studio path, tenant `stdio`,
  fail-open SSRF/OAuth, implicit/cross-user principal fallback, mutable published
  schemas, unbound invocation, wrong Compose ownership, memory above 3.5 GiB,
  or a required downstream Phase change.
