# AS-03 MCP Registry, Secret Boundary, and Health Actor Report

**Phase:** AS-03 - MCP and Connector Registry, Credential Principals, Secret Boundary, and Health  
**Feature:** AS-F004  
**Status:** passed — all Actor gates, amended iteration-3 independent Critic, and supported phase claim check passed  
**Date:** 2026-07-18  
**Actor:** primary implementation agent

## Summary

AS-03 now provides a tenant-scoped remote MCP registry and a shared explicit
credential-principal boundary for MCP and the existing Confluence Connector.
Server definitions, credential connections, discovered tools, immutable
snapshots/diffs, public-channel grants, health/circuit state, and Connector
principals are separate resources. PostgreSQL, public schemas, API responses,
validation errors, and logs carry only opaque Secret Store references or
redacted state; the new Agent Studio path never persists or returns credential
values.

Tenant-configurable MCP is Streamable HTTP only. The Assistant client enforces
TLS, URL/userinfo, DNS/address classification, rebinding, redirect, Origin,
OAuth resource/audience and PKCE, response streaming limits, JSON-RPC request
identity, protocol/session pinning, timeouts and stable errors. Discovery stores
a pure JSON schema hash and a separate immutable contract hash. Remote catalog
risk and `readOnlyHint` metadata cannot lower the platform classification.
Schema compatibility is checked recursively and unknown validation changes
fail closed as breaking. Exact Version bindings are revalidated inside the
AS-01 publish transaction and again at runtime.

After the iteration-1 Critic rejection, DNS pinning now reaches the actual
transport: MCP and OAuth requests use a validated IP literal while retaining
the original HTTP Host and TLS SNI. Public/embed read-only eligibility is now an
explicit Tenant Admin assertion bound to the current exact schema hash; catalog
refresh invalidates that grant until the Admin reviews and renews it. Legacy
grant rows without an approved hash fail closed.

For Agent execution, MCP and Confluence Connector definitions are exposed and
invoked only after the AS-02 immutable capability allowlist and the current
tenant/caller/channel principal check. Delegated grants never fall back to an
admin or another user. Public/embed use requires an exact-schema Admin-approved
read-only service-account tool grant. The legacy built-in Assistant retains its existing
non-Agent behavior only when no Agent capability allowlist exists; it cannot
expand an Agent run.

All Phase-owned local gates pass on the revised Actor source. Tenant MCP create
and update now reject non-global IP literals in the closed schema and resolve
network target hostnames before persistence; DNS failure or any disallowed
record fails closed without returning the submitted target. Runtime repeats the
policy and pins the actual connection, so later DNS changes remain blocked. The repository-owned
Compose stack was hot-updated without an image build, migration 074 was applied
to the real local PostgreSQL instance, live MCP/Connector CRUD and signed
Gateway-to-Assistant degradation were exercised, and all eight services remain
healthy. The iteration-3 independent Critic rechecked C-04 on the final source,
reran every required gate, and approved this frozen slice.

## Plan and Scope

- Fixed execution artifact:
  `docs/agent-studio-prd/reports/as-03-mcp-registry-secret-boundary-and-health-plan.md`
- No architecture/scope deviation: the Actor did not implement AS-04
  Skill/Knowledge, AS-05 UI, AS-06 publishing UX, Hosted/Embed/Runtime API,
  additional Connector types, production MCP configuration, or deployment.
- The existing Gateway Connector API still supports the built-in Assistant for
  backward compatibility. Agent runs use only the new secret-ref principal
  repository and exact ToolInvoker authorization path. Raw third-party error
  bodies were removed from the legacy Connector logs, database error text, and
  client errors while preserving its public success contract.

## Main Change Groups

| File/group | Result |
| --- | --- |
| `database/migrations/074_agent_mcp_registry.sql` | Seven additive tenant-scoped resource tables, composite references, secret-ref checks, immutable snapshot trigger, exact-schema channel approval, health/circuit state and Connector principals |
| `.../repositories/mcp_repository.py` | Tenant-safe CRUD, redaction, untrusted catalog normalization, recursive conservative diff, exact publish/runtime/Admin-grant authorization, principal/channel/scope/audience rules, circuit state and AS-02 resolver adapter |
| `src/api/schemas/mcp.py`, `src/api/v1/mcp.py`, `src/api/v1/connectors.py` | Closed Streamable HTTP/principal schemas, RBAC, redacted 422s, mutation/discovery audit references, signed discovery proxy, legacy route compatibility and Confluence principal surface |
| `apps/assistant-service/.../core/mcp` | Connection-effective DNS-pinned Streamable HTTP client, OAuth PKCE coordinator, operator-allowlisted Secret resolver, untrusted discovery and dynamic invocation with whole-call timeout/concurrency/circuit controls |
| `tool_invoker.py`, `agent_loop.py`, `confluence_tool.py`, composition roots | Exact AS-02 allowlist integration, current-principal Connector authorization, immediate revoke visibility, native-only rollback compatibility |
| Phase-owned tests | API, protocol, security, principals, exact binding, database isolation/immutability and regression evidence |

## Requirement Results

| Requirement | Actor result | Evidence |
| --- | --- | --- |
| R1 tenant registry/secret boundary | passed | Migration tests, API redaction/OpenAPI tests, live MCP and Connector CRUD; no token/client-secret columns in migration 074 |
| R2 protocol/network security | passed | 19 security tests include transport-observed validated-IP targets with original Host/SNI for MCP and OAuth, plus SSRF, rebinding, redirect, Origin, PKCE, streamed limits, request/session identity and leakage |
| R3 credential principal/Connector boundary | passed | Remote read-only/risk hints remain untrusted; public grants require an Admin-approved exact schema hash, service-account principal and channel; delegated/no-fallback, scope, audience, revoke and Connector parity tests pass |
| R4 immutable discovery/runtime | passed | PostgreSQL proves immutable snapshots, untrusted classification normalization, exact Version binding and schema-drift grant invalidation; recursive diff and exact-tool, whole-call timeout, global concurrency and circuit tests pass |
| R5 honest degradation | passed | stable redacted errors, signed live discovery failure, `AGENT_STUDIO_MCP_ENABLED` native-only rollback, mutation/discovery audit references |

## Required and Supplemental Validation

| Gate | Final result |
| --- | --- |
| MCP API/runtime exact command | exit 0: API `6 passed`; Assistant runtime/policy/Connector `23 passed`; no skips |
| MCP security exact command | exit 0: `19 passed`; no skips |
| AS-03 Ruff command | exit 0: `All checks passed!`; the adjacent Connector/internal route paths also pass Ruff |
| Assistant regression | final live-isolation run `6 passed`, no skips, using an explicit offline stub with `qwen3.7-plus`; AHR-01/02/03/04 `28/77/8/98` plus golden gate all pass |
| PostgreSQL migration contract | exit 0: `3 passed`; migration is idempotent in isolated PostgreSQL and covers immutable discovery, untrusted classification, exact publish, cross-tenant denial, exact-hash Admin grants, drift invalidation, revoke and circuit behavior |
| Real local migration | migration 074 transaction committed successfully; all seven expected tables and nullable fail-closed `approved_schema_hash` exist in the repository-owned PostgreSQL container |
| Live registry | authenticated create/list/update/disable/revoke/delete passed; validation marker was absent from the 422 body; request and audit references were present |
| Live Connector principal | secret-ref create/list/revoke passed; the opaque reference was absent from all responses |
| Live signed discovery degradation | Gateway-to-Assistant call returned a stable `MCP_SSRF_BLOCKED`/502 for a restricted `.invalid` resolution and did not expose hostname, address, upstream body or credential data |
| Final Compose state | 8/8 healthy; every ownership label points to this repository; Gateway/Assistant are `stub=false`; hot-source hashes match; sampled total memory about 733 MiB, below 3.5 GiB |
| Phase claim check | exit 0; legacy diagnostic compatibility and claim metadata passed with structure score `100/100`. The tool explicitly does not execute cited evidence; required commands above were run separately by Actor and Critic. |

The first final-source isolation invocation lacked credential environment
variables and reported two skips; it is not counted as full evidence. Two
subsequent explicit-stub attempts failed because `stub-model` was not a real
catalog ID and because the provider-filtered model list was empty; neither is
represented as passing. The final explicit `qwen3.7-plus` transport run passed
all six cases, made no provider call, and both containers were then recreated
back to `stub=false` with the final source hot-updated again.

## Security, Secret, and External-Service Boundary

- Durable matrix: `reports/agent-studio/as-03-mcp-security-matrix.json`.
- No API key, bearer token, OAuth code/token, Secret Store value, database
  password, JWT, or shared signing secret was printed or written to a report.
- Secret resolution accepts only operator-allowlisted reference-to-environment
  mappings; tenant input cannot name an arbitrary process environment variable.
- DNS validation is connection-effective: the request URL presented to the
  transport contains the chosen validated IP, while Host and TLS SNI retain the
  reviewed hostname. OAuth uses a non-reused request connection across distinct
  authorities.
- Remote MCP annotations cannot persist a low-risk or read-only classification.
  Anonymous service-account execution requires a Tenant Admin's exact-schema
  approval, and any current-snapshot hash change invalidates it.
- Remote catalog descriptions are length/control-character bounded and
  secret-shaped or instruction-shaped text is neutralized before model exposure.
- Real third-party MCP/OAuth success was not run because no approved client
  credential or production Secret Store writer is configured. This is the
  Phase's explicit external-smoke waiver; local OAuth/no-auth/bearer protocol
  evidence passes. No production/provider availability claim is made.

## Rollback and Residual Risk

- `AGENT_STUDIO_MCP_ENABLED=false` is tested to remove MCP/Connector Agent
  capabilities while preserving native tools and stored registry/audit rows.
- Connection/principal revocation immediately removes runtime eligibility;
  immutable discovery snapshots and audit history remain for recovery.
- Production Secret Store implementation, egress allowlist operations, issuer
  allowlist, real third-party OAuth lifecycle and provider availability remain
  deployment inputs, not mocked production evidence.
- The final source is Critic-approved, AS-F004 cites the Actor/Critic/matrix
  artifacts, and the supported phase claim check exited zero. The currently
  installed validator no longer supports legacy `--strict` certification and
  requests a whole-Harness v3 migration; that tool migration was not used to
  replace or weaken any required product, security, database or live gate.

## Iteration-1 Critic Remediation

The first fresh Critic returned `changes_requested`; its preserved verdict is
`docs/agent-studio-prd/reports/as-03-critic-verdict-iteration-1.md`.

| Finding | Remediation and executed evidence |
| --- | --- |
| C-01 actual DNS pin absent | MCP and OAuth now build the network request against a validated IP literal and set original Host plus `sni_hostname`; transport-observation tests reject hostname targets and pass for both authorities. |
| C-02 remote read-only trust | Discovery and repository persistence force tenant MCP catalog tools to medium risk/not-read-only. A public grant records the current `approved_schema_hash`; legacy null grants and refreshed hashes deny until an Admin renews the exact tool/channel approval. |
| C-03 shallow diff | Compatibility now recurses through object properties and array items; only annotation edits and required removals are accepted, while added properties, nested types, constraints and unknown validation changes are breaking. |
| Critic isolation evidence incomplete | The revised final-source live run passed 6/6 without skips using the explicit offline `qwen3.7-plus` stub, followed by the full AHR gate and restoration to `stub=false`. |

The second fresh Critic then found C-03b: an optional property addition is not
provably compatible when the old schema's default `additionalProperties=true`
already accepted that name with a broader value. The full preserved finding is
`docs/agent-studio-prd/reports/as-03-critic-verdict-iteration-2.md`. The Actor
removed that false-negative: all property additions are now conservatively
breaking, and the negative test proves one payload is valid under the old
schema, invalid under the new typed property, and reported breaking.

The third fresh Critic independently closed C-01/C-02/C-03/C-03b and reran all
required gates, but found C-04: HTTPS loopback/link-local literals and hostnames
resolving to private addresses could be persisted because private-network
validation ran only at discovery/invocation. Its preserved verdict is
`docs/agent-studio-prd/reports/as-03-critic-verdict-iteration-3.md`. The Actor
now rejects non-global literals in the request schema and performs a
multi-record DNS destination check for the base URL and OAuth metadata URL
before both create and update persistence. Negative API evidence covers
loopback, metadata/link-local, DNS-to-private, update non-mutation and OAuth
metadata; final Actor gates are API `6`, Assistant `23`, security `19`,
migration `3`, Ruff clean, isolation `6/6`, and AHR `28/77/8/98` plus golden.

## Independent Critic

- Status: approved. Iterations 1, 2 and the initial iteration-3 review returned
  changes requested; every finding is preserved and remediated. The same
  independent iteration-3 Critic rechecked C-04 on the frozen final source,
  independently reran all required gates without skips, and amended the verdict
  to approved.
- Requested scope: Phase and Feature Oracle, migration/repository/API/client
  code, protocol and principal boundaries, legacy Assistant compatibility,
  exact Agent binding, all required commands, real local runtime evidence,
  rollback, external-smoke honesty and minimal scope.
- Canonical artifact:
  `docs/agent-studio-prd/reports/as-03-critic-verdict.md`.
