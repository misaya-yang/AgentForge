# Native service trust deployment

The reference Compose and Helm entrypoints now connect native inbound and
outbound workload TLS, file secrets, and separate PostgreSQL role identities.
This is deployable configuration source. No stack, certificate rotation drill,
role-limit migration, chaos test, cluster or release acceptance was run here.
Gateway, Runtime and capability control remain singleton. Required mode is an
explicit operational retirement step after the real paired-service tests; the
existence of this configuration does not retire the dual acceptance window.

## Ports and identities

| Service / SPIFFE suffix | HTTP health/public port | Private native TLS port | Database role |
| --- | --- | --- | --- |
| Gateway / `gateway` | 8080 | 8443 | `ai_gateway_gateway` |
| Runtime / `agent-runtime` | 8094 | 8454 | `ai_gateway_runtime` |
| Control / `agent-capability-worker` | 8095 | 8455 | `ai_gateway_capability_worker` |
| Knowledge API / `knowledge` | 8092 | 8452 | `ai_gateway_knowledge_api` |
| Knowledge worker / `knowledge-worker` | 8092 | 8452, no business Service | `ai_gateway_knowledge_worker` |
| Optional SDK worker / `sdk` | 8096 | 8456 | none |
| Frontend / Python sandbox | existing HTTP / UDS | none | none |

Compose pins trust domain `agentforge.internal`; Helm makes the domain an
explicit validated value. Every allowlist uses the full
`spiffe://agentforge.internal/service/<suffix>`. Runtime/control/SDK private HTTP
clients receive exact origin maps; Knowledge's strict configuration receives
only its supported `ALLOWED_ORIGINS` map. No forwarded identity header grants
access. Native signed actor, lease, body, tenant and session checks still run.
The public Gateway listener, provider clients, public web tools, frontend and
credential-free Python sandbox do not receive an internal client transport.

HTTP health remains on its original listener and probe. Knowledge's
`KNOWLEDGE_GATEWAY_HEALTH_ORIGIN` points to Gateway HTTP health without a
certificate or transport bearer. Required mode rejects private business calls
on the plaintext listener. All workload TLS ports are application-network or
ClusterIP-only: no Compose publication, ingress, HTTPRoute or GRPCRoute exposes
them. Helm NetworkPolicies admit paired exact component selectors on business
TLS ports and preserve the separate Knowledge-to-Gateway health edge.

## Compose inputs and staged selection

First finish normal native authority preparation and its role/ACL/connection
limit admission using the existing project and database. Provision material
with `platform-tools data-security prepare` as documented in
[data-platform](../../docs/deployment/data-platform/README.md). The selected
PostgreSQL image and existing volume do not change. No deployment job adjusts
role limits or grants to make admission pass. The cert-only TCP HBA deliberately
rejects superuser connections: the security profile's `database-bootstrap` job
runs native **`deployment-file-admission --expected-database <database>`** on
already prepared roles instead of a TCP admin bootstrap. It preserves the
startup dependency gate and has no admin DSN or admin certificate mount. Fresh
bootstrap and recovery remain a separate, explicitly controlled authority step.

Supply these absolute external paths in the existing selected private env file:

* `WORKLOAD_PRIVATE_DIRECTORY`: one generation root per Compose service name.
  Each root contains `current -> <generation>` and immutable `ca.pem`, `cert.pem`,
  `key.pem` files; private keys are 0600 and owned by the service UID. The SDK
  directory is `sdk-agent-worker`, with identity suffix `sdk`.
* `PLATFORM_SECRET_DIRECTORY`: per-service directories, not a common directory
  mounted into every container. Gateway gets only `jwt-secret`, `encryption-key`,
  `internal-auth-keys`, `lease-private.json`; Knowledge roles get
  `internal-auth-keys`, `model-private.json` and the purpose-specific
  `storage-signing-key` (never the Gateway provider-decryption key). Only dual mode mounts each
  service's `internal-token`. Keep required file permissions private to the
  service UID. Keyring/file contents must satisfy the native authority formats.
* `WORKLOAD_PUBLIC_KEYRING_FILE`: the Gateway **public** lease/broker verifier
  keyring. Consumers mount only this public keyring; legacy HMAC proof and lease
  signing secrets are removed in both secure deployment stages.
* `KNOWLEDGE_MODEL_WORKLOAD_PUBLIC_KEYS_FILE`,
  `KNOWLEDGE_MODEL_GATEWAY_PUBLIC_KEYS_FILE`,
  `KNOWLEDGE_MODEL_PRICE_BOOK_REVISION`, `KNOWLEDGE_MODEL_REVISIONS_JSON`:
  activated model authority inputs. Only Gateway receives provider credentials.
* `DATA_PRIVATE_DIRECTORY`: native generated data material; each app mounts only
  its canonical role subdirectory. Default direct connections use `dsn-control`,
  `ca-bundle.pem`, `tls.crt`, `tls.key`; no inline password DSN or split Postgres
  password survives the effective app configuration. Match role certificate CN,
  actual database, host SAN, connection limit and process count.
* `DATA_AUTHORITY_DIRECTORY`: separate operator-installed `migrator`, `gateway`,
  `runtime`, `capability_worker`, `knowledge_api`, `knowledge_worker` directories.
  Each has `dsn`, `ca-bundle.pem`, `tls.crt`, `tls.key`. Only the read-only
  admission job sees all six. Migration sees only `migrator`; ordinary services
  never receive migrator/admin credentials. Migrator DSN is direct PostgreSQL,
  with explicit `ai_gateway_migrator` and the expected physical database.
* `DATA_CLIENT_CIDR`, `DATA_BACKEND_CIDR`: distinct owner-validated private CIDRs
  matching the generated HBA, current network plan and selected existing project.

Use the native `workload-credentials init-ca/issue/activate` tool to prepare a
small Compose CA and per-service generations. Keep the CA key solely in the
external issuer directory; never mount that directory into an app. Install
selected role/workload files for UID1000 (Gateway/Knowledge), UID10001
(Runtime/control/SDK), and the selected Postgres/PgBouncer UIDs. Do not perform
privileged ownership repair during application startup. Workload rotation adds
and activates a generation under the same read-only bind root; each new native
connection reloads the credential generation. Overlap trust bundles before CA
rotation, then remove the old CA only after peers use the successor. Database
and ordinary config secrets are startup inputs: prepare successor files and
perform an owner-reviewed rolling restart; a file update is not pool rotation
or drain evidence.

Always use **one base and one complete security override**. Compose5.6.0 was
observed restoring values removed by later reset fragments/late `extends` in
multi-file profiles. `docker-compose.security.yml` is a checked materialization
of the two security fragments. `deploy/local/compose.full.yml` resolves the
existing local infrastructure/runtime/app definitions before secret removal.
Native source-freshness tests reject drift. Do not merge the separate security
fragments directly or append the old local app `extends` file after security.
Default non-security entrypoints retain their existing behavior.

Published-image reference, initial dual stage (commands are for subsequent
owner-authorized integration, not an instruction to start an unreviewed stack):

```bash
# Existing selected env still satisfies legacy interpolation in the base;
# discarded values do not enter effective secure container environments.
files=(-f docker-compose.yml
       -f deploy/compose/docker-compose.security.yml
       -f deploy/compose/docker-compose.workload-dual.yml)
docker compose --env-file .env "${files[@]}" config --format json > "$PRIVATE_RENDER"
platform-tools helm-contract "$PRIVATE_RENDER"
# After ownership, image/source, authority, memory and paired-service admission:
# docker compose --env-file .env "${files[@]}" up -d --no-build --pull never
```

For the existing local project, replace the base with
`-f deploy/local/compose.full.yml -f deploy/local/compose.security.yml`, retain
`--project-directory "$PWD" --project-name agentforge-dev-7801d4a9a4
--env-file .env.dev`, and keep the same complete security override/dual file.
No second infrastructure, project, volume or implicit image build is introduced.
All image/source admission and `assert_compose_owner()` checks remain required.
Protect rendered files; unlike the checked synthetic fixtures, a real rendered
configuration can contain unrelated existing runtime secrets.

For required retirement, **remove** `docker-compose.workload-dual.yml` from the
selected file list, then optionally append `docker-compose.workload-required.yml`
to make the choice explicit. Recreate the owned containers so their environment
and token mounts are actually removed. Do not append required after dual and
claim its old token mount vanished. The effective configuration contract rejects
that composition. Required has no internal token setting, model-plane transport
bearer, shared proof/lease signer, or legacy bearer fallback. Signed lease and
actor authorization remain.

Optional session pooling appends `docker-compose.data-session-pooling.yml` with
`--profile data-pooling`; supply owner-verified PgBouncer>=1.24 image digest,
UID/GID and its generated private material. The direct security entrypoint does
not require a PgBouncer image or start a pooler. Native backend-pinned consumers
use `*_session` aliases, not transaction pooling. Gateway cancellation and
migration remain direct with the same Gateway role or the dedicated migrator.
The pooler backend network contains only Postgres/PgBouncer; direct clients use
the cert-only client network. No edge workload joins either data network.

Optional SDK appends `docker-compose.sdk-workload.yml --profile sdk-agents`, and
during dual only `docker-compose.sdk-workload-dual.yml`. Do not combine this
entrypoint with the separate legacy `sdk-agent-worker.yml` TLS listener. SDK
uses the native Gateway workload facade and never receives provider keys or
provider base URLs. Required removes the SDK purpose-scoped transport token.

## Helm: projected inputs, private regular files and rotation

Keep all existing release input gates: image digests, selected PVCs, exact store
peers, verified kernel revision, model authority and singleton counts. New values:

* `workloadIdentity.mode: disabled|dual|required` defaults to `disabled`.
* `workloadIdentity.secrets`: six independently scoped SVID Secret names
  (`gateway`, `runtime`, `control`, `knowledge`, `knowledgeWorker`, optional
  `sdk`). Each Secret contains exactly `ca.pem`, `cert.pem`, `key.pem`.
* `images.tools`: the native tools image pinned by digest; includes actual
  `workload-credentials` and OpenSSL. Apps do not need issuer binaries.
* `workloadIdentity.applicationSecretGeneration`: explicit ordinary file-secret
  rollout nonce. For local Knowledge storage, its selected credential Secret
  additionally supplies `KNOWLEDGE_STORAGE__SIGNING_KEY`. Changing the nonce rolls pods; application config files are copied to
  private owner-readable files by a bounded-key init container.
* `dataSecurity.enabled`, `generation`, `roles`: enable one distinct Secret per
  database role, including migration; `generation` is the database rotation
  restart nonce. Secret keys are `dsn`, `ca-bundle.pem`, `tls.crt`, `tls.key`;
  Gateway additionally needs direct `control-dsn`. Each strict data reader sees
  an explicit regular `subPath` file, not a projected symlink. SubPath mounts do
  not live-rotate; secret generation alone does not drain existing connections.
* `dataSecurity.mode: direct|session`, physical database, host, measured role
  limit, bounded pool and statement timeout. Session mode additionally requires
  `authority-dsn` in each application role Secret and an explicit
  `directPostgres.peer/port` plus `controlHost`. Migration remains direct; its
  Secret uses only `dsn`. `data.postgres.peer/port` then selects the pooler.
  NetworkPolicy preserves the direct authority/cancellation connection.

SVID Secrets may be fed by the existing SPIFFE/SPIRE/mesh distribution system.
Registration must bind namespace, selected ServiceAccount and exact component
pod selectors to the full service identity; a Secret name, HTTP header or
network location does not establish identity. This chart does not install a
SPIRE server or invent a mesh trust authority. A mesh must preserve native
end-to-end mutual TLS, or issue the native SVID inputs under that same contract.
Unverified mesh headers are rejected.

The pod mounts a bounded 0440 projected Secret **only** into the native importer
init container and rotation sidecar. The importer pins one `..data` generation,
checks URI identity, chain and key, copies it into a private memory emptyDir and
atomically activates `current`. Only this explicit projection-import boundary
accepts group-readable input keys; main service readers retain 0600 checks.
The service mounts only the private output read-only. Failed import never
certifies a new generation: sidecar readiness fails until successful import;
the service never silently substitutes another identity. Existing credential
validity still bounds established use. Track expiry and perform real rotation
and revocation drills before acceptance. EmptyDir is bounded; recreate pods
under the normal rotation runbook before accumulating old generations fills it.

The chart mounts no shared bearer key in required mode. Consumers get public
lease/broker verification keys only. Application credentials are selected by
key, never `envFrom`. It creates no application Secret, CA issuer, database or
external paid resource. Existing `agentforge-secrets`/secret-manager ownership
remains; provision the selected per-role/SVID Secrets before release.

## Offline verification and remaining runtime acceptance

`platform-tools helm-contract` accepts Helm YAML or effective Compose JSON.
Native tests verify schema/negative cases, importer boundary, role DSNs,
application read-only mounts, retained bearer rejection and checked-source
freshness. `tests/workload-rotation.sh` exercises shipped rotator control flow
with a mock importer; it is explicitly not TLS evidence. Native primitive tests
supply actual certificate/TLS and projected-input permission checks.

Observed in this implementation: eight Helm variants and ten root/local Compose
variants rendered using synthetic inputs; strict offline graph/mount/secret/port
checks passed. No native test, Docker image, real DB role identity, TLS handshake
between service processes, live rotation, install/upgrade/rollback or chaos test
was run. Root owns serialized compilation and paired-service acceptance. Keep
all original release and singleton retirement gates until that evidence exists.

### Runtime-local kernel bridge and optional strong pool

Both enabled workload stages configure the Runtime loopback
`http://127.0.0.1:8094/internal/v1/model-proxy` while its actual model-plane
target is the admitted Gateway HTTPS origin. Supply a separate Runtime-only
`RUNTIME_MODEL_PROXY_TOKEN` (at least 32 bytes) in Compose, or the Runtime
credential Secret's `AI_PLATFORM_RUNTIME_MODEL_PROXY_TOKEN` key in Helm. This
matches the Execution process's current local-token contract: it is a
Runtime-local credential, not a retired cross-service bearer, and is never sent
to Gateway/Knowledge/SDK or the sandbox. Public callers cannot invoke the bridge
because native admission checks the actual loopback socket peer as well as its
local token. Gateway itself also mounts the public lease/broker keyring; its
private Ed25519 issuer file remains exclusively Gateway-owned.

`sandbox.strongPool.enabled` defaults to false. Its complete enabled input schema
is the unchanged owning `deploy/sandbox/strong-pool.schema.json`, embedded in
chart validation. Set its dedicated namespace, node, measured RuntimeClass,
immutable sandbox/control images, shared node root, API-server CIDR, runtime
binary hash, seccomp/CSI and attestation inputs, plus the Worker's existing
purpose-scoped `sandbox.signingSecret`. `warm` cannot exceed `maximum`. The
legacy `sandbox.enabled` socket authority and strong pool are mutually exclusive.

The Worker is pinned to the pool's node and mounts only
`<sharedHostPath>/control`, read-only at `/run/agentforge-sandbox`, with
`AI_PLATFORM_SANDBOX_SOCKET=/run/agentforge-sandbox/control.sock`. It receives no
slot parent, containerd socket, host `/proc`, attestation private key or sandbox
ServiceAccount token. The separate controller/attestor template remains owned by
Execution. Native render checks recognize only that exact infrastructure role;
normal app hostPath/token/root restrictions remain. They reject a generic
DaemonSet and a Worker parent-directory/other-node mount. The attestor's explicit
node privileges and RBAC remain infrastructure admission inputs; rendering the
configuration is not measured-runtime or escape-test evidence.

## Knowledge dataset Cedar PDP

Defaults remain off. Secure Gateway configuration now sets
AI_PLATFORM_WORKLOAD_KNOWLEDGE_CLIENT_IDS to exactly the independent Knowledge
API and Knowledge worker SPIFFE identities. It is a strict subset of the normal
listener allowlist: Runtime, SDK and capability-worker identities are excluded.
The native route guard applies this subset to POST
/internal/v1/authorization/dataset and /internal/v1/authorization/dataset/batch;
an absent subset admits no certificate caller. Existing signed current-actor,
tenant and loaded dataset-fact checks still apply.

Activate the paired PDP only with workload mode required. For Helm set
datasetAuthorization.mode to shadow or enforce and supply
datasetAuthorization.expectedPolicySha256 and expectedMatrixSha256 from the
native **resource** policy/differential report for the admitted role map. The
chart emits one coherent mode/pin set into Gateway's
GATEWAY_CEDAR_CONFIGURATION.resources and both Knowledge processes'
KNOWLEDGE_CEDAR_DATASET_CONFIGURATION. Knowledge gets exactly origin, mode,
expected_policy_sha256 and expected_matrix_sha256; its origin is the admitted
private Gateway TLS Service. Capability policy pins are a different contract.

For Compose append exactly one of docker-compose.cedar-dataset-shadow.yml or
docker-compose.cedar-dataset-enforce.yml after the canonical security profile
and workload-required selection. Both consume the same
CEDAR_DATASET_POLICY_SHA256 and CEDAR_DATASET_MATRIX_SHA256 variables; missing
pins fail interpolation. These overlays configure all three processes together.
Do not combine them with a retained dual bearer overlay. The native rendered
deployment validator rejects dual/plaintext PDP transport, missing/widened TLS
subsets, one-sided rollout, mode/pin mismatch and unadmitted origins.

The templates' environment changes roll all affected Pods. A rolling interval
with mismatched receipts fails closed until both sides agree; there is no
legacy ACL fallback for a configured but unavailable/incompatible remote PDP.
Remove the optional Compose PDP file or set the Helm mode off to restore the
unconfigured default consistently across Gateway and both Knowledge roles.
The checked-in Cedar values are offline syntax fixtures with fake pins, never
an accepted native policy/matrix receipt. No deployment or policy activation
was performed by adding these source paths.
