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

## Native workload and database file-security profile

For the staged mTLS/file-secret profile, select **`compose.full.yml` as the sole
base**, then `compose.security.yml` and
`deploy/compose/docker-compose.security.yml`. Add
`docker-compose.workload-dual.yml` during the initial dual window; remove it for
required retirement. Do not append the default app `extends` file or merge the
two security fragments separately after this selection. Native source tests
verify that the full local base still matches this directory's existing
infrastructure/Runtime/app definitions and all original volume identities.

The canonical base avoids an observed Compose reset/late-extends merge defect
that can restore an inherited inline credential. Ten synthetic root/local
security configurations were rendered and checked offline; this is not runtime
acceptance. Full role material, model authority, UID ownership and existing
native authority admission must be prepared before startup. Keep project
`agentforge-dev-7801d4a9a4`, its selected `.env.dev`, exact admitted images,
`--project-directory "$PWD"`, ownership and memory checks, `--no-build --pull never`,
plain health probes and Gateway/Runtime singleton guards.

See the complete [native trust deployment contract](../workload-identity/DEPLOYMENT.md)
for exact ports/identities, role-only mounts, required token removal, optional
session pooler/SDK files, K8s native SVID importer and rotation semantics.

## Fresh local database, existing owned project

For the explicitly selected new local gateway_dev_fresh, use the native fresh
entry below. It installs the complete current schema through
database-authority fresh-development-init; historical migrations are not a
startup prerequisite. The original database and PostgreSQL/Redis/RustFS volumes
remain. Application files, Runtime home/workspaces and Qdrant vectors use separate
*-fresh volumes; Redis logical databases 11/12/13 isolate new state.

The input is the existing private .env.dev, an integrity-validated native
prepare-local-env output for gateway_dev_fresh, and already built image receipts.
No source env is rewritten, existing password rotated, image downloaded or second
project created. The --image-env argument accepts only image identity keys. The
tools image must contain current platform-tools and /usr/bin/openssl; it is an
operational image, never the native compiler image.

Prepare the data generation independently, then augment that same directory
with genuine application credentials before the core phase:

~~~sh
scripts/new/local-fresh.sh prepare \
  --directory /absolute/external/private-fresh-generation \
  --data-input /absolute/external/validated-data-import \
  --tools-image "$ADMITTED_TOOLS_IMAGE" \
  --image-env /absolute/external/tools-image.env \
  --image-env /absolute/external/migrator-image.env \
  --client-cidr 10.71.10.0/24 --backend-cidr 10.71.11.0/24

scripts/new/local-fresh.sh secure-extend \
  --directory /absolute/external/private-fresh-generation \
  --data-input /absolute/external/validated-data-import \
  --tools-image "$ADMITTED_TOOLS_IMAGE" \
  --image-env /absolute/external/tools-image.env \
  --security-generation transport-r1 \
  --client-cidr 10.71.10.0/24 --backend-cidr 10.71.11.0/24

scripts/new/local-fresh.sh up \
  --directory /absolute/external/private-fresh-generation \
  --data-input /absolute/external/validated-data-import \
  --tools-image "$ADMITTED_TOOLS_IMAGE" \
  --image-env /absolute/external/tools-image.env \
  --image-env /absolute/external/native-images.env \
  --image-env /absolute/external/frontend-image.env \
  --image-env /absolute/external/admin-bootstrap-image.env \
  --client-cidr 10.71.10.0/24 --backend-cidr 10.71.11.0/24
~~~

The default source input is the existing ignored .env.dev. Prepare is offline and
needs only the real tools/migrator images. It neither starts services nor contacts
PostgreSQL. Existing PREPARED directories are reused by database/up/check; never
repeat prepare on them. The explicit secure-extend operation also works on those
existing directories without rewriting fresh.env or invalidating data materials.
Core up requires its complete selected security release and all genuine app
images. Check repeats admission, HTTP readiness, and certificate-verified HTTPS
readiness without changing services. An incomplete release is refused. Private
env and rendered configuration are never printed. Make equivalents include
local-fresh-prepare, local-fresh-secure-extend, local-fresh-up and local-fresh-check.

The launcher checks existing Compose project/working-directory ownership before
every mutation, retains dependency/readiness gates, starts serially, checks Docker
memory before each service, and stops if observed memory reaches 80%. It uses
--no-build --pull never. One-shot completion is inspected without aborting the
other dependency containers, so authority completion does not stop PostgreSQL.
It never removes orphans, stops unrelated listeners, resets a volume or changes
an active network's subnet.

The complete compose.core.yml fresh profile retains PostgreSQL PGDATA, installs
verified TLS and the imported cert-only HBA, and shares only a new private socket
volume with the independent UID 70 Unix-peer authority operator. There is no TCP
admin exception. Native offline installers copy a fixed whitelist into new,
per-service private volumes owned by the real service UID; applications receive
only their own role files. The offline issuer/CA key is never mounted into a
service. Qdrant gets verified TLS and distinct genuine writer/read-only keys;
missing credentials are first-issued, selected existing ones preserved. Partial
TLS input fails. The original vector volume remains untouched.

Knowledge API/worker get independent genuine Ed25519 workload signing keys;
Gateway alone gets its lease/proof private signer. Consumers get public verifiers,
with V3 verify-only lease/proof mode. A fresh empty database has no activated
commercial price book: the default revision is explicitly unpublished, and model
reservations fail closed until an actual price book/model revision is published.
HTTP readiness does not establish model execution or business acceptance.

Core business traffic uses required native workload mTLS. Gateway, Runtime,
Worker and both Knowledge roles each receive only their own seven-day DNS/SPIFFE
SVID generation and exact peer/origin allowlists. Private TLS ports remain inside
the application network; HTTP listeners are retained for health probes. Shared
internal bearer, legacy proof/lease secrets and model-plane bearer fallbacks are
absent. Knowledge HMAC, model-plane Ed25519 and Gateway lease/API signing remain
separate domains. Only Gateway receives private platform/API signers. Runtime's
new private loopback bridge token is Runtime-only; it is never a service bearer.

The console is https://localhost:8081 (or the selected FRONTEND_PORT), with an
exact matching Gateway BFF origin and same-origin frontend API configuration.
The genuine browser CA is separate from workload trust and constrained to
localhost/loopback names. Its public import file is
security/releases/<selected-generation>/frontend/ca.pem under the private fresh
directory; the extension receipt records DER SHA256 fingerprints. CA private
keys stay outside service releases and are never mounted into an application.
The frontend publishes only TLS8443. Its unexposed HTTP8080 provides /health;
other plaintext paths return404. Proxy headers preserve HTTPS and the full Host
including the selected port, so normal secure cookies and CSRF checks apply.

Generating or explicitly validating a CA is not browser trust. No system trust
is changed by this flow. Import the public Console CA into an explicitly selected
private browser profile using the browser's supported local certificate manager,
or select an already trusted complete localhost certificate/key/CA trio using
--console-cert-file, --console-key-file and --console-ca-file on secure-extend.
Private keys must be mode0600. The readiness check uses the selected CA with
normal chain and hostname verification; real browser profile trust, secure cookie
login, CSRF and first-password-change remain separate acceptance checks. Never
use ignoreHTTPSErrors, insecure curl or SPKI exceptions.

The security extension creates a fixed private administrator JSON input from the
selected admin email settings and AUTH_ALLOWED_EMAIL_DOMAIN; if no email was
selected it uses admin at that domain. An existing selected initial password is
preserved; otherwise first issue uses OS randomness and normal strong-password
requirements. The input is never printed. Its dedicated offline installer copies
only that file into a UID1000 private volume. The native ADMIN_BOOTSTRAP_IMAGE
one-shot receives only Gateway's own data credentials and this file on stdin,
after full schema authority and before Gateway. Repeated startup verifies the
existing account, never resets its password or roles. The account retains
force_password_change=true. See docs/deployment/fresh-administrator.md.

For certificate renewal, run secure-extend with a new explicit generation name,
then up with the same original data directory and current image receipts. Issuers,
application keys and initial administrator input are reused byte-for-byte;
certificates are renewed. Failed releases remain unselected. The public native Gateway options are copied into the selected release with
allow_private_knowledge_http=false; the original options remain unchanged.
Each installer copies
into a private generation before atomically switching current; old generations
remain available. Generation labels cause services to restart/load the selected
credentials. Root/issuer replacement requires a separately reviewed trust-overlap
rotation; this operation never silently replaces a CA or application key.

Core has no Python sandbox/SDK services or their required interpolations.
Append --sandbox only with admitted purpose-specific keys/image/cgroup inputs;
it selects compose.core-sandbox.yml. Append --sdk with a real SDK-capable worker
image and finite retention. It uses the same native required workload trust,
a separate SDK SVID/private volume, and a private TLS8456 business listener with
independent HTTP health; no legacy callback bearer or extra callback PKI inputs
are used. Partial enabled options fail before startup. Strong warm pools and HA
remain separately admitted opt-ins; Gateway/Runtime stay singleton until genuine
chaos evidence admits retirement.


The database-only action can run before Runtime/Worker images are available:

~~~sh
scripts/new/local-fresh.sh database \
  --env-file "$PWD/.env.dev" \
  --directory /absolute/external/prepared-or-new-fresh-generation \
  --data-input /absolute/external/validated-data-import \
  --tools-image "$ADMITTED_TOOLS_IMAGE" \
  --image-env /absolute/external/native-image-receipt.env \
  --client-cidr 10.71.10.0/24 --backend-cidr 10.71.11.0/24
~~~

It uses the exact five-service compose.core-database.yml projection: three
bounded offline file/socket installers, the existing PostgreSQL service and full
schema authority. It admits only the actual tools/migrator/PostgreSQL images;
there is no fake Runtime image or bypass of a Runtime admission failure.
The Make equivalent is local-fresh-database. Once genuine paired images exist,
run up against the same prepared directory and append their image receipt.
Image-only overrides are validated and layered consistently into Compose;
credentials, signing generations and the source env remain unchanged.

Memory admission uses the selected engine's raw cgroup usage counters in bytes,
summed across every running container, divided by daemon MemTotal. Container
MemPerc is never summed: its denominator is the individual container limit.
The database phase has a separate nominal 1.5 GB floor; core requires nominal
4 GB (4,000,000,000 bytes), compatible with Docker Desktop's observed
4,107,141,120-byte daemon allocation. Both enforce actual usage below 80%
and actual remaining space for the next service's selected mem_limit plus
256 MB daemon reserve. Memory-limit overrides are resolved from the public
Compose source and private selected env, without rendering its values.
No process is killed or credential regenerated when admission refuses startup.


The authority receives the complete canonical file resolver contract for all six
principals: DSN/PASSWORD/CA/CERT/KEY files, direct mode, expected postgres host,
gateway_dev_fresh alias and physical database, and exact ai_gateway role name.
Only its bounded UID 70 installer copies all six existing password files. Service
data volumes remain role-specific and certificate-authenticated. No resolver
check or private-file permission is weakened.

After rebuilding an updated tools image, reuse an existing prepared generation
with an explicit image-only receipt containing LOCAL_FRESH_TOOLS_IMAGE and the
matching --tools-image argument. This overlays the public image selection without
rewriting fresh.env or reissuing any credential. Existing authority TLS/DSN files
must remain byte-identical; the installer can add the previously missing original
password files. Optional SDK state uses a separate *-fresh volume.
