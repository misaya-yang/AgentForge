# AgentForge

Development baseline: `make local-bootstrap`, then `make local-baseline`.
See [local development and gate results](docs/harness/local-development.md) for the
restored-source boundary, toolchain, and external prerequisites.

AI Gateway is an open-source AI application platform for gateway routing, a general AI assistant, and a knowledge-base service backed by PostgreSQL, Redis, and Qdrant.

The default Docker setup is intended for a local first run. `make quickstart` generates local infrastructure credentials, pulls versioned `linux/amd64` or `linux/arm64` application images, starts the complete stack, runs pending database migrations, and performs runtime validation. Configure a provider either before startup through environment variables or after startup in the web console.

## Services

| Service | Role | Default URL |
| --- | --- | --- |
| Frontend | Web console | `http://localhost:8081` |
| Gateway | Public API, auth, proxy, sessions | `http://localhost:8080` |
| Agent Runtime | Sole Agent loop, Thread/Turn/Item lifecycle | Internal: `http://agent-runtime:8094` |
| Capability worker | Tools, Office, code, MCP, and long jobs | Internal: `http://agent-capability-worker:8095` |
| Knowledge service | KB CRUD, document ingestion, retrieval | `http://localhost:8092` |
| PostgreSQL | Primary relational database | `127.0.0.1:5432` |
| Redis | Cache, sessions, queues | `127.0.0.1:6379` |
| Qdrant | Vector database | `127.0.0.1:6333` |

## Quick Start

Prerequisites:

- Docker and Docker Compose
- `make`
- About 4 GiB available to Docker for the complete low-memory profile
- A model-provider API key (it can be entered before or after startup)

Check the machine first. `make doctor` is read-only — it never starts, builds, or changes
anything — and reports missing tools, Docker memory, occupied ports, and containers that belong to
a different checkout of this project:

```bash
make doctor
```

1. Choose one model setup path.

CLI/environment setup for the default Qwen Assistant and KB:

```bash
export DASHSCOPE_API_KEY='your-key'
```

Or skip the export and use the web setup after the stack starts. With the
default `MODEL_SETUP_MODE=ui` the console boots without provider keys: the
dashboard shows a setup banner and first-run checklist, and the Services page
is where you configure a provider. Once a provider is configured the banner
disappears automatically. Set `MODEL_SETUP_MODE=environment` in production
when startup validation must reject that setup-only state.

2. Pull the fixed release images and start the complete stack:

```bash
make quickstart
```

The initializer creates `.env` with mode `0600` and generates PostgreSQL,
Redis, JWT, internal-runtime, capability, provider-encryption, and bootstrap-admin secrets without
printing their values. The same DashScope key is used for Qwen chat,
`text-embedding-v4`, and document generation unless you explicitly configure a
dedicated provider key. Never commit `.env`; only `.env.example` is public.

Maintainers who need to test the current checkout instead of published images
use the explicit source-build path. Builds are serialized by default to reduce
peak memory:

```bash
make quickstart-build
```

If your populated env file lives outside this repository, pass it through
`ENV_FILE` instead of copying it into the working tree:

```bash
make validate-config ENV_FILE=/path/to/.env
make validate ENV_FILE=/path/to/.env
make status ENV_FILE=/path/to/.env
```

Generated automatically for local quickstart:

- `POSTGRES_PASSWORD`
- `REDIS_PASSWORD`
- `JWT_SECRET`
- `AI_PLATFORM_INTERNAL_TOKEN`
- `AI_PLATFORM_CAPABILITY_LEASE_SIGNING_SECRET`
- `AI_PLATFORM_CAPABILITY_PROOF_SECRET`
- `GATEWAY_ENCRYPTION_KEY`
- `DEFAULT_USER_PASSWORD`

Optional overrides remain available for custom providers, dedicated embedding
credentials, ports, image references, and production scaling. Published image
references use immutable `2.0.0` tags by default and can be replaced through
`GATEWAY_IMAGE`, `FRONTEND_IMAGE`, `KNOWLEDGE_IMAGE`, `MIGRATE_IMAGE`,
`AI_PLATFORM_AGENT_RUNTIME_IMAGE`, and `AGENT_CAPABILITY_WORKER_IMAGE`.

3. Validate an existing configuration without starting containers:

```bash
make validate-config
```

4. Open the app:

- Frontend: `http://localhost:8081`
- API docs: `http://localhost:8080/docs`
- Gateway readiness: `http://localhost:8080/health/ready`
- Gateway metrics: `http://localhost:8080/metrics`

For web model setup, sign in as the generated bootstrap admin, open
`http://localhost:8081/services`, add a provider with the guided wizard, test
the connection, and synchronize its model catalog. The Assistant and KB
embedding pipeline resolve the saved encrypted provider key for that tenant on
the next operation; no service restart is required. Then open
`http://localhost:8081/assistant`.

Provider status separates configuration from observation. `/api/v1/health/providers` reports
`unverified` until an explicit connection test completes, then `healthy` or `unhealthy` with
the actual `last_check`; after 5 minutes it reports `stale`. Receipts are tenant scoped and
process local: saving provider configuration or restarting Gateway requires a new test.
Usage summaries return a null success rate when no requests were observed. The console
distinguishes empty data from collection failures and unavailable collectors.

Optional local demo data is available after the stack is running:

```bash
make seed-demo       # preview the deterministic SQL and routes
make seed-demo-apply # load the records into the configured local database
```

Run `make seed-demo` to preview the seeded routes and scope without writing to
the database.

The local bootstrap admin is `admin@example.com`; its generated password is
stored only in the local `.env` file and is never logged.
For non-local deployments, change `AUTH_ALLOWED_EMAIL_DOMAIN`, create your own
administrator, and rotate the bootstrap password immediately.
If `VITE_AUTH_EMAIL_DOMAIN` is set, keep it equal to
`AUTH_ALLOWED_EMAIL_DOMAIN`; otherwise username-only login will append a domain
that the backend rejects.
Leave `VITE_SUPPORT_EMAIL` blank unless you have a real support mailbox; the
frontend then defaults it to `admin@AUTH_ALLOWED_EMAIL_DOMAIN`.

The example env also sets generous localhost rate limits such as
`RATE_LIMIT_IP_LIMIT`, `RATE_LIMIT_NORMAL_LIMIT`, and
`RATE_LIMIT_ASSISTANT_CHAT_LIMIT` so first-run page loads and assistant testing
do not trip 429s. Tighten them for shared or production deployments.

## Validation Commands

Use these commands when debugging startup or before publishing a deployment:

```bash
make validate-example-config # checks committed .env.example shape and compose rendering
make validate-config   # checks .env and docker compose config only
make validate          # checks .env plus running Postgres, Redis, Qdrant, and services
make status            # prints compose state and service health; exits nonzero if any health check fails
docker compose ps
```

Makefile validation, deploy, status, migration, backup, stop/restart/log, and
development helper targets accept `ENV_FILE=/path/to/.env`. The default remains
`.env`.

`make validate-example-config` is the portable open-source check for contributors
and CI. It proves the committed example configuration is structurally usable
without requiring private secrets. It is not a production release gate.
The tracked follow-up checklist lives in
`deploy/runbooks/open-source-env-readiness-todo.md`.

Lower-level deploy, migrate, backup, and setup-dev scripts also accept
`--env FILE` when called directly.

The validator intentionally does not print secret values. It fails fast on missing keys, placeholder values, weak secrets, moving application-image tags, duplicate ports, invalid embedding provider names, failed compose interpolation, failed authenticated PostgreSQL/Redis checks, failed Qdrant/service health checks, and failed gateway metrics scraping. The legacy documented local bootstrap password remains accepted for backward-compatible local environments and is reported as a warning.

## Development Without Rebuilding

`make quickstart` uses published images and never builds source. Use
`make quickstart-build` once when working on this checkout. After that, backend
code changes do not need a full image rebuild during development.

For an existing local deployment, `make hot-update ARGS="--all"` copies Gateway,
Knowledge API/worker, shared core/contracts and frontend assets. Existing stopped
containers can receive the files before the final restart, so an old application
need not start against a newly migrated schema. `--no-restart` leaves services
stopped/running as they were. Source hot-update does not install dependencies or
rewrite container environment values.

Use source-mounted app services for continuous reload:

```bash
make dev-compose
make dev-compose-logs
```

This combines `docker-compose.build.yml` and `docker-compose.dev.yml` to
bind-mount the Gateway, Knowledge service, and shared core
package source into the containers and runs the Python services with
`uvicorn --reload`. Rebuild only when
dependencies, Dockerfiles, or image-level system packages change.

Frontend deployment config is runtime-injected. After the image is built once,
changes to `VITE_API_URL`, `VITE_API_BASE_URL`, `VITE_AUTH_EMAIL_DOMAIN`,
`VITE_SUPPORT_EMAIL`, `VITE_TELEMETRY_ENDPOINT`, or `VITE_SSE_DEBUG` only need
`docker compose up -d frontend` to recreate the container with the new values.
For shared or non-local deployments, `VITE_AUTH_EMAIL_DOMAIN` must match
`AUTH_ALLOWED_EMAIL_DOMAIN`.
Leave `VITE_SUPPORT_EMAIL` blank to default to `admin@AUTH_ALLOWED_EMAIL_DOMAIN`,
or set it to a real non-example support mailbox.
Leave `VITE_API_URL` and `VITE_API_BASE_URL` blank for the default same-origin
nginx `/api` proxy. For shared or non-local deployments, explicit absolute API
URLs and telemetry endpoints must use `https://` and must not point at
localhost or loopback. Same-origin paths such as `/api` and `/telemetry` remain
valid.

## Release Notes

See `CHANGELOG.md` for the module-level summary of the current open-source standalone release cleanup.
See `RELEASE.md` for the public release checklist, artifact workflow, and rollback gates.

## Contributing and Security

- Contribution guide: `CONTRIBUTING.md`
- Security reporting and supported-version policy: `SECURITY.md`
- Code of conduct: `CODE_OF_CONDUCT.md`
- Support expectations: `SUPPORT.md`

Do not report vulnerabilities through public issues and do not paste secrets,
tokens, provider keys, database URLs, private documents, or production data into
issues, pull requests, logs, or screenshots.

## Deployment Commands

```bash
make deploy        # start/migrate/health-check all services without rebuilding images
make deploy-build  # force rebuild
make deploy-infra  # start only postgres, redis, qdrant
make deploy-app    # start app services and their compose dependencies
make stop          # stop containers without deleting volumes
```

`deploy-infra` and `deploy-app` are separate partial-deploy modes; do not combine them in one command.
`deploy-app` still runs database migrations by default after PostgreSQL is healthy.
Use `make deploy-app ARGS="--no-migrate"` for application restarts that must not run migrations.
Automatic migrations call the single `python -m database.authority migrate`
plan. Compose's one-shot migrator owns DDL and every application container
waits for it; Gateway/Knowledge `AUTO_INIT` paths are disabled.
Automatic migration failures stop deployment; fix the failed migration before rerunning deploy.
Database restore uses the selected env file and stops on the first SQL error; do
not run restore against shared data without a current backup and explicit
approval.

For a destructive local reset, use Docker Compose explicitly and understand that this deletes local database/vector/cache data:

```bash
docker compose down -v --remove-orphans
```

## Microservice Communication

The compose file wires services through Docker DNS names:

- Gateway to Agent Runtime: `AI_PLATFORM_AGENT_RUNTIME_URL=http://agent-runtime:8094`
- Runtime to capability worker: `AI_PLATFORM_CAPABILITY_WORKER_URL=http://agent-capability-worker:8095`
- Gateway/worker to knowledge service: `KB_SERVICE_URL=http://knowledge-service:8092`
- Knowledge service to Qdrant: `http://qdrant:6333`
- Gateway/Runtime/worker/knowledge to PostgreSQL and Redis through internal service names

Internal service requests use `AI_PLATFORM_INTERNAL_TOKEN` plus scoped runtime
leases. Provider credentials remain in the Gateway model plane and are never
sent to Runtime or capability-worker containers.

Core service-to-service calls use the shared `ai-gateway-core` communication layer:

- Gateway proxy routes use `ServiceProxy` for request-id/trace propagation, header stripping, HMAC signing, SSE pass-through, bounded timeouts, retry budget, and per-route circuit breaking.
- Gateway-to-knowledge calls use `KBProxyClient`, backed by the same outbound client primitives.
- `src/proxy/transparent_proxy.py` remains for external or dynamically registered proxy targets; do not add new core gateway-to-service hops there.
- Internal clients apply bounded retry, optional token-bucket service limits, automatic idempotency keys for replay-safe non-GET calls, and low-cardinality service-call metrics.
- Admission control separates global, per-tenant, streaming, and adapter-level capacity so one tenant, long SSE stream, or failed AI backend cannot consume all service slots.

Local quickstart keeps `INTERNAL_AUTH_VERSION=v1` and `INTERNAL_COMM_STATE_BACKEND=memory`. For multi-worker or multi-replica deployments, set:

```env
INTERNAL_COMM_STATE_BACKEND=redis
INTERNAL_COMM_REDIS_URL=redis://:<password>@redis:6379/3
INTERNAL_AUTH_VERSION=v2
INTERNAL_AUTH_ACTIVE_KEY_ID=local
INTERNAL_AUTH_KEYS=local:<32-byte-secret>,previous:<old-secret>
INTERNAL_IDEMPOTENCY_BACKEND=redis
```

`v2` signs the canonical method, path, query, body hash, request id, timestamp, and key id. That prevents a valid internal signature from being replayed across a different route. Redis state is recommended when `knowledge-service` or any protected service runs with more than one worker, because in-memory replay and breaker state is process-local.

Useful communication controls:

```env
INTERNAL_SERVICE_RATE_LIMITS=knowledge-service=100:100,agent-runtime=50:50,agent-capability-worker=50:50
SERVICE_STREAMING_MAX_CONNECTIONS=16
ADMISSION_TENANT_SHARE_RATIO=0.2
SERVICE_BREAKER_RECOVERY_TIMEOUT_SECONDS=30
ADAPTER_BULKHEAD_LIMITS=langgraph:8,openai:16,comfyui:4
```

Keep local quickstart on memory-backed idempotency and replay state. Use Redis-backed state for multi-worker or multi-replica deployments.

The CORS env values used by compose are JSON array strings:

```env
KNOWLEDGE_CORS_ALLOW_ORIGINS_JSON=["http://localhost:8081","http://localhost:3000"]
```

For shared or non-local deployments, replace those localhost origins with the
actual `https://` frontend origin. `make validate-config` rejects missing CORS
values, wildcard origins, non-HTTP origins, and localhost origins once
`AUTH_ALLOWED_EMAIL_DOMAIN` is no longer the local `example.com` default.
If you set `VITE_API_URL` or `VITE_API_BASE_URL`, use a same-origin path such as
`/api` or an `https://` URL for non-local deployments; localhost and `http://`
absolute API URLs are rejected once `AUTH_ALLOWED_EMAIL_DOMAIN` is non-local.
If you set `VITE_TELEMETRY_ENDPOINT`, use a same-origin path such as
`/telemetry` or an `https://` URL under the same non-local deployment rule.
If you set `VITE_SUPPORT_EMAIL` for a non-local deployment, do not leave it as
`admin@example.com`; use a real support mailbox or leave it blank for the
default `admin@AUTH_ALLOWED_EMAIL_DOMAIN`.

Compose startup waits for infrastructure and microservices to become healthy before starting dependent services. Gateway, Knowledge, and Agent Runtime checks use `/health/ready`, not only shallow process liveness endpoints.

## Knowledge Base

The knowledge service supports document ingestion, chunking, vector indexing, and retrieval modes including keyword, vector, hybrid RRF, optional reranking, and MMR.

Dataset create/update validates parser backend names, cascade shape, options and limits before
saving. The dataset `/config` response reports effective parsing configuration and unavailable
backend warnings; ingestion uses the same compiler. Existing datasets without parsing settings
retain the legacy path. Dataset catalogs retain the array response and support `limit` (up to
200) and an opaque `cursor`; continue with `X-Next-Cursor` even when ACL filtering leaves a page
empty. Web and internal Knowledge clients traverse every page. Synchronous dedupe rejects
more than 10,000 segments with `KB_DEDUPE_INCOMPLETE` and performs no partial deletion.

Embedding configuration is controlled by:

```env
KB_EMBEDDING_PROVIDER=dashscope
KB_EMBEDDING_API_KEY=
KB_EMBEDDING_MODEL=text-embedding-v4
KB_EMBEDDING_DIMENSION=1024
```

Provider notes:

- `gemini`: set `KB_EMBEDDING_API_KEY` to your Google/Gemini embedding key.
- `dashscope`: the default `text-embedding-v4` path reuses `DASHSCOPE_API_KEY`; set `KB_EMBEDDING_API_KEY` only to override it with a dedicated embedding key.
- `siliconflow`: set `KB_EMBEDDING_PROVIDER=siliconflow`, use a SiliconFlow embedding model, and set `KB_EMBEDDING_API_KEY`.

## General AI Assistant

The Rust Agent Runtime and Capability Worker are private to the Docker network.
Public Agent traffic always goes through the gateway/frontend path.

Set at least one chat provider key in `.env`. Provider credentials stay in the
Gateway model plane; the Runtime and Capability Worker use short-lived scoped
leases and do not receive long-lived provider keys. Supported provider variables
include:

- `DASHSCOPE_API_KEY`
- `DASHSCOPE_CHAT_API_KEY`
- `GOOGLE_API_KEY`
- `GEMINI_API_KEY`
- `GOOGLE_CHAT_BACKEND`
- `VERTEX_CHAT_API_KEY`
- `VERTEX_API_KEY`
- `OPENAI_API_KEY`
- `ANTHROPIC_API_KEY`
- `DEEPSEEK_API_KEY`

### Agent Plugins 1.0.0

The Agent Runtime implements portable Agent Plugins 1.0.0 Skills plus trusted stdio
and Streamable HTTP MCP components. It recognizes the canonical local schema identifiers,
reads root `plugin.json` and `mcp.json`, and discovers immediate
`skills/*/SKILL.md` children. It does not download schemas at runtime,
recursively scan arbitrary paths, execute untrusted subprocesses, or honor a plugin's
self-declared tool permissions. Invalid skills and MCP server entries are
isolated from valid siblings, while a fatal manifest error rejects the plugin
before discovery. Local `stdio` execution is limited to operator-trusted plugin
identities, uses a minimal code-owned environment allowlist, and confines local
artifact imports to the plugin data directory. Legacy MCP `sse` remains unsupported.

Validate a package without executing it:

```bash
uv run python scripts/validate_agent_plugin.py /path/to/plugin
```

The bundled `agent-plugins/ai-docgen` package describes the Office skill used
by the capability worker. Generated files are persisted as Runtime artifacts;
there is no document-specific Agent loop or stdio child server.

The bundled `agent-plugins/ai-quiz` package documents the assistant's built-in
quiz capability: ask the assistant to create questions from knowledge-base
content and it calls the `generate_quiz` tool, renders an interactive quiz card
in chat, and supports public sharing through the artifact-share mechanism.
Plugin and skill bindings are versioned Agent Studio data. Package metadata
cannot grant credentials, reduce risk, bypass approval, or add arbitrary
request fields; Runtime snapshots carry only validated identities and grants.

### Code execution

Code execution is a capability-worker operation with a bounded execution
workspace. Input attachments are available under `input/`; generated artifacts
must be written under `output/`. The default stack never mounts the host Docker
socket into an application service.

## Database and Qdrant Notes

The compose project name is fixed to `ai-gateway`, and volumes are project-scoped:

- `ai-gateway_pg-data`
- `ai-gateway_redis-data`
- `ai-gateway_qdrant-data`
- `ai-gateway_kb-data`

PostgreSQL initializes its password only when the volume is first created. If you change `POSTGRES_PASSWORD` after the first run while keeping the old volume, authentication can fail even though `.env` looks correct. For local development, stop the stack and intentionally reset volumes:

```bash
docker compose down -v --remove-orphans
```

Qdrant data is also volume-backed. If you change embedding dimensions or providers for existing collections, create a new collection/dataset or reset the Qdrant volume in local development.

## Troubleshooting

Configuration fails:

```bash
make validate-config
```

Runtime fails:

```bash
make status
docker compose logs --tail=200 gateway
docker compose logs --tail=200 knowledge-service
docker compose logs --tail=200 agent-runtime agent-capability-worker
docker compose logs --tail=200 qdrant
```

Common causes:

- `.env` still contains `change_me_*` placeholders.
- The configured embedding provider has no usable credential. The default DashScope path reuses `DASHSCOPE_API_KEY`; `KB_EMBEDDING_API_KEY` is only a dedicated-key override.
- PostgreSQL password was changed after the first volume initialization.
- Qdrant is running with old vectors from a different embedding dimension.
- `FRONTEND_PORT`, `GATEWAY_PORT`, `POSTGRES_PORT`, `REDIS_PORT`, or `QDRANT_HTTP_PORT` conflicts with another local process.

Microservice communication checks:

```bash
curl -fsS http://localhost:8080/health/ready
curl -fsS -o /dev/null http://localhost:8080/metrics
curl -fsS http://localhost:8092/health/ready
docker compose exec gateway printenv INTERNAL_AUTH_VERSION INTERNAL_COMM_STATE_BACKEND
```

- `401 AUTH_DENIED` between services: confirm all internal services receive the same `AI_PLATFORM_INTERNAL_TOKEN`; if canonical v2 signing is enabled, confirm `INTERNAL_AUTH_KEYS` and `INTERNAL_AUTH_ACTIVE_KEY_ID` match.
- `429 Rate limit exceeded`: check local quickstart rate-limit env values and whether assistant chat is hitting `RATE_LIMIT_ASSISTANT_CHAT_LIMIT`.
- `429 GATEWAY_TENANT_CAPACITY_EXHAUSTED`: a single tenant reached its per-tenant admission share; adjust `ADMISSION_TENANT_SHARE_RATIO` or the service capacity config.
- `502` or `504`: check Agent Runtime, capability-worker, and knowledge-service logs, then their configured internal URLs and timeouts.
- `503 GATEWAY_LOAD_SHED`: adaptive load shedding is active because recent p99 latency exceeded the configured threshold.
- Duplicate POST response: check `Idempotency-Key`; internal clients generate a per-request key for non-GET service calls and reuse it only across retries of that same logical request. Services replay cached non-streaming responses for repeated keys.
- SSE starts then stops: inspect Gateway and Agent Runtime logs with the same `X-Request-Id`; streaming requests are not retried after bytes are emitted.
- KB retrieval timeout: check knowledge-service health, Qdrant health, embedding provider key/quota, and `KB_PROXY_READ_TIMEOUT_SECONDS`.

## Identity and Sessions

The gateway resolves user identity server-side and enforces per-user session isolation.

- Anonymous browser users receive an `HttpOnly` cookie, default `ag_anon_id`.
- Non-browser clients can use the anonymous ID header, default `X-AG-Anonymous-Id`.
- Authenticated users are resolved from JWT or API key at the gateway layer.
- `user_id` and `tenant_id` in request bodies are ignored.
- Session reads and writes are owner-checked.
- `POST /api/v1/stream` returns the effective session ID in `X-Session-Id` when sessioning is enabled.

Useful knobs:

```env
GATEWAY_ANONYMOUS__COOKIE_NAME=ag_anon_id
GATEWAY_ANONYMOUS__HEADER_NAME=X-AG-Anonymous-Id
GATEWAY_ANONYMOUS__TTL_DAYS=30
GATEWAY_SESSION__ANONYMOUS_TTL_SECONDS=86400
GATEWAY_SESSION__AUTHENTICATED_TTL_SECONDS=604800
```

## For agents

Read [`AGENTS.md`](AGENTS.md) before changing the repository — it is the contract every coding
agent follows here, and [`CLAUDE.md`](CLAUDE.md) adds the Claude Code specifics.

The full agent harness — architecture boundaries, canonical commands, gates, task loop, and the
mandatory Docker/secret rules — lives in [`docs/harness/`](docs/harness/README.md), with the
machine-readable contract in [`harness.yml`](harness.yml). Verify it with:

```bash
make harness-check
```

Everything else is indexed from [`docs/README.md`](docs/README.md). Multi-session programs live in
`deploy/runbooks/`, where each `loop-state.json` is the authoritative status.
