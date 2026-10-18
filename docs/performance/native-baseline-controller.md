# Accepted-build performance baseline controller

Status: executable source candidate for U-PE-04. No performance run, accepted
build, service deployment or provider result is produced by this document.
The existing `performance.rs` fixed-GET harness and its CLI semantics are unchanged.

`platform-performance-baseline` is a native serial controller. Its optional
`platform-tools performance-baseline` dispatch uses the same library entrypoint.
It does not log in, create users, change provider configuration, grant permissions,
reset limiters/counters, deploy containers, sign an acceptance, delete documents,
or replay a write. An operator prepares one existing isolated fixture, with its
normal authentication, ACL, limiter, pricing and accounting paths enabled.

```sh
platform-performance-baseline plan
# BLOCKED: an accepted build and owned profile are missing. No network action.

platform-performance-baseline plan \
  --repo-root /workspace/AgentForge \
  --profile /operator/private/performance-profile.json \
  --acceptance /operator/private/accepted-build.json \
  --signature /operator/private/accepted-build.sig \
  --trust-key /operator/trust/performance-acceptance.ed25519.pub \
  --trust-key-sha256 EXTERNALLY_PINNED_PUBLIC_KEY_FILE_SHA256 \
  --output /external/fresh-plan-directory

# Change plan to run only for the existing accepted fixture. Every output
# directory is fresh; a failed directory is retained, never overwritten/reused.
# Add --allow-real-provider only for a separately authorized real-provider case.
```

Equivalent Make targets are `performance-baseline-plan`, `performance-baseline-run`
and `performance-baseline-l0`; `ARGS` carries the same explicit inputs. Live
execution uses a prebuilt operational Linux observer with authorized read access
to the service PID namespace and a local Docker socket. Do not run it in the
network-disabled compiler container or mount runtime credentials into a compiler.
A macOS process or Docker Desktop host RSS is not substituted for Linux service
RSS. Inaccessible `/proc`, PID namespace, socket or ownership produces BLOCKED.
No automatic privilege escalation or namespace/configuration changes are offered.

## Existing inputs and trust

Start from the [profile template](../../rust/platform-native/crates/platform-tools/src/performance_baseline/profile.example.json).
Its resource IDs, hashes, corpus, index, model and span names are placeholders,
not a fixture or a measurement. Replace them with existing approved values. In
particular, use the actual route response pointers and current instrumentation;
`REPLACE_WITH_ACCEPTED_*` names do not fabricate stages when a span is absent.
Keep the profile, actor token, optional trace token and database input private,
owned by the operator, mode 0600, with absolute paths. The observer reads only the
existing actor's bearer token; `/api/v1/auth/me` must return the exact tenant and
user. Read-only fixture probes run before every case and after the run. Include
existing dataset/index generations and applicable policy/limiter/pricing state
in those probes; mismatches stop the run. Privilege/tenant/ACL/accounting evidence
from the existing fixture acceptance is also required.

The controller accepts an external acceptance handoff, schema
`agentforge-accepted-performance-build/v1`. It never creates one or signs it.
The existing acceptance owner must bind these fields after actual build/fixture
acceptance, not wrap a compile or liveness result as an accepted build:

- `accepted: true`, `scope: isolated-owned-performance-fixture`, `expires_unix`
  (future, at most 24 hours), `profile_sha256`, `controller_binary_sha256`.
- `source_manifest: {path,sha256}`, `source_roots` (explicit public build roots),
  `boot_id`, `compose_project`, and the actual host `deployment_working_dir` label.
  The source root read by the observer may be a mounted copy; its entire selected
  inventory and bytes must match the signed manifest. Native workspace manifest,
  lock, Gateway, Knowledge-domain and primitives anchors are mandatory. No subset
  silently replaces a changed inventory. Include supporting crates, Runtime/CW
  source-lock inputs and other actual build materials in the admitted roots.
- `docker_binary: {path,sha256}` and `docker_socket` for the existing local Unix
  socket. Only an exact bounded `docker --host unix://... container inspect`
  command is executed. No lifecycle, pull, exec, network creation or remote context.
- `services`: every service in the accepted deployment profile. The set must
  exactly match `profile.services`; Gateway, Knowledge API/worker and trace backend
  are mandatory, with Runtime/CW/SDK/data/other services included when deployed.
  Each entry contains `id`, `telemetry_service`, unique `instance_id`, application
  `pid`, `/proc` `start_ticks`, nonroot `uid`, full `container_id`, immutable
  `image_id` (`sha256:...` Docker image config ID), `binary_sha256`, private
  `environment_sha256`, `materials: [{path,sha256}]`, `network_addresses` and
  optional `listener_port`. Include mounted configuration/credentials that can
  alter the admitted behavior in materials; hashes are checked, contents withheld.
- Each service's `runtime_settings` exactly matches the selected live inspect
  object: `memory_limit_bytes`, `nano_cpus`, `cpu_quota`, `cpu_period`, `privileged`,
  `readonly_rootfs`, `security_opt`. Privileged containers are refused. Limits of
  zero mean the actual Docker setting is unbounded; do not relabel a declared VM
  budget as an observed per-container limit. Resource samples remain exact main
  application PID samples, not whole-container memory or descendant-process RSS.
- `gates` named `build_acceptance`, `privilege_tenant_acl`, `fixture_ownership`,
  `accounting`, each `{document:{path,sha256},pointer,expected}`. These reference
  existing private, successful acceptance receipts. Exact hashes and successful
  booleans/accepted/pass values are checked. All gates remain independently owned;
  the performance tool never issues a replacement approval.
- `real_provider_authorized` is true only when the owner has authorized the real
  provider workload and its cost. It does not bypass the explicit run flag.

The detached Ed25519 signature covers
`b"agentforge-performance-accepted-build/v1\0" || exact_accepted_build_file_bytes`.
The public key is raw 32 bytes; its file SHA256 must match the independently
pinned CLI fingerprint. The signature is raw 64 bytes or base64url without padding.
There is no key generator, signer or “accept” command. This exchange format binds
existing acceptance evidence; supplying a JSON `accepted` field alone never works.
The private source manifest is `{schema:"agentforge-performance-source-manifest/v1",
files:{"relative/public/source/path":"sha256",...}}` with all files in the selected
roots, excluding build/cache directories. Runtime `.env` and PKCS12/PFX files
are refused as public source materials. Public credential implementation source
and tracked upstream PEM fixtures remain part of the exact signed inventory;
operator runtime secrets belong only in private mounted-material references.

## Actual measurements and boundaries

A run verifies signature, source inventory/bytes, accepted tool binary and gate
receipts before HTTP. It then verifies the live Linux boot/PID/start time, binary
bytes, UID/capabilities, environment/mounted-material hashes, exact Docker image,
project/working-directory labels, container membership (including an application
child of an init process), network addresses and runtime limits/security profile.
The numeric-loopback Gateway/trace endpoint must map to the selected container's
actual listening socket, or share the controller's verified network namespace and
exact owned port. Identity/source checks repeat before/after cases and finally.

The tool uses standard reqwest with no proxy, redirects or retries. Gateway and
trace origins are exact numeric loopback HTTP/HTTPS URLs; optional `transports`
entries `gateway` and `trace` contain `ca_file` and `identity_file`, each null or
`{path,sha256}`. Private PEM client identities enable existing mTLS, with normal
certificate verification retained. No “accept invalid certificate” option exists.
POSTs use the original authenticated Gateway routes. Incomplete/invalid JSON,
status, timeout, body budget and unstarted work are retained, including a received
status/header identity when body transfer subsequently fails. Fresh request
trace IDs and benchmark markers link the request, service trace and owned upstream.
There is no automatic account, ACL, limiter, model or deployment configuration.

The deterministic upstream supports bounded authenticated nonstreaming OpenAI
chat, embeddings and rerank responses with fixed usage facts. It binds only the
profile's already configured loopback or assigned private IPv4 interface, never
wildcard/public/DNS. A private bridge interface lets an unchanged bridged Gateway
reach it; peers are restricted to the verified Gateway container addresses and
local direct control. Binding failure blocks work instead of replacing another
listener. The operator must have already registered the dedicated test provider,
model, credentials and price terms. The tool never changes that configuration.
Synthetic upstream observations are synthetic; they are not provider acceptance.

- Gateway cases use the admitted public `/v1/chat/completions` route. A complete
  overhead observation requires six real, same-request Gateway span/event points:
  receive, admit, upstream dispatch, first byte, upstream completion and response
  completion. Unique ordered points on the Gateway clock yield receive→dispatch
  plus upstream-complete→response-complete. Missing/ambiguous points are unknown;
  the middleware's response-object timer is not silently substituted for body end.
  A direct upstream control is optional, separately measured and compared only
  after exact upstream request-body equivalence. No percentile subtraction occurs.
- A real-provider case additionally requires an approved `upstream_identity`
  selector with actual Gateway span attributes `server.address`,
  `gen_ai.provider.name`, `gen_ai.request.model`. An unmatched identity remains
  unknown, and observed synthetic traffic in that case fails its classification.
  `provider_scope` and case kind stay separate in all summaries; real authorization
  is required for every case declared `authorized_real`, including Knowledge.
- Knowledge retrieval goes through the signed resource-authorized Gateway path
  against the pinned corpus/index. Embedding, cache lookup, lexical/vector,
  fusion, rerank and serialization have individual stage selectors. Exact verified
  service-instance spans provide sum-of-work and union-active interval metrics;
  those quantities are reported separately for parallel work. An optional timing
  response pointer is accepted only with that same service's correlated trace.
  Absent stages are unknown unless an actual response condition explicitly makes
  them not applicable. Top-k document recall/precision accompany timing; the
  operator-pinned `min_recall_at_k` is a correctness guard, never an invented SLO.
- Ingestion creates only bounded text documents in the operator's existing
  dedicated fixture dataset. It submits once, captures even an invalid accepted
  response's candidate resource identity privately, then polls that exact
  authorized document to a confirmed dataset/document terminal state. It reports
  submit→terminal wall time, confirmed documents, submitted UTF-8 input bytes and
  actual indexed chunks when exposed. Batch throughput uses completed facts over
  the full serial measured window, including intervening observation/quiet time;
  it is not a mean of inverse per-document times. Failed/unknown submissions are
  never reissued. Effects can continue after timeout or SIGINT; the private journal
  supports operator reconciliation/cleanup without automatic deletion.
- Optional `accounting: {config_file,config_sha256}` uses the existing primitives
  `ConnectionConfig` with `PERFORMANCE_*` environment-map keys, Gateway scoped role,
  normal file-secret/mTLS verification, loopback and `POOL_MAX=1`. It starts a
  READ ONLY transaction **before** binding the tenant and reads only fixed SQL
  for exact tenant/user/Gateway-request ID usage. Actual provider token totals or
  explicit request-meter facts and nonnegative stored costs must agree. This is
  raw same-request metering evidence, not proof of every downstream accounting
  stage or replay safety. Missing/lagging/ambiguous evidence is unknown and prevents
  a complete model-case baseline; the observer never writes billing data.

Trace reads use the existing owned Tempo-compatible JSON API; OTLP JSON hex and
Tempo protobuf-JSON base64 IDs are supported. `service.name` and a unique accepted
`service.instance.id` must match a currently verified sampled process. Raw spans,
attributes, response bodies, prompts, documents, tokens, private paths, actor IDs
and credentials never enter the public report. Only allowlisted metric fields,
opaque generated trace IDs and build/process identity are exported.

Sampling is bounded across every declared service with a stable process start
identity. Start/end/sample-peak main PID RSS, CPU ticks and sampling resolution are
reported; peaks between samples, descendants, whole-container memory, pool waiters,
SSE token timing and remote cancellation remain distinct unknowns. The whole run
resource window includes warmups, trace/receipt probes, policy quiet windows and
background work. RSS safety failure stops new dispatches; an in-flight request is
still bounded by its deadline. This serial closed-loop harness is not an overload
or capacity search. Warmups remain separate, and any failed/unknown warmup blocks
its measured case. No user token or arbitrary response error text is logged.

## Output and verification

Each fresh external directory is mode 0700; files are 0600 and atomically written.
`intent.json` and planned rows are persisted before dispatch. `observations.json`
retains all warmup/measured/unstarted rows; `resources.json` retains actual samples;
`private-state.json` retains opaque created/uncertain resource IDs for the owner;
`report.json` and `summary.md` report BLOCKED, PARTIAL_FAILED_OR_UNKNOWN, or
COMPLETE_BASELINE_NO_SLO_EVALUATION. The last status requires every selected case,
correlation, required stage/correctness/metering evidence and actual service
samples. It is never `accepted:true`, production readiness, provider acceptance,
capacity or an SLO pass. Retain failed directories. Only a separately reviewed,
redacted measured report belongs under `reports/`; this source delivery does not
create one.

Native regressions: `scripts/new/native-container.sh test -p ai-platform-tools
--lib performance_baseline::tests -- --test-threads=1`, then the standalone binary
CLI regression. No Cargo, Docker, network, real-provider or live baseline run was
performed while implementing this workflow. Existing builds without the required
stage/event/instance instrumentation yield partial/unknown facts; source support
for the controller does not make those measurements available retroactively.
