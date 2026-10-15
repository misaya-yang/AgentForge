# Deployment Guide

This guide covers Docker Compose deployment for the standalone AI Gateway project.

For a first local run, start with [`README.md`](README.md). Use this document when you want a
repeatable deployment checklist for a server or shared environment.

The shortest correct path is three commands:

```bash
make doctor && make quickstart && make status
```

Automation and coding agents must also read
[`docs/harness/runtime-and-secrets.md`](docs/harness/runtime-and-secrets.md) before any Docker or
deploy action — it holds the compose-ownership, rebuild, memory, and secret-handling rules that
this guide assumes.

## Deployment Model

The default stack contains:

- `postgres`: primary relational database
- `redis`: cache, sessions, and queues
- `qdrant`: vector database for the knowledge base
- `knowledge-service`: KB CRUD, ingestion, indexing, and retrieval
- `agent-runtime`: the sole Agent Thread/Turn/Item and model/tool loop
- `agent-capability-worker`: tools, Office, code, connectors, and long jobs
- `gateway`: public API, auth, proxy, sessions, and readiness
- `frontend`: web console

The gateway and frontend are the public entry points. Runtime and worker remain private to the Docker network. Infrastructure ports are bound to `127.0.0.1` by default.

## Required Configuration

Create a real `.env` from the example:

```bash
cp .env.example .env
```

Never commit `.env`.
If the real env file is managed outside this repository, pass it to Make with
`ENV_FILE`:

```bash
make validate-config ENV_FILE=/path/to/.env
make validate ENV_FILE=/path/to/.env
make deploy-app ENV_FILE=/path/to/.env ARGS="--no-migrate"
```

Fill these required values:

- `POSTGRES_PASSWORD`
- `REDIS_PASSWORD`
- `JWT_SECRET`
- `AI_PLATFORM_INTERNAL_TOKEN`
- `AI_PLATFORM_CAPABILITY_LEASE_SIGNING_SECRET`
- `AI_PLATFORM_CAPABILITY_PROOF_SECRET`
- At least one chat provider key
- `KB_EMBEDDING_PROVIDER`
- `KB_EMBEDDING_API_KEY`
- `KB_EMBEDDING_MODEL`
- `KB_EMBEDDING_DIMENSION`

Generate local secrets with:

```bash
openssl rand -hex 32
```

## Preflight

Two checks, in this order. The first inspects the machine, the second inspects the configuration.

```bash
make doctor
```

`make doctor` is read-only. It verifies required tooling and Compose v2, Docker memory and disk
headroom, host ports, `.env` presence and mode, and — most importantly — that any running
`ai-gateway-*` containers belong to **this** checkout. Acting on another checkout's stack is the
most damaging mistake available here, so an ownership conflict is a blocking failure. Exit code 0
means ready; any nonzero exit means fix something first.

Then validate the configuration:

```bash
make validate-config
```

Use `make validate-config ENV_FILE=/path/to/.env` when the env file is stored
outside the repository.

For repository-only or CI checks that must not depend on private secrets, run:

```bash
make validate-example-config
```

That target validates the committed `.env.example` shape and Docker Compose
rendering. It does not prove a deployment is release-ready; production and
shared environments must still pass `make validate-config` and `make validate`
with a populated real env file.

This checks:

- required secrets are present and not placeholders
- chat and embedding keys are configured
- provider names and ports are valid
- host ports are not duplicated
- bundled plugin paths and data roots are declared
- `VITE_AUTH_EMAIL_DOMAIN` matches `AUTH_ALLOWED_EMAIL_DOMAIN` when set for non-local auth domains
- `VITE_SUPPORT_EMAIL` is blank or uses a real non-example mailbox for non-local auth domains
- explicit frontend API and telemetry runtime URLs use a same-origin path or `https://` and are not localhost or loopback for non-local auth domains
- CORS values are explicit http(s) origin arrays, without wildcards or
  non-https or localhost origins for non-local auth domains
- Docker Compose interpolates successfully

The validator does not print secret values.

## Start

Build, start, migrate, and validate everything:

```bash
make quickstart
```

Lower-level start and migration commands:

```bash
docker compose --env-file .env up -d --build --remove-orphans
bash scripts/new/migrate.sh --env .env --auto
```

Validate the running stack:

```bash
make validate
make status
```

`make status` prints the full health table and exits nonzero if any check is not
available, so it can be used as a release gate instead of only a visual summary.

Validation, status, deploy, migration, backup, stop/restart/log, and development
helper Make targets accept `ENV_FILE=/path/to/.env`; the default is `.env`.
Lower-level deploy, migrate, backup, and setup-dev scripts also accept
`--env FILE` when called directly.

Expected public endpoints:

- Frontend: `http://localhost:8081`
- API docs: `http://localhost:8080/docs`
- Gateway readiness: `http://localhost:8080/health/ready`

## Gateway probes and deployment profiles

Gateway liveness is `GET /health/live`: a responsive process returns HTTP 200 without
probing dependencies. Gateway readiness is `GET /health/ready`: required core
admission checks return HTTP 200 only when healthy, otherwise HTTP 503. `/health`
is a compatibility/process response and must not be used as an admission gate.
A wrong dependency URL can return HTTP 200 with another service's payload; the
Gateway rejects an invalid Runtime readiness payload rather than marking it ready.
Optional Knowledge, capability-worker or image-worker failures degrade the relevant
capability without making core Gateway readiness fail. `make status` separately
checks the stack's services, so its broader release gate can still fail.

- Base Compose and its `compact`, `full`, `scale`, and development overlays retain
  Python Gateway by default and inherit its `/health/ready` container healthcheck.
  `make status` uses the same Gateway endpoint. Topology mode does not select a
  native implementation or expand a process's supported database epoch window.
- Helm default and production values use `/health/live` and `/health/ready` on
  port 8080. The template still accepts operator overrides through
  `gateway.probes.liveness.*` and `gateway.probes.readiness.*`; no other service's
  probe contract changes. Do not override readiness to `/health` to hide a failed
  dependency or incompatible deployment.
- The current Helm chart remains a retained Python profile: its schema-check init
  container runs `python -m database.authority startup-check`, and the chart has
  historical LangGraph templates but no managed Agent Runtime template. It does
  not supply the required managed Runtime/model-plane configuration. A standalone
  chart install is therefore not evidence of a complete ready platform; operators
  need a separately compatible Runtime and reviewed private routing/configuration.
  The corrected probe may expose those missing dependencies as unready.
- Native Gateway implements the same public probe paths, but replacing only a Helm
  image does not produce a reviewed native release profile. Retained Python Gateway
  supports schema epochs 2..10; the native-enterprise epoch-17 candidate needs its
  own compatible startup/image/profile evidence. The optional SDK Worker overlay
  explicitly requires native Gateway and does not select it automatically.

Offline regressions live in `tests/scripts/test_release_readiness_probes.py` and
`tests/api/test_gateway_readiness_contract.py`. The config tests parse committed
values/Compose and exercise real `helm template` for default, production, and
operator-override cases when Helm is installed; missing Helm is an explicit skip.
These checks do not prove Kubernetes rollout, a native binary, or live readiness.

## Update

Pull or apply the new source, then rebuild:

```bash
make deploy-build
```

For app-only updates, restart the public entrypoints and application microservices while leaving infrastructure volumes alone:

```bash
make deploy-app
```

For infrastructure-only startup:

```bash
make deploy-infra
```

`deploy-infra` and `deploy-app` are mutually exclusive. Run them as separate commands if both infrastructure and application services need work.
`deploy-app` still runs database migrations by default after PostgreSQL is healthy. Use `make deploy-app ARGS="--no-migrate"` when you only want to restart application services.
Automatic migrations initialize `database/schema.sql` first when the base
schema is missing, then run pending files from `database/migrations`.
Automatic migration failures stop deployment; fix the failed migration before rerunning deploy.
Database restore uses the selected env file and stops on the first SQL error;
restore against shared data requires a current backup and explicit approval.

## Stop and Reset

Stop without deleting data:

```bash
make stop
```

Destructive local reset:

```bash
docker compose down -v --remove-orphans
```

Use the destructive reset only when you intentionally want to delete local PostgreSQL, Redis, Qdrant, KB, and artifact volumes.

## Data Volumes

Compose uses the fixed project name `ai-gateway`, so volume names are stable:

- `ai-gateway_pg-data`
- `ai-gateway_redis-data`
- `ai-gateway_qdrant-data`
- `ai-gateway_gateway-data`
- `ai-gateway_kb-data`

Docgen plugin artifacts use the shared `ai-gateway_gateway-data` volume and are
imported into the Assistant artifact store. Older deployments may still have an
unused `ai-gateway_docgen-artifacts` volume; remove it only after confirming no
legacy files are needed.

PostgreSQL initializes its password when the volume is first created. If you change `POSTGRES_PASSWORD` after first boot without resetting the volume, authentication can fail. For local development, reset volumes intentionally with `docker compose down -v --remove-orphans`.

Qdrant collections are tied to embedding dimensions. If you change `KB_EMBEDDING_DIMENSION` or switch providers for an existing dataset, create new collections/datasets or reset Qdrant in local development.

## Production Notes

For production-like deployments:

- Put a reverse proxy or load balancer in front of gateway/frontend.
- Keep PostgreSQL, Redis, and Qdrant ports private.
- Use strong generated values for all secrets.
- Store `.env` in your secret-management system, not in Git.
- Configure explicit CORS origins for your frontend domain.
- Use S3-compatible storage by setting `GATEWAY_STORAGE__BACKEND=s3` and filling the S3 variables.
- Monitor gateway `/health/ready`; it checks DB, Redis, knowledge service, and
  the Rust Agent Runtime. Runtime validation also probes the Capability Worker
  catalog and execution endpoints.
- Scrape gateway `/metrics` from your monitoring system and include it in release smoke checks; `gateway_up` should be present after startup.

## Troubleshooting

Start here — it diagnoses the host, not the app:

```bash
make doctor
```

Config validation:

```bash
make validate-config
```

Runtime validation:

```bash
make validate
```

Service state:

```bash
make status
docker compose --env-file .env ps
```

Logs:

```bash
docker compose --env-file .env logs --tail=200 gateway
docker compose --env-file .env logs --tail=200 knowledge-service
docker compose --env-file .env logs --tail=200 agent-runtime agent-capability-worker
docker compose --env-file .env logs --tail=200 qdrant
```

Common causes:

- `.env` still contains `change_me_*` placeholders.
- A chat model key is configured, but `KB_EMBEDDING_API_KEY` is missing.
- Redis or PostgreSQL password was changed after volumes were initialized.
- Qdrant contains vectors from a different embedding dimension.
- A host port is already occupied.
