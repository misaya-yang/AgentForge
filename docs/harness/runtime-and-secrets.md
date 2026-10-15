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

## 3. Prefer hot update over rebuild

For source-only Python changes, copy the changed source into `ai-gateway-backend` /
`ai-gateway-knowledge-service` and restart those services — `make hot-update` does this.

Rebuild images only for dependency, Dockerfile, frontend bundle, or base-image changes.
Do not run pip-heavy builds just to refresh Python source.

## 4. Low-memory machines

Keep `COMPOSE_PARALLEL_LIMIT=1` (the Makefile exports it), start and build services serially,
and watch `docker stats`. Stop rather than continue if the stack approaches the operator's
memory ceiling. The complete low-memory profile needs roughly 4 GiB available to Docker.

### 4.1 Rust compilation boundary

#### Cloud native-migration exception (2026-10-11)

For the user-directed SaaS migration in `rust/platform-native` on the cloud
workspace, direct Rust formatting, checking, testing and compilation are allowed.
Use one Cargo job, no debug information, and nonincremental builds. Keep Cargo
targets, verification harnesses, fixtures, logs and other generated artifacts
outside the repository. This exception does not authorize deployment, use of
production credentials, changes to the user's computer, or changes to the
`rust/agent-runtime-overlay` Runtime/Worker build boundary below.

#### Cloud Runtime/Worker completion exception (2026-10-12)

The user has approved choosing the direct cloud build path to complete the
current AI Gateway upgrade when Docker is unavailable. In this cloud task only,
Runtime/Worker candidate formatting, dependency resolution, checks, tests and
compilation may run in an isolated external workspace. Use one Cargo job, no
debug information and nonincremental builds; keep generated targets, tools,
logs and candidate lockfiles outside the repository. Check available disk before
heavy work and stop before exhausting it. Do not delete caches to make room
without authorization. This exception does not authorize changes to sandbox or
system security, user-computer execution, publication, deployment or credentials.
A host-built candidate is compilation evidence, not proof of Docker image,
release identity, cross-platform or end-to-end acceptance.

#### User-directed local development baseline (2026-10-15)

For this restored checkout, the user explicitly requested local Rust minimal
toolchains, components, caches and compilation checks. Native 1.99.0 host
fmt/check/test/build is therefore authorized for this baseline, with one job,
no debug/incremental and targets outside the repository. Runtime 1.95.0 is
installed separately; its existing composed-source Docker builders remain the
Runtime/Worker product build entry points. See [local development](local-development.md).

Outside the cloud exceptions and user-directed local baseline above, do not run `cargo`, `rustc`, `rustfmt`, `clippy`, Rust `check/test/build`, or
`rust-changed-crate-gate` directly on the host. Host Rust builds create large repository/toolchain
artifacts and are not an accepted verification path on this machine.

Rust work is split deliberately:

- hosted CI runs fmt/check/changed-crate tests and publishes their evidence;
- local candidate binaries are compiled only inside the existing Docker multi-stage BuildKit
  builders (`agent-runtime-*-build-local` / `scripts/rust/build-update.sh`);
- `CARGO_BUILD_JOBS=1` is a container-builder/CI resource limit, not permission to invoke host Cargo;
- builder `target` directories stay in Docker cache mounts and must never be bind-mounted into the
  repository;
- local product acceptance runs the resulting Runtime/Worker images through API, UI, provider and
  rollback journeys.

Docker Rust builds remain serialized and resource-guarded. Do not automatically prune Docker or
Cargo caches after a build. If disk cleanup is later requested, remove only identified obsolete
Runtime/Worker builder cache or candidate images; never use broad prune commands implicitly.

## 5. Image and Compose distribution rules

- The default open-source quickstart uses the **versioned multi-architecture images** declared in
  `docker-compose.yml`. Source builds are opt-in through `docker-compose.build.yml`.
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
  authority fails closed when no DSN is configured. The legacy single-file entrypoint
  `database/run_migration.py` is retired and fails closed unconditionally,
  before reading a DSN or executing SQL; use `make migrate` or
  `python -m database.authority migrate`. Application `AUTO_INIT` is disabled;
  Compose and Helm run the same one-shot authority before service startup.
  `DEFAULT_USER_PASSWORD` is a runtime env value only; the local documented bootstrap
  default is accepted by `validate-env.sh` with a warning, never as a Python fallback.
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
