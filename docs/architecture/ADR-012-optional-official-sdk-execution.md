# ADR-012: Explicit optional official SDK execution services

Status: Proposed implementation amendment, 2026-10-12. Production adoption remains
blocked on the gates below; this record does not assert maintainer acceptance.

Scope: Explicitly selected `openai_agents` and `claude_agent_sdk` registered-service
adapters, their private worker and session data. Existing Codex Assistant and its
Runtime/Capability Worker contracts are unchanged.

## Context and decision

Users requested real OpenAI Agents SDK and Anthropic Claude Agent SDK execution
with their own API key. A completion proxy cannot supply those SDK loops, tool
selection and continuation semantics. Native Gateway remains the owner of identity,
service admission, credentials, quotas and public dispatch; a bounded optional Python
worker embeds the official SDKs. This is an explicit product integration requirement,
not measured evidence for splitting the core Gateway or replacing the Rust kernel.

Each run selects exactly one execution owner before dispatch. The Codex-based Rust
Runtime remains the sole kernel for the managed Assistant, Studio and their existing
Thread/Turn/approval/capability contracts. An explicit SDK service owns a separate SDK
session; no nested Codex/SDK run, implicit migration, failover, or SDK-as-core-kernel
substitution is allowed. The deleted first-party Python loop stays deleted.

## Precise amendments proposed

| Prior clause | Narrow amendment |
| --- | --- |
| ADR-006 Decision 1 and 2, universal single-kernel wording | Retain in full for managed Assistant/Studio/Runtime runs; allow separately registered official SDK services with distinct session namespaces and explicit execution choice. |
| ADR-006 Decision 8, language coexistence | One execution owner per run still holds. The SDK worker embeds upstream SDK code, never a reintroduced first-party orchestration implementation. |
| ADR-008 Decision 3, T0 resident-service freeze | Permit one optional `sdk-agent-worker` process role for the two official SDK integrations. Default Compose topology and all unrelated T1–T3 decisions are unchanged. |
| ADR-008 retained single-Rust-kernel invariant | Retained for core Runtime contracts; explicitly selected external SDK service runs are not projected as Codex Thread/Turn executions. |
| Harness architecture §5, only active loop | Read within the core Runtime scope, subject to acceptance of this bounded SDK-service exception. |

No other ADR clause is superseded. Bounded fork/upstream locks, immutable snapshots,
append-only ledgers, signed capability leases, one-time approval receipts, database
ownership and rollback to prior Rust images remain authoritative.

## Boundaries and deployment

- Worker receives a service-authenticated private launch and a per-run provider key,
  not user bearer tokens, database credentials, capability signing secrets or host mounts.
- End users enter a write-only provider key and choose model/instructions. Routing,
  service authentication and state storage are operator configuration.
- One unprivileged, resource-limited worker owns one private durable volume. It is
  not a replicated session store. Official SDK imports remain outside the Rust Gateway.
- The optional Compose sidecar shares the Gateway network namespace and listens only
  on loopback. It has no published port, standalone ingress or browser API.
- The namespace also exposes Gateway-local ports to the worker. This is a bounded
  trust tradeoff, not an egress sandbox. Strict deployments need a private TLS/mTLS
  origin and separately enforced network policy before broader tools are admitted.
- Read-only Knowledge delegation may use a Gateway-issued run-bound capability;
  the SDK must not directly bypass resource authorization. Side effects require a
  separately designed durable SDK-run authority and approval journal.

## Adoption gates and rollback

Production adoption requires dependency/hash locks and SBOM/license evidence;
amd64/arm64 bundled Claude binary checks; real API-key multi-turn/cancel/restart
verification for both SDKs; private transport and secret-redaction evidence; finite
retention, authenticated session/service/tenant deletion propagation and backup
expiry; and measured optional-worker resource overhead. Source packaging alone
satisfies none of those runtime gates.

Rollback disables new SDK admissions, drains/cancels in-flight SDK work, then uses
the prior compatible worker image with the same state. Never reinterpret SDK state
as Codex sessions, delete the volume implicitly, or silently retry unknown runs.
See [operator guide](../deployment/sdk-agent-worker.md) and
[SDK contract](native-sdk-agent-frameworks.md).
