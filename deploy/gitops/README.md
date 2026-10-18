# Candidate delivery, artifact admission and Compose includes

These are source contracts, **disabled by default**. No cluster, registry, repository branch,
existing Compose project or volume has been changed. The ApplicationSet starts with no elements,
its Applications have no automated sync/prune/self-heal, and the delivery chart renders no
resources with its default values. Apply/bootstrap/sync/publish operations need their ordinary
owner authorization and the original release/HA/rollback acceptance gates.

## Signed artifacts before migrations or application changes

Use the existing Rust topology chart and `deploy/gitops/chart` as two sources of one Argo CD
Application at the same exact reviewed 40-character Git revision. The supplied empty-list
ApplicationSet defines this wiring. Candidate values files contain immutable public image refs
and existing Secret/PVC/config names; never commit private credentials. The AppProject is
restricted to this repository and the existing `agentforge-candidate` namespace, with explicit
namespaced resource kinds and no cluster resource permissions or namespace creation.

The delivery chart's PreSync policy ConfigMap and NetworkPolicy run at wave **-30**. Its
short-lived verification Job runs at **-25**, before the native migration hook's existing Helm
weight **-5** and before normal application sync. Every image is checked using the same current
repository signing/scanner policy plus:

1. An actual keyless signature and current-policy critical scan for both amd64 and arm64,
   through the existing `supply-chain` verifier.
2. Fresh signed license-admission predicates for both platforms, under the exact current
   dependency-policy SHA. The predicate binds the canonical content hash of a signed SPDX
   document; missing, changed, stale, wrong-subject or wrong-policy artifacts fail closed.
3. Signed native release provenance binding the immutable index, actual admitted architecture
   source digests, source repository/revision and both policy hashes. No SLSA level is claimed.

The native publisher emits these before semver/SHA tag promotion, retaining BuildKit's existing
SBOM/provenance descriptors and all current critical, size and license rejection gates. The
release-set verifier also checks service-to-repository identity, the shared Knowledge image,
and equal signed Runtime/Worker source revisions. Existing source/image locks remain required;
this additional gate does not replace their exact source-release-unit proof.

The delivery Job has no service-account token, credentials or application network ingress. Its
public registry/Fulcio/Rekor HTTPS egress excludes private/link-local networks, with explicit DNS
access. It uses a numeric non-root identity, read-only root, dropped capabilities, seccomp and
bounded writable scratch/resources. The **verifier image itself** is a separately reviewed
bootstrap digest, not something trusted merely because it can verify other images. Build the
optional `delivery-verifier` target only after independently freezing the exact official cosign
image and binary SHA, using `COSIGN_IMAGE` and `COSIGN_BINARY_SHA256`. The latter has no default
and must match the copied executable. That target is not automatically published or added to
existing image budgets; its actual image measurement/signature/bootstrap acceptance must happen
before enabling the chart. Initial namespaces, Argo CD/Rollouts CRDs/controllers, private
metrics-read Secret, data operators/volumes and image-pull access remain operator responsibilities.

Do not enable an image/service absent from the signing policy. Runtime/Worker's controlled
publisher must produce the same artifacts and current policy-approved signing identity; the
five-image native publisher alone does not supply those images. Denied GPL/MPL/font/data terms
remain denied. A root Apache LICENSE/NOTICE in an image and a generated package inventory do
not fulfill every third-party source/notice obligation. No exception or ignore is introduced.

## Frontend-only canary

Gateway and Runtime stay at one replica, with no HPA or Rollout conversion until their original
HA/fencing/failover contracts pass. This candidate only permits the stateless **frontend**.
The combined rendered-manifest fitness gate rejects guarded scale/HPA/canary bypasses.

Canary enablement is a separate explicit input. First accept a stable frontend Rollout bootstrap
and then set `bootstrapAccepted`; first-ever Rollout creation is not a rollback drill. The main
chart must provide the referenced frontend Deployment with `frontend.replicaCount: 0` and
`frontend.autoscaling.enabled: false` while the Rollout owns replicas. Otherwise the Deployment
or HPA could roll the candidate directly and bypass analysis; the fitness gate rejects it.
The existing Deployment still supplies the full supported Pod template/config/TLS settings.

At four replicas, steps use pod ratios of 25%, 50% and 100%; these are not exact Layer-7 traffic
weights. A canary-specific Service probes the real `/health` JSON endpoint. Each intermediate
step also requires live Gateway traffic and both short/long availability and latency recording
windows. Empty results, NaN, zero/insufficient traffic, HTTP/Prometheus errors and threshold
failures do not promote. The operator's Prometheus address and existing metrics-read bearer
Secret are explicit inputs; no credentials are put in values or source. Analysis failure aborts
and scales down the canary, leaving the stable ReplicaSet. A real degraded-canary→abort→stable
traffic recovery drill is still mandatory; source templates do not establish that outcome.

The API SLO gate observes overall system health, not proof that a canary caused an error. The
canary health probe is per-candidate, while real synthetic user journeys and per-version RUM
remain separate product acceptance evidence. Non-production Application/Project/CRD schema,
restricted-network hook execution and initial Rollout adoption all need real cluster validation.

## One include bundle for explicitly selected Compose features

`compose-profiles.json` records the current root topology and ordered opt-in feature sources.
`platform-tools delivery-policy compose-plan --project-name EXISTING --mode full` prints a render
command only. Repeated `--feature NAME` selects features explicitly; dependencies must also be
explicitly selected. Unknown/duplicate/incompatible choices fail. Profiling requires
observability; SDK source build requires SDK service selection; source build and native candidate
are mutually exclusive. No feature is enabled by default.

With `--output /absolute/new/external/compose.json`, the planner writes one Compose `include:`
bundle containing a **single include entry with an ordered path list**, so overrides merge within
one included project rather than duplicating services between includes. It refuses existing,
internal or symlink-traversing output paths. `project_directory` and the explicit existing project
name retain root-relative mounts, networks and volume identity. The planner never reads `.env`,
executes Docker, starts a service or selects a new project. Actual owner labels and environment/
secret-file inputs must be checked before the printed render or any later action.

Modes remain full/compact/scale. Optional root features include source/dev/native candidates,
SDK worker/build, data security/pooling, Valkey split, sandbox packages, observability and profiling.
Standalone legacy KB/local-infrastructure descriptors remain under their existing ownership;
they are not blindly merged into the root application project. Existing `make hot-update` and
`make status` implementations/meaning remain untouched. Adopting this planner in those shared
entrypoints is an integration change for the primary, after real rendered-config equivalence.

## Fitness and verification

`platform-tools platform-fitness` checks current image-variable identity, existing hardening and
store gates, the previously uncovered monitoring/profiling hardening, action SHA/permissions,
Knowledge/SDK provider credential exclusion, dependency authority boundaries, unchanged strict
license/retired-crate policy, supported base lines and the current GitOps policy snapshots.
`rendered-compose --config FILE` additionally reuses exact identity/pairing, hardening/network
and service-database-role admission. No env values are returned. `rendered-kubernetes --config
FILE` protects Gateway/Runtime singletons, frontend workloadRef/HPA ownership and artifact-hook
ordering. Each new rule has negative unit cases, with original pairing/database/network tests
retained. No waiver or fake acceptance result is returned for missing evidence.

The primary must wire the two new tool modules/commands, add fitness and its negative cases to
harness/gate self-test, then run one controlled compile/test batch, exact Helm/CRD rendering,
Prometheus/Collector validation and real acceptance. Hosted native arm64/amd64 builds and the
single production Docker source already existed and remain intact. No QEMU, relaxed byte ceiling
or forced success is added. Actual build/release elapsed time ≤45 minutes, image byte budgets,
real SBOM/license fulfillment and real signed deployment/automatic rollback remain measured gates.
