# ADR-008: Platform bounded contexts, deployment freeze, and conformance to ADR-006/007

**Status:** Accepted

**Date:** 2026-08-29

**Deciders:** AI Gateway maintainers

**Scope:** Whole platform: Gateway service, Agent Runtime, Capability Worker, Knowledge service and worker, frontend and SDKs, PostgreSQL/Redis/Qdrant/object storage, and the architecture convergence program (`docs/plans/platform-architecture-convergence-prd-2026-08.md`).

## Context

The full Rust cutover is complete. The Full Rust Cutover program closed on
2026-08-25 with the Runtime and Capability Worker as the only Agent execution
path (`reports/agent-runtime/full-rust-cutover-final-2026-08-25.md`), and the
Python `AgentLoop` has been deleted; `src/harness/runtime_dependency_gate.py`
now permanently blocks its re-addition through forbidden-module and
forbidden-text rules.

Three follow-on obligations from the earlier ADRs were left open:

1. `docs/harness/architecture.md` §1 describes three *provisional* business
   contexts and states that "formal bounded-context acceptance is queued for
   the successor ADR". This is that successor ADR.
2. `docs/plans/rust-expansion-and-service-topology-2026-08.md` §6.4 required
   ADR-008 to compare deployment options T0 (keep current units), T1 (split
   Governance), T2 (split Edge + Governance), and T3 (add a Rust model-plane
   service).
3. ADR-006 and ADR-007 contain migration-time decisions whose stated end
   state has now been reached; their obsolete items must be superseded
   item-by-item so nobody re-applies them.

The fact base for this decision is the ARC-00A baseline set in
`docs/architecture/baselines/2026-08-post-rag/` (service topology, data-access
inventory, LOC, dependency, skip, and contract freeze), which is
machine-regenerable from the tree via
`python3 scripts/inventory/generate_baselines.py` and whose drift is checked
by `--verify`.

## Decision

### 1. Three bounded contexts are accepted

The platform formally consists of three business bounded contexts, one
product surface, and shared infrastructure, exactly as observed in
`service-topology.json`:

| Context | Runtime units | Language | Responsibility |
| --- | --- | --- | --- |
| gateway-control | `gateway` | Python | Public API, auth, tenant policy, provider/model plane, quota/billing, turn issuance and lease signing |
| agent-execution | `agent-runtime`, `agent-capability-worker` | Rust | Single Thread/Turn kernel; capability catalog, execution, artifact/tool dispatch; one version unit, two isolated processes |
| knowledge | `knowledge-service`, `knowledge-worker` | Python | Dataset/document API, retrieval, embedding/rerank control, durable ingestion; one code unit, two runtime roles |
| product-surface | `frontend` | TS/nginx | Web console and public static assets; no persistent data of its own |
| infrastructure | `postgres`, `redis`, `qdrant`, object storage, `tempo` | — | Shared substrate with logical namespaces, not ownerless buckets |

A logical module or process role does not automatically deserve a new
service. One-shot jobs (`gateway-init`, `migrate`) are bootstrap work, not
resident services.

### 2. Polyglot is a permanent property, not a transition

Rust owns the Agent kernel and every latency-sensitive lifecycle path.
Python owns the Gateway and Knowledge APIs and workers. TypeScript owns the
console. This coexistence is the accepted end state, not a phase:

- Cross-language contracts travel as versioned JSON fixtures and schemas
  (see `contract-freeze.json`);
- the Rust contract crate (`ai-platform-capability-contract`,
  `CapabilityDescriptorV2`) is the single authority for the capability
  contract; Python `CapabilityDescriptor` is its read-only projection;
- apps never import Gateway or one another (dependency-baseline records the
  current violations; ARC-00B gates them statically);
- language coexistence never becomes kernel coexistence inside one Run
  (ADR-006 §8 retained).

### 3. No new resident services (T0 adopted)

Of the §6.4 candidates, this ADR adopts **T0: keep the current deployment
units**. The compose service set — 13 entries including infrastructure and
one-shot jobs, 6 business services — is the deployment architecture:

- the T1 Governance split, T2 Edge split, and T3 Rust model-plane service are
  rejected as of this date: no measured evidence shows the current units
  cannot meet their SLOs after the convergence fixes (interference, health,
  topology ownership) land;
- the model plane stays a private Gateway route: every model call is
  lease-reserved and ledgered, which gives the accounting and isolation T3
  was meant to buy without an extra hop, process, or supply-chain unit;
- adding or splitting a service later requires a new ADR plus measured
  evidence and adoption gates. The rule "new services must write an ADR"
  (`architecture.md` §6) stands unchanged.

### 4. Same database, role separation

One PostgreSQL database remains the system of record. Separation is enforced
by schema and privilege, not by database:

- three schemas exist and stay: `gateway`, `assistant`, `knowledge`
  (data-access-inventory: 166 tables, 45 functions, 1 grant today);
- ARC-03 creates least-privilege per-service roles
  (`ai_gateway_gateway`, `ai_gateway_runtime`,
  `ai_gateway_capability_worker`, `ai_gateway_knowledge_api`,
  `ai_gateway_knowledge_worker`) generated from the data-access inventory;
- append-only runtime stores in `assistant` are written only through their
  SECURITY DEFINER functions (e.g. `issue_assistant_runtime_turn`,
  `reserve_assistant_runtime_model_call`); application roles get no direct
  write grant on those tables;
- cross-context data access goes through those functions or authenticated
  HTTP APIs. Direct cross-schema writes from application code are
  architecture violations, and the inventory's
  `tables_with_multiple_python_unit_writers` list is the remediation input.

### 5. Superseded and retained decisions

Superseded (obsolete migration-time items):

| Source | Superseded item | Replacement decision |
| --- | --- | --- |
| ADR-006 §1 | "The Python `AgentLoop` remains only as a migration control and rollback implementation" | The Python loop is deleted. `runtime_dependency_gate.py` permanently blocks its re-addition. Rollback never returns to Python (see ADR-007 rollback item below). |
| ADR-006 "Migration and deletion rule" | The five-criteria deletion gate as an *open* obligation | Fulfilled and consumed by the FRC program evidence (contract tests, eval cohort, Docker/browser evidence, rollback rehearsal, zero-call stability window). The rule text remains the standard for any *future* kernel replacement. |
| ADR-007 "Agent runtime snapshot" | "The Agent Runtime creates and stores an immutable `AgentRuntimeSnapshotV1`" | Snapshot resolution and lease signing live in the Gateway control plane, which issues the turn through the DB function `issue_assistant_runtime_turn`; the Rust Runtime consumes and executes the signed snapshot. Immutability of the snapshot is retained. |
| ADR-007 "Rollback" | "New-session assignment can be switched back to Python … a legacy projector keeps quiescent Agent sessions resumable" | No Python path exists to switch back to. Rollback deploys the prior Runtime/Worker image against the same schema and event projection — the last sentence of that section is now the whole rule. |
| ADR-007 "Model plane" retry/failover description | Model plane owns retry policy as a loop-level behavior | Retries are Runtime-initiated **new reservations**: each model call is authorized and reserved via `reserve_assistant_runtime_model_call` and reaches a terminal ledger state via complete/fail/mark-unknown. The model plane keeps provider selection, credential lookup, wire adaptation, quota, and billing; it never re-runs a call on its own. The reserve/complete ledger is append-only. |
| ADR-007 "Capability plane" | "The existing Assistant service is reduced to a private capability service" | There is no assistant service in the deployment. Capability execution lives in the Rust Capability Worker; Python-native capability implementations live inside Gateway/Knowledge behind versioned contracts; tool-call authority follows ADR-007's lifecycle contract unchanged. |
| `docs/plans/rust-expansion-and-service-topology-2026-08.md` §6.4 | Open candidate list T0–T3 | Resolved: T0 adopted (§3 above). T1/T2/T3 remain possible only through a new ADR with measured evidence. |

Retained in full (conformance, no change):

- ADR-006: single Rust kernel per Run; kernels never nest; bounded fork plus
  immutable OCI release unit; isolated `AI_PLATFORM_AGENT_HOME`; all kernel
  invariants (one terminal tool result per published call, policy outside
  model output, capability-profile-driven options, compaction preservation,
  operational-event-only traces).
- ADR-007: `ModelLeaseV1` semantics (short-lived, scoped, nonce-bound, no
  provider secret); append-only stores `assistant_session_runtime_assignments`
  / `assistant_runtime_threads` / `assistant_runtime_items` /
  `assistant_runtime_snapshots` / `assistant_runs`; the published →
  dispatched → terminal tool lifecycle; Gateway and Web as the only public
  services; `sessions.history` as legacy input only.
- ADR-004 (bounded plugin/sub-agent delegation) and ADR-005 (model capability
  profiles) are unaffected by this ADR.

## Evidence at acceptance time

| Fact | Evidence |
| --- | --- |
| Rust is the only execution kernel | `src/harness/runtime_dependency_gate.py` (blocks Python loop re-addition); FRC final report `reports/agent-runtime/full-rust-cutover-final-2026-08-25.md` |
| Gateway issues turns through a DB function | `src/services/agent_runtime/control_plane.py` — `SELECT issue_assistant_runtime_turn(...)` |
| Model calls are lease-reserved and ledgered | `src/services/agent_runtime/model_plane.py` — `authorize_and_reserve`, `SELECT reserve_assistant_runtime_model_call($1..$6)`, `_complete_call`, `_fail_call`, `_mark_unknown_if_dispatched` |
| No assistant service exists | `service-topology.json` (compose resolution; 13 services) |
| Three schemas, function-mediated writes | `data-access-inventory.json` (166 tables, 45 functions, `function_mediated_access`) |
| Import-boundary state | `dependency-baseline.json` (violations: `ai-gateway-core → gateway`, `gateway → knowledge-service`) |
| Public-contract digests | `contract-freeze.json` (OpenAPI snapshot, SSE fixture, Capability V2 crate + fixtures, DB function digest, compose resolution, image revisions) |

## Consequences

- The architecture convergence program executes inside this freeze: packages
  ARC-00…ARC-08 change modules, grants, gates, and documentation — never the
  service set or the kernel count.
- A service or kernel addition is a decision event, not an implementation
  convenience; it needs an ADR, measured evidence, and adoption gates.
- `architecture.md` §1's "provisional" wording is satisfied; its bounded
  context table now restates this ADR.
- Every later package compares against the 2026-08-post-rag baselines;
  drift requires regeneration plus a named reason, and public-contract drift
  requires the contract-delta manifest (PRD AC-M04).
