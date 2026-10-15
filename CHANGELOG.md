# Changelog

## Unreleased

### Connector authorization return

- OAuth callbacks accept an optional provider error with no code. Valid issued
  state returns to personal connections with a fixed, non-authoritative failure
  hint; no provider descriptions, credentials, or arbitrary redirect are reflected.
  Missing both values remains 422; ambiguous code/error and invalid state reject.
  Existing token exchange failures remain HTTP errors and are never auto-retried.


### Scheduled receipt history

- Extend native shared client, platformctl and desktop with bounded Older/Latest history and exact task/execution receipt recovery. Reads preserve owner/tenant authorization and never dispatch or replay work.

- Add owner-scoped cursor pages and exact scheduled task/execution reads; connect older-history navigation and stable deep links in the Web console. Scheduler execution and Python hybrid behavior remain unchanged.

### Saved-connection Knowledge source binding

- Knowledge Sources can bind an existing owned Confluence connection to a verified
  space homepage and descendants, then use explicit manual synchronization. Safe
  paginated selection, current authorization, idempotent save and exact receipt
  recovery are included; credentials, scheduling and live acceptance remain separate.

### Existing-binding Confluence manual synchronization

- Add explicit manual-sync submission and exact request-ID progress/result readback
  through the existing signed Python Knowledge proxy. Sources exposes eligible
  manual actions and retains uncertain submissions for exact readback.
- Revalidate actor, tenant, ownership and dataset authority during execution;
  preserve shared documents when one binding removes its source reference.
- Keep provider failures visible, preserve retry ownership and settle gracefully
  interrupted work. Hard-crash task reconciliation and live provider/database/
  browser acceptance remain separate release gates; no scheduler is enabled.
- The Gateway OpenAPI surface remains the existing Knowledge wildcard proxy.
  Downstream request, receipt and uncertainty semantics are documented in the
  owning [Knowledge plan](docs/plans/product-feature-completion-2026-09-23/03-knowledge.md#manual-sync-additive-api-contract).

### Python generic-task result persistence

- Add task-owned typed-result persistence/reconstruction using the native private
  result format; preserve bytes versus JSON kinds and unrelated current metadata.
- Project typed binary results consistently as base64 in task detail/list/result
  responses and callbacks. Raw JSON cannot create a typed-result marker.
- Keep None-as-omission and legacy falsy-save compatibility; reject unsupported
  fractional-second timezone offsets instead of silently changing their instant.
- Controlled offline pipeline/regression evidence does not establish PostgreSQL,
  provider, cross-process compatibility or deployment acceptance.

### Python generic-task cancellation

- Return HTTP409 with `detail: task_state_conflict` when cancellation observes a
  completed or failed task, preserving its receipt and result. Repeated observed
  cancellation is idempotent. Ownership checks remain unchanged. This bounded
  guard does not add atomic storage transitions or resolve concurrent/stale-cache
  races in the Python profile.

### Native enterprise implementation

- Add native durable scheduled-task APIs and epoch 011 storage, with Web,
  desktop and platformctl management. Execution admission follows the current
  owner identity, bounded concurrency, and explicit unknown-launch handling.
- Add opt-in Dify chat streaming, Google Drive metadata search and compatible
  provider model discovery with manual capability admission for unknown IDs.
- Add a caller-filtered plugin catalog and metadata-only configuration audit
  reads for administrators; plugin installation and runtime upgrades remain
  separate, incomplete capabilities.
- Add enterprise workbench/navigation, tenant tool-policy management, knowledge
  processing diagnostics, and tenant connector-template overrides. Disabled
  overrides now shadow enabled global templates in both native and legacy APIs.
- Add shared native management clients and CLI packaging definitions. These
  implementation changes do not declare full product or release acceptance.

### R3 evaluation contracts

- Complete local J14–J18 acceptance with immutable Agent A/B runs, approval
  and cancellation, audience revocation and rollback, Trace review, controlled
  provider/judge failures, original-turn restart recovery, and selected retries.
- Use one complete dataset manifest for execution, rescore and linked release;
  preserve unknown measurements and native tool receipts. Trace imports remain
  pending and idempotent without overwriting manual review.
- Keep unchanged provider revisions stable across Gateway restarts, accept
  valid channel-bound V2 controls, and bind Trace attribution from the original
  session/snapshot. Historical recovery only fills missing records.
- Repair public and Embed launches: canonical expiry identity, the established
  Runtime permission projection, explicit empty tool catalogs, and signed Embed
  token/origin headers. Current access and origin checks remain enforced.
- Add the optional `output_equals` assertion for exact complete-response contracts
  after trimming surrounding whitespace. Quoting an expected refusal phrase in an
  unrelated answer no longer satisfies a case that explicitly chooses this assertion.
  Existing substring assertions and frozen run scores retain their semantics.

### Phase-one tenant boundaries

- Expose the authenticated tenant through `/api/v1/auth/me`; Eval verifies it before creating a
  candidate. V2 thread creation accepts an optional tenant precondition and rejects binding changes
  before accessing session storage. Missing or mismatched identities fail closed without trace writes.
- Store Knowledge role grants as tenant-bound subjects while preserving the public user/role ACL
  contract, explicit user sharing, public visibility and creator ownership. Epoch 001 adds the
  old-reader/writer-safe discriminator and constraints, and removes Knowledge account/RBAC-table
  privileges. Gateway now owns current actor resolution and dataset deletion password confirmation.
- Epoch 002 adds Eval claim leases, heartbeat and runtime-handle reconciliation; the application
  compatibility window requires epoch 2. These changes do not by themselves certify a complete
  release, native CLI acceptance, frozen rollback or Git merge.

### Product convergence (program: deploy/runbooks/product-convergence)

- Removed the Confluence fossil stack: the gateway REST surface (`src/api/v1/confluence.py`,
  `src/api/schemas/confluence.py`) that returned 503 unconditionally since KB was split into a
  microservice, the zero-reference gateway-side sync code (`src/services/knowledge/confluence/`),
  and the console UI (`web/src/pages/confluence/`, the Confluence tab of `/tasks`, and the
  Confluence sync cards in knowledge dataset sources). Confluence connectivity is now exclusively
  the connector stack (`src/api/v1/connectors.py`); `ConnectorsPanel` in the assistant chat was
  re-pointed to the connector catalog/OAuth/MCP APIs.
- Removed the standalone exam management surface (`src/api/v1/exams.py`, `/exams` routes, the
  Services-page exams tab, i18n keys). Exam data tables remain in place; the capability returns
  through the assistant quiz tool and the ai-quiz plugin.
- Trimmed `src/api/v1/quiz.py` to a deprecated shim: deleted `POST /generate/stream`,
  `GET /list`, and `GET /{quiz_id}/attempts/export`; the load-bearing generate/get/submit/
  attempts/delete/share endpoints and the public share routes stay (consumed by the in-chat
  quiz card, QuizShareDialog, and public quiz page).

### Platform architecture

- Accepted ADR-006 and ADR-007: Agent Runtime is the single target Agent
  kernel, with a bounded external source, immutable source/schema/SBOM/OCI lock,
  an internal model-only Gateway plane, append-only PostgreSQL ThreadStore, and
  a Python capability service that cannot own a model loop. Added the
  `agent-runtime-single-kernel` program and fail-closed
  `make agent-runtime-source-contract` supply-chain gate; no runtime traffic is
  moved by this checkpoint.
- Added tenant-configurable model capability profiles and typed Provider
  Capability Adapters. Reasoning options, budgets, prompt caching, and native
  search now flow from the Gateway model control plane through Assistant and
  the console without model-name or user-prompt keyword routing.
- Added [`docs/harness/platform-architecture.md`](docs/harness/platform-architecture.md): the product
  law behind form and capability diversification — four layers (surfaces / contract / kernel /
  extensions over the gateway), five rules, the `permissions` + `capabilities` coexistence decision,
  and the admission table for what may live in the kernel versus ship as an extension.
- Added the `agent-contract-unification` program under `deploy/runbooks/`: seven phases that make the
  built-in assistant an `AgentSpec` instance, unify subagents into the same type, move builder
  settings out of the chat request, put the console on the public runtime contract, extend the SDKs
  to it, and gate domain logic out of the shared core package.
- `make harness-check` now validates each program against its own `loop-state` schema version rather
  than asserting one contract on all of them, which removed eight permanent warnings.

### Agent harness and documentation

- Added an explicit agent harness: `harness.yml` (machine-readable contract) plus
  `docs/harness/` covering architecture boundaries, the canonical command catalog, the
  working agreement, and the mandatory Docker/secret rules.
- Rewrote `AGENTS.md` as a short tier-1 contract (repo map, canonical commands, gates, safety)
  and added `CLAUDE.md` for Claude Code specifics. Both are line-budgeted.
- Added `make harness-check` (`scripts/harness/check_harness.py`) and wired it into CI: it fails
  when a declared command no longer exists, a required doc is missing, an instruction file exceeds
  its budget, or a harness link breaks.
- Stopped ignoring `docs/` in `.gitignore`. About 100 design, plan, and program files were
  invisible to every other machine and every coding agent; the repository is now the system of
  record. Root-only scratch patterns are anchored so they no longer swallow real documents.
- Reorganized `docs/` into `harness/`, `design/`, `plans/`, `research/`, `architecture/`, and
  dated `archive/`, with `docs/README.md` as the index.
- Consolidated all multi-session programs under `deploy/runbooks/`; `loop-state.json` stays the
  authoritative status. Archived the root `HANDOFF.md`, which depended on a directory outside
  this repository, and `PLATFORM_REVIEW_2026-07-14.md`.

### Workspace hygiene

- `make harness-check` now enforces where files go: Playwright specs only under `web/e2e/`
  (or `web/src/`, `sdk/`), Playwright configs only under `web/`, and no screenshots, archives, or
  stray test files at the repository root. This is what previously let verification screenshots
  accumulate in the root.
- Added [`web/e2e/README.md`](web/e2e/README.md) documenting the E2E layout, the five Playwright
  configs, where run artifacts go, and how to point browser tooling at `tmp/browser/` instead of
  the repository root. Added `web/e2e/fixtures/` for static test data.
- Removed root strays: three verification screenshots, a scratch orchestration script, and the
  `.playwright-mcp/` dump directory.
- Removed `claude-code`, an orphan git submodule reference with no `.gitmodules` entry and an empty
  working directory, and `.kiro/skills/` — 21 tracked symlinks into the gitignored `.agents/`
  directory that were dangling on every checkout, including this one.

### Deployment

- Added `make doctor`: a read-only host preflight covering required tooling, Compose v2, Docker
  memory and disk headroom, host ports, `.env` presence/mode/placeholder keys, and — blocking —
  whether running `ai-gateway-*` containers belong to this checkout.
- Documented the shortest correct startup path as `make doctor && make quickstart && make status`
  in `README.md` and `DEPLOY.md`.
- CI now verifies the harness contract and the syntax of the deploy scripts.

### Model defaults

- Made `DEFAULT_MODEL` (unprefixed env var) the single deployment default:
  SDKs, the console, and the gateway now omit `model_id` unless a caller
  explicitly requests one, and the assistant service applies the deployment
  default. Added `.env.example` `DEFAULT_MODEL` documentation with an
  optional non-empty format check in `validate-env.sh`.

### First-run onboarding and navigation

- Added a first-run onboarding flow: `GET /api/v1/setup/state` reports provider
  setup status, the console shows a dismissible setup banner and a three-step
  dashboard checklist until a provider is configured, and the existing
  `MODEL_SETUP_MODE` env var (`ui` | `environment`) is forwarded to the gateway.
- Regrouped the sidebar navigation into 使用/Use (assistant, agents),
  构建/Build (knowledge, playground, eval), and 治理/Govern (services, users,
  tasks, settings) sections, with Dashboard ungrouped on top; the model-tester
  rail stays flat. The playground entry is now labelled 模型调试 / "Model Debug".
- Added one-line "why you are here" hints to the assistant welcome screen, the
  playground empty state, and the agents empty state.
- Added a frontend CI step running `pnpm -C web i18n:check`, and extended the
  open-source e2e suite with first-run and nav-group specs.

### Quiz plugin and artifact shares

- Replaced the standalone quiz-generation API (`POST /assistant/quiz/generate`) with the
  in-chat `generate_quiz` assistant tool as the supported path, documented by the new
  `agent-plugins/ai-quiz` data-only plugin (skills + inert agent; no MCP server — stdio
  children have no DB/KB/model access).
- Moved quiz generation/orchestration from `packages/ai-gateway-core` into
  `apps/assistant-service/core/quiz/`; grading stays in the shared leaf because the gateway
  grades anonymous share submissions in-process. Deleted the orphaned exam service.
- Generalized quiz sharing into kind-generic `artifact_shares` (migration 083, with a frozen
  payload/answer-keys snapshot backfilled from `quiz_shares`): new
  `POST/DELETE /api/v1/artifact-shares` endpoints, and the public `/quiz/shared/{code}`
  routes now alias over the same rows so legacy links stay valid. Demo seed data was
  re-pointed at `artifact_shares`.

### Connectors

- Added a connector catalog admin API (`/api/v1/connectors/admin/configs`) with CRUD,
  an enabled toggle, a delete guard while users are connected, and write-only
  `client_secret` handling via `RedactedValidationRoute`; gated by `console:settings:view`.
- Added connector `mode` semantics (`live`/`ingest`/`both`) via migration
  `084_connector_modes.sql` (backfills `both` where `supports_sync`).
- Added the connector catalog settings page at `/settings/connectors` with full
  en-US / zh-CN i18n coverage.
- Made catalog-model connector bindings effective only when the provider has an
  enabled `connector_configs` row and the calling user holds a connected
  `user_connectors` row; added stable deny codes
  (`CONNECTOR_CATALOG_UNAVAILABLE`/`INGEST_ONLY`/`PRINCIPAL_DENIED`/`NOT_CONNECTED`)
  and documented the semantics in the agent-studio architecture contract.
- Renamed `src/connectors` to `src/transports` and `ConnectorType` to
  `TransportType` (field name `connector_type` unchanged); no behaviour change.
- Added `docs/design/connectors.md` (live vs. ingest modes, OAuth token custody,
  MCP tool model) and `docs/connectors/ingest-mode.md` (KB upload reuse, token
  relay boundary — design intent only this round).

### Runtime

- Bundled document generation as the trusted `ai-docgen` Agent Plugin inside
  the Assistant image and removed the standalone docgen service, image, port,
  signed-download endpoint, and release artifact.
- Isolated Agent Plugin MCP parsing in a focused validation module.

### Open-source standalone release

- Repositioned the project as a standalone AI Gateway platform with a general AI assistant and a knowledge-base service.
- Removed legacy domain-specific application surfaces, mock servers, ingestion scripts, tests, and frontend pages from the default product path.
- Removed private repository memory, stale planning files, generated SDK archives, and outdated product-specific test/refactor artifacts.
- Generalized SDK package names, CLI storage paths, Java namespaces, docgen package naming, sample URLs, Helm defaults, and docgen fixtures.
- Renamed the gateway-authored LangGraph runtime model payload to `gateway_model`.
- Added a root `.env.example` for local Docker deployment without committing real secrets.
- Added open-source governance files for license, contributing, security reporting, support, code of conduct, issue templates, and pull request review expectations.
- Added strict environment validation for required secrets, model keys, embedding keys, provider names, port conflicts, CORS JSON shape, Docker Compose interpolation, and runtime dependency health.
- Updated `README.md` with a downloader-ready quickstart, required env variables, deployment commands, validation commands, microservice communication notes, DB/Qdrant volume caveats, and troubleshooting.
- Added deterministic local demo data with dry-run/apply commands for knowledge-base, public share, quiz, and exam route checks.
- Added a public release checklist that covers local gates, GitHub artifact workflows, post-release smoke checks, and rollback handling.
- Updated CI toward stable open-source contributor checks: script contracts, focused tests, Docker Compose rendering, harness JSON checks, frontend typecheck/lint/build, and release documentation gates.

### Startup and deployment

- Updated `docker-compose.yml` to use the fixed project name `ai-gateway` and project-scoped volumes.
- Switched default storage to local persisted volumes so a first-time open-source deployment does not require S3.
- Changed the default frontend host port to `8081` to avoid privileged/commonly occupied port `80`.
- Added healthy dependency ordering across PostgreSQL, Redis, Qdrant, knowledge service, assistant service, MCP docgen, gateway, and frontend.
- Updated Qdrant health checks to avoid relying on missing container tools.
- Updated gateway health checks to use `/health/ready` instead of shallow process liveness.
- Updated Make targets:
  - `make validate-config`
  - `make validate`
  - `make quickstart`
  - `make stop` now stops containers without deleting volumes.

### Microservice communication

- Standardized Docker-internal service URLs:
  - `ASSISTANT_SERVICE_URL=http://assistant-service:8093`
  - `KB_SERVICE_URL=http://knowledge-service:8092`
  - `MCP_DOCGEN_SERVICE_URL=http://mcp-docgen-server.internal:8765`
- Kept assistant-service private to the Docker network.
- Kept knowledge-service host-bound to `127.0.0.1` for local debugging.
- Added gateway readiness checks for downstream knowledge, assistant, and MCP docgen services.
- Preserved HMAC protection through `GATEWAY_ASSISTANT_SHARED_SECRET` across gateway-to-assistant and gateway-to-knowledge calls.

### Knowledge base

- Simplified the knowledge service toward a provider-neutral KB/RAG surface.
- Removed domain-specific chunking, metadata, authority, synonym, ingestion, and evaluation paths from the default code path.
- Exposed embedding provider configuration through root env variables:
  - `KB_EMBEDDING_PROVIDER`
  - `KB_EMBEDDING_API_KEY`
  - `KB_EMBEDDING_MODEL`
  - `KB_EMBEDDING_DIMENSION`
- Added compose mappings for Gemini, DashScope, and SiliconFlow embedding keys.

### Assistant

- Kept the general assistant service as an extracted microservice behind the gateway.
- Continued to pass common provider keys into gateway, assistant, and docgen containers where relevant.
- Retained frontend and SDK paths around the general assistant and knowledge-base flows.

### Safety

- Ensured root `.env` remains ignored.
- Ensured `.env.example` is explicitly committable.
- Avoided printing secret values from the validator.
- Added runtime checks for authenticated PostgreSQL and Redis access rather than only checking container liveness.
