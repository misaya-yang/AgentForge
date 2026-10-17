# Local source profiles

The existing `agentforge-dev-7801d4a9a4` project owns the local PostgreSQL, Redis,
Qdrant and RustFS services and their volumes. `compose.runtime.yml` adds the
admitted Runtime, capability control and independent credential-free sandbox.
`compose.apps.yml` optionally adds the full Gateway, Knowledge API/worker,
frontend and existing database-authority bootstrap/migrate jobs. It declares no
second infrastructure stack and no project name. Existing infrastructure and
sandbox volume identities remain in their original files.

This is configuration source, not deployment acceptance. Complete code review,
interfaces and compilation first. The current machine's memory budget is not
sufficient authorization to start another profile. No service was started or
existing host process stopped while preparing this file.

## Full application inputs

Use the same selected ignored `.env.dev`. The primary writer supplies these
additional public names/private runtime inputs; do not print or paste their
values into reports:

| Input | Meaning |
| --- | --- |
| `AGENTFORGE_REPOSITORY_ROOT` | Absolute path to this checkout; used for canonical external Compose/config references |
| `GATEWAY_IMAGE`, `KNOWLEDGE_IMAGE`, `MIGRATE_IMAGE`, `FRONTEND_IMAGE` | Already built/admitted local image identities; no published fallback or implicit build |
| `LOCAL_CONTAINER_GATEWAY_NATIVE_OPTIONS_FILE` | Absolute existing NativeDeployment JSON options file with the verified kernel revision; not a host GatewayDocument or placeholder example |
| `LOCAL_CONTAINER_GATEWAY_DSN` | Container-only Gateway role DSN to `postgres:5432` and the existing database |
| `LOCAL_CONTAINER_KNOWLEDGE_API_DSN` | Container-only `knowledge_api` role DSN to `postgres:5432` |
| `LOCAL_CONTAINER_KNOWLEDGE_WORKER_DSN` | Container-only `knowledge_worker` role DSN to `postgres:5432` |
| `LOCAL_CONTAINER_STORAGE_BACKEND` | Existing local storage behavior by default (`local`); explicitly select `s3` to use the existing RustFS bucket |
| `LOCAL_S3_BUCKET` / `S3_BUCKET`, `LOCAL_S3_REGION` | Existing bucket and region when S3 is selected |

Keep the existing host-native DSNs/config paths unchanged. Do not rewrite host
DSNs to `postgres:5432`: that hostname belongs inside the Compose networks.
Container DSNs must retain the assigned scoped username, search path, statement
and idle-transaction timeout options; the rendered database-identity gate checks
the actual role. Runtime/control retain their existing split Postgres variables.
The inherited admin-only bootstrap owns the admin DSN; migrator/application
permissions, one-shot migration and readiness dependencies are not removed.

Other existing selected inputs remain mandatory: distinct scoped DB passwords,
platform/internal/auth/encryption/provider values, exact Runtime/control/sandbox
image identities and verifier/signing-file inputs, Qdrant TLS files and separate
read/write keys, the sandbox-only delegated cgroup subtree, and read-only
compatibility metadata. Native options, TLS and signing files must already exist
with the documented container UID permissions. This overlay does not generate
credentials, repair volumes, create buckets or prepare cgroups.

Gateway/Knowledge inherit the root services' roles, authentication, TLS/Qdrant,
hardening and health checks. Writable application volumes retain their canonical
names. Paths and container-name/port overrides use Compose `!override` / `!reset`,
requiring Compose **2.24.4 or newer**. The root Compose reference and read-only
binds are absolute/explicit, independent of the first file's directory. The
non-root Gateway directory check explicitly invokes `/bin/sh -ec` rather than the
Gateway image's application entrypoint.

Runtime/control callbacks use `gateway` and `knowledge-service` internal DNS;
Gateway uses the existing Runtime/control DNS. Internal origins are appended to
proxy bypass lists. Gateway/Knowledge's optional S3 endpoint is
`http://object-store:9000` on their existing owner-only object network, using the
same selected local object-store credentials. With `local`, data stays in the
existing app volumes. S3 browser URL behavior and the Knowledge read-only API's
remaining mutation responsibilities still require business acceptance; no
routing/security bypass is introduced to hide those checks.

## Future authorized rendering and startup

Run from the repository root after the primary writer has completed review,
compilation, image/source admission, ownership checks and a sufficient memory
budget. Use only the existing project and selected env file. The following are
commands for that later stage; they have **not** been executed:

```bash
compose_args=(
  --project-name agentforge-dev-7801d4a9a4
  --project-directory "$PWD"
  --env-file .env.dev
  -f deploy/local/compose.yml
  -f deploy/local/compose.runtime.yml
  -f deploy/local/compose.apps.yml
)
docker compose "${compose_args[@]}" config --quiet
```

The primary writer also runs the existing image/source, database-identity and
container-security gates on a protected external rendered configuration without
logging its values. Confirm the existing project/working-directory labels before
any mutation. Resolve host-native listeners on the retained loopback ports
(8080 Gateway, 8092 Knowledge API, 8081 frontend; Runtime/control retain their
configured 8094/8095 bindings) through the owning process workflow, never by
blindly killing listeners.

After those checks and explicit permission to enter real integration:

```bash
bash scripts/rust/locks.sh run \
  --resource integration-runtime \
  --timeout-seconds 7200 \
  --heartbeat-seconds 10 \
  --expected-end-condition 'existing local full application profile admission and readiness complete' \
  -- docker compose "${compose_args[@]}" up -d --no-build --pull never \
  database-bootstrap migrate gateway-init knowledge-service knowledge-worker \
  agent-python-sandbox agent-capability-worker agent-runtime gateway frontend
```

Do not add `--no-deps`, remove health checks or use a second project to bypass
readiness/port conflicts. Do not run `down -v` or delete existing volumes. This
command may reconcile existing infrastructure definitions as dependencies; the
operator must review that protected diff, including pending Qdrant TLS/non-root
ownership changes, before applying anything.

For optional SDK agents, append the existing
`-f deploy/compose/docker-compose.sdk-agent-worker.yml --profile sdk-agents`
arguments to the same Compose invocation, select the admitted
`SDK_AGENT_WORKER_IMAGE`, and provide the separate worker/callback TLS input
files documented in `deploy/security/CONTAINER_SECURITY.md`. Retain `--no-build
--pull never`; the optional overlay's explicit build definition is not permission
for an implicit source build. No SDK service is required for ordinary Gateway,
Knowledge, Runtime or frontend startup.
