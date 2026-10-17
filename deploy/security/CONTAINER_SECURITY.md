# Compose container and data-plane admission

`platform-tools container-security source` checks public source profiles;
`platform-tools container-security compose --config FILE` checks rendered Compose
JSON/YAML. Neither discovers env files nor prints environment values. The source
matrix covers root, full/compact/scale, build/dev/native-candidate, local
infrastructure/runtime/delegation, KB infrastructure and the SDK worker overlay.
Source merging is an admission precheck; the exact Docker-rendered configuration
must also pass before applying it. No running project or volumes were changed by
this implementation.

Every service has an explicit numeric non-root UID/GID, a read-only root,
`cap_drop: [ALL]`, no-new-privileges, a PID bound and bounded noexec/nosuid/nodev
`/tmp`. PostgreSQL also has a bounded owned socket-directory tmpfs. RustFS has a
bounded log tmpfs. Existing persistent volume names/paths and loopback published
ports remain. Gateway's directory-check job uses the Gateway image and UID 1000,
with no network or privileged ownership repair.

Networks are `edge` (Frontend/Gateway), `app` (service RPC/provider egress), and
internal per-store segments: `data-postgres`, `data-redis`, `data-object-store`,
and `data` reserved for Qdrant. Only Qdrant and the Knowledge API/worker join the
Qdrant segment. Gateway routes vector requests through Knowledge. Object-store
owners are Gateway, Knowledge API/worker, Runtime and the Capability control
process; Web/SDK processes do not join it. Capability control retains its scoped
Postgres access; Python child sandboxes still require their separate network
namespace and never acquire a Compose network or datastore credential.

## Existing-volume preparation

Do not delete, rename or recreate volumes to satisfy a permission check. Before
an authorized maintenance restart, the operator must check the single existing
project's ownership labels and inspect volume ownership. PostgreSQL Alpine uses
70:70, Redis Alpine 999:1000, Qdrant 1000:1000, native Gateway/Knowledge 1000:1000,
and Runtime/Capability/SDK/RustFS/Tempo 10001:10001. These IDs are explicit
container contracts; replacement image overrides must support them. Existing
Qdrant root-owned storage and Runtime home mountpoints may need a separately
controlled, backed-up ownership migration. This is not automated through a root
service, and has not been performed. A non-writable volume correctly prevents
startup. The project's names, volume contents and database authority remain
unchanged.

## Qdrant TLS and read/write roles

The operator provisions private runtime inputs outside source control:

- `QDRANT_API_KEY` and distinct `QDRANT_READ_ONLY_API_KEY`.
- `QDRANT_TLS_CERT_FILE`, with DNS SAN `qdrant` (and loopback SANs when host clients
  use loopback URLs), and `QDRANT_TLS_KEY_FILE` readable by container UID 1000.
- `QDRANT_CA_BUNDLE_FILE`: a PEM trust bundle containing the Qdrant CA **and system
  roots**, so Knowledge's other provider HTTPS clients retain their trust roots.

No default key, optional empty-key fallback, plaintext datastore transport or
TLS-verification bypass is provided. Binds are read-only with
`create_host_path: false`. Qdrant snapshots/temp/initialization marker stay under
the existing storage volume. Its health probe verifies the TLS chain and DNS
identity with OpenSSL; `/healthz` is a liveness probe, not an authenticated data
access proof. Knowledge uses HTTPS with the supplied trust bundle. The API gets
the read-only key; the durable worker gets the write key. Compact's `all` process
also owns worker duties, so it explicitly gets the write key.

Read-only API admission may expose remaining API-side vector mutation paths.
Those responsibilities require source-owner reconciliation and real deletion,
ingestion and lexical cutover acceptance; this Compose change does not establish
that all API operations already respect the read/write split.

## SDK private TLS, in both directions

The SDK overlay uses app-only HTTPS instead of sharing Gateway's network
namespace. Provision **separate-purpose** inputs:

- `SDK_WORKER_TLS_CERT_FILE`, `SDK_WORKER_TLS_KEY_FILE`, `SDK_WORKER_TLS_CA_FILE`:
  the worker certificate has DNS SAN `sdk-agent-worker`; its private key is
  owner-only and readable by UID 10001. Gateway receives only its public CA.
- `SDK_GATEWAY_TLS_CERT_FILE`, `SDK_GATEWAY_TLS_KEY_FILE`,
  `SDK_GATEWAY_TLS_CA_FILE`: Gateway callback certificate has DNS SAN
  `gateway-sdk-callback`; its key is readable by UID 1000. Worker receives only
  its public CA.

Worker settings are `SDK_AGENT_WORKER_BIND` (default loopback 8096), and
`SDK_AGENT_WORKER_TLS_CERT_FILE`, `KEY_FILE`, `CA_FILE`. A non-loopback bind
requires all three valid TLS files. Health connects locally while checking the
`sdk-agent-worker` DNS identity; it never disables
hostname verification. Default loopback HTTP remains available for local native
development; HTTP to any non-loopback origin remains rejected.

The Gateway callback TLS listener defaults off. The SDK overlay enables
`SDK_AGENT_GATEWAY_TLS_ENABLED`, `_BIND`, `_CERT_FILE`, `_KEY_FILE`. The bind is
`gateway-sdk-callback:8097`, an alias assigned on **app only**. Startup requires
one private app address, and binds that IP rather than all Gateway interfaces;
ambiguous/public resolution fails. No host port is published. The listener
clones the already assembled Gateway Router, retains its authentication,
authorization and middleware, and allows only the four existing SDK-tools POST
paths. SDK bridge `SDK_AGENT_GATEWAY_TLS_CA_FILE` adds this CA to its client trust
without affecting provider clients. The default callback origin is
`https://gateway-sdk-callback:8097`.

This is server TLS plus existing bearer/scoped grants, **not mutual TLS** and not
completion of U-PE-12. Configuration, wrong CA/name, malformed/partial files,
key mismatch/permissions, plaintext downgrade and route-scope negative tests do
not replace an integrated container run.

## Acceptance still required

Run the primary writer's serialized package tests and exact Compose render gate.
Then verify non-root IDs, read-only filesystem failures, bounded writable paths,
existing data/volume continuity, provider/HTTP/WS/SSE/upload behavior, Qdrant data
requests rejecting absent/wrong keys, read-only write rejection, worker ingestion,
TLS wrong CA/hostname rejection, edge/SDK/sandbox datastore reachability denial,
legitimate S3 artifact access, SDK stream/reconcile/delete callbacks and shutdown
with both private listeners. No Docker/Cargo build, restart, credential generation,
Helm change or real Qdrant authentication/network test was performed here.

Official source references used for configuration/UID contracts:
- https://github.com/qdrant/qdrant/blob/v1.18.2/config/config.yaml
- https://github.com/qdrant/qdrant/blob/v1.18.2/Dockerfile
- https://github.com/qdrant/qdrant/blob/v1.18.2/src/startup.rs
- https://github.com/docker-library/redis/blob/master/7.4/alpine/Dockerfile
- https://github.com/docker-library/postgres/blob/master/16/alpine3.23/Dockerfile
- https://github.com/rustfs/rustfs/blob/1.0.0/Dockerfile
- https://github.com/grafana/tempo/blob/v2.5.0/cmd/tempo/Dockerfile

The independent Python sandbox is now an explicit network-free exception to
service networking (it joins no segment). Its separate image, public verifier
arguments, private work/journal volumes, read-only control socket sharing and
sandbox-only delegated cgroup are enforced by the same gate. See
[sandbox-service.md](sandbox-service.md#compose-integration-implemented) for the
purpose key/file inputs and unrun exact-image/Linux acceptance. The former
control Worker cgroup binding is rejected rather than retained as an exception.
