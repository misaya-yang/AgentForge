# U-CW-04: independent credential-free sandbox service

The control Worker now dispatches `execute_python_code` **only** through `RemoteSandboxBroker`; an existing `with_python_executor(LocalPythonSandboxBroker)` configuration remains source-compatible but cannot launch user code in the control process. Without the remote endpoint/key configuration Python fails closed. Office and all other existing tools, original Runtime/Gateway HMAC lease validation, fences, approvals, tenant checks and artifact persistence remain in the control Worker.

The standalone `ai-platform-python-sandbox` process owns the existing U-CW-01 Linux executor in a separate container/process tree. It listens only on a private Unix socket, has no TCP listener, no DB adapter/configuration, no platform token or HMAC secret, and only an Ed25519 **public** verification key. Its production constructor rejects non-allowlisted environment variables and accessible known credential mounts; its binary closes inherited descriptors >=3 before runtime initialization. An explicit default-deny network namespace/container configuration remains required. No signer/private key is needed by this process.

The purpose-separated `sandbox-job/v1` contract signs code, inputs, limits, durable job ID, tenant/user/session, run/tool-call identity, request hash and short expiry. It does not serialize a platform lease, approval proof or bearer token. Poll and cancellation messages have distinct signed operations and the exact job digest/scope. The service constructs only synthetic unsigned local metadata for the existing executor. Results are bytes returned over the Unix socket; only the control Worker persists artifacts using its existing tenant-bound broker.

New dispatches are bounded by a separate service semaphore and a bounded journal registry. Duplicate IDs replay the original result even when capacity is full; changed payloads conflict and foreign scopes are hidden. Initial metadata is fsynced before launching code; result then terminal marker are atomically fsynced. A journal has one `flock` owner. On restart an unfinished job becomes unknown without re-execution, or reconciles from its already-fsynced result. Only expired terminal receipts are pruned after configured retention; original expired signatures cannot execute again. The journal complements, and does not replace, the control plane's durable fence/idempotency authority. Transport uncertainty returns `SideEffectUnknown`; the client never automatically resubmits an execution. Signed cancellation reaches the existing cancellation/cgroup cleanup path.

## Required integration by the primary writer

Cargo/lockfile, Compose and shared source-manifest changes belong to the primary writer. Main/bootstrap and the Worker/sandbox Dockerfiles are now implemented directly, as detailed below. The contract requires the following dependency, which the primary writer has already acknowledged adding:

```toml
ed25519-dalek = { workspace = true }
```

The workspace already defines 2.2.0. The new binary at `src/bin/ai-platform-python-sandbox.rs` is auto-discovered by Cargo; it may be listed explicitly:

```toml
[[bin]]
name = "ai-platform-python-sandbox"
path = "src/bin/ai-platform-python-sandbox.rs"
```

The Worker already has all required dependencies, including Unix-capable reqwest 0.12, Tokio, Axum, libc and hex. Keep source-lock/overlay identities and the controlled Docker build target list synchronized. Build/copy the new binary alongside the existing `ai-platform-python-supervisor` binary. The control image no longer needs to execute the supervisor; do not copy any control configuration or secret into the sandbox image.

Use a **distinct** clean sandbox final image based on the selected Python runtime, with the same runtime libraries and `unshare` utility required by U-CW-01. Do not derive it from the configured Worker runtime stage: inherited Worker `ENV` keys deliberately fail the sandbox environment guard. Its image owns private 0700 directories `/workspace`, `/run/agentforge-sandbox`, `/var/lib/agentforge-sandbox/jobs` under UID/GID 10001. Copy only the sandbox and supervisor executables plus their runtime dependencies, set USER 10001:10001, no platform ENV, and ENTRYPOINT `["ai-platform-python-sandbox"]`. Perform the normal exact-binary/schema/image admission and library inspection; source-only availability is not acceptance.

### Control startup

The implemented main/bootstrap uses typed optional fields `sandbox_socket`, `sandbox_key_id`, `sandbox_signing_key_file` and a **new purpose-specific** 32-byte Ed25519 seed from the control-only secret file. It does not reuse the capability HMAC secret or put seed bytes in args/env. Its equivalent builder wiring is:

```rust
use ai_platform_capability_contract::sandbox_job::SandboxJobSigner;
use ai_platform_capability_worker::sandbox_transport::RemoteSandboxBroker;
let bytes = std::fs::read(&args.sandbox_signing_key_file)?;
let seed: [u8; 32] = bytes.try_into()
    .map_err(|_| anyhow::anyhow!("sandbox signing key must be 32 bytes"))?;
let signer = Arc::new(SandboxJobSigner::new(args.sandbox_key_id.clone(), seed)
    .map_err(anyhow::Error::msg)?);
let python_executor = Arc::new(RemoteSandboxBroker::new(args.sandbox_socket.clone(), signer)?);
// On WorkerState, before router(state):
let state = state.with_remote_python_executor(python_executor);
```

The old local broker construction/import/builder were removed from main; the existing artifact-store wiring is retained. If no remote service/key is selected, omit the new builder; only Python is unavailable. Its health checks the real remote UDS readiness endpoint with a one-second bound. The immutable plugin registry still accepts existing configured executors, but actual dispatch has no local fallback.

### Separate service / Compose seam

Keep the existing project and volume ownership. Create one distinct service; share **only** the private socket volume with control. Use the same selected UID/GID for both sides of the 0600 Unix socket. The control-only signing file is not mounted in this service. The public key hex in arguments is public configuration.

```yaml
agent-python-sandbox:
  image: ${AGENT_PYTHON_SANDBOX_IMAGE:?Select the admitted sandbox image}
  entrypoint: ["ai-platform-python-sandbox"]
  command:
    - --public-key-id
    - ${AGENT_SANDBOX_PUBLIC_KEY_ID:?Select the verifier key id}
    - --public-key-hex
    - ${AGENT_SANDBOX_PUBLIC_KEY_HEX:?Select the Ed25519 public key}
    - --socket
    - /run/agentforge-sandbox/control.sock
  network_mode: none
  user: "10001:10001"
  cap_drop: [ALL]
  security_opt:
    - no-new-privileges:true
    # Adjust the relative path to the first Compose file as in README.md.
    - seccomp=./deploy/security/capability-worker-seccomp.json
  volumes:
    - sandbox-control-socket:/run/agentforge-sandbox
    - sandbox-private-workspaces:/workspace
    - sandbox-private-journal:/var/lib/agentforge-sandbox/jobs
    - type: bind
      source: ${AGENT_CAPABILITY_SANDBOX_CGROUP_DELEGATION:?Explicit sandbox-only delegation}
      target: /sys/fs/cgroup/agentforge-sandbox
      bind:
        create_host_path: false
  mem_limit: 1536m
  cpus: 2
  init: true
```

On `agent-capability-worker` add only `sandbox-control-socket:/run/agentforge-sandbox:ro`, mount its new signing-key file read-only **there only**, and wire its socket path. Remove its Python workspace/cgroup mount and namespace-specific seccomp exceptions once all Python execution uses this service; preserve any unrelated Office workspace requirement. A read-only mount of the Unix socket directory allows connecting without granting control permission to replace the socket. Initialize socket/workspace/journal volume ownership through the owning image's 0700 UID10001 directories; never chmod them world-writable. Do not add DB/provider/proxy/shared env anchors, service-account tokens, Docker sockets, host network, a privileged flag, extra capabilities or any platform secret volume to the sandbox. Mount only its own explicitly delegated cgroup subtree, not the control Worker's hierarchy.

## Verification boundary

Added contract seal/tampering/expiry/no-lease tests; deterministic service tests cover full-capacity replay, same-key concurrency, conflicts/cross-scope denial, signed cancel, bad signature/no dispatch, restart unknown/result reconciliation, journal ownership, environment/file credential guards and a real UDS transport roundtrip with a deterministic executor. These are protocol tests, not sandbox isolation acceptance.

The ignored real-process tests launch the exact standalone binary with cleared environment, inspect `/proc/<pid>/environ`, known credential paths and open descriptors, send sealed work through the actual U-CW-01 executor, inspect the user process's environment, and verify a process started with an inherited control token fails closed. Run them inside the exact dedicated Linux image with required delegation. Known-path guards/these tests do not replace the release's full image/mount inspection for unexpected credentials.

Suggested primary-writer gates (not executed during implementation):

```sh
cargo test -p ai-platform-capability-contract sandbox_job
cargo test -p ai-platform-capability-worker --lib sandbox_service
cargo test -p ai-platform-capability-worker --lib sandbox_domain_acceptance_tests -- --ignored --nocapture
cargo test -p ai-platform-capability-worker --lib python_code_execution::escape_tests -- --ignored --nocapture
```

Then run control-to-separate-container Python/artifact execution, foreign-tenant cancellation, full-queue replay, control/sandbox kill-before/after-journal/result faults, cancellation/timeout cleanup and mixed-load p95 against the exact admitted images. Verify sandbox env, mounted files and descriptors contain no platform token, secret, DSN or signing material, and prove its network mode/egress denial. No Cargo/Docker commands, host resource creation, kernel changes or external mutations were performed for this implementation.

## Implemented bootstrap and CPython 3.14 follow-up

The control `main.rs` now owns the three optional fields described above (`AI_PLATFORM_SANDBOX_SOCKET`, `AI_PLATFORM_SANDBOX_KEY_ID`, `AI_PLATFORM_SANDBOX_SIGNING_KEY_FILE`) and calls `sandbox_bootstrap::configure`. It rejects partial selection, symlink/non-regular/wrong-length key files and keys not private/owned by the control UID. It never constructs the legacy local broker. Bootstrap tests cover omitted/partial configuration and private key-file validation. No new dependency beyond the already-requested contract ed25519-dalek is needed.

Two auto-discovered binary targets must be included by the shared manifest/build writer:

```toml
[[bin]]
name = "ai-platform-python-sandbox"
path = "src/bin/ai-platform-python-sandbox.rs"

[[bin]]
name = "ai-platform-python-sandbox-health"
path = "src/bin/ai-platform-python-sandbox-health.rs"
```

`Dockerfile.capability-worker` is now a Rust-only Debian 13 slim control runtime. `Dockerfile.python-sandbox` builds the CPython 3.14/Debian 13 sandbox with exact-hash amd64/arm64 wheel locks, a separate read-only distribution tree and tenant allowlists. Their identical serialized Rust builder stages can reuse the existing BuildKit cache. Both use an explicit trusted CA bundle bootstrap plus `Acquire::https::CaInfo` and `APT::Update::Error-Mode=any`; neither disables TLS verification nor treats partial apt results as success. The standalone health binary probes only the Unix socket and requires no Python, network credential or private config.

The controlled source build context must include `deploy/agent-runtime-source/python/` as `python/` beside `codex-rs`, LICENSE and NOTICE for the sandbox Dockerfile's COPY. Shared source manifests/locks must include every new Rust source, both Dockerfiles and all lock/catalog assets. New build args are `CAPABILITY_RUST_BUILD_IMAGE`, `CAPABILITY_WORKER_RUNTIME_IMAGE` and `PYTHON_SANDBOX_IMAGE`. Defaults identify the family only (`rust:1.95.0-bookworm`, `debian:trixie-slim`, `python:3.14-slim-trixie`); the primary writer must measure and select the exact official patch/digest for release. These defaults are not digest verification evidence.

Delivery should add the three control startup variables and mount only its private seed file read-only. Both images now pre-create the shared socket directory as UID/GID10001 mode0700, so named-volume copy-up has matching ownership regardless of startup order. The sandbox uses the new image/entrypoint, `ai-platform-python-sandbox-health`, network_mode:none and only socket/workspace/journal/delegated-cgroup mounts. To activate authorized optional packages, add:

```yaml
command:
  # Retain --public-key-id, --public-key-hex and --socket arguments.
  - --package-policy
  - /etc/agentforge/python-package-policy.json
volumes:
  # Retain the four sandbox-only mounts documented above.
  - ${AGENT_SANDBOX_TENANT_PACKAGE_POLICY:?Select the matching catalog/tenant policy}:/etc/agentforge/python-package-policy.json:ro
read_only: true
```

The example policy and metadata/hash limits are documented in `deploy/agent-runtime-source/python/README.md`. Tenant package grants are enforced by per-execution Landlock roots, not merely by import lint. This required a narrow extension of the existing supervisor/isolation config: immutable server-derived package roots, read-only rules, controlled sys.path bootstrap and workspace-only cache/temp env. The existing namespace, cgroup, no-network, no-fork, cancellation and unknown-effect rules remain.

Additional unrun acceptance command in the exact admitted Linux sandbox image:

```sh
cargo test -p ai-platform-capability-worker --lib python_code_execution::package_acceptance_tests -- --ignored --nocapture
```

It exercises NumPy/pandas/SciPy, PNG rendering, Pillow, XLSX output and an unauthorized tenant that tries both direct package file reads and a manually modified sys.path. Default policy unit tests check dependency closure, cross-tenant selection, unknown-tenant denial, catalog hash binding and path rejection. Build-time import/ldd/no-compiler checks are implemented in Dockerfiles but were not executed while preparing this source.

Current source ownership handoff: platform-lease verification may proceed in `http_service/execution.rs` at `lease_verification::verify(&request, &state.lease_secret)` (the platform-verifier agent has already installed this seam) and `http_service.rs` at the WorkerState lease-verifier field/construction. These are independent of `sandbox_job` purpose signatures. Main/bootstrap and package/image source are frozen after this handoff; any platform verifier startup args can be integrated by the primary/main writer without changing the dedicated sandbox seed/key contract.

Additional startup/policy unit gates, not run during this implementation:

```sh
cargo test -p ai-platform-capability-worker --bin ai-platform-capability-worker sandbox_bootstrap
cargo test -p ai-platform-capability-worker --lib python_package_policy
```

## Compose integration implemented

The root Compose file and `deploy/local/compose.runtime.yml` now include the
independent `agent-python-sandbox` service in the existing project. Its only
transport is `/run/agentforge-sandbox/control.sock`; `network_mode: none` has no
ports, exposed TCP endpoints, proxy environment, env files, configs or platform
secrets. It uses the independent admitted image, reviewed seccomp profile,
non-root UID/GID10001, read-only root, dropped capabilities, no-new-privileges,
bounded scratch/PIDs/memory/CPU and UDS-only health. The existing control Worker
retains its Office workspace but loses Python cgroup/namespace exceptions. It
receives a read-only socket directory and its purpose signing seed at
`/run/agentforge-sandbox-signing/signing.seed` only, then awaits sandbox UDS health.

Three new project-owned volumes are reserved exclusively for this pair:
`sandbox-control-socket`, `sandbox-private-workspaces`, and
`sandbox-private-journal`. Only the socket directory is shared with control;
workspace and journal never leave the sandbox. Existing volumes and public
ports were retained. Both admitted images must provide the documented private
UID10001 directories for volume copy-up; do not fix ownership through a
privileged service or world-writable chmod.

The primary writer must provision these inputs without logging their values:

- `AGENT_PYTHON_SANDBOX_IMAGE`: admitted independent sandbox image identity.
- `AGENT_SANDBOX_PUBLIC_KEY_ID` and `AGENT_SANDBOX_PUBLIC_KEY_HEX`: public
  purpose-specific Ed25519 verifier metadata.
- `AGENT_SANDBOX_SIGNING_KEY_FILE`: absolute existing control-only file containing
  exactly 32 raw seed bytes, owned/readable by UID10001 with owner-only permissions.
  Derive the above public key from this new purpose seed; never reuse a platform
  HMAC/lease signing credential. Only the file path enters configuration.
- `AGENT_CAPABILITY_SANDBOX_CGROUP_DELEGATION`: explicitly prepared empty cgroup2
  subtree owned by this sandbox. Compose refuses to create its host path. This
  replaces the former control-only `AGENT_CAPABILITY_WORKER_CGROUP_DELEGATION` use.

`deploy/local/compose.sandbox.yml` now adjusts sandbox-specific resource limits;
it no longer delegates anything to the control Worker. The sandbox-only cgroup
bind is mandatory in root/local runtime configuration, so missing delegation
cannot silently enable an unisolated fallback. Kernel/controller checks still
belong to the executor and require real Linux acceptance.

Optional `deploy/compose/docker-compose.sandbox-packages.yml` preserves all public
verifier/path arguments and adds an existing read-only policy file selected by
`AGENT_SANDBOX_TENANT_PACKAGE_POLICY`. The policy must match the admitted catalog;
it does not grant packages globally or install anything at startup. This overlay
can be merged with either root Compose or the same local base/runtime project.

`container-security source` now checks this seam and the package overlay, including
separate image/UIDs, UDS-only health, public arguments, exclusive private state,
read-only control socket, signing-file isolation and sandbox-only cgroups. New
negative tests reject platform/proxy env, private command flags, TCP, missing
service/key/delegation, shared private state, alternative seed mount names,
control cgroups, wrong UDS owner, relaxed health dependencies and writable control
socket directories. The operational gate remains Rust.

An isolated Rust harness using the existing cached dependencies passed all ten
container-security test groups. Public Compose YAML and source format checks
are separate from Docker rendering and runtime admission. No Docker/Cargo build,
start, private input read/generation, volume mutation or cgroup resource creation
was performed; the old infrastructure remains running with its previous applied
configuration. Exact-image environment/mount inspection, real UDS/Python/artifact
execution, restart/cancel/fault handling and kernel isolation remain unverified.

### Controlled build entrypoints

`make agent-python-sandbox-build-local` selects the independent Dockerfile through
`scripts/rust/build-update.sh --artifact sandbox`. `--artifact all` now runs
Runtime, control Worker, then sandbox serially under the same `rust-build` lock;
App Server remains an explicit separate artifact. Direct Worker/sandbox wrapper
calls re-enter that preflight unless the shared lock is already held. The clean
platform/source checks, source-lock verification, existing-builder resource
ceilings, host memory guard, Cargo `--locked`/one-job rule, local-only behavior
and post-build immutable image admission remain required.

Both control and sandbox contexts include the verified upstream archive, Rust
overlay and public Python lock/catalog directory as `python/`. Only the dedicated
sandbox Dockerfile copies package assets into its runtime. The sandbox wrapper
checks source/overlay/schema plus `python_sandbox`/binary labels before
`source-lock record-local-image --artifact python_sandbox`; a control image cannot
satisfy this admission. Exact base references are required through
`CAPABILITY_RUST_BUILD_IMAGE`, `CAPABILITY_WORKER_RUNTIME_IMAGE` and
`PYTHON_SANDBOX_IMAGE` (sandbox only). These are public image inputs and must use
independently verified `@sha256:` pins selected by the primary writer. No digest
was invented or measured while implementing the wrapper. Base selections are
also retained as OCI labels for inspection.

Build syntax and static command/mutation contracts were checked without executing
builds or Docker commands. The existing shared source-lock/manifest writer must
refresh build-input hashes for the changed/new wrappers before any actual build.
This source change provides no successful image, wheel-import, size/scan or
runtime acceptance evidence.
