# Native AgentForge Helm release

`ai-gateway` v2 replaces the retired Python/LangGraph chart. It packages native
Gateway, Knowledge API and ingestion worker, Runtime, capability control worker,
frontend and the native database authority hook. Each code unit uses its own
admitted `repository@sha256:digest`; Knowledge roles share the Knowledge image.
Gateway/Runtime/control remain one process owner with Recreate deployment strategy.
Only frontend and Knowledge API use HPA. Knowledge worker optionally uses a KEDA
PostgreSQL queue query and independently scoped TriggerAuthentication.

Defaults intentionally fail admission. `tests/values-render.yaml` is an **offline
fixture**, not deployment configuration: its digests/revisions/CIDR are dummy.
No chart creates PostgreSQL, Valkey, Qdrant or object storage. Install their actual
operators first (CloudNativePG, the selected Valkey operator and Qdrant distributed
chart), provision their backups/TLS, then reference their real namespace AND pod
selectors. NetworkPolicy enforcement requires an enforcing CNI. Provider egress
allows only explicit CIDRs on TCP443; an empty list denies provider traffic.

## Prerequisite credentials and storage

The default chart is a shared-storage profile: `global.deploymentMode=multi-instance`
and `data.objectStore.enabled=true`. Configure its real endpoint, bucket and credential
Secret. Explicit multi-instance mode, multiple Knowledge replicas, or Knowledge HPA/KEDA
refuse local storage. A single-instance local-storage profile must keep Knowledge replicas
at one and disable autoscaling. The offline base fixture covers that local profile; the
full fixture retains multi-replica/S3/HPA/KEDA coverage. Gateway and Runtime's independent
singleton admission guards remain enforced.

Run database authority deployment-bootstrap separately under an explicitly
controlled admin identity; the application release never receives ADMIN_DSN.
`databaseRoles.existingSecret` must already hold distinct GATEWAY_DSN,
KNOWLEDGE_API_DSN, KNOWLEDGE_WORKER_DSN, RUNTIME_DSN, CAPABILITY_WORKER_DSN and
MIGRATOR_DSN. Include the correct search_path/timeouts in each scoped DSN.
The pre-install/pre-upgrade hook consumes MIGRATOR_DSN only; each application
initContainer runs `database-authority startup-check --principal <role>` before
starting. Hooks cannot wait on Secrets/operators created later by the same release.

Use the separate `agentforge-secrets` chart with an already configured External
Secrets v1 controller/SecretStore, or pre-provision equivalent Secrets. Its values
contain **remote references only**. Wait for each ExternalSecret Ready before the
application upgrade; never put secret bytes in Helm values or command arguments.
Credential key names are explicit in each native template:

- Gateway: REDIS_URL, GATEWAY_AUTHENTICATION__JWT__SECRET, GATEWAY_ENCRYPTION_KEY,
  AI_PLATFORM_INTERNAL_TOKEN, AI_PLATFORM_CAPABILITY_PROOF_SECRET.
- Knowledge: REDIS_URL, INTERNAL_COMM_REDIS_URL, AI_PLATFORM_INTERNAL_TOKEN,
  AI_PLATFORM_CAPABILITY_PROOF_SECRET, GATEWAY_ENCRYPTION_KEY.
- Runtime/control: POSTGRES_HOST/PORT/USER/PASSWORD/DB for their own DB role,
  AI_PLATFORM_INTERNAL_TOKEN and AI_PLATFORM_CAPABILITY_PROOF_SECRET; Runtime
  additionally AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_INTERNAL_TOKEN.
- Qdrant client Secret: QDRANT_READ_ONLY_API_KEY for API, QDRANT_API_KEY for worker.
  `caConfigMap` contains `ca-bundle.pem` with system roots plus Qdrant CA.
- Optional S3: ACCESS_KEY/SECRET_KEY in the selected existing Secret; endpoint,
  bucket and region are public values. Gateway/Knowledge share artifact storage;
  Runtime/control retain the necessary S3 network edge but do not receive unused
  storage credentials.

`lease.gatewayPrivateSecret` contains `private.json` for Gateway only;
`lease.publicConfigMap` contains consumer-only `public.json`. Model-plane refs
separately supply Knowledge's own workload `private.json`, its workload public
`public.json` to Gateway, and Gateway verification-only `public.json` to Knowledge.
`modelPlane.priceBookRevision` must identify a genuinely activated price book;
`modelRevisions` must match registered provider/model revisions. Supplying a string
passes shape validation, **not** database activation/readiness acceptance.

Existing PVCs are never created/deleted by this chart. Gateway owns UID1000 data,
logs and private memory subdirectories. Knowledge API/worker share an externally
prepared RWX claim at UID1000. Runtime/control own UID10001 claims. Native database
init containers create required subdirectories without root. Inspect actual
storage-driver ownership behavior and backup/restore before deployment.

## Optional isolated Python user code

The sandbox is disabled by default. Enabling it requires the independently built
sandbox digest, pre-installed RuntimeClass and node-local seccomp profile, an
explicit node hostname and prepared socket/workspace/journal claims. Control and
sandbox are independent Pods pinned to the same node: UDS is not a cross-node or
NFS transport. Socket storage must support local Unix sockets and its driver must
have fsGroupPolicy None; `socketFsGroupMutationDisabled: true` asserts that verified
operator prerequisite. Sandbox directories must already be private0700 UID10001.
Only control receives a purpose-specific 32-byte signing.seed Secret; an unprivileged
init copies it to an owned memory volume with mode0400. Sandbox clears Kubernetes service-discovery environment with env -i and receives only public
verifier arguments, no environment, platform credentials, service token, Service,
TCP port, hostPath or network permissions. `sandbox.cgroupCSI` references an actual
pre-installed CSI driver exposing **only** that sandbox's delegated cgroup2 subtree;
this chart does not invent a working driver or mount the host cgroup hierarchy.
Without those verified prerequisites, keep Python execution disabled/fail closed.
Cgroup/PID enforcement, seccomp availability and socket ownership require actual
Linux acceptance. See `../security/sandbox-service.md`.

## Routing and verification

Optional Gateway API HTTPRoute routes through canonical frontend nginx (HTTP,
WebSocket, SSE and upload paths preserved); optional Certificate uses an existing
cert-manager issuer. Select the actual controller namespace/pod labels for ingress.
An optional internal GRPCRoute binds Knowledge8093 only when explicitly enabled;
use a dedicated **private** parent listener. Workload authentication stays in the
native RPC handler. Do not attach it to a public parent. No Ingress annotation or
legacy Python port is retained.

With pre-installed tools, offline checks are:

```sh
helm lint deploy/helm/ai-gateway -f deploy/helm/ai-gateway/tests/values-render.yaml
helm template contract deploy/helm/ai-gateway -f deploy/helm/ai-gateway/tests/values-render.yaml > /tmp/agentforge-chart.yaml
helm template contract deploy/helm/ai-gateway -f deploy/helm/ai-gateway/tests/values-render.yaml -f deploy/helm/ai-gateway/tests/values-features.yaml > /tmp/agentforge-chart-full.yaml
helm unittest deploy/helm/ai-gateway
# Run compiled tests/contracts.rs checker against /tmp/agentforge-chart.yaml.
# kubeconform -strict requires locally supplied schemas for GatewayAPI/KEDA/cert-manager.
```

`tests/contracts.rs` is a Rust schema and rendered-resource checker with substantive
negative tests; integrate it into platform-tools using the workspace serde_yaml alias, serde/serde_json and a jsonschema test dependency (shared Cargo/main wiring belongs to primary).
Its main accepts one rendered YAML file; its tests require CARGO_MANIFEST_DIR set to
this chart directory when compiled standalone. It rejects mutable image tags,
missing native units, admin DSNs, consumer signer keys, hostPath and credentialed
sandbox Pods. `native_test.yaml` additionally provides helm-unittest render assertions.

Observed locally using primary-verified cached Helm4.3.1: application chart lint,
base and all-feature synthetic template rendering, seven malformed-input rejection
cases, ExternalSecrets chart lint, rendered resource/volume/env/hardening checks,
and ten standalone Rust schema/render-contract tests passed. Fake fixture digests
are shape tests only, never evidence that an image exists or is admitted. The Rust
checker passed against both actual Helm-rendered synthetic resource sets.
helm-unittest, kubeconform and kind install/upgrade/rollback were **not run**.
No cluster access, image build, startup or publishing occurred.
U-PE-30 release acceptance remains open until those real checks pass. The migration-only hook policy intentionally survives its Job; explicitly clean
that release-labelled policy on uninstall. Persistent application policies are
ordinary release resources and are never deleted/recreated by pre-upgrade hooks.

Optional `sdkWorker.enabled` installs the native SDK worker with its own admitted
image, UID10001 existing state claim, finite retention and purpose-specific TLS.
Its workerTLSSecret holds tls.crt/tls.key/ca.pem; callbackTLSSecret independently
holds Gateway callback tls.crt/tls.key/ca.pem. Certificates must cover the generated
`<fullname>-sdk` and `<fullname>-gateway` service DNS names respectively. The worker
receives only callback CA; Gateway receives only worker CA. credentialsSecret holds
SDK_AGENT_WORKER_TOKEN. The callback listener uses the same native authenticated
SDK-tool router and is reachable only by SDK Pods on8097; public HTTPRoute remains
frontend-only. This is verified-server TLS plus bearer, not mTLS acceptance.

## Native workload and database file security

The application chart supports `workloadIdentity.mode: dual|required` with
per-service projected SVID inputs and a digest-pinned native tools importer.
It copies projections into private memory volumes before strict application
readers consume them; a rotation sidecar validates and activates successor
credentials. `dataSecurity.enabled` selects distinct per-role mTLS/DSN Secrets,
startup/cancellation direct connections and an explicit database restart
rotation generation. Original defaults, image/source/authority gates, HTTP
health probes and singleton guards remain. Required mounts no shared bearer
or consumer signing secret. See
[the deployment contract](../workload-identity/DEPLOYMENT.md) for all input keys,
port/origin graph and the mandatory admission/rotation acceptance boundaries.
`tests/values-security.yaml` is a synthetic offline render fixture, not release
values or an admission receipt. Native `platform-tools helm-contract` also
validates effective secure Compose JSON.

Dataset Cedar PDP remains opt-in through ai-gateway datasetAuthorization.
Only required workload mTLS is admitted. One mode and resource-policy/matrix
pin pair generates Gateway and both Knowledge parser inputs together; see
[the native trust contract](../workload-identity/DEPLOYMENT.md#knowledge-dataset-cedar-pdp).
