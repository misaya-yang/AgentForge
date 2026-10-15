# The AI Gateway Harness

> A *harness* is the structured environment — instructions, commands, contracts, gates, and docs —
> that lets an agent work on this repository autonomously and get a comparable result on any
> machine. It is the thing we maintain so that "which laptop is this running on" stops mattering.

**Schema:** `harness/index/v2` · **Contract:** [`harness.yml`](../../harness.yml) · **Gate:** `make harness-check`

---

## Read in this order

| # | File | What it gives you |
| --- | --- | --- |
| 1 | [`AGENTS.md`](../../AGENTS.md) | The contract: repo map, canonical commands, safety. Every agent reads this first. |
| 1b | [`platform-architecture.md`](platform-architecture.md) | The product law: five layers, five rules, what may be added where. Read before proposing a new surface, extension mechanism, or `AgentSpec` field. |
| 2 | [`architecture.md`](architecture.md) | Runtime topology, module ownership, dependency direction, contracts that must not drift. |
| 3 | [`commands.md`](commands.md) | The canonical command for every routine action, and which gate to run for what. |
| 4 | [`workflow.md`](workflow.md) | The task loop, definition of done, multi-phase program convention, git rules. |
| 4b | [`work-packages.md`](work-packages.md) | Single-owner contract for architecture and repository-wide changes. |
| 5 | [`runtime-and-secrets.md`](runtime-and-secrets.md) | Mandatory before any Docker, deploy, or E2E action. |
| 5b | [`integration-and-rollback.md`](integration-and-rollback.md) | Singleton-runtime ownership, verification ladder, integration and rollback. |
| 5c | [`repository-quality.md`](repository-quality.md) | Dead code/docs/tests, dependency, size and browser-evidence lifecycle. |
| 6 | [`docs/README.md`](../README.md) | Index of everything else: designs, plans, runbooks, reports. |

For a restored archive or local Codex continuation, use the existing
[local acceptance handoff](../handoff/local-codex-acceptance.md) after the cold start.
It is an operator procedure, not a second execution-status authority or permission
to start services.

`CLAUDE.md` adds Claude Code specifics on top of `AGENTS.md`. Agent, Cursor, and other tools read
`AGENTS.md` directly.

## The five principles

1. **The repository is the system of record.** If an agent cannot reach it in-context from a
   versioned file, it does not exist. No external directories, no "as discussed earlier",
   no knowledge that lives only on one machine.
2. **Progressive disclosure.** `AGENTS.md` is a short table of contents, not an encyclopedia.
   Depth lives in `docs/`, one hop away, so context is spent on the task and not on preamble.
3. **Boundaries become mechanical.** Architectural rules that only exist in prose decay. Every
   invariant must name its real gate or a queued work package that will add one; prose must not
   claim enforcement that does not yet exist.
4. **Verification is a command, not a claim.** Each kind of change maps to a named gate in
   `commands.md` §7. "Done" means that gate ran and passed.
5. **Feedback gets encoded.** A correction that would apply to the next task becomes a rule, a
   command, or a gate — see `workflow.md` §6.

## What the harness is made of

| Layer | Artifacts |
| --- | --- |
| Instructions | `AGENTS.md`, `CLAUDE.md`, `.claude/commands/*.md` |
| Contract | `harness.yml` — canonical commands, required docs, loop stages |
| Commands | `Makefile` targets backed by `scripts/new/*.sh` |
| Gates | `make doctor`, `harness-check`, `validate*`, `status`, `test-isolation`, `eval-e1-gate`, `verify-*` |
| CI | `.github/workflows/ci.yml` runs a current subset of gates on `main` and `dev`; ARC-00 closes the parity gap |
| Knowledge | `docs/` (design + plans), `deploy/runbooks/` (programs), `reports/` (evidence) |
| Observability | `/health/ready`, `/metrics`, `docker/monitoring/`, trace + eval console |

## Truth hierarchy

Different documents own different kinds of truth:

| Question | Authority |
| --- | --- |
| What runs and who owns it now? | [`architecture.md`](architecture.md) + current code/Compose |
| What product boundary should outlive this implementation? | [`platform-architecture.md`](platform-architecture.md) + accepted ADRs |
| What should be built next? | One queued/active plan linked from [`docs/README.md`](../README.md) |
| What is executing now? | The one active program's `loop-state.json` |
| What actually passed? | A dated receipt/report with command, SHA, environment and result |
| How may a large change be divided? | [`work-packages.md`](work-packages.md) |

Queued plans are not current implementation facts. Historical reports and superseded runbooks are
evidence, not instructions. If two programs claim the same domain, stop and resolve the lifecycle
conflict before changing code.

## Keeping it honest

`make harness-check` currently verifies the structural harness has not rotted:

- every command declared in `harness.yml` still exists as a Make target;
- every required doc exists and is non-empty;
- `AGENTS.md` and `CLAUDE.md` stay within their line budgets (long instruction files measurably
  reduce agent task success);
- relative links inside the harness docs resolve.

It runs in CI as part of the *Compose and Harness* job. Static imports and core ownership have their
own `architecture-boundary-gate` and `core-boundary-gate`; affected-gate selection and CI enforcement
also exist. The structural check alone does not prove their results, program lifecycle semantics,
or non-zero collection without unexpected skips. The active `agent-platform-vnext` phase-one program
owns the remaining exception, lifecycle and evidence conformance work.
When the structural check fails, fix the harness — do not raise the budget.

## When to change the harness

| Situation | Change |
| --- | --- |
| A new service or package | `architecture.md` §2–3 + an isolation test |
| A new routine command | `Makefile` + `commands.md` + `harness.yml` |
| A recurring agent mistake | A rule in `AGENTS.md` (only if load-bearing) or the owning doc |
| A new class of regression | A gate in the `Makefile`, wired into CI |
| A boundary decision | An ADR in `docs/architecture/` |
| Work spanning many sessions | A program under `deploy/runbooks/` (`workflow.md` §4) |
| A cross-service or architecture change | A single-owner package sequence under [`work-packages.md`](work-packages.md) |
| Dead code/docs/tests or generated evidence | [`repository-quality.md`](repository-quality.md) |
| Docker/DB/provider integration or rollback | [`integration-and-rollback.md`](integration-and-rollback.md) |
