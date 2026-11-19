# Platform architecture convergence — final closeout template

> Template only. It is not evidence and every verdict starts as `NOT_RUN`. Replace a verdict with
> `PASS` only after the exact command ran at the recorded candidate SHA with durable evidence.

## Candidate identity

| Field | Value |
| --- | --- |
| Overall verdict | `NOT_RUN` |
| Release id | `NOT_RUN` |
| Base SHA | `NOT_RUN` |
| Candidate Git SHA/tree SHA | `NOT_RUN` |
| Compatibility manifest SHA-256 | `NOT_RUN` |
| Runtime Compose owner | `NOT_RUN` |
| Review blocker/high count | `NOT_RUN` |

## Artifact and contract identity

Record Gateway, Frontend, Knowledge API/Worker, migrator, Agent Runtime and Capability Worker image
digests and architectures. Also record Runtime fork/overlay/schema, OpenAPI, SSE, Capability, Agent
event, DB baseline/migration/grants, Compose profile, Qdrant/embedding/BM25, topology, data-access,
quality-baseline and evidence-policy revisions. Initial verdict: `NOT_RUN`.

## Executed gate ledger

For every command record argv, start/end time, exit code, pass/fail/skip counts, files/tests
exercised, output SHA-256 and durable receipt path. A dry run, missing prerequisite, empty file set,
unexpected skip, previous-SHA result or manual observation is not `PASS`.

| Layer | Required scope | Verdict | Durable evidence |
| --- | --- | --- | --- |
| L0/L1 offline and domain | Harness, affected selector, boundaries, OpenAPI, SDK/SSE, Python/Web/Rust, hygiene/LOC | `NOT_RUN` | `NOT_RUN` |
| L2 integration | DB convergence, agent execution, Knowledge | `NOT_RUN` | `NOT_RUN` |
| L3 release runtime | Candidate Docker identity, UI/provider journeys, health/degraded semantics | `NOT_RUN` | `NOT_RUN` |
| Fresh environment | Candidate manifest pull, quickstart, validate/status | `NOT_RUN` | `NOT_RUN` |
| Rollback | current → frozen → current plus DB recovery classes | `NOT_RUN` | `NOT_RUN` |

## Live journey ledger

Record Assistant short/long/multi-turn/refresh/cancel, read/write tools with approval reject/approve,
Worker restart recovery, Knowledge upload/ingestion/query/citation/concurrency/failure recovery,
admin healthy/degraded view, compact/scale topology, paid DashScope/Qwen, and configured negative
paths. Every row begins `NOT_RUN`; state any genuine external prerequisite as `BLOCKED`.

## Rollback and recovery matrix

Record application rollback compatibility, forward database recovery, matched PostgreSQL plus
Qdrant/object-store disaster recovery, frozen image availability, and session/tool/Knowledge ledger
fingerprints before/after each transition. Initial verdict: `NOT_RUN`.

## Final decision

- Release candidate: `NOT_RUN`
- Production/GHCR release: `NOT_RUN` (requires separate user authorization)
- Remaining blockers: `NOT_RUN`
- Remaining non-blocking risks: `NOT_RUN`
- Reviewer and timestamp: `NOT_RUN`

The authoritative machine state is `deploy/release/release-rollback-matrix.json`; this report must
not disagree with it.
