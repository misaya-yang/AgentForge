# Phase 05 - Conditionally isolate Governance

- PHASE_ID: PPR-05
- FEATURE_ID: PPR-F006
- DEPENDS_ON: PPR-02

## Outcome

Eval, audit and trace consumption become a separate deployment unit only if reproducible Governance load harms Edge/Data and bounded asynchronous execution in the existing unit cannot meet the precommitted gate.

## Scope

In:

- Measure heavy Eval/trace/audit load against local timing components, streaming p99, CPU, RSS and failures.
- Compare bounded queues, worker/process isolation and a separate Governance service.
- If adopted, preserve asynchronous ingestion, fixtures, retention, audit and failure semantics.
- Keep lifecycle, catalog, policy, Studio and billing in Control unless separately approved.

Out:

- Rewriting Eval scoring, changing golden/RAG fixtures or moving billing.
- Declaring process separation a hard requirement without noisy-neighbor evidence.

## Done when

- [ ] The same PPR-00 profile proves interference and the simpler bounded in-place option cannot satisfy the adoption gate, or physical separation is measured-not-adopted.
- [ ] If adopted, Governance load no longer moves Edge/Data local SLI or streaming p99 beyond the predeclared noise tolerance.
- [ ] Eval, trace, audit and retention outputs remain identical.
- [ ] The actual CPU/RSS and operational delta is recorded; idle RSS is not used as proof.
- [ ] Rollback, independent Eval/architecture review and shared regression gates pass.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Interference baseline | Heavy Governance load during named stream profile | Problem is reproducible |
| Alternative comparison | Bounded in-place worker versus separate unit | Least complex fix wins |
| Eval contract | Existing Eval and RAG gates | Quality semantics unchanged |
| Failure isolation | Queue saturation and Governance outage | User path remains bounded |

## Stop or confirm

- Ask before changing Compose topology, deployed budgets or billing ownership.
- Stop if separation requires scoring, retention, audit or public-contract changes.
- Required review: independent architecture and Eval review; add security review for new trust boundaries.
