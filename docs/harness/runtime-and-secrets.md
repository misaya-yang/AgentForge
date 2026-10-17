# Runtime, Docker, and Secrets Rules

> Mandatory reading before any Docker, live-runtime, deployment, or E2E action.
> Referenced from `AGENTS.md`. These rules exist because breaking them has
> previously mutated the wrong checkout's containers or leaked provider keys.

**Schema:** `harness/runtime-and-secrets/v1`

---

## 1. Compose ownership (read this before touching Docker)

Multiple checkouts of this project can exist on one machine. Containers are shared
by name, not by directory.

- Before any Docker, live-runtime, or E2E action, verify ownership through container
  labels — especially `com.docker.compose.project.working_dir`. For this repo's
  containers (`ai-gateway-backend`, `ai-gateway-frontend`, `ai-gateway-agent-runtime`,
  `ai-gateway-knowledge-service`) the expected value is **this repository root**.
- If running `ai-gateway-*` containers carry another checkout's `working_dir`, they are
  the wrong runtime. Stop/remove that compose project first, then re-check labels before testing.
- Unexpected legacy container names almost always indicate a different checkout,
  not this repository.
- Every deployment/startup script must self-check ownership before `stop`, `up`, or `build`,
  and abort with a clear error rather than mutate the wrong runtime.
  The shared implementation is `assert_compose_owner()` in `scripts/new/common.sh`.
- All one-click startup, hot-update, E2E, and account-prep scripts live **in this repository**
  and operate from this repository root.

`make doctor` performs this check plus tool, memory, and port preflight.

## 2. Inspect before you act

- Run `docker ps`, `docker compose ps`, and the relevant health endpoints before validating anything.
- Docker Desktop running does **not** mean the gateway or assistant service is running.
- Do not run `docker compose up --build`, `docker compose build`, or heavy rebuilds by default.
  Prefer existing containers/images and `docker compose up -d`.
- Rebuild images only when source changes require it or the user explicitly asks.
- **Never** prune, remove, or reset Docker images, containers, volumes, or build caches without
  explicit user confirmation. If cleanup is needed, show the exact commands and their impact first.

## 3. Source updates

Native services execute compiled binaries. Use the existing native build/hot-update
workflow for changed services; copying source into a container does not apply a change.
Runtime/Worker images must match their source lock before acceptance. Reuse unchanged
images and build cache when their identities remain valid.

## 4. Low-memory machines

Keep `COMPOSE_PARALLEL_LIMIT=1` (the Makefile exports it), start and build services serially,
and watch `docker stats`. Stop rather than continue if the stack approaches the operator's
memory ceiling. The complete low-memory profile needs roughly 4 GiB available to Docker.

### 4.1 Rust compilation boundary

The owner corrected the development workflow on 2026-10-17: native server builds
and tests use Rust 1.99.0 inside Docker, one Cargo job, debug=0, incremental=0,
and one Docker-managed native development cache. `make native-check` and
`make native-test` use `scripts/new/native-container.sh`; selected tests use the
same entry point. Do not create another host server target directory. Keep
source mounts read-only and limited to public build inputs; never mount the
whole checkout with its private env or forward runtime credentials into a build.
Host Rust is reserved for explicitly selected macOS client/OS acceptance and a
small std-only operational launcher bootstrap, not the service dependency graph.
Preserve existing operational binaries and evidence during cache migration;
regenerable host server intermediates may be removed as part of the owner's
requested elimination of duplicate caches. Application data and unrelated
checkouts remain outside that cleanup scope.

Runtime/Worker composition remains pinned to Rust 1.95.0 and the controlled Docker
source/build workflow. Reuse this project's existing Buildx builder; do not create
another builder or bind-mount generated Cargo targets into the source checkout.
Builds are serialized and resource guarded. Do not automatically prune caches.
Compilation evidence does not establish image, release or end-to-end acceptance.

The current local project is `agentforge-dev-7801d4a9a4`, selected through
`COMPOSE_PROJECT_NAME` in ignored `.env.dev`. Its definitions are
`deploy/local/compose.yml` and `deploy/local/compose.runtime.yml`. Repository
relocation preserves this project and its volumes; recreate only its owned
containers when their working-directory labels need updating. Leave unrelated
projects alone. The owner has authorized local provider, database and browser
acceptance and test-admin management; historical live-test pauses are superseded.

## 5. Image and Compose distribution rules

- The default open-source quickstart uses the **versioned multi-architecture images** declared in
  `docker-compose.yml`. Source builds are opt-in through `deploy/compose/docker-compose.build.yml`.
- Do not reintroduce `build:` blocks into the base Compose file, and do not make normal users
  rebuild the project locally.
- Dockerfiles and Compose files must contain **no developer-machine absolute paths**.
- Published application images use immutable release tags or digests, support both `linux/amd64`
  and `linux/arm64`, and stay overridable through non-secret environment variables
  (`GATEWAY_IMAGE`, `FRONTEND_IMAGE`, `KNOWLEDGE_IMAGE`, `MIGRATE_IMAGE`,
  `AI_PLATFORM_AGENT_RUNTIME_IMAGE`, `AGENT_CAPABILITY_WORKER_IMAGE`).

## 6. Secrets

- Provider credentials and model configuration are **runtime inputs only**. Never place API keys in
  Docker build arguments, image layers, Dockerfiles, committed Compose values, or CI metadata.
- Local infrastructure credentials and bootstrap passwords are generated by
  `scripts/new/init-env.sh`. Do not hardcode, guess, or log their values.
  This covers `POSTGRES_PASSWORD`, `REDIS_PASSWORD`, `JWT_SECRET`,
  `AI_PLATFORM_INTERNAL_TOKEN`, `AI_PLATFORM_CAPABILITY_LEASE_SIGNING_SECRET`,
  `AI_PLATFORM_CAPABILITY_PROOF_SECRET`, `GATEWAY_ENCRYPTION_KEY`, and
  `DEFAULT_USER_PASSWORD`.
- External provider keys are user-supplied. Detect or copy them from the shell environment only
  while initializing local env — never print or invent them.
- Generated `.env` files are untracked and stay that way. Only `.env.example` is public.
- If `.env` is missing or Compose interpolation fails, fix the project init/env workflow.
  Do not force Compose with ad-hoc inline environment variables.
- Keep secret values redacted in logs, reports, and final answers.
- Do not hardcode DSN passwords or bootstrap admin passwords in source. The
  authority fails closed when no DSN is configured. Use `make migrate` or the native
  `database-authority` executable. Application `AUTO_INIT` is disabled; Compose and
  Helm run the same one-shot authority before service startup.
  `DEFAULT_USER_PASSWORD` is a runtime env value only. The validator accepts the
  documented local bootstrap default with a warning.
- Local Compose and `setup-dev.sh` publish host ports on `127.0.0.1`. The optional
  monitoring stack interpolates the same env secrets and must not embed `gateway123`
  or Grafana `admin/admin`.

## 7. Providers and readiness

- The default live chat provider is **DashScope/Qwen**, default model `qwen3.8-flash`.
- OpenAI is not the primary provider. An absent or invalid `OPENAI_API_KEY` is **not** a
  deployment blocker. The environment is blocked only when no usable
  `DASHSCOPE_CHAT_API_KEY` / `DASHSCOPE_API_KEY` is available, or the configured default Qwen
  model is unavailable.
- Google/Gemini/Vertex keys are fallback plus embedding/image support. Reuse them when mirroring
  SaaS config, but never make Google or OpenAI availability the core chat readiness gate while
  Qwen is configured.
- The default quickstart should require only `DASHSCOPE_API_KEY`; reuse it for DashScope
  embeddings unless the user explicitly supplies a dedicated embedding key.
- **Shell environment variables override `.env` during Compose interpolation.** If the shell holds
  a stale or fake `OPENAI_API_KEY`, start or recreate services with `OPENAI_API_KEY=` or
  `env -u OPENAI_API_KEY` so it does not leak into containers and confuse readiness checks.

## 8. Live tests

Before running live assistant tests, confirm `GATEWAY_BASE_URL` health and the required test
credentials. Report skipped versus failed tests distinctly — a skipped live test is not a pass.
Playwright `web/e2e/global.setup.ts` now defaults to the existing dedicated account and fails
closed when credentials or login are unavailable; omitted flags never provision or reset users.
Only a fresh isolated environment may explicitly set `E2E_PROVISION_ACCOUNTS=1` to initialize
accounts. `E2E_EXISTING_ACCOUNT_ONLY=1` remains an additional no-write guard.
