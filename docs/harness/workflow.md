# Working Agreement

> How work moves through this repository, for humans and for agents on any device.

**Schema:** `harness/workflow/v2`

---

## 1. Cold start

Any agent picking up this repo, on any machine, boots in this order:

1. `AGENTS.md` — contract, repo map, canonical commands.
2. `docs/harness/README.md` — this harness.
3. `docs/README.md` — documentation index; find the doc that owns your area.
4. If the task belongs to a running program: that program's `loop-state.json` and active phase file.
5. For architecture or repository-wide work: [`work-packages.md`](work-packages.md) and the active
   package receipt.

Everything an agent needs must be reachable from those entry files. **If knowledge is not in the
repository, it does not exist.** Do not rely on chat history, a previous session's context, an
external directory, or a teammate's machine.

### Resume without inheriting stale claims

Before resuming work, record a short checkpoint in the active package receipt:

- **Identity:** current source manifest/digest and release profile; Git base/head only
  when real Git metadata exists. An archive's historical commit is not its current identity.
- **Scope:** the user outcome, owning paths and contracts, next smallest action, and
  the existing requirement/journey IDs it advances. Link the owning plan; do not copy it.
- **Evidence:** last applicable receipt per required layer, its tested source and
  limits; separately list implemented, compiled, executed and accepted facts.
- **Environment:** current resource owner, capacity, service/image/schema identities
  and permissions required before the next action. Cloud build exceptions do not
  transfer to a local machine.
- **Resume condition:** unresolved blocker, safe next command and expected result;
  stop rather than substitute an older binary, a fixture or a different environment.

Read the newest relevant source-bound records in `loop-state.json`; an older
summary is not proof for later changes. If records conflict, reconcile them against
source and receipts before changing the top-level summary. Keep historical evidence.
The current enterprise archive/local procedure is the existing
[local acceptance handoff](../handoff/local-codex-acceptance.md).

## 2. The task loop

### Cloud platform migration and enterprise upgrade (2026-10-11)

For this user-directed program, deliver a complete first implementation across
migration and enterprise upgrade scope before the consolidated regression pass.
Independent feature implementation must not wait for every preceding module to
pass full acceptance. Fix errors that block further implementation immediately;
record remaining defects for the consolidated pass. Keep implementation,
execution and verification status distinct, and retain the release safety gates.

The root owns integration, shared manifests, lockfiles and serialized builds.
Authorized parallel workers edit disjoint source paths directly. Build caches,
temporary checks, fixtures and evidence stay outside the repository. Daily native
server compilation and tests use `scripts/new/native-container.sh` and its single
Docker cache; do not retain a second host server Cargo target. Runtime/Worker use
their controlled Docker source workflow. Host compilation is reserved for explicit
macOS client/OS acceptance and the small std-only launcher bootstrap. Historical
cloud exceptions are not local build instructions. These workflows do not authorize
publication or production access and do not establish product acceptance.

Every unit of work runs `observe → act → verify → decide`.

| Stage | Meaning | Output |
| --- | --- | --- |
| observe | Read the code and the doc that owns the area. Reproduce the current behaviour. | A one-line statement of what is actually true now |
| act | Make the smallest change that satisfies the task. | A diff |
| verify | Run the gate from `docs/harness/commands.md` §7 that matches what you touched. | Command + real output |
| decide | Done, blocked, or needs a follow-up task. | An explicit verdict |

Never merge `act` and `verify` into a claim. A change is unverified until a gate has run.

## 3. Definition of done

A task is done when **all** hold:

- The change is scoped to the request — no drive-by refactors, renames, or reformatting.
- Public contracts (`docs/harness/architecture.md` §6) are unchanged, or changed deliberately with
  their gate re-run and `CHANGELOG.md` updated.
- The matching gate ran and passed, and you can quote the command and result.
- Anything not verified is stated explicitly.
- The doc that owns the area is updated in the same change, not "later".
- The result receipt identifies the tested base/head SHA and distinguishes passed, failed, blocked,
  and skipped checks.

## 4. Programs and large changes

Work that crosses several contracts or may outlive one context window becomes a **program** under
`deploy/runbooks/<program-name>/`. Its `loop-state.json` is the only execution-status authority.
The file contract is determined by the state's declared schema. The current harness structurally
inspects known schemas but treats several semantic/file issues as warnings; strict lifecycle,
package and evidence validation is queued in ARC-00. This document does not maintain a competing
hard-coded list.

A program must record lifecycle, base SHA, active package, blockers, last update, evidence receipts,
and its successor when superseded. Terminal or superseded programs may not retain an executable
`next_action`, `active_phase`, or `active_feature`. Their nested phase records remain immutable
historical snapshots even if they still say `ready/pending`; top-level lifecycle makes them
non-executable and a successor program must create new package state.

One large product outcome still has one primary implementation session and one writer worktree.
The session may execute sequential packages and survive context compaction through the program
ledger. Parallel agents are read-only explorers or reviewers unless the user explicitly creates a
different independent product outcome. See [`work-packages.md`](work-packages.md).

Rules:

- One package at a time runs `observe → freeze → act → verify → review → integrate → decide`.
- `passes: true` requires a named command, real output, tested SHA and permitted skip count.
- Status lives in `loop-state.json`, never only in prose or `docs/README.md`.
- A program is complete only when every required package is verified, blockers are empty, the
  integrated candidate passed its live gates, and rollback evidence matches the full release unit.

## 5. Worktrees and shared resources

- Two writers never share a worktree.
- A product-scale architecture change uses one owning worktree; packages are review boundaries,
  not concurrent branches.
- Docker, Docker-contained Rust image builds, migrations, local browser state and provider quota
  are machine-global resources. They are serialized under the integration owner; Rust fmt/check/test
  runs in hosted CI except for the explicitly scoped cloud exceptions in
  `runtime-and-secrets.md`.
- A worktree that does not own the running Compose project may inspect but may not mutate it.
- Shared integration paths listed in [`work-packages.md`](work-packages.md) are changed only after
  their implementation packages are stable.
- Read [`integration-and-rollback.md`](integration-and-rollback.md) for the verification ladder,
  low-memory rules and release receipts.

## 6. Encoding feedback

The harness improves by absorbing corrections. When a review comment, a bug, or a user correction
would apply to the *next* task too, encode it instead of just fixing the instance:

| Kind of correction | Where it goes |
| --- | --- |
| "The agent keeps doing X wrong" | A rule in `AGENTS.md` (only if load-bearing) or the owning harness doc |
| A boundary that must not be crossed | `docs/harness/architecture.md` + a test that fails when crossed |
| A command people keep getting wrong | `docs/harness/commands.md` + a Make target |
| A recurring class of defect | A gate in the Makefile, wired into `.github/workflows/ci.yml` |

Prefer a mechanical check over a written rule. A rule nobody enforces decays; a gate does not.

## 7. Git

- Do not commit or push unless the user asks.
- Branch off `main`; never commit directly to `main` for feature work.
- Conventional commit subjects, matching existing history: `feat(assistant): …`, `fix(api): …`,
  `refactor: …`, `docs: …`, `ci: …`, `gate: …`.
- One logical change per commit. Keep PRs short-lived — a follow-up correction beats a week-long review.
- `.github/PULL_REQUEST_TEMPLATE.md` states what a PR must show; verification evidence is not optional.

## 8. Repository quality

Dead code, stale docs, and drifted comments are tracked, not tolerated:

- The 2026-08-13 hygiene scan is historical input, not a current count. Rebuild the baseline after
  major merges.
- Before deleting anything, prove static and dynamic non-use, check entrypoints/contracts, and run
  direct gates before and after the deletion.
- Dead production code is deleted rather than moved into an `old/` directory; accepted ADRs,
  migrations and evidence are preserved or explicitly superseded.
- Empty/self-proving tests, false-green gates, stale active docs, unowned dependencies, oversized
  no-growth files, and ignored “durable” screenshots are quality defects.
- Follow [`repository-quality.md`](repository-quality.md) for classification, evidence retention,
  screenshot policy and deletion receipts.
