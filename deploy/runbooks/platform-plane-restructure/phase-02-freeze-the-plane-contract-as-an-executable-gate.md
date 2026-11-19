# Phase 02 - Decide topology and enforce plane contracts

- PHASE_ID: PPR-02
- FEATURE_ID: PPR-F003
- DEPENDS_ON: PPR-00, PPR-01

## Outcome

ADR-008 selects the least complex deployment topology supported by PPR-00 evidence, and a fail-closed manifest/gate enforces the five logical planes and every selected physical handoff before code moves.

## Scope

In:

- Compare T0 existing gateway scaling, T1 Governance isolation, T2 Edge plus Governance isolation, and optional T3 Rust model plane.
- Record load evidence, operational cost, failure domains, rollback, capacity and rejected alternatives.
- Map every top-level module to Edge, Control, Data, Index or Governance; storage systems are infrastructure substrate.
- Define any selected cross-service schema version, service authentication, claims propagation, anti-replay, timeout, retry, idempotency, backpressure, error mapping, observability and SSE owner.
- Add `make plane-boundary-gate` with failing fixtures for unmapped modules and forbidden synchronous dependencies.

Out:

- Moving production code, publishing ports, or preselecting a new service because the target diagram shows one.
- Ad hoc headers or unversioned internal RPC.

## Done when

- [ ] ADR-008 is independently approved and links every topology choice to PPR-00 evidence.
- [ ] Logical planes and physical deployment units are explicitly distinguished.
- [ ] Every selected handoff has complete success, failure and security semantics.
- [ ] Plane ownership exists as data; unmapped modules fail closed.
- [ ] Injected import, synchronous-call and missing-ownership violations all fail the gate.
- [ ] Harness registration and shared regression gates pass.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| ADR review | Approved architecture/security receipt | Topology is a decision, not an assumption |
| Gate green | `make plane-boundary-gate` | Current declared ownership is conformant |
| Gate teeth | Repository-owned negative fixtures | Violations fail closed without dirtying the worktree |
| Harness registration | `make harness-check` | Command and docs agree |

## Stop or confirm

- Stop if the current tree cannot satisfy the logical boundary without a named, dated exception and removal phase.
- Ask before accepting an exception, new network trust boundary or public/internal port.
- Required review: independent architecture and security approval.
