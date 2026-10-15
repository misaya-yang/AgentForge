# ADR-006: Agent Runtime is the single target Agent kernel

**Status:** Accepted

**Date:** 2026-08-21

**Deciders:** AI Gateway maintainers

**Scope:** Assistant runtime, Agent Studio runtime, sub-agents, public runtime contracts, and Harness source governance

## Context

The platform currently owns a large Python agent loop in addition to model
adapters, tool execution, approvals, persistence, Knowledge, artifacts, and
product surfaces. Reliability fixes have consequently been repeated across
model turns, streaming tool loops, compaction, cancellation, resume, and
sub-agent orchestration. Adding Agent as a tool or optional nested backend
would retain those duplicate state machines and create an Agent-inside-Agent
failure mode.

The open-source Agent repository provides the Thread/Turn/Item lifecycle,
context compaction, interruption, tool-call normalization, a storage-neutral
`ThreadStore`, a replaceable `ModelProvider`, and host extension contributors.
Its App Server schemas are version-coupled to the exact binary that generated
them, and its remote WebSocket transport is explicitly experimental and not a
production boundary.

## Decision

1. Agent Runtime becomes the only target Agent execution kernel. The Python
   `AgentLoop` remains only as a migration control and rollback implementation.
   One Run is owned by exactly one kernel and the kernels never nest.
2. AI Assistant is the default `AgentSpec`; Studio, hosted, embed, API, and
   sub-agent executions use the same kernel and contract.
3. Platform capabilities enter Agent through typed extension contributors.
   Business tools, Knowledge, connectors, artifacts, Local Node, approvals,
   billing, and tenant policy are not copied into upstream core.
4. The platform maintains a bounded fork of the upstream open-source Rust agent kernel. Platform changes
   are limited to new `ai-platform-*` crates, the extension installation seam,
   and necessary upstreamable abstraction fixes.
5. The fork is released as an immutable OCI image. The main repository locks
   upstream SHA, fork SHA, schema digest, toolchain, image digest, license
   notice, and SBOM as one version unit.
6. Production uses a dedicated Rust HTTP/SSE service built on Agent crates and
   the in-process App Server API. It does not spawn the Agent CLI per request
   and does not expose the experimental App Server WebSocket transport.
7. The Runtime starts with an isolated, non-user-controlled `AI_PLATFORM_AGENT_HOME`; host
   instructions, plugins, MCP servers, credentials, and filesystem state are
   never inherited implicitly.
8. The migration is intentionally polyglot. Rust owns orchestration and every
   latency-sensitive lifecycle path; Python capability services remain valid
   behind versioned internal contracts until a measured, contract-compatible
   Rust replacement exists. Language coexistence must never become kernel
   coexistence inside one Run.

## Kernel invariants

- Every published tool call receives exactly one terminal tool result before
  the Turn terminal event, including denial, cancellation, timeout, or crash.
- Risk, approval, tenant scope, idempotency, and side-effect policy are
  enforced outside model output.
- Reasoning, cache, search, and model options come from the immutable Model
  Capability Profile; prompt text and model-name branches do not select them.
- Compaction preserves active objective, approvals, evidence IDs, citations,
  action receipts, and unresolved side-effect state.
- Runtime events describe operational state and visible reasoning summaries;
  private hidden reasoning is never exposed or persisted as a product trace.

## Options considered

| Option | Decision | Reason |
| --- | --- | --- |
| Keep Python and add Agent as an optional backend | Rejected | Permanently preserves two loops and makes correctness/eval results engine-dependent. |
| Invoke Agent as a tool or sub-agent | Rejected | Creates nested loops, duplicated budgets, ambiguous cancellation, and tool transcript risk. |
| Copy upstream source into this repository | Rejected | Encourages platform code to spread through upstream core and makes upgrades difficult to audit. |
| Bounded fork plus immutable OCI | Chosen | Gives source-level control, reproducible releases, narrow diffs, and clean rollback. |

## Migration and deletion rule

Migration proceeds through source lock, durable state, pure text, read-only
capabilities, write safety, lazy session import, canary, and V2 contract phases.
The control loop receives no new product capability during migration.

No Python orchestration module is deleted until its Agent replacement has:

1. deterministic contract tests;
2. the unchanged Agent Eval cohort result;
3. authenticated Docker/browser evidence where user-visible;
4. a completed rollback rehearsal; and
5. zero production calls during the declared stability window.

Database changes remain additive. During canary, quiescent sessions may switch
owner only after a verified projection exists; an in-flight Turn never moves
between kernels.

## Consequences

The project takes on Rust, fork synchronization, schema pinning, and a
cross-repository release manifest. In return, one runtime owns the difficult
lifecycle semantics and platform engineering can concentrate on capabilities,
policy, quality, and product surfaces rather than maintaining parallel loops.

Rust migration is ordered by observed bottlenecks and failure ownership, not by
file count. Runtime scheduling, streaming, persistence, tool lifecycle, and
connection-heavy services move first. Python-native document/RAG/business
capabilities may remain out-of-process where model or external I/O dominates;
they move only when profiling shows a useful benefit and the same contract and
Agent Eval gates pass.


## Source-only stable refresh (2026-10-12)

The bounded-fork model also admits a clean immutable upstream snapshot plus a
separately reviewed, hash-bound platform overlay. The stable candidate uses
`rust-v0.162.1` (`092d3acd6bec3e3a14bdc7e7a2810ab628ab759d`) and an actual
Cargo-resolved platform lock. Compiled protocol schema and source/Worker SBOMs
are bound in one source-only release unit. No invented composed Git revision or
Docker provenance is used. `local_source_locked` invalidates every old OCI
identity and grants no startup permission; Runtime/Worker images and behavioral,
provider and rollback acceptance remain separate gates. This source refresh does
not reopen the historical migration or change ownership of any execution loop.
