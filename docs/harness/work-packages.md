# Work-Package Contract

> Durable execution contract for architecture changes and other repository-wide work. A work
> package is a scope and verification boundary; it is not permission to split one product change
> across concurrent writers.

**Schema:** `harness/work-package/v1`
**Status:** policy active; machine validation queued for ARC-00. Fact snapshot based on
`main@47b7a9b` on 2026-08-29 (pre-RAG merge)

---

## 1. One outcome, one owning session

A product-scale change has one primary implementation session, one writer worktree, and one
end-to-end owner. That session executes every package in dependency order and remains responsible
for integration, real-runtime verification, and the final verdict.

Parallel agents are useful for read-only exploration, contract review, threat review, and test
criticism. They must not write into the owning worktree. A reviewer may propose a patch, but the
owning session decides and applies it after checking that it stays inside the active package.

Work packages exist to:

- keep each change reviewable;
- make dependencies and protected paths explicit;
- separate pure moves from behaviour changes;
- map claims to direct, integration, and live evidence;
- give one long session safe checkpoints across context compaction.

They do **not** create independent product owners or authorize parallel feature branches.

## 2. Authority by question

There is no total ordering that lets an implementation silently override an accepted ADR or lets an
old ADR deny current fact:

| Question | Authority |
| --- | --- |
| What code/data/runtime exists now? | Current code, schema, Compose and verified runtime receipt |
| What boundary is accepted? | Current accepted ADR/product law |
| What is executing now? | Active program state and active package |
| What is planned next? | Queued plan with satisfied prerequisite |
| What actually passed before? | Dated report/receipt at its recorded SHA/environment |

If accepted architecture and current implementation disagree, stop and write/approve a successor or
conformance decision; do not choose whichever source is convenient. A queued plan is not an active
instruction. Chat history is never repository truth.

## 3. Required package fields

Every package must declare all of the following before code is changed:

| Field | Meaning |
| --- | --- |
| `id` | Stable identifier, unique inside the program |
| `result` | User-visible or system-visible outcome, not a file-edit description |
| `base_sha` | Clean Git revision from which the package was observed |
| `depends_on` | Packages and required predecessor state (`direct_verified`, `awaiting_final_live`, or `verified`) |
| `owned_paths` | Paths the primary session may modify in this package |
| `forbidden_paths` | Paths that require stopping and redesigning the package |
| `integration_paths` | Shared entrypoints changed only in the package's integration commit |
| `must_preserve` | Public, data, security, performance, and rollback contracts |
| `direct_gates` | Offline checks for the changed implementation |
| `integration_gates` | Isolated DB/service/contract checks |
| `live_gates` | Docker, browser, provider, failure, and rollback journeys |
| `skip_policy` | Which optional checks may skip; release-required checks must be `none` |
| `rollback` | Code/image rollback plus DB/data compatibility class |
| `review` | Required independent review and its severity threshold |
| `evidence` | Receipt paths and fields that prove the result |
| `stop_conditions` | Conditions that invalidate the package scope or architecture decision |

`owned_paths` is an allowlist, not a suggestion. Discovered work outside it becomes a finding or a
new sequential package; it is not silently absorbed.

### Journey and interface bindings

Expand the existing `result`, `must_preserve`, gates and `evidence` fields instead
of adding a parallel plan or status ledger. For each affected existing journey ID,
record the actor/tenant and entry surface, preconditions, externally observable
success, and denial/failure/recovery expectations. Link each expectation to its
actual command/scenario and source-bound receipt; an unrun expectation stays open.

For every changed service edge, name the producer and consumer owners, versioned
schema/fixtures, auth and current-policy checks, durable write authority, timeout
phases and retry/unknown-effect semantics. Use `architecture.md` sections 2–6 as
invariants, not another independently maintained copy of the service map.

Freeze expected scenario IDs before execution. Count-only passes, missing cases,
fixture-only responses and a valid export are not proof of the complete journey.
For quality comparisons, bind baseline and candidate to the same case/dataset and
evaluator versions; retain critical-case regressions even when averages improve.
A newer run does not silently become the approved baseline.

These additions are a review checklist; the current structural harness check does
not mechanically validate every journey binding. A future validator must extend
existing harness/evidence tooling with positive and negative fixtures, not claim
coverage before it runs.

## 4. Protected integration paths

The following paths are cross-cutting and may be changed only in a named integration commit by the
primary session:

```text
rust/platform-native/crates/platform-gateway/src/bin/gateway.rs
rust/platform-native/crates/platform-gateway/src/migration_host/bootstrap/**
docker-compose*.yml
Makefile
harness.yml
.env.example
.github/workflows/**
docs/README.md
database/schema.sql
```

A package may read them and may declare a future change, but implementation files must be stable
before the integration path is edited. Do not mix module movement, migration grants, Compose
changes, and CI changes in one commit.

## 5. Package cycle

Each package runs this exact cycle:

1. **Observe** — recheck `base_sha`, dirty paths, current call graph, contracts, and known debt.
2. **Freeze** — capture the relevant API/schema/fixture/performance baseline before moving code.
3. **Act** — make the smallest scoped change; pure movement and behavioural fixes use separate
   commits.
4. **Verify direct** — run the exact checks for the changed paths and their failure paths.
5. **Review** — read-only review of diff, ownership, compatibility, security, and rollback.
6. **Repair** — fix accepted blocker/high findings and rerun affected gates.
7. **Integrate direct** — edit protected entrypoints and run required isolated gates. When a heavy
   live scenario is intentionally batched into the final serialized window, record its exact ARC-08
   scenario and enter `awaiting_final_live`; do not call the package complete.
8. **Decide** — `direct_verified`, `awaiting_final_live`, `verified`, `blocked`, or `superseded`;
   unexpected or required skipped evidence cannot produce `verified`.

The primary session may create a checkpoint commit after a package is verified. A Git merge is not
verification, and a green fixture is not a live result.

## 6. Work-package states

| State | Meaning |
| --- | --- |
| `queued` | Dependencies or authorization are not yet satisfied |
| `in_progress` | The primary session owns the package and its paths |
| `review` | Direct gates passed; independent read-only review is pending |
| `direct_verified` | Direct and required isolated gates passed; the next sequential package may start |
| `awaiting_final_live` | Direct/isolated proof passed and named heavy live scenarios are bound to ARC-08 |
| `verified` | Direct and required integration/live gates passed with receipts |
| `blocked` | A named external decision or unavailable required environment prevents completion |
| `superseded` | A newer package or ADR replaced the work before completion |

Only one package may be `in_progress` for a single-owner architecture program. A later package may
depend on `direct_verified/awaiting_final_live` only when the predecessor names no unresolved
contract or data blocker. The program cannot complete until ARC-08 backfills every deferred live
receipt and all required packages become `verified`. A blocked package does not authorize a
conflicting writer elsewhere.

The durable machine location is:

```text
deploy/runbooks/<program>/
  loop-state.json
  work-packages.yml
  receipts/<package-id>.yml
```

Until ARC-00 implements a validator/schema, these are reviewed manual contracts; agents must not
pretend a hand-edited `verified` field is mechanical proof.

## 7. Package receipt

Every package reaching `direct_verified` or later records:

```yaml
package_id: ARC-XX
state: direct_verified | awaiting_final_live | verified
base_sha: <sha>
head_sha: <sha>
changed_paths: []
commits: []
contracts_compared: []
commands:
  - command: <exact command>
    exit_code: 0
    passed: <count or null>
    failed: 0
    skipped: 0
direct_verdict: PASS
integration_verdict: PASS
live_verdict: PASS | NOT_REQUIRED | DEFERRED_TO_ARC08
deferred_live_scenarios: []
review_findings: {blocker: 0, high: 0, medium: 0}
rollback_class: <old-binary-compatible | forward-fix-only | restore-required>
remaining_risks: []
```

State validation is strict: only `awaiting_final_live` may use `DEFERRED_TO_ARC08`, with a non-empty
scenario list. `verified` requires `PASS` or `NOT_REQUIRED` and an empty deferred list. ARC-08 replaces
the deferred receipt with final evidence; it does not edit the old command result into a pass.

For Docker, browser, or provider work, the receipt also records Compose owner, expected source SHA,
actual service/image revision, database revision, model/provider identity, and artifact manifest.
It never records secrets, prompt contents, raw credentials, DSNs, or sensitive tool arguments.

## 8. Applying this to the architecture convergence program

[`platform-architecture-convergence-prd-2026-08.md`](../plans/platform-architecture-convergence-prd-2026-08.md)
was activated on 2026-08-29 after the KB RAG upgrade merged, on branch
`platform-arch-convergence-2026-08` from the clean post-RAG base `336851c1`,
governed by [`ADR-008`](../architecture/ADR-008-bounded-contexts-no-new-services.md)
(Accepted). Its rules of engagement:

- one primary session owns exactly one authoritative program; all commits are
  performed by that session;
- the program executes in the main working directory with mutually exclusive
  owned paths per package;
- read-only specialists may review architecture, database, security, tests, and runtime evidence;
- Docker, Docker-contained Rust image builds, migrations, E2E, and provider calls remain globally
  serialized; Rust fmt/check/test runs in hosted CI, never through host Cargo;
- no package is called complete until the primary session has integrated it with everything before
  it.

### Registered packages

Truthful per-package fields (owned paths, gates, rollback, stop conditions) live in the durable
machine location [`deploy/runbooks/platform-architecture-convergence/work-packages.yml`](../../deploy/runbooks/platform-architecture-convergence/work-packages.yml);
the table below is an index, not a second source of truth.

| Package | Result (short) | State at registration |
| --- | --- | --- |
| ARC-00A | Fact + contract freeze, successor ADR, single program, terminal ledgers | in_progress (user-directed parallel) |
| ARC-00B | Trustworthy gates (type-check, OpenAPI split, import boundary, gate schema/CI, hygiene/LOC) | in_progress (user-directed parallel) |
| ARC-00C | Rust build/distribution economics; absorbs PPR-01 | in_progress (user-directed parallel) |
| ARC-01 | Assistant Gateway API modularization | in_progress (user-directed parallel) |
| ARC-01B | agent_runtime.py split by use case | queued |
| ARC-02 | Runtime control/model planes, self-HTTP removal, ResolvedAgentLaunchV1, Capability V2 | in_progress (user-directed parallel) |
| ARC-02B | Runtime/Worker HTTP boundary split, zero wire drift | in_progress (user-directed parallel) |
| ARC-03 | Single migration authority + per-service PostgreSQL roles | in_progress (user-directed parallel) |
| ARC-04 | Shrink ai-gateway-core behind I/O-free contracts layer | in_progress (user-directed parallel) |
| ARC-05 | Durable jobs, health, service identity, trace | queued |
| ARC-06 | Compact/Scale modes, admin architecture status, topology guards | queued |
| ARC-07 | Repository quality and evidence governance | queued |
| ARC-08 | Release compatibility matrix, final regression, rollback, doc retirement | queued |

Execution deviation on record: §1/§6 allow only one `in_progress` package in a single-owner
architecture program. The user explicitly directed same-directory parallel execution of the
packages marked above, under mutually exclusive owned paths, with commits reserved to the primary
session. The sequential rule governs every package not covered by that direction.
