# AGENTS.md

Shared instructions for agents working on AI Gateway / AgentForge. The user's explicit
instructions take precedence. Read linked area documentation when needed.

## Current direction

- All platform services and operational tools are Rust. The optional Python user-code sandbox
  is retained; do not restore retired Python services or backup source trees.
- Work in this repository, not the Downloads import. The Git root is
  `/Users/misaya.yanghejazfs.com.au/misaya_project/AI-Platform-Rust-Rebuild`.
- The dots Rust import is complete. Its original import receipt is historical evidence;
  previous Python backup/recovery directories were subsequently deleted at the owner's request.
- The owner authorized local database, real-provider and built-in-browser acceptance, including
  management of test admin accounts. Original Git history is unavailable and is not required.
- Finish the repository cleanup, commit and push a checkpoint, then begin subsequent work and
  real-machine validation from a clean working tree. Commit/push is authorized for this checkpoint.
- Preserve existing source modifications, public APIs, business semantics and tenant isolation.
  Compilation, isolated tests and backup creation are milestones, not product acceptance.

## Repository map

| Path | Owns |
| --- | --- |
| `rust/platform-native/` | Native Cargo workspace: Gateway, Knowledge, SDK Worker, auth, clients, DB authority and tools |
| `rust/agent-runtime-overlay/` | Assistant Runtime, capability worker and shared capability contract |
| `web/` | React console; browser specifications go in `web/e2e/` |
| `apps/` | Service packaging and native OS helpers |
| `sdk/` | SDK delivery and OpenAPI contracts |
| `database/` | Frozen baseline, ordered migrations and verification |
| `deploy/local/` | Repository-owned development environment |
| `deploy/compose/` | Optional Compose overlays; primary `docker-compose.yml` stays at root |
| `deploy/runbooks/` | Programs and authoritative `loop-state.json` status |
| `scripts/` | Shell operational entry points |
| `docs/` | Architecture, current guides and dated history |
| `reports/` | Measured verification evidence |
| `src/`, `packages/` | Static catalogs/evaluation resources; no Python service implementation |

Native dependency boundaries are enforced by `make architecture-boundary-gate`. See
[architecture](docs/harness/architecture.md) and [product boundaries](docs/harness/platform-architecture.md).

## Commands and toolchains

Use [the canonical catalog](docs/harness/commands.md), `Makefile` and `harness.yml`.

| Purpose | Command |
| --- | --- |
| Machine preflight | `make doctor` |
| Existing local environment | `make dev-status` / `make dev-start` |
| Native compilation | `make native-check` with external `CARGO_TARGET_DIR` |
| Native verification | Affected package/target, or `make native-test` when justified |
| Harness contract | `make harness-check` |
| Frontend | App/node TS checks, lint, tests and build from the command catalog |
| Release config/runtime | `make validate-config` / `make validate` / `make status` |

Native Rust is pinned to 1.99.0; Runtime/Worker to 1.95.0. Frontend uses Node 26.11.1 and
pnpm 12.11.2. Native host builds are authorized with one Cargo job, no debug/incremental
output and an external target directory. Runtime/Worker builds use the controlled Docker
source workflow. Coordinate shared manifests, source locks and builds through the primary writer.

## Working principles

1. Stay focused on the outcome. Once there is enough evidence for the next useful step, act.
2. Resolve material ambiguities from code and context; ask only when the outcome depends on
   unavailable essential information or authorization.
3. Preserve continuity. Apply corrections and answer follow-ups, then resume unaffected work.
4. Prefer the simplest complete solution using existing patterns. Avoid unrelated refactors.
5. Keep documentation with its owning area. Dated receipts and frozen baselines describe past
   facts; do not rewrite them to claim current acceptance.
6. Use `reports/` for durable evidence and an external cache for scratch, captures and build
   artifacts. Keep the repository root limited to standard entry points.
7. Multi-session work follows [the existing harness](docs/harness/workflow.md); continue the
   current program rather than creating duplicate audits or roadmaps.
8. The owner permits up to nine Luna max subagents. Delegate disjoint paths only; one writer
   owns shared lockfiles, integration and Cargo/Docker builds.

## Runtime and secret handling

Before Docker, deployment or E2E work, read
[runtime-and-secrets.md](docs/harness/runtime-and-secrets.md).

- Reuse the single `agentforge-dev-7801d4a9a4` Compose project and selected ignored `.env.dev`.
  Preserve its volumes. Do not create duplicate infrastructure or additional Buildx builders.
- Verify ownership labels before mutations; leave unrelated checkouts and containers alone.
- Keep secrets, tokens, DSNs, local env files and private captures out of source control,
  logs, screenshots and reports. Only public examples are tracked.
- Database transitions go through the native authority and frozen verification. Do not patch
  fingerprints or bypass privilege checks to unblock a live workflow.
- User-authorized cleanup and checkpoint publishing may proceed. Force-push, history rewrite,
  volume deletion and unrelated destructive actions remain outside that scope.

## Verification and reporting

Run the gates appropriate to the change. Report actual successes, failures and unrun work
separately. Do not claim a test passed unless it ran and passed. Browser workflows require
real interaction and evidence. For major blockers, record cause, affected scope and next action,
then advance independent work. See [integration and rollback](docs/harness/integration-and-rollback.md).
