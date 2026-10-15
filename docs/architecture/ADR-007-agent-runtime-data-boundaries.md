# ADR-007: Agent Runtime model, state, and capability boundaries

**Status:** Accepted

**Date:** 2026-08-21

**Deciders:** AI Gateway maintainers

**Scope:** Gateway model plane, Agent Runtime service, PostgreSQL ThreadStore, and Assistant capability service

## Context

Agent Runtime currently speaks the Responses wire protocol through a replaceable
`ModelProvider`. The platform's public `/v1/responses` endpoint is an
Agent-level compatibility API backed by the Python loop; pointing Agent at that
route would execute one Agent inside another. Direct provider access from the
Runtime would instead duplicate tenant model resolution, credentials,
capability adapters, quota, billing, and failover.

The current `sessions.history` JSONB document is also unsuitable as the durable
Thread/Turn/Item authority: concurrent append, cursor replay, item idempotency,
and crash recovery require an append-only store. Existing Python tools remain
valuable but must not retain an Agent loop merely to be callable.

## Decision

### Model plane

Gateway exposes a private model-only Responses stream used exclusively by
`AiGatewayModelProvider`. It performs provider selection, credential lookup,
wire adaptation, retry/failover policy, usage, quota, and billing. It never
loads history, selects tools, compacts context, or calls either Agent kernel.

Gateway issues a short-lived `ModelLeaseV1` bound to tenant, user, session,
run, allowed model/profile revision, request/token/cost limits, expiry, and a
nonce. The lease contains no provider secret and cannot widen the resolved
model profile. Model calls use service authentication plus the lease; replay,
expiry, scope mismatch, and budget exhaustion fail before provider I/O.

### Agent runtime snapshot

The Agent Runtime creates and stores an immutable `AgentRuntimeSnapshotV1`
from the authenticated request and resolved `AgentSpec`. It contains runtime
identity, AgentSpec hash, permissions, pinned capabilities, model option,
budgets, tool/MCP snapshot references, and component revisions. It contains no
long-term credential. Failover appends a model-attempt record rather than
mutating the original snapshot.

### Durable state

`PostgresThreadStore` is the authority for Agent state:

- `assistant_session_runtime_assignments` immutably binds one tenant/user/session
  to the Agent Runtime before any Turn;
- `assistant_runtime_threads` binds one tenant/user/session to one Agent Runtime
  root Thread and immutable kernel owner;
- `assistant_runtime_thread_members` records the root, sub-agent, and fork
  Threads that belong to that platform session;
- `assistant_runtime_items` stores append-only, root-monotonic Item events and
  identifies the exact member Thread that owns each history item;
- `assistant_runtime_snapshots` stores immutable runtime snapshots;
- `assistant_runs` records Agent Runtime Thread/Turn, kernel, snapshot, capability,
  and schema revisions.

`sessions.history` remains legacy input only. V1 reads are projected from
Items; existing history is imported transactionally on first quiescent resume.

### Capability plane

The existing Assistant service is reduced to a private capability service. It
may host typed tool execution, approval records, command/idempotency ledgers,
Artifacts, Local Node, and Python-native capability implementations. It cannot
call a model or own a tool-selection loop. Agent accesses capabilities through
typed internal HTTP contracts or MCP contributors.

Knowledge remains a separate service. Cross-service work uses authenticated
HTTP/MCP contracts; apps do not import Gateway or one another.

This is a Rust/Python coexistence boundary, not a permanent Python
orchestration layer. New model-loop, cancellation, compaction, tool-selection,
and event-ordering logic belongs only in Rust. Existing Python implementations
may remain as capability endpoints, and can later be replaced module by module
with Rust implementations without changing the Agent kernel contract.

## Tool lifecycle contract

Each call is durably tracked as `published -> dispatched -> terminal`.

- interruption before dispatch produces `cancelled`;
- a dispatched read-only call receives bounded cancellation/timeout handling;
- a dispatched write or unknown-effect call produces `side_effect_unknown`
  when its outcome cannot be proven;
- terminal result uniqueness is enforced by database identity, not prompt text;
- a Turn cannot become terminal until every published call is terminal.

Approval consumption is bound to tenant, user, session, run, tool revision,
arguments hash, expiry, and one dispatch attempt.

## Runtime topology and exposure

Gateway and Web remain the only public services. Agent Runtime, capability
service, Knowledge, PostgreSQL, Redis, Qdrant, MCP children, and sandboxes stay
private to the deployment network. The Agent Runtime uses one private internal
endpoint; stable session assignment is persisted and is never chosen from user
text.

Before the private model plane exists, Agent Runtime-assigned Turn requests fail
locally with `AI_PLATFORM_AGENT_RUNTIME_TURNS_NOT_READY`; they never fall through to
the Python loop. The private Compose `agent-runtime` service is part of the
default runtime topology and is not an optional execution profile.

## Rollback

New-session assignment can be switched back to Python without moving active
Turns. A legacy projector keeps quiescent Agent sessions resumable by the
control release during canary. Database migrations are additive and are not
downgraded. After the legacy loop is deleted, rollback deploys the prior image
against the same schema and event projection.

## Consequences

The internal request path gains one network hop for model calls and one private
capability boundary. Connection pooling, bounded queues, stable-prefix caching,
and measured TTFT decide whether a later in-process optimization is justified;
provider and credential logic is not duplicated merely to remove the hop.
