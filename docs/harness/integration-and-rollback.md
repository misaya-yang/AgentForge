# Integration, Runtime Ownership, and Rollback

> The release-side contract for changes that cross services, databases, images, or user journeys.
> Read this together with [`runtime-and-secrets.md`](runtime-and-secrets.md) before any Docker,
> migration, browser, or provider action.

**Schema:** `harness/integration-rollback/v1`
**Status:** policy active; mechanical shared locks/gates queued for ARC-00. Fact snapshot based on
`main@47b7a9b` on 2026-08-29 (pre-RAG merge)

---

## 1. The local runtime is a singleton

All worktrees share Docker Desktop, fixed host ports, local databases, image caches, and provider
quota. The current Compose project also fixes its project name, network, and container names.
Therefore the machine has one mutable AI Gateway runtime, not one runtime per worktree.

Only the primary integration session may mutate that runtime. Other sessions may inspect it but may
not start, stop, rebuild, migrate, hot-update, or run stateful E2E against it.

Until mechanical shared locks are implemented, the owning session records a manual lease under the
resolved Git common directory (`git rev-parse --git-common-dir`), not inside one worktree. The target
layout is:

```text
<git-common-dir>/ai-gateway-locks/
  integration-runtime/owner.json
  rust-build/owner.json
```

`integration-runtime` exclusively covers Docker mutation/hot-update, DB migration, live E2E,
browser auth state and paid-provider execution. `rust-build` covers Docker-contained Runtime/Worker
image builds only; native host Cargo follows the scoped exception in `runtime-and-secrets.md`. On a low-memory host the two
locks are mutually exclusive; hosted Rust CI is a different host/resource.

The owner receipt contains:

- resource, hostname, PID, worktree path and Git HEAD;
- intended command and affected services;
- start time, heartbeat time, timeout and expected end condition;
- current Compose owner labels and running image revisions.

Acquisition uses atomic directory creation; command execution installs signal/exit cleanup and
refreshes heartbeat. A same-host lock is stale only when its PID is dead and its timeout elapsed.
Cross-host or ambiguous locks require explicit user/maintainer force-release; deleting a lock merely
because it is old is forbidden. If future operations need more than one compatible lock, acquire in
the documented global order; the current low-memory policy never acquires these two together.

The architecture program must replace this manual receipt with one tested lock command shared by
all Docker/DB/E2E/provider scripts and one Rust-build command; ad-hoc locks per script are not allowed.

## 2. Integration preflight

Before touching shared state, the primary session must prove:

1. the feature worktree is clean except for the intended integration change;
2. the target includes the latest accepted `main` and every preceding package;
3. container labels identify the same checkout;
4. the expected source SHA can be compared with each service/image revision;
5. current database baseline/epoch/revision and pending migrations are known;
6. a backup exists before any migration classified `restore-required` or destructive;
7. enough memory and disk are available for the chosen operation;
8. no other session holds or is using Docker, Rust build, DB migration, or E2E ownership.

If any identity cannot be proven, stop. Testing an unknown image or another worktree is not partial
evidence; it is invalid evidence.

## 3. Verification ladder

Run the cheapest trustworthy layer first. A higher layer never repairs a lower-layer failure.

| Layer | Environment | Required evidence | Skip rule |
| --- | --- | --- | --- |
| L0 static | No services | format/lint, real TypeScript file count, import boundaries, migration/checksum lint, Compose render | No skip for changed language |
| L1 domain | Local process with native Rust host/hosted checks per the current toolchain policy | changed-domain unit/contract tests, in-process OpenAPI, affected native Rust changed-crate tests, Web Node tests | Only declared platform-specific tests |
| L2 isolated integration | Prefer hosted CI disposable PG/Redis/Qdrant; local containers require `integration-runtime` lease | fresh/upgrade DB, service contracts, concurrency, restart and failure paths | Required package scenarios may not skip |
| L3 release runtime | Singleton Docker + browser + configured provider | actual source/image SHA, UI journeys, paid-provider path, degraded states, current→frozen→current | Zero unexpected skips |

`PASS`, `FAIL`, `BLOCKED`, and `SKIPPED` are distinct. A required layer that cannot run produces
`BLOCKED`, never `PASS`.

### Cross-service recovery scenarios

Select applicable cases in the existing package's integration/live gates. Reuse
its journey IDs and receipts; this table does not authorize a live operation.

| Failure boundary | Observable invariant |
| --- | --- |
| Approval response lost after durable decision | Re-read/retry the same decision identity; no second dispatch or broader approval; conflicting or stale decisions remain denied |
| Worker crashes before/after external dispatch | Owner/fence and durable result govern recovery; uncertain writes stay `side_effect_unknown`, never automatic replay |
| Stream disconnect or client refresh | Reconnect observation with the existing cursor/run identity; no implicit execution POST; durable terminal state remains discoverable |
| Role, credential or capability revoked while paused | Revalidate the current actor and selected capability at resume; no stale grant or silent capability substitution |
| Provider timeout or routing fallback | Record timeout phase and attempted/selected provider; enforce the admitted model/budget policy; no cross-engine fallback or replay of the whole Agent run |
| DB/schema, Knowledge generation or plugin version mismatch | Refuse incompatible admission or expose scoped degradation; never turn missing data, evidence or a required binding into success |

For each case, bind the injection point, exact source/profile, expected durable
state, actual transport/process/database observations, cleanup and recovery result.
Memory stores and synthetic transports remain L1 evidence even when they model
these failures. Exercise the real store and paired services for the corresponding
L2/L3 claim. Do not infer provider-side cancellation from a closed HTTP connection.

## 4. Resource-aware execution

The default development machine is low-memory. Integration must optimize for completion, not peak
parallelism:

- prefer prebuilt, revision-pinned Runtime and Capability Worker images;
- native host builds use Rust 1.99.0, one job, no debug/incremental output and an external target
  directory; Runtime/Worker retain the controlled Docker source workflow;
- run Docker-contained Runtime/Worker image builds once in the final integration window, not in
  every package;
- keep Cargo jobs at one inside the Docker builder and hosted CI, with a measured resource ceiling;
- stop application containers before a heavy build when that reduces peak memory and rollback is
  already prepared;
- build Runtime and Capability Worker as one Agent Execution release unit even if their artifacts
  remain separate;
- never launch two Compose builds from different worktrees;
- abort before the machine swaps uncontrollably or Docker approaches its memory ceiling.

The program must record build duration, peak memory, artifact digest, architecture, and whether a
cache was used. “It eventually built” is not a build-economics gate.

## 5. Integration order

Cross-cutting architecture work integrates in this order unless an accepted ADR says otherwise:

1. contract and harness truth gates;
2. stable facades and module movement;
3. shared contract/owner cleanup;
4. migration runner, schema convergence and expand/backfill/contract preparation;
5. service identities, health, and trace;
6. Compose/distribution changes;
7. UI/admin projection;
8. repository hygiene and documentation lifecycle;
9. compatibility manifest and final release journeys.

Protected entrypoints are edited only after their underlying implementations are stable. Conflicts
are resolved by re-reading the current owner and contract; never choose “ours” or “theirs” across a
whole file.

## 6. Database integration

Database rollback is not the same as reverting a Git commit. Every change declares one class:

| Class | Meaning |
| --- | --- |
| `old-binary-compatible` | N-1 application remains valid through the compatibility window |
| `forward-fix-only` | Rollback means deploy a correcting migration and compatible application |
| `restore-required` | Returning to the old system requires a tested backup/restore procedure |

Use expand → backfill/migrate → contract. The contract step waits at least one release window and
must not run while a supported old binary still depends on the old shape.

The post-KB baseline requires separate receipts for:

- last supported real initialization path upgraded to the convergence revision;
- fresh baseline initialization;
- old filename, numeric, and per-service ledger adoption;
- public-layout and split-layout schema convergence;
- structural, owner/ACL/default-privilege, extension, and reference-data fingerprints;
- table/function/sequence role-positive and role-negative tests;
- concurrent migrator and crash-at-ledger-boundary tests;
- backup restore and the declared rollback class of every non-additive migration.

Application startup checks only the compatible revision and required objects. Full fingerprinting
belongs to the migrator/status path and must be read-only outside an authorized migration.

## 7. Application, image, and data rollback

The release plan distinguishes:

1. **Application rollback** — deploy prior Gateway/Frontend/Knowledge/Runtime/Worker images that are
   declared compatible with the current DB and protocol revisions.
2. **Forward database recovery** — apply a correcting change without pretending an irreversible
   migration can be undone safely.
3. **Disaster recovery** — restore PostgreSQL plus Qdrant/object-store state from a matched recovery
   point and then deploy its compatible image set.

For RAG, PostgreSQL alone is not the whole data plane. Receipts also identify Qdrant collection or
alias revision, embedding provider/model/dimension, lexical/BM25 revision, and object-store data
generation.

`current → frozen → current` is a release journey. It must demonstrate that sessions, tool
executions, Knowledge data, and capability receipts remain intelligible across the transition; the
existing Runtime-only rehearsal is supporting evidence, not proof of whole-platform rollback.

## 8. Go/no-go

Release is **no-go** when any of these holds:

- an expected gate skipped or exercised zero files;
- the live service/image revision does not match the candidate;
- a service reports healthy without checking the state it owns;
- schema or protocol compatibility is inferred rather than compared;
- a migration lacks an owner, checksum, lock, or rollback class;
- an explicit tool/KB binding is silently dropped during degradation;
- a long-running operation depends on one HTTP connection surviving;
- a blocker/high review finding remains accepted but unfixed;
- rollback depends on an unavailable image or an untested backup.

The primary session may prepare a merge- and push-ready candidate. Push, production deployment,
destructive cleanup, and shared-data migration still require the user's explicit authorization.

## 9. Required release receipt

The final receipt includes:

- base/head SHA and release id;
- every image digest and platform architecture;
- public/internal protocol digests;
- DB baseline, epoch, revision, grants and reference-data fingerprints;
- Qdrant/object-store generation metadata;
- exact commands, exits, pass/fail/skip counts and durations;
- Compose owner and actual service revisions;
- browser/provider journeys and redacted artifacts;
- degraded/failure-injection outcomes;
- application rollback, DB recovery, and disaster-recovery verdicts;
- remaining risks and user decisions.
